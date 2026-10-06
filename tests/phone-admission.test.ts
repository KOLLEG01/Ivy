import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { PhoneAdmission, phoneCallReservation } from '../services/phone-bridge/src/runtime/admission.js';
import type { PhoneCall, PhoneIncoming, PhonePolicyDefinition } from '../services/phone-bridge/src/runtime/admission.js';
import { PhoneJournal, phoneOutcomeReservation } from '../services/phone-bridge/src/runtime/journal.js';
import type { PhoneJournalLimits } from '../services/phone-bridge/src/runtime/journal.js';
import type { PhoneIntent } from '../services/phone-bridge/src/runtime/native.js';
import { digest } from '../packages/contracts/src/canonical.js';

const definition: PhonePolicyDefinition = {
  recipients: [{ id: 'personal', destination: 'sip:user@127.0.0.1' }, { id: 'other', destination: 'sip:other@127.0.0.1' }],
  incoming: [{ peerAddress: '127.0.0.1', transport: 'udp', fromUri: 'sip:known@fixture' }]
};
const policy = new PhoneAdmission(definition);
function fixture(t: TestContext, limits?: PhoneJournalLimits) {
  const root = mkdtempSync(join(tmpdir(), 'ivy-phone-admission-')), handles: PhoneJournal[] = [];
  const open = () => { const journal = new PhoneJournal(root, { hostId: 'fixture', serviceNodeId: 'phone' }, limits); handles.push(journal); return journal; };
  t.after(() => { for (const handle of handles) handle.close(); assert.equal(dirname(resolve(root)), resolve(tmpdir()));
    assert.ok(basename(root).startsWith('ivy-phone-admission-')); rmSync(root, { recursive: true, force: true }); });
  return { root, open };
}
function command(call: PhoneCall, method: PhoneIntent['method']): PhoneIntent {
  return { epoch: call.epoch, callId: call.callId, method, operationId: randomUUID(), requestHash: digest('fixture command') };
}
function release(journal: PhoneJournal, call: PhoneCall) {
  const intent = command(call, 'call.release'); journal.submit(intent);
  journal.finish(intent, { version: 1, epoch: call.epoch, requestId: 1, ok: true, result: { callId: call.callId, released: true }, error: null });
  journal.releaseCall(call.callId, intent.operationId); return intent;
}
function incoming() {
  const context: PhoneIncoming = { sipCallId: 'original-wire-id', fromUri: 'sip:known@fixture', peerAddress: '127.0.0.1', peerPort: 5060, transport: 'udp' };
  return { id: randomUUID(), direction: 'incoming', state: 'ringing', sipCallId: context.sipCallId, error: null, incoming: context };
}

test('optional asserted caller match requires the exact number and never substitutes for peer or From', () => {
  const guarded = new PhoneAdmission({ ...definition, incoming: [{ ...definition.incoming[0]!, expectedAssertedNumber: '+49123456789' }] });
  const observation = incoming(), epoch = randomUUID();
  assert.throws(() => guarded.incoming(epoch, randomUUID(), 'secretary', observation), { code: 'phone_caller_refused' });
  observation.incoming.assertedNumbers = ['+49123456789'];
  assert.equal(guarded.incoming(epoch, randomUUID(), 'secretary', observation).callId, observation.id);
  observation.incoming.peerAddress = '127.0.0.2';
  assert.throws(() => guarded.incoming(epoch, randomUUID(), 'secretary', observation), { code: 'phone_caller_refused' });
});

test('an expired unclaimed incoming offer frees only its local admission; unknown claim remains fenced', t => {
  const f = fixture(t), journal = f.open(), epoch = randomUUID(); journal.beginEpoch(epoch);
  const first = journal.admitCall(policy.incoming(epoch, randomUUID(), 'secretary', incoming()));
  const claim = command(first, 'call.claim'); journal.submit(claim);
  assert.throws(() => journal.discardUnpreparedCall(first.callId), { code: 'phone_release_unconfirmed' });
  journal.finish(claim, { version: 1, epoch, requestId: 1, ok: false, result: null, error: 'phone_offer_expired' });
  journal.discardUnpreparedCall(first.callId); assert.equal(journal.currentCall(), null);
  const second = journal.admitCall(policy.incoming(epoch, randomUUID(), 'secretary', incoming()));
  assert.notEqual(second.callId, first.callId); assert.deepEqual(journal.getCall(first.callId), first);
  assert.throws(() => journal.submit(claim), { code: 'phone_operation_retained' });
});

test('completed phone calls outlive the bounded hot journal without reusing their operation identities', t => {
  const f = fixture(t), journal = f.open(), epoch = randomUUID(); journal.beginEpoch(epoch);
  let first: PhoneCall | null = null;
  for (let i = 0; i < 110; i++) {
    const call = journal.admitCall(policy.outgoing(epoch, randomUUID(), 'main', 'personal')); first ??= call; release(journal, call);
  }
  assert.ok(journal.archiveStatus().records > 100); assert.deepEqual(journal.getCall(first!.callId), first);
  assert.deepEqual(journal.admitCall(policy.outgoing(epoch, first!.operationId, 'main', 'personal')), first);
  assert.equal(journal.currentCall(), null);
});

test('Phone policy binds every authenticated principal to exact configured recipients without mutable aliases', () => {
  const input = structuredClone(definition), original = new PhoneAdmission(input), epoch = randomUUID(), operation = randomUUID();
  input.recipients[0]!.destination = 'sip:changed@fixture';
  assert.equal(original.outgoing(epoch, operation, 'main', 'personal').destination, 'sip:user@127.0.0.1');
  assert.equal(original.outgoing(epoch, operation, 'other', 'personal').destination, 'sip:user@127.0.0.1');
  assert.throws(() => original.outgoing(epoch, operation, 'main', 'sip:arbitrary@fixture'), { code: 'phone_destination_refused' });
  assert.throws(() => { original.definition.recipients[0]!.destination = 'sip:changed-again@fixture'; });
  assert.throws(() => new PhoneAdmission({ ...definition, recipients: [definition.recipients[0]!, definition.recipients[0]!] }));
  assert.throws(() => new PhoneAdmission({ ...definition, incoming: [{ ...definition.incoming[0]!, peerAddress: 'hostname.invalid' }] }));
  assert.throws(() => new PhoneAdmission({ ...definition, recipients: [{ ...definition.recipients[0]!, destination: 'sip:user@host\r\nInjected: value' }] }));
});

test('Phone incoming policy checks original native direction, ringing state, wire identity, peer and From', () => {
  const observed = incoming(), epoch = randomUUID(), operation = randomUUID();
  const admitted = policy.incoming(epoch, operation, 'secretary', observed);
  assert.equal(admitted.callId, observed.id); assert.equal(admitted.destination, null); assert.deepEqual(admitted.incoming, observed.incoming);
  for (const wrong of [{ ...observed, direction: 'outgoing' }, { ...observed, state: 'local_ended' },
    { ...observed, sipCallId: 'another-wire-id' }, { ...observed, incoming: { ...observed.incoming, peerAddress: '127.0.0.2' } },
    { ...observed, incoming: { ...observed.incoming, transport: 'tcp' as const } }, { ...observed, incoming: { ...observed.incoming, fromUri: 'sip:other@fixture' } }])
    assert.throws(() => policy.incoming(epoch, operation, 'secretary', wrong), { code: 'phone_caller_refused' });
  assert.equal(policy.incoming(epoch, operation, 'main', observed).callId, observed.id);
  observed.incoming.peerPort = 1234; assert.equal(admitted.incoming!.peerPort, 5060);
});

test('Phone policy changes stop positive call actions while every authenticated caller retains read and cleanup', t => {
  const f = fixture(t), journal = f.open(), epoch = randomUUID(); journal.beginEpoch(epoch);
  const call = journal.admitCall(policy.outgoing(epoch, randomUUID(), 'main', 'personal'));
  policy.authorize('main', call, 'connect'); policy.authorize('admin', call, 'cleanup');
  policy.authorize('admin', call, 'connect'); policy.authorize('other', call, 'read');
  const changed = new PhoneAdmission({ ...definition, recipients: [{ ...definition.recipients[0]!, destination: 'sip:new@fixture' }] });
  changed.authorize('main', call, 'cleanup'); changed.authorize('admin', call, 'read');
  assert.throws(() => changed.authorize('main', call, 'connect'), { code: 'phone_policy_changed' });
});

test('Phone admission atomically retains one call UUID and slot across retries, conflicts and reopen', t => {
  const f = fixture(t), journal = f.open(), epoch = randomUUID(); journal.beginEpoch(epoch);
  const request = policy.outgoing(epoch, randomUUID(), 'main', 'personal'), call = journal.admitCall(request);
  assert.equal(journal.status().operations, 0); assert.equal(journal.status().calls, 1);
  assert.equal(journal.status().reservedBytes, phoneCallReservation);
  assert.deepEqual(journal.admitCall(request), call); assert.deepEqual(journal.callForOperation(request.operationId), call);
  assert.throws(() => journal.admitCall({ ...request, destination: 'sip:changed@fixture' }), { code: 'mutation_conflict' });
  assert.throws(() => journal.admitCall({ ...request, principalId: 'other' }), { code: 'mutation_conflict' });
  assert.throws(() => journal.admitCall({ ...request, operationId: randomUUID() }), { code: 'phone_call_busy' });
  call.destination = 'sip:tampered@fixture'; assert.equal(journal.currentCall()!.destination, request.destination);
  journal.close(); const reopened = f.open();
  assert.equal(reopened.currentCall()!.callId, call.callId); assert.equal(reopened.admitCall(request).callId, call.callId);
});

test('Phone native release must be confirmed for the exact admitted call before another slot is granted', t => {
  const journal = fixture(t).open(), epoch = randomUUID(); journal.beginEpoch(epoch);
  const call = journal.admitCall(policy.outgoing(epoch, randomUUID(), 'main', 'personal'));
  const hanging = command(call, 'call.hangup'); journal.submit(hanging);
  journal.finish(hanging, { version: 1, epoch, requestId: 1, ok: true, result: { callId: call.callId, released: true }, error: null });
  assert.throws(() => journal.releaseCall(call.callId, hanging.operationId), { code: 'phone_release_unconfirmed' });
  const pending = command(call, 'call.release'); journal.submit(pending);
  assert.throws(() => journal.releaseCall(call.callId, pending.operationId), { code: 'phone_release_unconfirmed' });
  journal.finish(pending, { version: 1, epoch, requestId: 2, ok: true, result: { callId: randomUUID(), released: true }, error: null });
  assert.throws(() => journal.releaseCall(call.callId, pending.operationId), { code: 'phone_release_unconfirmed' });
  const released = release(journal, call); assert.equal(journal.currentCall(), null);
  assert.throws(() => journal.submit(command(call, 'call.dial')), { code: 'phone_call_conflict' });
  const next = journal.admitCall(policy.outgoing(epoch, randomUUID(), 'main', 'personal'));
  journal.releaseCall(call.callId, released.operationId); assert.equal(journal.currentCall()!.callId, next.callId);
});

test('Phone incoming native identity cannot be readmitted under a new operation or redirected to an outgoing action', t => {
  const journal = fixture(t).open(), epoch = randomUUID(); journal.beginEpoch(epoch);
  const observed = incoming(), request = policy.incoming(epoch, randomUUID(), 'secretary', observed), call = journal.admitCall(request);
  assert.equal(call.callId, observed.id); assert.throws(() => journal.submit(command(call, 'call.prepare')), { code: 'phone_call_conflict' });
  assert.throws(() => journal.submit(command(call, 'call.dial')), { code: 'phone_call_conflict' });
  journal.submit(command(call, 'call.answer')); release(journal, call);
  assert.throws(() => journal.admitCall({ ...request, operationId: randomUUID() }), { code: 'phone_call_conflict' });
});

test('Phone lost epoch preserves admission and unknown command without making them reusable in the next owner', t => {
  const f = fixture(t), journal = f.open(), epoch = randomUUID(); journal.beginEpoch(epoch);
  const request = policy.outgoing(epoch, randomUUID(), 'main', 'personal'), call = journal.admitCall(request), dial = command(call, 'call.dial');
  journal.submit(dial); journal.loseEpoch(epoch); assert.equal(journal.currentCall(), null);
  assert.equal(journal.get(dial.operationId)!.phase, 'outcome_unknown'); journal.close();
  const reopened = f.open(), next = randomUUID(); reopened.beginEpoch(next);
  assert.deepEqual(reopened.getCall(call.callId), call); assert.deepEqual(reopened.admitCall(request), call);
  assert.throws(() => reopened.admitCall({ ...request, epoch: next }), { code: 'mutation_conflict' });
  assert.throws(() => reopened.submit({ ...dial, epoch: next, operationId: randomUUID() }), { code: 'phone_call_conflict' });
  const fresh = reopened.admitCall(policy.outgoing(next, randomUUID(), 'main', 'personal'));
  assert.notEqual(fresh.callId, call.callId); assert.equal(reopened.get(dial.operationId)!.phase, 'outcome_unknown');
});

test('Phone refuses admission before effects when it cannot reserve a full call and cleanup', t => {
  const journal = fixture(t, { maxOperations: 1, maxBytes: phoneCallReservation + phoneOutcomeReservation, maxEpochs: 2 }).open();
  const epoch = randomUUID(); journal.beginEpoch(epoch);
  const usage = journal.status();
  assert.throws(() => journal.admitCall(policy.outgoing(epoch, randomUUID(), 'main', 'personal')), { code: 'phone_journal_capacity' });
  assert.deepEqual(journal.status(), usage); assert.equal(journal.currentCall(), null);
});

test('Phone admission rollback and fresh-only storage format prevent partial ownership or implicit migration', t => {
  const f = fixture(t), setup = f.open(), epoch = randomUUID(); setup.beginEpoch(epoch); setup.close();
  const path = join(f.root, 'phone-commands.sqlite'); let db = new DatabaseSync(path);
  db.exec("CREATE TRIGGER call_failure BEFORE INSERT ON calls BEGIN SELECT RAISE(ABORT, 'fixture'); END;"); db.close();
  const journal = f.open(), before = journal.status(), request = policy.outgoing(epoch, randomUUID(), 'main', 'personal');
  assert.throws(() => journal.admitCall(request)); assert.equal(journal.currentCall(), null); assert.equal(journal.callForOperation(request.operationId), null);
  assert.deepEqual(journal.status(), before); journal.close();
  db = new DatabaseSync(path); db.exec('PRAGMA user_version=1;'); db.close();
  assert.throws(f.open, { code: 'unsupported_storage' });
  db = new DatabaseSync(path); assert.equal(db.prepare('PRAGMA user_version').get()!['user_version'], 1); db.close();
});

test('caller numbers survive URI formatting changes while asserted identity and peers remain required', () => {
  const rule = { ...definition.incoming[0]!, fromUri: 'sip:+491701234567@tel.example', expectedAssertedNumber: '+491701234567' };
  const guarded = new PhoneAdmission({ ...definition, incoming: [rule] });
  const observation = incoming(); observation.incoming.assertedNumbers = ['+491701234567'];
  for (const fromUri of ['sip:+491701234567@TEL.EXAMPLE;user=phone', 'sips:+491701234567@another.example']) {
    observation.incoming.fromUri = fromUri;
    assert.equal(guarded.incoming(randomUUID(), randomUUID(), 'secretary', observation).callId, observation.id);
  }
  observation.incoming.fromUri = 'sip:+491701234568@tel.example';
  assert.throws(() => guarded.incoming(randomUUID(), randomUUID(), 'secretary', observation), { code: 'phone_caller_refused' });
  const registered = new PhoneAdmission({ ...definition, incoming: [{ ...rule, peerAddress: 'registered' }] });
  observation.incoming.fromUri = rule.fromUri;
  assert.throws(() => registered.incoming(randomUUID(), randomUUID(), 'secretary', observation), { code: 'phone_caller_refused' });
  observation.incoming.registrationPeer = true;
  assert.equal(registered.incoming(randomUUID(), randomUUID(), 'secretary', observation).callId, observation.id);
  observation.incoming.assertedNumbers = ['+491701234568'];
  assert.throws(() => registered.incoming(randomUUID(), randomUUID(), 'secretary', observation), { code: 'phone_caller_refused' });
});
