import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';
import { PassThrough } from 'node:stream';
import { canonical, digest } from '../packages/contracts/src/canonical.js';
import { runCommand } from '../packages/host-runtime/src/process.js';
import { PhoneJournal, phoneOutcomeReservation } from '../services/phone-bridge/src/runtime/journal.js';
import type { PhoneJournalLimits, RetiredVoiceTask } from '../services/phone-bridge/src/runtime/journal.js';
import { PhoneAdmission } from '../services/phone-bridge/src/runtime/admission.js';
import { PhoneNativeClient, phoneFrameBytes } from '../services/phone-bridge/src/runtime/native.js';
import type { PhoneIntent, PhoneReply } from '../services/phone-bridge/src/runtime/native.js';
import phoneManifest from '../services/phone-bridge/deploy.json' with { type: 'json' };

const owner = { hostId: 'phone-fixture-host', serviceNodeId: 'phone-fixture-node' };
const tick = () => new Promise<void>(done => setImmediate(done));
function fixture(t: TestContext, limits?: PhoneJournalLimits) {
  const root = mkdtempSync(join(tmpdir(), 'ivy-phone-journal-')), handles: PhoneJournal[] = [];
  const open = (identity = owner) => { const journal = new PhoneJournal(root, identity, limits); handles.push(journal); return journal; };
  t.after(() => { for (const journal of handles) journal.close(); assert.ok(relative(tmpdir(), root).startsWith('ivy-phone-journal-')); rmSync(root, { recursive: true, force: true }); });
  return { root, open, filename: join(root, 'phone-commands.sqlite') };
}
function intent(epoch: string, method: PhoneIntent['method'] = 'call.dial', callId: string | null = randomUUID()): PhoneIntent {
  return { epoch, operationId: randomUUID(), method, callId: method.startsWith('call.') ? callId : null, requestHash: digest('synthetic Phone request') };
}
function reply(original: Pick<PhoneIntent, 'epoch'>, result: unknown = { accepted: true }): PhoneReply {
  return { version: 1, epoch: original.epoch, requestId: 1, ok: true, result, error: null };
}

test('Phone deployment declares the actual journal format and refuses older development storage', t => {
  const f = fixture(t); f.open().close();
  const db = new DatabaseSync(f.filename);
  try {
    const format = Number(db.prepare('PRAGMA user_version').get()!['user_version']);
    assert.deepEqual(phoneManifest.storage, { minReadableFormat: format, maxReadableFormat: format, writeFormat: format });
  } finally { db.close(); }
  for (const format of [2, 4]) {
    const obsolete = new DatabaseSync(f.filename); obsolete.exec('PRAGMA user_version=' + format); obsolete.close();
    assert.throws(() => f.open(), { code: 'unsupported_storage' });
  }
});

test('Phone archive queue excludes historical deletion evidence after reopen', t => {
  const f = fixture(t), journal = f.open(), epoch = randomUUID(); journal.beginEpoch(epoch);
  const admission = new PhoneAdmission({ incoming: [], recipients: [{ id: 'personal', destination: 'sip:fixture@127.0.0.1' }] });
  const call = journal.admitCall(admission.outgoing(epoch, randomUUID(), 'main', 'personal', 'voice'));
  const archived: RetiredVoiceTask = {
    threadId: randomUUID(), principalId: call.principalId, successorId: randomUUID(), callId: call.callId,
    phase: 'archived', archiveOperationId: randomUUID(), deleteOperationId: null,
    archivedAt: '2026-08-25T10:00:00.000Z', deleteAfter: '2026-09-01T10:00:00.000Z',
  };
  const deleting: RetiredVoiceTask = { ...archived, threadId: randomUUID(), phase: 'deleting', deleteOperationId: randomUUID() };
  const pending: RetiredVoiceTask = {
    ...archived, threadId: randomUUID(), phase: 'pending_archive', archiveOperationId: null, archivedAt: null, deleteAfter: null,
  };
  journal.close();
  const db = new DatabaseSync(f.filename);
  try {
    const insert = db.prepare('INSERT INTO meta(key,value) VALUES (?,?)');
    for (const task of [archived, deleting, pending]) insert.run(`voiceRetired:${task.threadId}`, canonical(task));
  } finally { db.close(); }
  const reopened = f.open();
  assert.deepEqual(reopened.retiredVoicePage(), [pending], 'previous deletion work never enters the archive worker');
  for (const task of [archived, deleting]) assert.deepEqual(reopened.retiredVoiceTask(task.threadId), task,
    'historical deletion evidence remains unchanged');
});

test('Phone audio preparation cannot be replaced after receipt or owner loss', t => {
  for (const completed of [false, true]) {
    const f = fixture(t), journal = f.open(), epoch = randomUUID(); journal.beginEpoch(epoch);
    const original = intent(epoch, 'call.audio.prepare'); journal.submit(original);
    if (completed) journal.finish(original, reply(original, { callId: original.callId, state: 'suspended', route: null }));
    assert.throws(() => journal.submit({ ...original, operationId: randomUUID() }), { code: 'phone_call_conflict' });
    const hangup = { ...original, operationId: randomUUID(), method: 'call.hangup' as const };
    journal.submit(hangup); journal.finish(hangup, reply(hangup));
    journal.loseEpoch(epoch); journal.close();
    const reopened = f.open(), next = randomUUID(); reopened.beginEpoch(next);
    assert.equal(reopened.get(original.operationId)!.phase, completed ? 'result' : 'outcome_unknown');
    assert.throws(() => reopened.submit({ ...original, epoch: next, operationId: randomUUID() }), { code: 'phone_call_conflict' });
    assert.equal(reopened.get(hangup.operationId)!.phase, 'result');
  }
});

test('Phone client commits credential-free original intent before IPC and reuses saved evidence after reopen', async t => {
  const f = fixture(t), journal = f.open(), epoch = randomUUID(), operationId = randomUUID(), callId = randomUUID();
  journal.beginEpoch(epoch);
  const input = new PassThrough(), output = new PassThrough(), requests: Record<string, unknown>[] = [];
  input.on('data', bytes => {
    assert.equal(journal.get(operationId)!.phase, 'submitted');
    requests.push(JSON.parse(String(bytes).trim()) as Record<string, unknown>);
  });
  const client = new PhoneNativeClient(epoch, input, output, journal.hooks);
  t.after(() => { client.close(); input.destroy(); output.destroy(); });
  const args = { callId, destination: 'sip:fixture@127.0.0.1', username: 'synthetic-user', password: 'synthetic-password-never-retained', ringSeconds: 1, waiting: false };
  const pending = client.request('call.dial', args, operationId); await tick();
  assert.equal(requests.length, 1);
  const original = journal.get(operationId)!.intent;
  assert.equal(original.callId, callId); assert.equal(original.requestHash, digest(canonical({ method: 'call.dial', params: args })));
  output.write(JSON.stringify(reply(original, { id: callId, state: 'connected' })) + '\n');
  await pending;
  const saved = journal.get(operationId)!; assert.equal(saved.phase, 'result');
  await assert.rejects(client.request('call.dial', args, operationId), { code: 'phone_operation_retained', outcome: 'completed' });
  await assert.rejects(client.request('call.dial', { ...args, destination: 'sip:other@127.0.0.1' }, operationId), { code: 'mutation_conflict' });
  assert.equal(requests.length, 1);
  client.close(); journal.loseEpoch(epoch); journal.close();
  const bytes = readFileSync(f.filename).toString('utf8');
  assert.equal(bytes.includes(args.password), false); assert.equal(bytes.includes(args.username), false); assert.equal(bytes.includes(args.destination), false);
  assert.deepEqual(f.open().get(operationId), saved);
});

test('Phone lost process epoch keeps the original call unknown and prohibits a new dial identity', t => {
  const f = fixture(t), journal = f.open(), epoch = randomUUID(); journal.beginEpoch(epoch);
  const original = intent(epoch); journal.submit(original);
  assert.throws(() => journal.submit(original), { code: 'phone_operation_retained', outcome: 'unknown' });
  assert.throws(() => journal.submit({ ...original, operationId: randomUUID() }), { code: 'phone_call_conflict' });
  assert.throws(() => journal.submit({ ...original, method: 'call.answer', operationId: randomUUID() }), { code: 'phone_call_conflict' });
  const hangup = { ...original, operationId: randomUUID(), method: 'call.hangup' as const }; journal.submit(hangup);
  journal.finish(hangup, reply(hangup)); // Cleanup stays available despite a pending dial.
  journal.loseEpoch(epoch);
  assert.equal(journal.get(original.operationId)!.phase, 'outcome_unknown');
  assert.equal(journal.get(hangup.operationId)!.phase, 'result');
  assert.throws(() => journal.finish(original, reply(original)), { code: 'phone_receipt_conflict' });
  const next = randomUUID(); journal.beginEpoch(next); journal.loseEpoch(epoch); assert.equal(journal.epoch, next);
  assert.throws(() => journal.submit({ ...original, operationId: randomUUID(), epoch: next }), { code: 'phone_call_conflict' });
  assert.throws(() => journal.submit({ ...original, epoch: next }), { code: 'mutation_conflict' });
  journal.close(); const reopened = f.open();
  assert.equal(reopened.epoch, next); assert.equal(reopened.get(original.operationId)!.phase, 'outcome_unknown');
});

test('Phone journal preserves exclusive ownership, active epoch and host identity across reopen', t => {
  const f = fixture(t), journal = f.open(), epoch = randomUUID(); journal.beginEpoch(epoch);
  assert.throws(() => f.open(), { code: 'phone_owner_already_running' });
  assert.throws(() => journal.beginEpoch(randomUUID()), { code: 'phone_epoch_active' });
  journal.close();
  assert.throws(() => f.open({ ...owner, serviceNodeId: 'different-phone' }), { code: 'target_conflict' });
  const reopened = f.open(); assert.equal(reopened.epoch, epoch);
  assert.throws(() => reopened.beginEpoch(randomUUID()), { code: 'phone_epoch_active' });
  reopened.loseEpoch(epoch); assert.throws(() => reopened.beginEpoch(epoch), { code: 'mutation_conflict' });
});

test('Phone reserves full-sized results before effects and retains evidence at count, byte and epoch limits', t => {
  for (const boundary of ['count', 'bytes', 'epochs'] as const) {
    const f = fixture(t, { maxOperations: boundary === 'count' ? 1 : 3, maxBytes: phoneOutcomeReservation * (boundary === 'bytes' ? 1 : 3), maxEpochs: 1 });
    const journal = f.open(), epoch = randomUUID(); journal.beginEpoch(epoch); const original = intent(epoch); journal.submit(original);
    const full = reply(original, { payload: '' });
    (full.result as { payload: string }).payload = 'x'.repeat(phoneFrameBytes - Buffer.byteLength(canonical(full)));
    journal.finish(original, full); const before = journal.get(original.operationId), usage = journal.status();
    if (boundary === 'epochs') {
      journal.loseEpoch(epoch); journal.beginEpoch(randomUUID());
      assert.equal(journal.status().epochs, 1); assert.ok(journal.archiveStatus().records > 0);
      const next = journal.epoch!; journal.loseEpoch(next); assert.throws(() => journal.beginEpoch(epoch), { code: 'mutation_conflict' });
    }
    else assert.throws(() => journal.submit(intent(epoch)), { code: 'phone_journal_capacity' });
    if (boundary !== 'epochs') assert.deepEqual(journal.status(), usage);
    assert.deepEqual(journal.get(original.operationId), before);
    journal.close(); assert.deepEqual(f.open().get(original.operationId), before);
  }
});

test('Phone failed durable writes roll back admission, receipts and epoch retirement without losing evidence', t => {
  const f = fixture(t), setup = f.open(), epoch = randomUUID(); setup.beginEpoch(epoch); setup.close();
  let db = new DatabaseSync(f.filename);
  db.exec("CREATE TRIGGER admission_failure BEFORE INSERT ON commands BEGIN SELECT RAISE(ABORT, 'fixture admission failure'); END;"); db.close();
  let journal = f.open(); const original = intent(epoch), usage = journal.status();
  assert.throws(() => journal.submit(original)); assert.equal(journal.get(original.operationId), null); assert.deepEqual(journal.status(), usage); journal.close();
  db = new DatabaseSync(f.filename); db.exec('DROP TRIGGER admission_failure;'); db.close();
  journal = f.open(); journal.submit(original); journal.close();
  db = new DatabaseSync(f.filename); db.exec("CREATE TRIGGER result_failure BEFORE UPDATE ON commands BEGIN SELECT RAISE(ABORT, 'fixture result failure'); END;"); db.close();
  journal = f.open(); const before = journal.get(original.operationId);
  assert.throws(() => journal.finish(original, reply(original))); assert.deepEqual(journal.get(original.operationId), before);
  assert.throws(() => journal.loseEpoch(epoch)); assert.equal(journal.epoch, epoch); assert.deepEqual(journal.get(original.operationId), before);
});

test('Phone abrupt owner death retains the possible effect and requires explicit original-epoch retirement', { timeout: 20000 }, async t => {
  const f = fixture(t), original = intent(randomUUID()), script = join(f.root, 'crash.mjs'), marker = join(f.root, 'synthetic-effect.txt');
  const module = new URL('../services/phone-bridge/src/runtime/journal.js', import.meta.url).href;
  writeFileSync(script, `import {PhoneJournal} from ${JSON.stringify(module)}; import {appendFileSync} from 'node:fs';
    const journal = new PhoneJournal(${JSON.stringify(f.root)}, ${JSON.stringify(owner)});
    journal.beginEpoch(${JSON.stringify(original.epoch)}); journal.submit(${JSON.stringify(original)});
    appendFileSync(${JSON.stringify(marker)}, 'one synthetic effect'); process.kill(process.pid, 'SIGKILL');`);
  await assert.rejects(runCommand({ executable: 'node', args: [script], timeoutMs: 10000 }, resolve('.'), { node: process.execPath }),
    { code: 'command_failed' }, 'the original synthetic child must actually execute and die; missing launchers or timeouts are not crash evidence');
  assert.equal(readFileSync(marker, 'utf8'), 'one synthetic effect');
  const reopened = f.open(); assert.equal(reopened.epoch, original.epoch); assert.equal(reopened.get(original.operationId)!.phase, 'submitted');
  assert.throws(() => reopened.beginEpoch(randomUUID()), { code: 'phone_epoch_active' });
  reopened.loseEpoch(original.epoch); assert.equal(reopened.get(original.operationId)!.phase, 'outcome_unknown');
  assert.throws(() => reopened.submit(original), { code: 'phone_operation_retained' }); assert.equal(readFileSync(marker, 'utf8'), 'one synthetic effect');
});

test('Phone rejects altered ownership, malformed intents and conflicting or late receipts', t => {
  const f = fixture(t), journal = f.open(), original = intent(randomUUID()); journal.beginEpoch(original.epoch);
  for (const invalid of [{ ...original, callId: null }, { ...original, operationId: 'invalid' }, { ...original, method: 'status' },
    { ...original, epoch: randomUUID() }, { ...original, requestHash: 'not-a-hash' }]) assert.throws(() => journal.submit(invalid as PhoneIntent));
  assert.equal(journal.status().operations, 0); journal.submit(original);
  assert.throws(() => journal.finish(original, { ...reply(original), epoch: randomUUID() }), { code: 'phone_receipt_conflict' });
  assert.throws(() => journal.finish(original, { ...reply(original), ok: false }), { code: 'phone_receipt_conflict' });
  assert.throws(() => journal.finish({ ...original, requestHash: digest('different') }, reply(original)), { code: 'phone_receipt_conflict' });
  const unknown: PhoneReply = { ...reply(original), ok: false, result: null, error: 'operation_outcome_unknown' };
  const saved = journal.finish(original, unknown); assert.equal(saved.phase, 'result'); assert.equal(saved.receipt!.error, 'operation_outcome_unknown');
  assert.throws(() => journal.submit(original), { code: 'phone_operation_retained', outcome: 'unknown' });
  assert.deepEqual(journal.finish(original, { ...unknown, requestId: 2 }), saved);
  assert.throws(() => journal.finish(original, reply(original)), { code: 'phone_receipt_conflict' });
  journal.loseEpoch(original.epoch); assert.deepEqual(journal.get(original.operationId), saved);
});

test('Phone keeps a saved native call uncertainty unknown even when the response envelope succeeded', t => {
  const f = fixture(t), journal = f.open(), original = intent(randomUUID()); journal.beginEpoch(original.epoch); journal.submit(original);
  journal.finish(original, reply(original, { id: original.callId, state: 'outcome_unknown' }));
  assert.throws(() => journal.submit(original), { code: 'phone_operation_retained', outcome: 'unknown' });
});

test('resolved unknown command evidence moves to the receipt archive before active capacity fills', t => {
  const f = fixture(t, { maxOperations: 2, maxBytes: phoneOutcomeReservation * 3, maxEpochs: 2 });
  const journal = f.open(), epoch = randomUUID(); journal.beginEpoch(epoch);
  const original = intent(epoch, 'registration.reconnect', null); journal.submit(original);
  const saved = journal.finish(original, { ...reply(original), ok: false, result: null, error: 'operation_outcome_unknown' });
  journal.submit(intent(epoch, 'registration.reconnect', null));
  assert.equal(journal.status().operations, 1);
  assert.deepEqual(journal.get(original.operationId), saved);
  assert.throws(() => journal.submit(original), { code: 'phone_operation_retained', outcome: 'unknown' });
});

test('Phone preparation cannot be repeated under another command and saved result copies are isolated', t => {
  const f = fixture(t), journal = f.open(), original = intent(randomUUID(), 'call.prepare'); journal.beginEpoch(original.epoch); journal.submit(original);
  const saved = journal.finish(original, reply(original, { id: original.callId, state: 'prepared' }));
  assert.throws(() => journal.submit({ ...original, operationId: randomUUID() }), { code: 'phone_call_conflict' });
  (saved.receipt!.result as { state: string }).state = 'corrupted'; saved.intent.callId = randomUUID();
  const retained = journal.get(original.operationId)!;
  assert.equal((retained.receipt!.result as { state: string }).state, 'prepared'); assert.equal(retained.intent.callId, original.callId);
});
