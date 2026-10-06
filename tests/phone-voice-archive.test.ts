import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { PhoneJournal } from '../services/phone-bridge/src/runtime/journal.js';
import { PhoneAdmission } from '../services/phone-bridge/src/runtime/admission.js';
import { PhoneVoiceArchive } from '../services/phone-bridge/src/runtime/desktop-voice-archive.js';
import type { PhoneVoiceArchiveSettings } from '../services/phone-bridge/src/runtime/desktop-voice-archive.js';

test('Voice generations archive only their bound task and never reuse another generation receipt', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-voice-scoped-')), databasePath = join(root, 'desktop.sqlite');
  const db = new DatabaseSync(databasePath), journal = new PhoneJournal(join(root, 'phone'), { hostId: 'fixture', serviceNodeId: 'phone' });
  t.after(() => { journal.close(); db.close(); rmSync(root, { recursive: true, force: true }); });
  db.exec('CREATE TABLE threads(id TEXT PRIMARY KEY,archived INTEGER,thread_source TEXT,recency_at_ms INTEGER); CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT);');
  const actor = randomUUID(), first = randomUUID(), second = randomUUID(), unrelated = randomUUID();
  const add = db.prepare('INSERT INTO threads VALUES(?,?,?,?)'); add.run(actor, 0, null, 0);
  for (const id of [first, second, unrelated]) add.run(id, 0, 'voice_chat', 1);
  const epoch = randomUUID(); journal.beginEpoch(epoch);
  const policy = new PhoneAdmission({ incoming: [], recipients: [{ id: 'personal', destination: 'sip:fixture@test' }] });
  const call = journal.admitCall(policy.outgoing(epoch, randomUUID(), 'main', 'personal'));
  const settings: PhoneVoiceArchiveSettings = { databasePath, appTools: { nodeExecutable: process.execPath,
    nodeExecutableHash: 'sha256:' + '0'.repeat(64), serverPath: join(root, 'server.mjs'), serverHash: 'sha256:' + '0'.repeat(64),
    pipePath: '\\\\.\\pipe\\fixture', actorThreadId: actor } };
  const sent: string[] = [];
  const archive = new PhoneVoiceArchive(journal, () => ({
    async readThread(id) { return { id, hostId: 'local', kind: 'codex', status: { type: 'idle', activeFlags: [] } }; },
    async archiveThread(id, operationId, before) {
      await before();
      const generation = id === first ? 0 : 1, retained = journal.callCommand(call.callId, 'call.archiveVoice', generation);
      assert.ok(retained?.intent.method === 'call.archiveVoice' && retained.intent.archivePlan.threadId === id && retained.intent.archivePlan.operationIds.includes(operationId));
      sent.push(id); db.prepare('UPDATE threads SET archived=1 WHERE id=?').run(id); return { threadId: id, archived: true };
    }, async close() {},
  }));
  const idle = async () => {};
  await archive.run(call, settings, idle, { generation: 0, threadId: first });
  await archive.run(call, settings, idle, { generation: 1, threadId: second });
  await archive.run(call, settings, idle, { generation: 0, threadId: first });
  assert.deepEqual(sent, [first, second]);
  assert.equal(db.prepare('SELECT archived FROM threads WHERE id=?').get(unrelated)?.['archived'], 0);
  await assert.rejects(archive.run(call, settings, idle, { generation: 1, threadId: unrelated }), { code: 'mutation_conflict' });
  await archive.reconcile(call.callId, 1);
  assert.equal(journal.callCommand(call.callId, 'call.archiveVoice', 0)?.archiveResolution, undefined);
  assert.equal(journal.callCommand(call.callId, 'call.archiveVoice', 1)?.archiveResolution?.state, 'observed_archived');
});

test('Voice archive uses the original journal and fences partial results across native epochs', async t => {
  for (const scenario of ['complete', 'not-loaded', 'unknown-child', 'flagged-child', 'lost-ack', 'active-child', 'scope-change', 'actor-missing', 'voice-active', 'plan-capacity']) {
    await t.test(scenario, async st => {
      const root = mkdtempSync(join(tmpdir(), 'ivy-voice-archive-')), databasePath = join(root, 'desktop.sqlite');
      const db = new DatabaseSync(databasePath);
      const journal = new PhoneJournal(join(root, 'phone'), { hostId: 'fixture', serviceNodeId: 'phone' });
      st.after(() => { journal.close(); db.close(); rmSync(root, { recursive: true, force: true }); });
      db.exec('CREATE TABLE threads(id TEXT PRIMARY KEY,archived INTEGER,thread_source TEXT,recency_at_ms INTEGER); CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT);');
      const actor = randomUUID(), target = '00000000-0000-4000-8000-000000000001', child = randomUUID(), second = '00000000-0000-4000-8000-000000000002';
      const add = db.prepare('INSERT INTO threads VALUES(?,?,?,?)');
      if (scenario !== 'actor-missing') add.run(actor, 0, null, 0);
      if (scenario !== 'empty') {
        add.run(target, 0, 'voice_chat', 2); add.run(child, 0, 'voice_chat', 1);
        db.prepare('INSERT INTO thread_spawn_edges VALUES(?,?)').run(target, child);
      }
      const epoch = randomUUID(); journal.beginEpoch(epoch);
      const policy = new PhoneAdmission({ incoming: [],
        recipients: [{ id: 'personal', destination: 'sip:person@fixture' }] });
      const call = journal.admitCall(policy.outgoing(epoch, randomUUID(), 'main', 'personal'));
      const settings: PhoneVoiceArchiveSettings = { databasePath, appTools: { nodeExecutable: process.execPath,
        nodeExecutableHash: 'sha256:' + '0'.repeat(64), serverPath: join(root, 'server.mjs'), serverHash: 'sha256:' + '0'.repeat(64),
        pipePath: '\\\\.\\pipe\\fixture', actorThreadId: actor } };
      if (scenario === 'plan-capacity') settings.appTools.serverPath = join(root, 'x'.repeat(32700));
      const sends: string[] = []; let idleChecks = 0;
      const archive = new PhoneVoiceArchive(journal, () => ({
        async readThread(id) { return { id, hostId: 'local', kind: 'codex', status: {
          type: scenario === 'not-loaded' ? 'notLoaded' : id === child && scenario === 'unknown-child' ? 'unknown' : scenario === 'active-child' && id === child ? 'running' : 'idle',
          activeFlags: scenario === 'flagged-child' && id === child ? ['waitingOnApproval'] : [] } }; },
        async archiveThread(id, operationId, before) {
          if (scenario === 'scope-change') db.prepare('UPDATE threads SET thread_source=NULL WHERE id=?').run(child);
          await before();
          const retained = journal.callCommand(call.callId, 'call.archiveVoice');
          assert.equal(retained?.phase, 'submitted', 'whole plan is durable before any effect');
          assert.ok(retained?.intent.method === 'call.archiveVoice' && retained.intent.archivePlan.operationIds.includes(operationId));
          sends.push(id);
          if (scenario === 'lost-ack') throw new Error('lost original acknowledgement');
          db.prepare('UPDATE threads SET archived=1 WHERE id=? OR id IN (SELECT child_thread_id FROM thread_spawn_edges WHERE parent_thread_id=?)').run(id, id);
          return { threadId: id, archived: true };
        }, async close() {},
      }));
      const run = () => archive.run(call, settings, async () => { idleChecks++; if (scenario === 'voice-active') throw new Error('Voice capture is active'); }, { threadId: target });
      if (scenario === 'complete' || scenario === 'not-loaded' || scenario === 'empty') {
        await Promise.all([run(), run()]); await run();
        assert.deepEqual(sends, scenario === 'empty' ? [] : [target], 'nested Voice child is never separately submitted');
        assert.equal(journal.unresolvedArchive(), false);
        assert.equal(idleChecks, 1, 'completed original batch is reused without new observations/effects');
      } else {
        await assert.rejects(run());
        const unknown = scenario === 'lost-ack';
        assert.equal(journal.unresolvedArchive(), unknown);
        if (unknown) {
          const sent = [...sends]; await assert.rejects(run(), { code: 'phone_archive_unknown' }); assert.deepEqual(sends, sent);
          const saved = journal.callCommand(call.callId, 'call.archiveVoice');
          assert.deepEqual((saved?.receipt?.result as { archivedIds: string[] }).archivedIds, []);
          journal.loseEpoch(epoch); const next = randomUUID(); journal.beginEpoch(next);
          const replacement = journal.admitCall(policy.outgoing(next, randomUUID(), 'main', 'personal'));
          journal.discardUnpreparedCall(replacement.callId);
          await assert.rejects(archive.reconcile(call.callId), { code: 'phone_archive_unconfirmed' });
          assert.equal(journal.unresolvedArchive(), true);
          // Simulate later durable state becoming visible; reconciliation performs no mutation.
          db.exec("UPDATE threads SET archived=1 WHERE thread_source='voice_chat'");
          const receipt = structuredClone(saved?.receipt);
          await archive.reconcile(call.callId); await archive.reconcile(call.callId);
          assert.deepEqual(sends, sent);
          assert.equal(journal.unresolvedArchive(), false);
          assert.deepEqual(journal.callCommand(call.callId, 'call.archiveVoice')?.receipt, receipt, 'later evidence never rewrites an original unknown receipt');
          assert.equal(journal.callCommand(call.callId, 'call.archiveVoice')?.archiveResolution?.state, 'observed_archived');
          journal.admitCall(policy.outgoing(next, randomUUID(), 'main', 'personal'));
        } else assert.deepEqual(sends, []);
      }
    });
  }
});

test('unscoped Micro cleanup never opens App Tools or archives unrelated Voice tasks', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-voice-unscoped-'));
  const journal = new PhoneJournal(root, { hostId: 'test', serviceNodeId: 'phone' });
  t.after(() => { journal.close(); rmSync(root, { recursive: true, force: true }); });
  const epoch = randomUUID(); journal.beginEpoch(epoch);
  const policy = new PhoneAdmission({ incoming: [], recipients: [{ id: 'main', destination: 'sip:fixture@test' }] });
  const call = journal.admitCall(policy.outgoing(epoch, randomUUID(), 'main', 'main'));
  const archive = new PhoneVoiceArchive(journal, () => { throw new Error('App Tools must not be opened'); });
  await archive.run(call, { databasePath: join(root, 'absent'), appTools: {} } as PhoneVoiceArchiveSettings,
    async () => assert.fail('unscoped cleanup must not query capture'));
  assert.equal(journal.callCommand(call.callId, 'call.archiveVoice'), null);
});

test('cleanup retains the exact task still referenced by the Desktop Voice overlay', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-voice-retained-')), databasePath = join(root, 'state.sqlite');
  const journal = new PhoneJournal(join(root, 'phone'), { hostId: 'test', serviceNodeId: 'phone' });
  t.after(() => { journal.close(); rmSync(root, { recursive: true, force: true }); });
  const voice = randomUUID();
  writeFileSync(join(root, '.codex-global-state.json'), JSON.stringify({
    'electron-persisted-atom-state': { 'realtime-voice-most-recent-thread': { conversationId: voice, hostId: 'local' } },
  }));
  const epoch = randomUUID(); journal.beginEpoch(epoch);
  const policy = new PhoneAdmission({ incoming: [], recipients: [{ id: 'main', destination: 'sip:fixture@test' }] });
  const call = journal.admitCall(policy.outgoing(epoch, randomUUID(), 'main', 'main'));
  const archive = new PhoneVoiceArchive(journal, () => { throw new Error('retained Voice task must not be archived'); });
  await archive.run(call, { databasePath, appTools: {} } as PhoneVoiceArchiveSettings,
    async () => assert.fail('retained Voice cleanup must not recheck capture'), { threadId: voice });
  assert.equal(journal.callCommand(call.callId, 'call.archiveVoice'), null);
});

test('cleanup keeps a PhoneBridge-created task and verifies capture has stopped', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-voice-created-'));
  const journal = new PhoneJournal(root, { hostId: 'test', serviceNodeId: 'phone' });
  t.after(() => { journal.close(); rmSync(root, { recursive: true, force: true }); });
  const epoch = randomUUID(); journal.beginEpoch(epoch);
  const policy = new PhoneAdmission({ incoming: [], recipients: [{ id: 'main', destination: 'sip:fixture@test' }] });
  const call = journal.admitCall(policy.outgoing(epoch, randomUUID(), 'main', 'main'));
  const threadId = randomUUID();
  const intent = { epoch, callId: call.callId, operationId: randomUUID(), method: 'call.createVoice' as const,
    model: 'gpt-6-sol', reasoningEffort: 'high', requestHash: `sha256:${'0'.repeat(64)}` };
  journal.submit(intent); journal.finishVoiceCreation(intent, threadId);
  const archive = new PhoneVoiceArchive(journal, () => { throw new Error('Created task must not be archived'); });
  let idleChecks = 0;
  const settings = { databasePath: join(root, 'absent'), appTools: {} } as PhoneVoiceArchiveSettings;
  await archive.run(call, settings, async () => { idleChecks++; }, { threadId });
  assert.equal(idleChecks, 1);
  assert.equal(journal.callCommand(call.callId, 'call.archiveVoice'), null);
  await assert.rejects(archive.run(call, settings, async () => { throw new Error('Capture remains active'); }, { threadId }),
    /Capture remains active/);
});
