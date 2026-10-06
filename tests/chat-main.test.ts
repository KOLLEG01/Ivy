import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { hashJson } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Chat } from '../packages/contracts/src/generated.js';
import { nativePath } from '../services/chat-bridge/src/native-driver.js';
import { ChatAdmission } from '../services/chat-bridge/src/admission.js';
import { ChatMain } from '../services/chat-bridge/src/chat-main.js';
import { ChatOperations } from '../services/chat-bridge/src/operations.js';
import { ChatNativeDriver } from '../services/chat-bridge/src/native-driver.js';
import { resolveChatPlan } from '../services/chat-bridge/src/native-plan.js';
import { chatNativeFixture as fixture } from './fixtures/chat-native.js';

const hasCode = (code: string) => (error: unknown) => error instanceof IvyError && error.code === code;
const reconfigured = (f: Awaited<ReturnType<typeof fixture>>, overrides: Partial<Chat.Definition> = {}) => {
  const admission = new ChatAdmission({ ...f.admission.definition,
    channels: [{ channel: f.channel, displayName: 'Updated configuration' }], ...overrides });
  const owner = f.clients.second.context;
  return new ChatMain(new ChatOperations(f.first.store, admission), new ChatNativeDriver(f.first.store, admission,
    { serviceNodeId: owner.serviceNodeId!, generation: owner.generation! }));
};

test('explicit Main resumption retains the native conversation and predecessor across configurations without replaying history', async t => {
  const f = await fixture(t), created = await f.first.create('alice', f.request('initial'));
  if (created.outcome?.action !== 'createMain') throw Error('Expected Main.');
  const source = (await f.first.find())!, next = reconfigured(f);
  const request = { ...f.request('resume'), expectedBridge: next.admission.expected('alice'), resumeMain: source.pin };
  f.loseReply(); const resumed = await next.create('alice', request);
  assert.equal(resumed.phase, 'succeeded'); if (resumed.outcome?.action !== 'createMain') throw Error('Expected resumed Main.');
  assert.deepEqual(resumed.outcome.binding.data.primary, created.outcome.binding.data.primary);
  assert.deepEqual(resumed.outcome.binding.data.predecessor, created.outcome.binding.object);
  assert.equal(resumed.outcome.binding.data.definitionHash, next.admission.definitionHash);
  assert.deepEqual(f.frames.map(frame => frame['method']), ['thread/start', 'thread/resume']);
  assert.equal((f.frames[1]!['params'] as Record<string, unknown>)['threadId'], 'native-main-1');
  assert.equal((f.frames[1]!['params'] as Record<string, unknown>)['excludeTurns'], true);
  assert.deepEqual(await f.first.find(), source);
  assert.deepEqual((await next.store.read('chat-bridge/binding', created.outcome.binding.object)).value, created.outcome.binding.data);
  f.unavailable(true); assert.deepEqual(await next.create('alice', request), resumed); assert.equal(f.frames.length, 2);
});

test('Main resumption refuses stale, busy or unrelated source definitions before accepting native work', async t => {
  const f = await fixture(t), created = await f.first.create('alice', f.request('initial'));
  if (created.outcome?.action !== 'createMain') throw Error('Expected Main.');
  const source = (await f.first.find())!, next = reconfigured(f);
  const request = { ...f.request('resume'), expectedBridge: next.admission.expected('alice'), resumeMain: source.pin };
  await assert.rejects(next.create('alice', { ...request, resumeMain: { ...source.pin, revision: source.pin.revision - 1 } }), hasCode('revision_conflict'));
  const foreign = reconfigured(f, { workspaceId: 'unrelated-workspace' });
  await assert.rejects(foreign.create('alice', { ...request, expectedBridge: foreign.admission.expected('alice') }), hasCode('chat_definition_mismatch'));
  await f.first.queue.send('alice', { action: 'send', operationId: f.operation('queued'), expectedBridge: f.admission.expected('alice'),
    channel: f.channel, messageId: 'queued', expectedBinding: created.outcome.binding.object, payload: { text: 'Pending input.', images: [] } });
  await assert.rejects(next.create('alice', { ...request, resumeMain: (await f.first.find())!.pin }), hasCode('chat_main_busy'));
  assert.equal(await next.operations.find('alice', request.operationId), null); assert.equal(f.frames.length, 1);
});

for (const version of ['0.154.0'] as const) test('explicit Chat Main ' + version + ' saves original native intent before dispatch, replaces an empty Main and preserves historical receipts', async t => {
  const f = await fixture(t, version), request = f.request('initial');
  assert.equal(await f.first.find(), null);
  f.beforeNative(async call => {
    const main = (await f.second.find())!;
    assert.equal(main.value.queue.length, 0); assert.ok(main.value.pendingAction);
    const action = await f.second.store.read('chat-bridge/operation', main.value.pendingAction.objectId);
    assert.equal(action.value.phase, 'applying'); assert.ok(action.value.nativeCall);
    const saved = await f.second.native.get(action.pin.objectId, action.value.nativeCall);
    const original = await f.second.native.evidence.request(action.pin.objectId, saved.value);
    assert.equal(saved.value.operationId, call.operationId); assert.equal(saved.value.callerPrincipalId, call.callerPrincipalId);
    assert.deepEqual(original.params, call.arguments); assert.equal(saved.value.requestHash, hashJson({ method: call.definition.name, params: call.arguments }));
  });
  f.loseReply(); const created = await f.first.create('alice', request);
  assert.equal(created.phase, 'succeeded'); if (created.outcome?.action !== 'createMain') throw Error('Expected Main.');
  assert.equal(created.outcome.binding.data.primary.nativeId, 'native-main-1');
  assert.equal(created.outcome.binding.data.predecessor, null); assert.equal(f.frames.length, 1);
  const before = (await f.first.find())!; assert.equal(before.value.pendingAction, null);
  await assert.rejects(f.second.create('alice', f.request('stale')), hasCode('revision_conflict'));
  const replaced = await f.second.create('bob', f.request('replacement', before.pin.revision, 'bob'));
  assert.equal(replaced.phase, 'succeeded'); if (replaced.outcome?.action !== 'createMain') throw Error('Expected replacement.');
  assert.deepEqual(replaced.outcome.binding.data.predecessor, created.outcome.binding.object);
  assert.equal(replaced.outcome.binding.data.primary.nativeId, 'native-main-2');
  assert.deepEqual((await f.first.store.read('chat-bridge/binding', created.outcome.binding.object)).value, created.outcome.binding.data);
  f.unavailable(true); assert.deepEqual(await f.second.create('alice', request), created);
  assert.equal(f.frames.length, 2, 'Historical action lookup must not require a new native thread.');
});

test('Chat Main exact concurrent retries share one native start; different creations keep one winner and one definite admission refusal', async t => {
  const f = await fixture(t), request = f.request('same');
  const same = await Promise.all([f.first.create('alice', request), f.second.create('alice', request)]);
  assert.deepEqual(same[0], same[1]); assert.equal(same[0]!.phase, 'succeeded'); assert.equal(f.frames.length, 1);
  const revision = (await f.first.find())!.pin.revision;
  let arrivals = 0; const gate = Promise.withResolvers<void>();
  f.intercept(async (_params, result) => {
    if (result.object.contractKey === 'chat-bridge/operation' && result.revision.revision === 1) { if (++arrivals === 2) gate.resolve(); await gate.promise; }
  });
  const competing = await Promise.all([f.first.create('alice', f.request('one', revision)), f.second.create('bob', f.request('two', revision, 'bob'))]);
  f.intercept(null);
  assert.deepEqual(competing.map(value => value.phase).sort(), ['failed', 'succeeded']); assert.equal(f.frames.length, 2);
  const refused = competing.find(value => value.phase === 'failed')!; assert.equal(refused.error?.outcome, 'not_executed');
  assert.deepEqual(await f.second.create(refused.callerPrincipalId, refused.request as Chat.CreateMainRequest), refused);
  assert.equal((await f.first.find())!.value.pendingAction, null);
});

test('concurrent first messages automatically create exactly one Main and bind every input to it', async t => {
  const f = await fixture(t);
  const request = (label: string, caller: string): Chat.SendRequest => ({ action: 'send', operationId: f.operation(label), expectedBridge: f.admission.expected(caller),
    channel: f.channel, messageId: label, expectedBinding: null, payload: { text: 'First input from ' + caller, images: [] } });
  const admitted = await Promise.all([f.first.queue.send('alice', request('alice-first', 'alice')), f.second.queue.send('bob', request('bob-first', 'bob'))]);
  assert.ok(admitted.every(value => value.phase === 'succeeded' && value.outcome?.action === 'send'));
  const main = await f.first.queue.main(); assert.ok(main.value.binding); assert.equal(main.value.queue.length, 2);
  assert.ok(main.value.queue.every(ticket => ticket.binding.objectId === main.value.binding!.objectId && ticket.binding.revision === main.value.binding!.revision));
  assert.equal(f.frames.filter(frame => frame['method'] === 'thread/start').length, 1);
  assert.equal((await f.first.store.read('chat-bridge/binding', main.value.binding)).value.primary.nativeId, 'native-main-1');
});

test('a queued original input blocks Main replacement before accepting a new action', async t => {
  const f = await fixture(t), created = await f.first.create('alice', f.request('initial'));
  if (created.outcome?.action !== 'createMain') throw Error('Expected Main.');
  const send: Chat.SendRequest = { action: 'send', operationId: f.operation('message'), expectedBridge: f.admission.expected('alice'), channel: f.channel,
    messageId: 'message', expectedBinding: created.outcome.binding.object, payload: { text: 'Queued for this Main.', images: [] } };
  await f.second.queue.send('alice', send);
  const before = (await f.first.find())!;
  await assert.rejects(f.first.create('alice', f.request('busy', before.pin.revision)), hasCode('chat_main_busy'));
  assert.equal(await f.first.operations.find('alice', f.operation('busy')), null); assert.equal(f.frames.length, 1);
  assert.deepEqual((await f.first.find())!.pin, before.pin);
});

test('dispatched Main creation and confirmed epoch loss block replacement under the original action without replay', async t => {
  const f = await fixture(t), request = f.request('held'); f.hold();
  const pending = await f.first.create('alice', request); assert.equal(pending.phase, 'applying'); assert.equal(f.frames.length, 1);
  const call = await f.first.native.get((await f.first.operations.find('alice', f.operation('held')))!.pin.objectId, pending.nativeCall!);
  assert.equal(call.value.state, 'dispatched');
  assert.equal((await f.second.create('alice', request)).phase, 'applying'); assert.equal(f.frames.length, 1);
  await assert.rejects(f.second.create('bob', f.request('replacement', (await f.first.find())!.pin.revision, 'bob')), hasCode('chat_pending_action'));
  f.absence('owner'); await assert.rejects(f.first.create('alice', request), hasCode('chat_native_absence_conflict')); f.absence(null);
  f.restart(); const unknown = await f.second.create('alice', request);
  assert.equal(unknown.phase, 'outcome_unknown'); assert.equal(unknown.error?.outcome, 'unknown'); assert.ok((await f.first.find())!.value.pendingAction);
  f.unavailable(true); assert.deepEqual(await f.first.create('alice', request), unknown); assert.equal(f.frames.length, 1);
});

test('generic native absence cannot authorize Main creation; exact original owner recovery can', async t => {
  const f = await fixture(t), request = f.request('absence'); f.absence('generic');
  await assert.rejects(f.first.create('alice', request)); assert.equal(f.frames.length, 0);
  const action = (await f.first.operations.find('alice', request.operationId))!; assert.ok(action.value.nativeCall);
  f.absence(null); const completed = await f.second.create('alice', request); assert.equal(completed.phase, 'succeeded'); assert.equal(f.frames.length, 1);
});

test('an exact failed native start retains the failure and releases its claim for an explicitly observed new action', async t => {
  const f = await fixture(t); f.reply({ error: { code: -32600, message: 'Synthetic thread creation refused.' } });
  const failed = await f.first.create('alice', f.request('failed')); assert.equal(failed.phase, 'failed'); assert.equal(failed.error?.code, 'native_error');
  assert.ok(failed.nativeCall); assert.equal((await f.first.find())!.value.pendingAction, null); assert.equal((await f.first.find())!.value.binding, null);
  assert.deepEqual(await f.second.create('alice', f.request('failed')), failed); assert.equal(f.frames.length, 1);
  const replacement = await f.second.create('bob', f.request('explicit-new-attempt', (await f.first.find())!.pin.revision, 'bob'));
  assert.equal(replacement.phase, 'failed'); assert.equal(f.frames.length, 2);
});

test('Main admits every Hive caller while refusing substituted requests, stale service generations and wrong configured project roots before native effects', async t => {
  const f = await fixture(t); let writes = 0; f.intercept(() => { writes++; });
  assert.equal(f.admission.expected('mallory').callerPrincipalId, 'mallory'); assert.equal(writes, 0);
  f.projects.projects[0]!.paths = ['/fixture-other'];
  await assert.rejects(f.first.create('alice', f.request('wrong-path')), hasCode('chat_project_mismatch')); assert.equal(writes, 0); assert.equal(f.frames.length, 0);
  f.projects.projects[0]!.paths = ['/fixture']; const created = await f.first.create('alice', f.request('original'));
  await assert.rejects(f.first.create('alice', { ...f.request('original'), reason: 'Changed original request.' }), hasCode('mutation_conflict'));
  assert.deepEqual(await f.second.create('alice', f.request('original')), created);
  const connection = f.clients.first.context;
  f.kernel.execute({ credentialDigest: connection.credentialDigest, principalId: connection.principalId, transport: 'ws' }, { jsonrpc: '2.0', id: randomUUID(), method: 'service.connect',
    params: { serviceNodeId: 'chat-a', serviceName: 'chat-bridge', hostId: 'fixture', version: 'test', buildId: hashJson('new-boot'), hiveProtocol: 1 } });
  await assert.rejects(f.first.create('alice', f.request('original'))); assert.equal(f.frames.length, 1);
  assert.equal(nativePath('D:\\Testing\\Project\\..\\Project\\'), 'd:/testing/project'); assert.equal(nativePath('/a/project/../project/'), '/a/project');
  assert.throws(() => nativePath('relative'), hasCode('chat_project_mismatch'));
});

test('a prepared Main rechecks project membership immediately before dispatch', async t => {
  const f = await fixture(t), request = f.request('changed-project'); f.projects.projects[0]!.paths = ['/elsewhere'];
  await assert.rejects(f.first.create('alice', request), hasCode('chat_project_mismatch')); assert.equal(f.frames.length, 0);
  f.projects.projects[0]!.paths = ['/fixture'];
  const completed = await f.second.create('alice', request); assert.equal(completed.phase, 'succeeded'); assert.equal(f.frames.length, 1);
});

test('native request preparation rejects a different workspace before publishing any evidence bytes', async t => {
  const f = await fixture(t), operation = await f.first.operations.begin('alice', f.request('foreign-parent'));
  await f.first.store.write('chat-bridge/operation', { ...operation.value, workspaceId: 'different-workspace' }, randomUUID(),
    { objectId: operation.pin.objectId, expectedRevision: operation.pin.revision });
  let writes = 0; f.intercept(() => { writes++; });
  const plan = await resolveChatPlan(f.first.store, f.admission.definition.nativePlan);
  await assert.rejects(f.first.native.prepare(operation.pin.objectId, 'original-native-request', { nativeVersion: plan.nativeVersion,
    catalogSourceHash: plan.catalogSourceHash, method: 'thread/start', params: plan.threadStart } as Chat.NativeRequest), hasCode('chat_scope_mismatch'));
  assert.equal(writes, 0); assert.equal(f.frames.length, 0);
});
