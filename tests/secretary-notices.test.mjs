import { assignmentFor } from './secretary-fixture.mjs';
import test from 'node:test';
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
import { IvyError, newOperationId } from '../dist/packages/sdk/src/node.js';
import { SecretaryEngine } from '../dist/services/secretary/src/engine.js';
import { SecretaryNotices } from '../dist/services/secretary/src/notices.js';
import { AssignmentDelivery } from '../dist/services/secretary/src/assignment-delivery.js';
import { assignmentName, defaultConfiguration } from '../dist/services/secretary/src/assignment-schema.js';
import { secretaryRegistry } from '../dist/services/secretary/src/schema.js';
import { assessment, message, rootObject, settingsFor } from './secretary-fixture.mjs';

async function fixture(t) {
  assert.ok(process.env.IVY_TEST_CHAT_FIXTURE, 'Use the public isolated harness with --chat-fixture.');
  const { testChatFixture } = await import(pathToFileURL(process.env.IVY_TEST_CHAT_FIXTURE).href), f = await testChatFixture(t);
  const channel = f.connect('secretary-notice', 'secretary', secretaryRegistry());
  // Exercise the actual RPC error envelope between independent SDK packages; classes are not shared.
  const client = { async request(...args) {
    // This in-process provider has no service loop to renew its heartbeat.
    await f.clients.first.client.request('service.heartbeat', { ready: true, diagnostics: [] });
    try { return structuredClone(await channel.client.request(...args)); } catch (error) { if (!error.toWire) throw error; const wire = error.toWire(); throw new IvyError(wire.data.code, wire.message, wire.data.outcome, wire.data.details); }
  } };
  const root = await rootObject(client), settings = settingsFor(root, channel.owner.serviceNodeId, 'chat-owner');
  let now = new Date('2026-09-07T09:00:00.000Z'), engine;
  const reopen = async () => { engine = new SecretaryEngine(client, settings, channel.owner, channel.signal, () => now); await engine.initialize(); return new SecretaryNotices(engine); };
  const notices = await reopen();
  const save = async (value = assessment()) => {
    const capture = await engine.action('chat-owner', { action: 'capture', operationId: await newOperationId(client), expectedScope: settings.identity.scope, sourceId: 'personal-inbox', message: message() });
    const decision = await engine.action('chat-owner', { action: 'assess', operationId: await newOperationId(client), expectedScope: settings.identity.scope, item: capture.effect, assessment: value });
    return decision.effect.objectId;
  };
  const browserNotices = async () => {
    const items = []; let afterSequence = 0;
    for (;;) {
      const page = await client.request('events.read', { afterSequence, filter: { topics: ['secretary.browser-notification'] } });
      items.push(...page.items); if (!page.hasMore) return items; afterSequence = page.throughSequence;
    }
  };
  return { ...f, client, settings, notices, reopen, save, browserNotices, setNow(value) { now = new Date(value); }, get engine() { return engine; } };
}

test('Secretary defers notices, recovers a lost admission and continues the original delivery after an uncertain transport acknowledgement', { timeout: 60000 }, async t => {
  const f = await fixture(t); f.settings.policy.silentTime = { timeZone: 'Europe/Berlin', start: '22:00', end: '07:00' }; f.setNow('2026-09-07T21:00:00.000Z');
  const id = await f.save(); await f.notices.step(id); assert.equal(f.calls.length, 0); assert.equal((await f.engine.item(id)).value.notice.reason, 'silent_time');
  assert.equal((await f.browserNotices()).length, 0);
  await f.save(assessment({ disposition: 'record', notification: 'none' }));
  const checkpoint = await f.engine.action('chat-owner', { action: 'checkpoint', operationId: await newOperationId(f.client), expectedScope: f.settings.identity.scope, sourceId: 'personal-inbox', previousCursor: null, nextCursor: 'observed-empty-page', captures: [] });
  assert.equal(checkpoint.phase, 'succeeded', 'Silent time must not pause source progression or triage.');
  f.setNow('2026-09-08T06:00:00.000Z'); await f.notices.step(id);
  assert.equal((await f.browserNotices()).length, 1);
  const prepared = (await f.engine.item(id)).value.notice; assert.ok(prepared.request); assert.equal(prepared.state, 'dispatching'); assert.equal(f.calls.length, 0);
  f.beforeNotify(async () => assert.deepEqual((await f.engine.item(id)).value.notice.request, prepared.request));
  f.lose(); await f.notices.step(id); assert.equal((await f.engine.item(id)).value.notice.state, 'dispatching');
  const restarted = await f.reopen(); await restarted.step(id); assert.equal((await f.engine.item(id)).value.notice.state, 'dispatching');
  assert.equal(f.calls.filter(call => call.name === 'notify').length, 2, 'A lost admission is retried idempotently while Main is still answering.');
  await f.offer(); await restarted.step(id); assert.equal((await f.engine.item(id)).value.notice.state, 'outcome_unknown');
  await f.acknowledge(); await restarted.step(id); const confirmed = await f.engine.item(id); assert.equal(confirmed.value.notice.state, 'confirmed'); assert.ok(confirmed.value.notice.evidence);
  const calls = f.calls.length; await restarted.step(id); assert.equal(f.calls.length, calls); assert.deepEqual((await f.engine.item(id)).pin, confirmed.pin);
  assert.equal((await f.browserNotices()).length, 1, 'Recovery does not duplicate the browser notice.');
  const turns = f.frames.filter(frame => frame.method === 'turn/start');
  assert.equal(turns.length, 1, 'Secretary must hand off exactly one turn to the existing Main.');
  assert.match(JSON.stringify(turns[0].params.input), /internal service notification/);
});

test('Secretary waits until the configured minimum message age before handing a notice to Main', { timeout: 60000 }, async t => {
  const f = await fixture(t); f.setNow('2026-09-07T08:04:59.999Z');
  const id = await f.save(); await f.notices.step(id);
  assert.equal(f.calls.length, 0); assert.equal((await f.engine.item(id)).value.notice.reason, 'minimum_message_age');
  f.setNow('2026-09-07T08:05:00.000Z'); await f.notices.step(id);
  const prepared = (await f.engine.item(id)).value.notice;
  assert.equal(prepared.state, 'dispatching'); assert.equal(prepared.reason, null); assert.ok(prepared.request);
});

for (const state of ['sent', 'outcome_unknown']) test('a critical legacy Voice notice follows its forwarded prompt: ' + state, async () => {
  const operationId = '00000000-0000-4000-8000-000000000001', callId = '00000000-0000-4000-8000-000000000002';
  const serviceNodeId = 'fixture.phone-bridge', definitionHash = 'sha256:' + '1'.repeat(64), updates = [], reads = [];
  let request;
  const client = { async request(method, args) {
    if (method === 'serviceNodes.get') return { serviceName: 'phone-bridge', connected: true, synced: true, ready: true };
    if (method === 'tools.list') return { provider: { node: { serviceNodeId } }, items: [{ qualifiedName: 'phone.' + args.namePrefix,
      definition: { interfaceVersion: '1.0.0' }, definitionHash }], nextCursor: null };
    if (method === 'tools.call' && args.qualifiedName === 'phone.request') {
      request = args.arguments;
      return { callId, operationId: 'original-incoming-call', direction: 'incoming', recipientId: null, principalId: 'user', route: 'voice' };
    }
    if (method === 'tools.call' && args.qualifiedName === 'phone.operation') {
      reads.push(args.arguments);
      assert.equal(args.arguments.operationId, operationId);
      return { intent: { method: 'call.forwardVoice', callId, operationId, prompt: request.voicePrompt },
        phase: 'result', receipt: { ok: true, result: { state } } };
    }
    assert.fail('Unexpected RPC method: ' + method);
  } };
  const escalation = { serviceNodeId, recipientId: 'personal', requestDefinitionHash: definitionHash };
  const notice = { text: 'Critical event.', voice: { operationId, callId: null, state: 'queued', errorCode: null } };
  const item = { pin: { objectId: 'item', revision: 2 }, value: { notice,
    decision: { operationId: 'decision', assessment: { notification: 'voice', urgency: 'critical' }, policy: { voiceEscalation: escalation } } } };
  const engine = { client, settings: { identity: { principalId: 'secretary' } }, async verifyOwner() {}, now: () => new Date('2026-09-25T10:00:00.000Z'),
    store: { async write(_key, value) { updates.push(value.notice); } } };
  const notices = new SecretaryNotices(engine);
  await notices.voice(item, notice);
  if (state === 'sent') await notices.voice({ ...item, value: { ...item.value, notice: updates.at(-1) } }, updates.at(-1));
  assert.equal(updates.at(-1).voice.state, state === 'sent' ? 'confirmed' : 'outcome_unknown');
  assert.ok(reads.every(read => !read.method));
});

test('a critical legacy Voice assessment retains a PhoneBridge UUID before dispatch', { timeout: 60000 }, async t => {
  const f = await fixture(t);
  f.settings.policy.voiceEscalation = { serviceNodeId: 'fixture.phone-bridge', recipientId: 'personal', requestDefinitionHash: 'sha256:' + '0'.repeat(64) };
  const id = await f.save(assessment({ urgency: 'critical', notification: 'voice' }));
  const item = await f.engine.item(id);
  assert.match(item.value.notice.voice.operationId, /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i);
});

test('parallel Secretary notice recovery preserves original recipients and accepts refreshed tool definitions and refuses rewritten message evidence', { timeout: 60000 }, async t => {
  const f = await fixture(t), id = await f.save(); await f.notices.step(id);
  const original = (await f.engine.item(id)).value.notice;
  const other = await f.reopen(), results = await Promise.allSettled([f.notices.step(id), other.step(id)]);
  for (const value of results) if (value.status === 'rejected') assert.equal(value.reason.code, 'revision_conflict');
  await other.step(id); assert.deepEqual((await f.engine.item(id)).value.notice.request, original.request);
  const offered = await f.acknowledge(); assert.equal(offered.replies.length, 1); await other.step(id); assert.equal((await f.engine.item(id)).value.notice.state, 'confirmed');
  const changed = await f.save(), before = f.calls.filter(call => call.name === 'notify').length;
  await other.step(changed); await other.step(changed); assert.equal((await f.engine.item(changed)).value.notice.reason, 'awaiting_main');
  assert.equal(f.calls.filter(call => call.name === 'notify').length, before + 1);
  const tampered = await f.engine.item(changed);
  await f.engine.store.amend(tampered, 'secretary/item', { ...tampered.value, notice: { ...tampered.value.notice, text: 'Changed summary' } });
  await assert.rejects(other.step(changed), { code: 'secretary_evidence_conflict' });
});

test('assignment waits five minutes and reconciles a late Main confirmation after archival', { timeout: 60000 }, async t => {
  const f = await fixture(t);
  const at = '2026-09-07T08:00:00.000Z';
  const configuration = defaultConfiguration(f.settings, at);
  await f.engine.store.create('secretary/configuration', 'configuration', configuration, await newOperationId(f.client));
  const builtIn = assignmentFor(at, { minimumNotificationAgeMinutes: 5 });
  const assignment = { ...builtIn, enabled: true };
  const selected = await f.engine.store.create('secretary/assignment', assignmentName(assignment.assignmentId), assignment, await newOperationId(f.client));
  const decision = { schemaVersion: 1, urgency: 'normal', notification: 'main', text: 'Ein Kontakt hat eine ungelesene Teams-Nachricht gesendet.', reason: 'Neue Arbeitsnachricht' };
  const execution = {
    schemaVersion: 1, executionId: 'teams-event-one', assignment: selected.pin, assignmentSnapshot: assignment,
    trigger: { kind: 'event', key: 'event:one', occurredAt: at, payload: {} }, effectiveRules: configuration.rules,
    executionTarget: { serviceNodeId: 'fixture.agent-manager', threadCwd: 'C:\\projects\\sample-project', model: null, effort: 'medium', permissions: ':read-only' },
    phase: 'completed', serviceNodeId: 'fixture.agent-manager', nativeTarget: null, threadId: null, turnId: null,
    nativeOperations: { threadStart: null, turnStart: null, archive: null, delete: null }, preflightOutput: null,
    result: JSON.stringify(decision), errorCode: null, createdAt: at, startedAt: at, completedAt: at, archivedAt: null,
    deleteAfter: null, deletedAt: null, updatedAt: at,
  };
  const record = await f.engine.store.create('secretary/execution', 'execution-teams-event-one', execution, await newOperationId(f.client));
  const delivery = new AssignmentDelivery(f.engine);
  f.setNow('2026-09-07T08:04:59.999Z');
  assert.equal(await delivery.step(record), false);
  assert.equal(f.calls.length, 0);
  assert.equal(({ value: (await f.engine.store.read('secretary/execution', record.pin.objectId)).value.delivery }).value.reason, 'minimum_message_age');
  assert.equal((await f.browserNotices()).length, 0);
  f.setNow('2026-09-07T08:05:00.000Z');
  assert.equal(await delivery.step(record), false);
  assert.equal((await f.browserNotices()).length, 1);
  assert.equal((await f.browserNotices())[0].payload.body, decision.text);
  const prepared = ({ value: (await f.engine.store.read('secretary/execution', record.pin.objectId)).value.delivery });
  assert.ok(prepared.value.request);
  assert.deepEqual(prepared.value.request.source, record.pin);
  assert.equal(f.calls.filter(call => call.name === 'notify').length, 1);
  const { target: originalTarget, ...withoutTarget } = prepared.value;
  const current = await f.engine.store.read('secretary/execution', record.pin.objectId);
  const existing = await f.engine.store.amend(current, 'secretary/execution', { ...current.value, delivery: withoutTarget });
  assert.equal(await delivery.step(record), false, 'An existing prepared handoff can still use its unchanged configured owner.');
  await f.engine.store.amend(existing, 'secretary/execution', { ...existing.value, delivery: { ...withoutTarget, target: originalTarget } });
  const restarted = new AssignmentDelivery(f.engine);
  const disabled = await f.engine.store.amend(selected, 'secretary/assignment', { ...assignment, enabled: false });
  assert.equal(await restarted.step(record), false);
  assert.equal(f.calls.filter(call => call.name === 'notify').length, 2, 'Disabling an assignment stops retries while the original outcome remains recoverable.');
  await f.engine.store.amend(disabled, 'secretary/assignment', assignment);
  assert.equal(await restarted.step(record), false);
  assert.equal(f.calls.filter(call => call.name === 'notify').length, 3);
  assert.deepEqual(f.calls.filter(call => call.name === 'notify').map(call => call.args.operationId), Array(3).fill(prepared.value.operationId));
  await f.offer();
  assert.equal(await restarted.step(record), true);
  const uncertain = await f.engine.store.read('secretary/execution', record.pin.objectId);
  assert.equal(uncertain.value.delivery.state, 'outcome_unknown');
  assert.ok(uncertain.value.delivery.evidence);
  const archived = await f.engine.store.amend(uncertain, 'secretary/execution', { ...uncertain.value,
    phase: 'archived', archivedAt: '2026-09-07T08:05:00.000Z', deleteAfter: '2026-09-14T08:05:00.000Z' });
  await f.acknowledge();
  f.setNow('2026-09-07T08:05:31.000Z');
  const { AssignmentRunner } = await import('../dist/services/secretary/src/assignment-runner.js');
  await new AssignmentRunner(f.engine).tick();
  const recovered = await f.engine.store.read('secretary/execution', record.pin.objectId);
  assert.equal(recovered.value.phase, archived.value.phase);
  assert.equal(recovered.value.delivery.state, 'confirmed');
  assert.equal(f.engine.recoveryIssues.has('delivery:' + execution.executionId), false);
  const confirmed = ({ value: (await f.engine.store.read('secretary/execution', record.pin.objectId)).value.delivery });
  assert.equal(confirmed.value.state, 'confirmed');
  assert.ok(confirmed.value.evidence);
  assert.equal(await delivery.step(record), true);
  assert.equal(f.calls.filter(call => call.name === 'notify').length, 3);
  assert.equal(f.frames.filter(frame => frame.method === 'turn/start').length, 1);
  assert.equal((await f.browserNotices()).length, 1);
  const callAssignment = { ...assignmentFor(at, { assignmentId: 'immediate-assignment' }), enabled: true };
  const callPin = await f.engine.store.create('secretary/assignment', assignmentName(callAssignment.assignmentId), callAssignment, await newOperationId(f.client));
  const callExecution = { ...execution, executionId: 'teams-call-one', assignment: callPin.pin, assignmentSnapshot: callAssignment,
    trigger: { kind: 'event', key: 'event:call', occurredAt: at, payload: {} }, result: JSON.stringify(decision) };
  const callRecord = await f.engine.store.create('secretary/execution', 'execution-teams-call-one', callExecution, await newOperationId(f.client));
  f.setNow(at);
  await delivery.step(callRecord);
  assert.equal(f.calls.filter(call => call.name === 'notify').length, 4, 'an assignment with no configured delay sends immediately');
});

test('a refused Main admission gets a fresh operation after backoff', { timeout: 60000 }, async t => {
  const f = await fixture(t), at = '2026-09-07T08:00:00.000Z';
  const configuration = defaultConfiguration(f.settings, at);
  await f.engine.store.create('secretary/configuration', 'configuration', configuration, await newOperationId(f.client));
  const assignment = { ...assignmentFor(at, { minimumNotificationAgeMinutes: 5 }), enabled: true };
  const selected = await f.engine.store.create('secretary/assignment', assignmentName(assignment.assignmentId), assignment, await newOperationId(f.client));
  const execution = {
    schemaVersion: 1, executionId: 'refused-event', assignment: selected.pin, assignmentSnapshot: assignment,
    trigger: { kind: 'event', key: 'event:refused', occurredAt: at, payload: {} }, effectiveRules: configuration.rules,
    executionTarget: { serviceNodeId: 'fixture.agent-manager', threadCwd: 'C:\\projects\\sample-project', model: null, effort: 'medium', permissions: ':read-only' },
    phase: 'completed', serviceNodeId: 'fixture.agent-manager', nativeTarget: null, threadId: null, turnId: null,
    nativeOperations: { threadStart: null, turnStart: null, archive: null, delete: null }, preflightOutput: null,
    result: JSON.stringify({ schemaVersion: 1, urgency: 'normal', notification: 'main', text: 'A new message.', reason: 'Relevant' }), errorCode: null,
    createdAt: at, startedAt: at, completedAt: at, archivedAt: null, deleteAfter: null, deletedAt: null, updatedAt: at,
  };
  const record = await f.engine.store.create('secretary/execution', 'execution-refused-event', execution, await newOperationId(f.client));
  const delivery = new AssignmentDelivery(f.engine);
  let refused = false;
  f.beforeNotify(async () => {
    if (refused) return;
    const saved = ({ value: (await f.engine.store.read('secretary/execution', record.pin.objectId)).value.delivery });
    const operation = await f.first.operations.begin('chat-owner', saved.value.request);
    await f.first.operations.rejectUnapplied(operation.pin.objectId, 'chat_queue_full', 'The Main queue is full.');
    refused = true;
  });
  f.setNow('2026-09-07T08:05:00.000Z');
  assert.equal(await delivery.step(record), false);
  const first = ({ value: (await f.engine.store.read('secretary/execution', record.pin.objectId)).value.delivery });
  assert.equal(first.value.state, 'queued');
  assert.equal(first.value.attempts, 1);
  assert.equal(f.calls.filter(call => call.name === 'notify').length, 1);
  f.setNow('2026-09-07T08:05:31.000Z');
  assert.equal(await delivery.step(record), false);
  const second = ({ value: (await f.engine.store.read('secretary/execution', record.pin.objectId)).value.delivery });
  assert.notEqual(second.value.operationId, first.value.operationId);
  assert.equal(f.calls.filter(call => call.name === 'notify').length, 2);
});
