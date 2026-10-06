import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync, appendFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { PhoneAdmission } from '../services/phone-bridge/src/runtime/admission.js';
import { PhoneJournal } from '../services/phone-bridge/src/runtime/journal.js';
import { PhoneVoiceTasks } from '../services/phone-bridge/src/runtime/desktop-voice-tasks.js';
import { PhoneVoiceArchive } from '../services/phone-bridge/src/runtime/desktop-voice-archive.js';
import { PhoneVoiceRetention } from '../services/phone-bridge/src/runtime/desktop-voice-retention.js';
import { IvyError, digest } from '../packages/sdk/src/node.js';
import type { AppToolsSettings } from '../packages/sdk/src/codex-app-tools.js';

function fixture(t: test.TestContext) {
  const root = mkdtempSync(join(tmpdir(), 'ivy-voice-tasks-'));
  const databasePath = join(root, 'state.sqlite'), journalRoot = join(root, 'phone');
  const db = new DatabaseSync(databasePath);
  let journal = new PhoneJournal(journalRoot, { hostId: 'fixture', serviceNodeId: 'phone' });
  t.after(() => { journal.close(); db.close(); rmSync(root, { recursive: true, force: true }); });
  db.exec('CREATE TABLE threads(id TEXT PRIMARY KEY,archived INTEGER,thread_source TEXT,recency_at_ms INTEGER,rollout_path TEXT); CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT);');
  const actor = randomUUID(), previous = randomUUID(), voice = randomUUID();
  const add = db.prepare('INSERT INTO threads(id,archived,thread_source,recency_at_ms) VALUES(?,0,?,?)');
  add.run(actor, null, 1); add.run(previous, 'voice_chat', 2);
  const reference = (id: string | null) => writeFileSync(join(root, '.codex-global-state.json'), JSON.stringify({
    'electron-persisted-atom-state': id ? { 'realtime-voice-most-recent-thread': { conversationId: id, hostId: 'local' } } : {},
  }));
  reference(previous);
  const settings = { actorThreadId: actor } as AppToolsSettings;
  const admission = new PhoneAdmission({ incoming: [], recipients: [{ id: 'personal', destination: 'sip:test@fixture' }] });
  let epoch = randomUUID(); journal.beginEpoch(epoch);
  const admit = (principalId = 'main') => journal.admitCall(admission.outgoing(epoch, randomUUID(), principalId, 'personal', 'voice'));
  let call = admit();
  const prompts: { id: string; prompt: string; selection: unknown }[] = [];
  const transfers: { source: string; target: string }[] = [];
  const created: string[] = [], opened: string[] = [];
  let closes = 0, reads = 0, failure: 'create' | 'prompt' | 'transfer' | null = null;
  let onRead: ((id: string) => void) | null = null;
  let onTransfer: (() => void) | null = null;
  let sessionDelay = 0;
  let status = 'idle';
  const session = (id: string, active = true) => {
    const path = join(root, id + '.jsonl');
    if (!db.prepare('SELECT rollout_path FROM threads WHERE id=?').get(id)?.['rollout_path']) {
      writeFileSync(path, JSON.stringify({ type: 'session_meta', payload: { id } }) + '\n');
      db.prepare('UPDATE threads SET rollout_path=? WHERE id=?').run(path, id);
    }
    appendFileSync(path, JSON.stringify({ type: 'realtime_item', payload: {
      type: active ? 'realtime_session_started' : 'realtime_session_closed', realtime_session_id: randomUUID(),
    } }) + '\n');
  };
  const observation = (id: string) => ({ id, hostId: 'local', kind: 'codex', status: { type: status, activeFlags: [] } });
  const open = (value: AppToolsSettings) => {
    opened.push(value.actorThreadId);
    return {
      async verifyActor() { return observation(value.actorThreadId); },
      async createLocalThread(_prompt: string, _selection: unknown, _operationId: string, before: () => Promise<void>) {
        await before();
        const id = randomUUID(); created.push(id); add.run(id, 'agent_created_thread', 10 + created.length);
        if (failure === 'create') throw new Error('lost creation acknowledgement');
        return id;
      },
      async transferVoiceCall(source: string, target: string) {
        transfers.push({ source, target }); onTransfer?.();
        if (failure === 'transfer') throw new IvyError('app_tools_transfer_unknown', 'lost transfer acknowledgement', 'unknown');
        reference(target);
        if (sessionDelay < 0) return;
        if (sessionDelay) setTimeout(() => session(target), sessionDelay);
        else session(target);
      },
      async readThread(id: string) { reads++; onRead?.(id); return observation(id); },
      async sendMessage(id: string, prompt: string, _operationId: string, before: () => Promise<void>, selection: unknown) {
        await before(); prompts.push({ id, prompt, selection });
        if (failure === 'prompt') throw new Error('lost prompt acknowledgement');
        return { threadId: id, sent: true as const };
      },
      async close() { closes++; },
    };
  };
  const tasks = () => new PhoneVoiceTasks(journal, open);
  const start = (id = voice, flush = true, ready = true) => { add.run(id, 'voice_chat', 3); if (ready) session(id); if (flush) reference(id); return id; };
  const cancel = () => journal.submit({ epoch, callId: call.callId, operationId: randomUUID(), method: 'call.hangup', requestHash: digest('hangup') });
  return {
    root, db, databasePath, settings, actor, previous, voice, prompts, transfers, created, opened, tasks, start, reference, cancel,
    get journal() { return journal; }, get call() { return call; }, get reads() { return reads; }, get closes() { return closes; },
    fail(value: typeof failure) { failure = value; }, session, delaySession(ms: number) { sessionDelay = ms; },
    status(value: string) { status = value; },
    onRead(value: typeof onRead) { onRead = value; }, onTransfer(value: typeof onTransfer) { onTransfer = value; },
    restartJournal(principalId = 'main') {
      const intent = { epoch, callId: call.callId, operationId: randomUUID(), method: 'call.release' as const, requestHash: digest('release') };
      journal.submit(intent); journal.finish(intent, { version: 1, epoch, requestId: 1, ok: true, result: { callId: call.callId, released: true }, error: null });
      journal.releaseCall(call.callId, intent.operationId); journal.loseEpoch(epoch); journal.close();
      journal = new PhoneJournal(journalRoot, { hostId: 'fixture', serviceNodeId: 'phone' });
      epoch = randomUUID(); journal.beginEpoch(epoch); call = admit(principalId);
    },
  };
}

test('initial Voice binds the original Desktop chat without creating, navigating or transferring a task', async t => {
  const f = fixture(t), tasks = f.tasks();
  assert.equal(await tasks.prepare(f.call, f.databasePath, f.settings), null);
  assert.deepEqual(f.created, []);
  f.start();
  // Outgoing ringing and pre-start verification must retain the first baseline.
  assert.equal(await tasks.prepare(f.call, f.databasePath, f.settings), null);
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 0, null, true), f.voice);
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 0, null, true), f.voice);
  assert.equal(f.journal.createdVoiceTask(f.call.callId, 0), null);
  assert.equal(f.journal.voiceTask(f.call.callId, 0), f.voice);
  assert.deepEqual(f.transfers, []);
  const reads = f.reads;
  await tasks.prompt(f.call, f.databasePath, f.settings, 0, f.voice, 'Call prompt', async () => undefined);
  await tasks.prompt(f.call, f.databasePath, f.settings, 0, f.voice, 'Call prompt', async () => undefined);
  assert.equal(f.reads, reads, 'the confirmed binding sends its prompt without another App read');
  assert.deepEqual(f.prompts, [{ id: f.voice, prompt: 'Call prompt', selection: { model: 'gpt-6-sol', reasoningEffort: 'high' } }]);
  assert.equal(f.opened.length, 1); assert.equal(f.closes, 1);
  await assert.rejects(tasks.bind(f.call, f.databasePath, f.settings, 0, f.previous), { code: 'mutation_conflict' });
  f.cancel();
  await assert.rejects(tasks.prepare(f.call, f.databasePath, f.settings), { code: 'phone_voice_owner_changed' });
});

for (const timing of ['stale-reference', 'delayed-root', 'ambiguous'] as const) {
  test('initial Voice startup: ' + timing, async t => {
    const f = fixture(t), tasks = f.tasks();
    await tasks.prepare(f.call, f.databasePath, f.settings);
    if (timing === 'delayed-root') {
      // Ordinary stale references cannot be mistaken for a ready Voice chat.
      f.reference(f.actor);
      await tasks.release(f.call.callId);
      await tasks.prepare(f.call, f.databasePath, f.settings);
      setTimeout(() => f.start(f.voice, false), 150);
    } else f.start(f.voice, false);
    if (timing === 'ambiguous') f.start(randomUUID(), false);
    if (timing === 'ambiguous') {
      await assert.rejects(tasks.bind(f.call, f.databasePath, f.settings, 0, null), { code: 'phone_voice_reference_changed' });
      assert.equal(f.journal.voiceTask(f.call.callId, 0), null);
    } else assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 0, null), f.voice);
    assert.equal(f.created.length, 0); assert.equal(f.transfers.length, 0);
    await tasks.release(f.call.callId);
  });
}

for (const failure of ['observation', 'journal', 'reference', 'cancellation'] as const) {
  test('initial binding reconciles ' + failure + ' without mutations or another task', async t => {
    const f = fixture(t), tasks = f.tasks();
    await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
    let failed = false;
    f.onRead(() => {
      if (failed) return; failed = true;
      if (failure === 'observation') throw new Error('temporary App read failure');
      if (failure === 'reference') f.reference(f.actor);
      if (failure === 'cancellation') f.cancel();
    });
    if (failure === 'journal') t.mock.method(f.journal, 'submit', () => { throw new IvyError('fixture_journal_failed', 'unavailable'); });
    if (failure === 'observation') {
      assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 0, null), f.voice);
      assert.equal(f.reads, 2); assert.equal(f.closes, 2);
    } else {
      await assert.rejects(tasks.bind(f.call, f.databasePath, f.settings, 0, null), {
        code: { journal: 'fixture_journal_failed', reference: 'phone_voice_reference_changed', cancellation: 'phone_voice_owner_changed' }[failure],
      });
      assert.equal(f.reads, 1); assert.equal(f.journal.callCommand(f.call.callId, 'call.bindVoice'), null);
    }
    assert.equal(f.created.length, 0); assert.equal(f.transfers.length, 0);
    await tasks.release(f.call.callId);
  });
}

test('a fresh Voice chat accepts its complete prompt while its task is notLoaded', async t => {
  const f = fixture(t), tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings);
  f.start(); f.status('notLoaded');
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 0, null, true), f.voice);
  const prompt = 'Ask which appointment the caller prefers: Tuesday at 10 or Thursday at 15. '
    + 'The caller is answering this question; preserve these options as conversation context.';
  await tasks.prompt(f.call, f.databasePath, f.settings, 0, f.voice, prompt, async () => undefined);
  assert.deepEqual(f.prompts.map(value => value.prompt), [prompt]);
  assert.deepEqual(f.prompts[0]?.selection, { model: 'gpt-6-sol', reasoningEffort: 'high' });
});

test('archived App controller is replaced without moving the Voice conversation', async t => {
  const f = fixture(t), replacement = randomUUID();
  f.db.prepare('UPDATE threads SET archived=1 WHERE id=?').run(f.actor);
  f.db.prepare('INSERT INTO threads(id,archived,thread_source,recency_at_ms) VALUES(?,0,NULL,5)').run(replacement);
  const tasks = f.tasks(); await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 0, null), f.voice);
  assert.deepEqual(f.opened, [replacement, replacement]); assert.equal(f.created.length, 0);
});

test('model selection keeps the original chat; an explicit restart alone creates and transfers', async t => {
  const f = fixture(t), tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
  const selection = { model: 'gpt-6-luna', reasoningEffort: 'max' } as const;
  await tasks.select(f.call, f.databasePath, f.settings, f.voice, { commandSequence: 1 }, selection, async () => undefined);
  await tasks.select(f.call, f.databasePath, f.settings, f.voice, { commandSequence: 1 }, selection, async () => undefined);
  assert.equal(f.prompts.length, 1); assert.equal(f.prompts[0]!.id, f.voice); assert.equal(f.transfers.length, 0);
  const operationId = randomUUID();
  await tasks.select(f.call, f.databasePath, f.settings, f.voice, { operationId }, selection, async () => undefined);
  await tasks.select(f.call, f.databasePath, f.settings, f.voice, { operationId }, selection, async () => undefined);
  assert.equal(f.prompts.length, 2);
  await assert.rejects(tasks.select(f.call, f.databasePath, f.settings, f.voice, { operationId },
    { ...selection, reasoningEffort: 'low' }, async () => undefined), { code: 'mutation_conflict' });
  const next = await tasks.prepare(f.call, f.databasePath, f.settings, 1, selection);
  assert.ok(next && next !== f.voice);
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 1, next, true), next);
  await tasks.prompt(f.call, f.databasePath, f.settings, 1, next, 'Call prompt', async () => undefined, selection);
  assert.deepEqual(f.transfers, [{ source: f.voice, target: next }]);
  assert.equal(f.journal.latestVoiceGeneration(f.call.callId), 1);
  assert.equal(f.journal.voiceTask(f.call.callId, 0), f.voice);
  let idleChecks = 0;
  const archives = new PhoneVoiceArchive(f.journal, () => { throw new Error('original chat must remain in Recents'); });
  await archives.run(f.call, { databasePath: f.databasePath, appTools: f.settings }, async () => { idleChecks++; }, { generation: 0, threadId: f.voice });
  assert.equal(idleChecks, 1, 'original chat retention still checks that capture stopped');
  assert.equal(f.db.prepare('SELECT archived FROM threads WHERE id=?').get(f.voice)?.['archived'], 0);
  assert.equal(f.journal.retiredVoiceTask(f.voice), null, 'the original Voice chat is never queued for archival');
});

for (const lostReply of [false, true]) test('forwarded Voice prompts retain each request without repeating a send: ' + lostReply, async t => {
  const f = fixture(t), tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  await tasks.bind(f.call, f.databasePath, f.settings, 0, null, true);
  await tasks.prompt(f.call, f.databasePath, f.settings, 0, f.voice, 'Greeting', async () => undefined);
  const forwarded = { operationId: randomUUID(), requestHash: digest('forwarded request') };
  const send = (request = forwarded) => tasks.prompt(f.call, f.databasePath, f.settings, 0, f.voice,
    'Unchanged request text', async () => undefined, undefined, request);
  f.fail(lostReply ? 'prompt' : null);
  if (lostReply) await assert.rejects(send());
  else await send();
  const operation = f.journal.get(forwarded.operationId);
  assert.equal(operation?.intent.method, 'call.forwardVoice');
  assert.equal((operation?.receipt?.result as { state: string }).state, lostReply ? 'outcome_unknown' : 'sent');
  f.fail(null);
  if (lostReply) await assert.rejects(send(), { code: 'phone_voice_prompt_unknown' });
  else await send();
  assert.equal(f.prompts.length, 2);
  await assert.rejects(send({ ...forwarded, requestHash: digest('changed request') }), { code: 'mutation_conflict' });
  await send({ operationId: randomUUID(), requestHash: digest('another request') });
  assert.equal(f.prompts.length, 3);
  assert.equal(f.prompts[1]?.prompt, 'Unchanged request text');
  assert.equal(f.prompts[2]?.prompt, 'Unchanged request text');
  assert.equal(f.journal.callCommand(f.call.callId, 'call.promptVoice')?.intent.method, 'call.promptVoice');
});

test('forwarded Voice prompt refuses a Desktop task switch before submission', async t => {
  const f = fixture(t), tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
  const forwarded = { operationId: randomUUID(), requestHash: digest('forwarded request') };
  f.onRead(() => f.reference(f.previous));
  await assert.rejects(tasks.prompt(f.call, f.databasePath, f.settings, 0, f.voice,
    'Request text', async () => undefined, undefined, forwarded), { code: 'phone_voice_task_changed' });
  assert.equal(f.prompts.length, 0);
  assert.equal(f.journal.get(forwarded.operationId), null);
});

test('unknown prompt, model change and creation acknowledgements never repeat their mutations', async t => {
  const f = fixture(t), tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  await tasks.bind(f.call, f.databasePath, f.settings, 0, null, true);
  f.fail('prompt');
  await assert.rejects(tasks.prompt(f.call, f.databasePath, f.settings, 0, f.voice, 'Call prompt', async () => undefined));
  await assert.rejects(tasks.prompt(f.call, f.databasePath, f.settings, 0, f.voice, 'Call prompt', async () => undefined), { code: 'phone_voice_prompt_unknown' });
  const selection = { model: 'gpt-6-sol', reasoningEffort: 'high' } as const;
  await assert.rejects(tasks.select(f.call, f.databasePath, f.settings, f.voice, { commandSequence: 1 }, selection, async () => undefined));
  await assert.rejects(tasks.select(f.call, f.databasePath, f.settings, f.voice, { commandSequence: 1 }, selection, async () => undefined), { code: 'phone_voice_selection_unknown' });
  assert.equal(f.prompts.length, 2);
  f.fail('create');
  await assert.rejects(tasks.prepare(f.call, f.databasePath, f.settings, 1));
  await assert.rejects(tasks.prepare(f.call, f.databasePath, f.settings, 1), { code: 'phone_voice_create_unknown' });
  assert.equal(f.created.length, 1);
});

for (const mode of ['restart', 'reuse'] as const) test(mode + ' retries only definite not-ready transfers and reconciles a lost reply once', async t => {
  const f = fixture(t); let tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
  if (mode === 'reuse') { await tasks.release(f.call.callId); f.restartJournal(); tasks = f.tasks(); }
  const generation = mode === 'restart' ? 1 : 0;
  const next = await tasks.prepare(f.call, f.databasePath, f.settings, generation); assert.ok(next);
  if (mode === 'reuse') f.start(randomUUID());
  f.fail('transfer');
  f.onTransfer(() => {
    if (f.transfers.length < 3) throw new IvyError('app_tools_voice_not_ready', 'Realtime still starting');
    setTimeout(() => { f.reference(next); f.session(next); }, 150);
  });
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, generation, next), next);
  assert.equal(f.transfers.length, 3);
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, generation, next), next);
  assert.equal(f.transfers.length, 3, 'lost transfer acknowledgement is never repeated');
});

for (const mode of ['initial', 'reuse', 'restart'] as const) test(mode + ' waits for fresh realtime readiness only before greeting', async t => {
  const f = fixture(t); let tasks = f.tasks(), target: string | null, generation = 0;
  if (mode === 'initial') {
    target = await tasks.prepare(f.call, f.databasePath, f.settings);
    f.start(f.voice, true, false);
    setTimeout(() => f.session(f.voice), 300);
  } else {
    await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
    await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
    if (mode === 'reuse') { await tasks.release(f.call.callId); f.restartJournal(); tasks = f.tasks(); }
    else generation = 1;
    target = await tasks.prepare(f.call, f.databasePath, f.settings, generation);
    if (mode === 'reuse') f.start(randomUUID());
    f.delaySession(300);
  }
  const bound = await tasks.bind(f.call, f.databasePath, f.settings, generation, target, true);
  const greeting = tasks.prompt(f.call, f.databasePath, f.settings, generation, bound, 'Greeting', async () => undefined);
  await new Promise(resolve => setTimeout(resolve, 100));
  assert.equal(f.journal.voiceTask(f.call.callId, generation), bound, 'binding preserves the call while realtime is still starting');
  assert.equal(f.prompts.length, 0);
  await greeting;
  assert.equal(f.prompts.length, 1);
  assert.equal(f.transfers.length, mode === 'initial' ? 0 : 1, 'readiness observations never replay transfer');
});

test('definitely rejected reuse preserves the exact startup chat and both histories', async t => {
  const f = fixture(t); let tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
  await tasks.release(f.call.callId); f.restartJournal(); tasks = f.tasks();
  const target = await tasks.prepare(f.call, f.databasePath, f.settings);
  assert.equal(target, f.voice);
  const startup = f.start(randomUUID());
  f.onTransfer(() => { throw new IvyError('app_tools_voice_not_ready', 'Transfer definitely rejected'); });
  let clock = 0;
  t.mock.method(performance, 'now', () => clock += 6000);
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 0, target, true), startup);
  await tasks.prompt(f.call, f.databasePath, f.settings, 0, startup, 'Greeting', async () => undefined);
  assert.equal(f.prompts[0]?.id, startup);
  assert.equal(f.journal.reusableVoiceTask('main'), startup);
  assert.equal(f.journal.retiredVoiceTask(startup), null);
  assert.equal(f.journal.retiredVoiceTask(f.voice), null);
  assert.ok(f.transfers.length > 0);
  assert.equal(f.journal.callCommand(f.call.callId, 'call.hangup'), null);
  assert.equal(f.created.length, 0);
});

test('an unloaded reused chat does not delay preparation and a fresh resumed session needs no transfer', async t => {
  const f = fixture(t); let tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
  await tasks.release(f.call.callId); f.restartJournal(); tasks = f.tasks();
  f.status('notLoaded');
  const target = await tasks.prepare(f.call, f.databasePath, f.settings); assert.equal(target, f.voice);
  f.status('idle'); f.session(f.voice);
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 0, target, true), target);
  await tasks.prompt(f.call, f.databasePath, f.settings, 0, target!, 'Greeting', async () => undefined);
  assert.equal(f.transfers.length, 0);
  assert.equal(f.prompts.length, 1);
});

test('a previous active session cannot prove that the reused chat resumed', async t => {
  const f = fixture(t); let tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
  await tasks.release(f.call.callId); f.restartJournal(); tasks = f.tasks();
  const target = await tasks.prepare(f.call, f.databasePath, f.settings);
  let clock = 0; t.mock.method(performance, 'now', () => clock += 16000);
  await assert.rejects(tasks.bind(f.call, f.databasePath, f.settings, 0, target), { code: 'phone_voice_task_missing' });
  assert.equal(f.transfers.length, 0);
});

test('realtime readiness timeout keeps the confirmed reused binding and never sends an inaudible greeting', async t => {
  const f = fixture(t); let tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
  await tasks.release(f.call.callId); f.restartJournal(); tasks = f.tasks();
  const target = await tasks.prepare(f.call, f.databasePath, f.settings); assert.ok(target);
  f.start(randomUUID()); f.delaySession(-1);
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 0, target, true), target);
  let clock = 0;
  t.mock.method(performance, 'now', () => clock += 16000);
  await assert.rejects(tasks.prompt(f.call, f.databasePath, f.settings, 0, target, 'Greeting', async () => undefined),
    { code: 'phone_voice_not_ready' });
  await assert.rejects(tasks.prompt(f.call, f.databasePath, f.settings, 0, target, 'Greeting', async () => undefined),
    { code: 'phone_voice_not_ready' });
  assert.equal(f.journal.voiceTask(f.call.callId, 0), target);
  assert.equal(f.journal.callCommand(f.call.callId, 'call.promptVoice'), null);
  assert.equal(f.journal.callCommand(f.call.callId, 'call.hangup'), null);
  assert.equal(f.prompts.length, 0);
  assert.equal(f.transfers.length, 1);
});

test('retention archives only superseded explicitly created tasks, preserving the original Voice chat', async t => {
  const f = fixture(t), tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start(); await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
  const first = await tasks.prepare(f.call, f.databasePath, f.settings, 1); assert.ok(first);
  await tasks.bind(f.call, f.databasePath, f.settings, 1, first);
  const second = await tasks.prepare(f.call, f.databasePath, f.settings, 2); assert.ok(second);
  await tasks.bind(f.call, f.databasePath, f.settings, 2, second);
  let archived = 0;
  const retention = new PhoneVoiceRetention(f.journal, { databasePath: f.databasePath, appTools: f.settings }, () => ({
    async readThread(id: string) { return { id, hostId: 'local', kind: 'codex', status: { type: 'idle', activeFlags: [] } }; },
    async archiveThread(id: string, operationId: string, before: () => Promise<void>) {
      await before(); archived++; assert.equal(id, first); assert.equal(f.journal.retiredVoiceTask(id)?.archiveOperationId, operationId);
      f.db.prepare('UPDATE threads SET archived=1 WHERE id=?').run(id); return { threadId: id, archived: true as const };
    }, async close() {},
  }));
  await retention.tick(); await retention.tick();
  assert.equal(archived, 1); assert.equal(f.journal.retiredVoiceTask(first)?.phase, 'archived');
  assert.equal(f.journal.retiredVoiceTask(first)?.deleteAfter, null);
  assert.equal(f.db.prepare('SELECT archived FROM threads WHERE id=?').get(f.voice)?.['archived'], 0);
});

test('a later call after process restart reuses the last explicit projectless task', async t => {
  const f = fixture(t); let tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start(); await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
  const explicit = await tasks.prepare(f.call, f.databasePath, f.settings, 1); assert.ok(explicit);
  await tasks.bind(f.call, f.databasePath, f.settings, 1, explicit);
  assert.equal(f.journal.reusableVoiceTask('main'), explicit);
  await tasks.release(f.call.callId); f.restartJournal(); tasks = f.tasks();
  assert.equal(await tasks.prepare(f.call, f.databasePath, f.settings), explicit);
  const second = f.start(randomUUID());
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 0, explicit), explicit);
  assert.equal(f.journal.createdVoiceTask(f.call.callId, 0), null); assert.equal(f.created.length, 1);
  assert.deepEqual(f.transfers, [{ source: f.voice, target: explicit }, { source: second, target: explicit }]);
});

test('a returning caller reuses a promoted Desktop chat and archives only its transient startup chat', async t => {
  const f = fixture(t); let tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
  f.db.prepare("UPDATE threads SET thread_source='vscode' WHERE id=?").run(f.voice);
  assert.equal(f.journal.reusableVoiceTask('main'), f.voice);
  assert.equal(f.journal.reusableVoiceTask('someone-else'), null);
  await tasks.release(f.call.callId); f.restartJournal(); tasks = f.tasks();
  assert.equal(await tasks.prepare(f.call, f.databasePath, f.settings), f.voice);
  const startup = randomUUID();
  setTimeout(() => f.start(startup, false), 150); // saved reference still names the old chat
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 0, f.voice, true), f.voice);
  await tasks.prompt(f.call, f.databasePath, f.settings, 0, f.voice, 'Welcome back', async () => undefined);
  assert.equal(f.created.length, 0);
  assert.deepEqual(f.transfers, [{ source: startup, target: f.voice }]);
  assert.equal(f.prompts[0]!.id, f.voice);
  assert.equal(f.journal.retiredVoiceTask(f.voice), null);
  assert.equal(f.journal.retiredVoiceTask(startup)?.successorId, f.voice);
  let archived = 0;
  const retention = new PhoneVoiceRetention(f.journal, { databasePath: f.databasePath, appTools: f.settings }, () => ({
    async readThread(id: string) { return { id, hostId: 'local', kind: 'codex', status: { type: 'idle', activeFlags: [] } }; },
    async archiveThread(id: string, _operationId: string, before: () => Promise<void>) {
      await before(); assert.equal(id, startup); archived++;
      f.db.prepare('UPDATE threads SET archived=1 WHERE id=?').run(id); return { threadId: id, archived: true as const };
    }, async close() {},
  }));
  await retention.tick(); await retention.tick();
  assert.equal(archived, 1);
  assert.equal(f.db.prepare('SELECT archived FROM threads WHERE id=?').get(f.voice)?.['archived'], 0);
});

test('an older retained call cannot mask the latest archived Voice binding', async t => {
  const f = fixture(t); let tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
  const older = f.call;
  await tasks.release(f.call.callId); f.restartJournal(); tasks = f.tasks();
  // The caller explicitly starts a different conversation during the later call.
  const next = await tasks.prepare(f.call, f.databasePath, f.settings, 1); assert.ok(next);
  await tasks.bind(f.call, f.databasePath, f.settings, 1, next);
  await tasks.release(f.call.callId); f.restartJournal();
  f.journal.close();
  const journalDb = new DatabaseSync(join(f.root, 'phone', 'phone-commands.sqlite'));
  // Retained unresolved calls precede archived calls in public history. Move the
  // old admission into that section without altering its immutable bind receipt.
  journalDb.prepare('INSERT INTO calls(call_id,operation_id,value) VALUES(?,?,?)')
    .run(older.callId, older.operationId, JSON.stringify(older));
  journalDb.close();
  const journal = new PhoneJournal(join(f.root, 'phone'), { hostId: 'fixture', serviceNodeId: 'phone' });
  try {
    assert.equal(journal.history('main', 64)[0]!.callId, older.callId);
    assert.equal(journal.reusableVoiceTask('main'), next);
  } finally { journal.close(); }
});

for (const unavailable of ['archived', 'missing', 'different-caller'] as const) test('a previous chat is not reused when ' + unavailable, async t => {
  const f = fixture(t); let tasks = f.tasks();
  await tasks.prepare(f.call, f.databasePath, f.settings); f.start();
  await tasks.bind(f.call, f.databasePath, f.settings, 0, null);
  await tasks.release(f.call.callId);
  f.restartJournal(unavailable === 'different-caller' ? 'someone-else' : 'main');
  if (unavailable === 'archived') f.db.prepare('UPDATE threads SET archived=1 WHERE id=?').run(f.voice);
  if (unavailable === 'missing') f.db.prepare('DELETE FROM threads WHERE id=?').run(f.voice);
  tasks = f.tasks();
  assert.equal(await tasks.prepare(f.call, f.databasePath, f.settings), null);
  const startup = f.start(randomUUID());
  assert.equal(await tasks.bind(f.call, f.databasePath, f.settings, 0, null), startup);
  assert.equal(f.transfers.length, 0);
  assert.equal(f.created.length, 0);
});
