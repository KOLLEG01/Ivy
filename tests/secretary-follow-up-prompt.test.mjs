import test from 'node:test';
import assert from 'node:assert/strict';
import { deriveOperationId, hashJson, IvyError, NativeOwner, operationId } from '../dist/packages/sdk/src/node.js';
import { followUpPrompt, SecretaryFollowUp } from '../dist/services/secretary/src/follow-up.js';
import { SecretaryStore } from '../dist/services/secretary/src/store.js';

function fixture(t) {
  const now = new Date(), calls = [], bindings = [], operations = new Map(), reads = [];
  const target = { serviceNodeId: 'agent-one', hostId: 'fixture', nativeVersion: 'test', nativeExecutableHash: hashJson('native'), catalogHash: hashJson('catalog') };
  const configuration = { execution: { serviceNodeId: target.serviceNodeId, threadCwd: process.cwd(), model: 'original-model', effort: 'medium' } };
  const item = { message: { text: 'Original message' }, decision: null, media: [] };
  const state = { unavailable: false, loseMethod: null, failedMethod: null, readError: false, status: 'completed', answer: 'Original answer', turnId: 'original-turn', version: 1, itemReads: 0 };
  const client = { async request(method, args) {
    if (method === 'objects.stat') return { contractKey: state.sourceContract ?? 'secretary/item', parentId: 'root' };
    if (method === 'tools.list') return { provider: { node: { serviceNodeId: args.serviceNodeId } },
      items: ['status', 'projects'].map(name => ({ qualifiedName: 'agent.' + name, definitionHash: hashJson(name), definition: { interfaceVersion: '1.0.0' } })), nextCursor: null };
    assert.equal(method, 'tools.call');
    if (args.qualifiedName === 'agent.projects') return { source: 'native', observedAt: now.toISOString(), projects: [
      { nativeId: state.projectId ?? 'internal-project', source: 'native', name: 'Internal', paths: [process.cwd()] }] };
    assert.equal(args.qualifiedName, 'agent.status');
    return { ...target, serviceNodeId: args.serviceNodeId, epoch: 'native-epoch' };
  } };
  const store = new SecretaryStore(client, 'root'); t.after(() => store.close());
  store.named = async key => { assert.equal(key, 'secretary/configuration'); return { value: structuredClone(configuration) }; };
  const engine = { client, store, settings: { identity: { principalId: 'secretary' }, recordsPerTick: 2 }, signal: new AbortController().signal,
    recoveryIssues: new Map(), issue(key, code) { this.recoveryIssues.set(key, code); }, now: () => now, async verifyOwner() {}, async item() { state.itemReads++; return { value: structuredClone(item) }; } };
  t.mock.method(NativeOwner.prototype, 'binding', async method => { bindings.push(method); return { definitionHash: hashJson([method, state.version]) }; });
  t.mock.method(NativeOwner.prototype, 'checkOperation', function(observed, call) {
    assert.equal(observed.serviceNodeId, this.target.serviceNodeId);
    for (const field of ['operationId', 'method', 'params', 'definitionHash']) assert.deepEqual(observed[field], call[field]);
  });
  t.mock.method(NativeOwner.prototype, 'operation', function(call) {
    if (state.unavailable) throw new IvyError('outcome_unknown', 'Reply lost.', 'unknown');
    const saved = operations.get(call.operationId);
    if (saved) { this.checkOperation(saved, call); return structuredClone(saved); }
    return { operationId: call.operationId };
  });
  t.mock.method(NativeOwner.prototype, 'dispatch', async function(call, guard) {
    await guard();
    if (operations.has(call.operationId)) return structuredClone(operations.get(call.operationId));
    calls.push({ ...structuredClone(call), serviceNodeId: this.target.serviceNodeId });
    if (call.method === 'turn/start') state.turnId = 'turn-' + calls.length;
    const result = call.method === 'thread/start' ? { thread: { id: 'context-thread' } } : call.method === 'turn/start' ? { turn: { id: state.turnId } } : {};
    const observed = { ...structuredClone(call), serviceNodeId: this.target.serviceNodeId, phase: state.failedMethod === call.method ? 'failed' : 'succeeded', reply: { result } };
    operations.set(call.operationId, observed);
    if (state.loseMethod === call.method) { state.unavailable = true; return undefined; }
    return structuredClone(observed);
  });
  t.mock.method(NativeOwner.prototype, 'status', async () => ({ epoch: 'native-epoch' }));
  t.mock.method(NativeOwner.prototype, 'checkRead', (read, method, params, epoch) => { assert.equal(read.method, method); assert.deepEqual(read.params, params); assert.equal(read.epoch, epoch); });
  t.mock.method(NativeOwner.prototype, 'read', async (method, params) => {
    if (state.readError) throw new IvyError('outcome_unknown', 'Read unavailable.', 'unknown');
    reads.push({ method, params });
    if (method === 'thread/items/list') return { method, params, epoch: 'native-epoch', reply: { error: { code: -32601, message: 'Fixture item pagination unavailable.' } } };
    return { method, params, epoch: 'native-epoch', reply: { result: { data: [{ id: state.turnId, status: state.status, itemsView: params.itemsView,
      items: params.itemsView !== 'full' || state.answer === null ? [] : [{ id: 'answer', type: 'agentMessage', phase: 'final_answer', text: state.answer }] }] } } };
  });
  const context = () => store.technicalCreate('secretary/follow-up-context', 'item-one', { schemaVersion: 1, itemId: 'item-one', threadId: 'context-thread', target,
    model: 'original-model', effort: 'medium', threadCwd: process.cwd(), developerInstructions: 'Original instructions', itemSnapshot: 'Original message',
    retainedAt: now.toISOString(), expiresAt: now.getTime() + 90 * 86400000 });
  const request = nonce => ({ operationId: operationId('fixture', now.getTime(), nonce), itemId: 'item-one', question: 'What happened?' });
  const record = value => store.technicalNamed('secretary/follow-up', 'follow-up-secretary-' + value.operationId);
  const advance = async (request, caller = 'secretary') => {
    const worker = new SecretaryFollowUp(engine);
    await worker.run(request, caller);
    await worker.advance('follow-up-' + caller + '-' + request.operationId);
    const result = await worker.read(request.operationId, caller);
    if (record(request).value.phase === 'failed') throw new IvyError(result.errorCode, 'Original follow-up failed.');
    return result;
  };
  return { engine, store, state, calls, reads, bindings, operations, configuration, item, context, request, record, advance, worker: () => new SecretaryFollowUp(engine) };
}

test('resumed Secretary follow-up explicitly prevents recursive followUp dispatch', () => {
  const prompt = followUpPrompt('item-1', 'Ist die Information noch aktuell?');
  assert.match(prompt, /bereits geöffnet/);
  assert.match(prompt, /nicht erneut auf/);
  assert.match(prompt, /read-only Recherche/);
  assert.match(prompt, /item-1/);
});

test('running follow-ups resume without history and poll metadata until the complete answer is needed', async t => {
  const f = fixture(t), request = f.request('metadata'); f.context(); f.state.status = 'inProgress';
  await f.advance(request); await f.advance(request);
  assert.equal(f.calls.find(call => call.method === 'thread/resume').params.excludeTurns, true);
  assert.ok(f.reads.length); assert.ok(f.reads.every(read => read.method === 'thread/turns/list' && read.params.itemsView === 'notLoaded'));
  f.state.status = 'completed'; assert.equal((await f.advance(request)).answer, 'Original answer');
  assert.equal(f.reads.filter(read => read.params.itemsView === 'full').length, 1);
});

test('completed follow-ups only replay the same original request, independent of property order', async () => {
  const request = { operationId: 'original', itemId: 'item-one', question: 'What happened?' };
  const saved = { value: { request, callerPrincipalId: 'secretary', phase: 'succeeded', threadId: 'thread-one', turnId: 'turn-one', answer: 'Original answer' } };
  const engine = { settings: { identity: { principalId: 'secretary' } }, async verifyOwner() {}, store: { technicalNamed() { return saved; } } };
  const followUp = new SecretaryFollowUp(engine);
  assert.equal((await followUp.run({ question: request.question, itemId: request.itemId, operationId: request.operationId }, 'secretary')).answer, saved.value.answer);
  for (const changed of [{ itemId: 'item-two' }, { question: 'A different question' }])
    await assert.rejects(followUp.run({ ...request, ...changed }, 'secretary'), { code: 'secretary_follow_up_conflict' });
});

test('a retained follow-up turn recovers without repeating native start operations', async t => {
  const f = fixture(t), request = f.request('recover'); f.context(); f.state.readError = true;
  await assert.rejects(f.advance(request, 'secretary'), { code: 'outcome_unknown' });
  const original = f.record(request).value.turnId;
  assert.ok(original); assert.equal(f.calls.length, 2);
  f.state.readError = false; f.state.version = 2;
  const result = await f.advance(request, 'secretary');
  assert.equal(result.turnId, original); assert.equal(result.answer, 'Original answer');
  assert.equal(f.calls.length, 2); assert.equal(f.bindings.length, 2);
});

test('uncertain follow-up starts recover their saved parameters after context or catalog changes', async t => {
  const f = fixture(t), request = f.request('lost-turn'); f.context(); f.state.loseMethod = 'turn/start';
  await assert.rejects(f.advance(request, 'secretary'), { code: 'outcome_unknown' });
  const original = f.record(request).value.calls;
  assert.ok(original.turn);
  const context = f.store.technicalNamed('secretary/follow-up-context', request.itemId);
  f.store.technicalAmend('secretary/follow-up-context', context, { ...context.value, developerInstructions: 'Changed instructions', itemSnapshot: 'Changed message', model: 'changed-model' });
  f.state.unavailable = false; f.state.loseMethod = null; f.state.version = 2;
  await f.advance(request, 'secretary');
  assert.equal(f.calls.length, 2); assert.equal(f.bindings.length, 2);
  assert.deepEqual(f.calls[1].params, original.turn.params);
  assert.equal(f.record(request).value.calls, null);
});

test('new follow-up contexts retain their original target and item before creating a task', async t => {
  const f = fixture(t), request = f.request('lost-context'); f.state.loseMethod = 'thread/start';
  await assert.rejects(f.advance(request, 'secretary'), { code: 'outcome_unknown' });
  const prepared = f.store.technicalNamed('secretary/follow-up-context', request.itemId);
  assert.equal(prepared.value.threadId, null); assert.ok(prepared.value.start);
  assert.equal(prepared.value.start.params.projectId, 'internal-project');
  f.state.projectId = 'changed-default-project';
  f.configuration.execution.serviceNodeId = 'agent-two'; f.configuration.execution.model = 'changed-model';
  f.item.message.text = 'Changed message'; f.state.unavailable = false; f.state.loseMethod = null;
  await f.advance(request, 'secretary');
  assert.deepEqual(f.calls.map(call => call.method), ['thread/start', 'thread/resume', 'turn/start']);
  assert.ok(f.calls.every(call => call.serviceNodeId === 'agent-one'));
  assert.match(f.calls[2].params.input[0].text, /Original message/);
  assert.equal(f.state.itemReads, 1);
});

test('a previously prepared asynchronous context recovers its original start after a lost reply', async t => {
  const f = fixture(t), request = f.request('prepared'), retained = f.context(), context = retained.value;
  f.store.technicalDelete('secretary/follow-up-context', retained.pin.objectId);
  f.store.technicalCreate('secretary/follow-up-context', 'follow-up-secretary-' + request.operationId, { ...context, threadId: '' });
  const id = deriveOperationId(request.operationId, 'start-secretary-follow-up-context');
  f.operations.set(id, { operationId: id, method: 'thread/start', definitionHash: hashJson(['thread/start', 1]),
    serviceNodeId: context.target.serviceNodeId, phase: 'succeeded', reply: { result: { thread: { id: context.threadId } } },
    params: { cwd: context.threadCwd, ephemeral: false, permissions: ':read-only', approvalPolicy: 'never', serviceName: 'secretary',
      threadSource: 'ivy-secretary-follow-up', developerInstructions: context.developerInstructions, model: context.model } });
  f.configuration.execution.serviceNodeId = 'changed-agent'; f.configuration.execution.model = 'changed-model';
  assert.equal((await f.advance(request)).phase, 'succeeded');
  assert.deepEqual(f.calls.map(call => call.method), ['thread/resume', 'turn/start']);
  assert.ok(f.calls.every(call => call.serviceNodeId === context.target.serviceNodeId));
  assert.equal(f.state.itemReads, 0);
});

test('a lost initial context reply cannot change the original follow-up item or question', async t => {
  const f = fixture(t), request = f.request('initial-identity'); f.state.loseMethod = 'thread/start';
  await assert.rejects(f.advance(request, 'secretary'), { code: 'outcome_unknown' });
  for (const changed of [{ itemId: 'item-two' }, { question: 'Different question' }])
    await assert.rejects(f.advance({ ...request, ...changed }, 'secretary'), { code: 'secretary_follow_up_conflict' });
  assert.equal(f.calls.length, 1);
  f.state.unavailable = false; f.state.loseMethod = null;
  assert.equal((await f.advance(request, 'secretary')).answer, 'Original answer');
});

test('a failed context start is retained for its original request without blocking a new question', async t => {
  const f = fixture(t), request = f.request('failed-context'); f.state.failedMethod = 'thread/start';
  await assert.rejects(f.advance(request, 'secretary'), { code: 'secretary_follow_up_native_failed' });
  await assert.rejects(f.advance(request, 'secretary'), { code: 'secretary_follow_up_native_failed' });
  assert.equal(f.calls.length, 1);
  f.state.failedMethod = null;
  assert.equal((await f.advance(f.request('new-question'), 'secretary')).answer, 'Original answer');
  assert.equal(f.calls.filter(call => call.method === 'thread/start').length, 2);
  const calls = f.calls.length;
  await assert.rejects(f.advance(request, 'secretary'), { code: 'secretary_follow_up_native_failed' });
  assert.equal(f.calls.length, calls, 'The failed request cannot reuse a newer context.');
});

test('missing or expired contexts cannot create replacement tasks for retained follow-ups', async t => {
  const f = fixture(t), request = f.request('missing-context'); f.context(); f.state.readError = true;
  await assert.rejects(f.advance(request, 'secretary'), { code: 'outcome_unknown' });
  const context = f.store.technicalNamed('secretary/follow-up-context', request.itemId);
  f.store.technicalAmend('secretary/follow-up-context', context, { ...context.value, expiresAt: 0 });
  await assert.rejects(f.advance(request, 'secretary'), { code: 'secretary_follow_up_context_missing' });
  f.store.technicalDelete('secretary/follow-up-context', context.pin.objectId);
  await assert.rejects(f.advance(request, 'secretary'), { code: 'secretary_follow_up_context_missing' });
  assert.equal(f.calls.length, 2); assert.equal(f.state.itemReads, 0);
});

test('an unresolved follow-up blocks another question in the same context until recovery finishes', async t => {
  const f = fixture(t), first = f.request('first'), second = f.request('second'); f.context(); f.state.readError = true;
  await assert.rejects(f.advance(first, 'secretary'), { code: 'outcome_unknown' });
  await assert.rejects(f.advance(second, 'secretary'), { code: 'secretary_follow_up_busy' });
  assert.equal(f.calls.length, 2);
  f.state.readError = false;
  await f.advance(first, 'secretary'); await f.advance(second, 'secretary');
  assert.equal(f.calls.filter(call => call.method === 'turn/start').length, 2);
});

test('failed follow-ups settle durably and let the next question use the context', async t => {
  const f = fixture(t); f.context();
  for (const [index, scenario] of [
    { failedMethod: 'thread/resume', code: 'secretary_follow_up_native_failed' },
    { failedMethod: 'turn/start', code: 'secretary_follow_up_native_failed' },
    { status: 'failed', code: 'secretary_follow_up_turn_failed' },
    { answer: null, code: 'secretary_follow_up_answer_missing' },
  ].entries()) {
    Object.assign(f.state, { failedMethod: null, status: 'completed', answer: 'Answer' }, scenario);
    const request = f.request('failure-' + index);
    await assert.rejects(f.advance(request, 'secretary'), { code: scenario.code });
    assert.equal(f.record(request).value.phase, 'failed');
    const calls = f.calls.length;
    await assert.rejects(f.advance(request, 'secretary'), { code: scenario.code });
    assert.equal(f.calls.length, calls);
  }
  Object.assign(f.state, { failedMethod: null, status: 'completed', answer: 'Answer' });
  assert.equal((await f.advance(f.request('next'), 'secretary')).answer, 'Answer');
});

test('concurrent requests cannot reuse one follow-up identity for different questions', async t => {
  const f = fixture(t), request = f.request('same-id'); f.context();
  const result = await Promise.allSettled([f.advance(request, 'secretary'), f.advance({ ...request, question: 'Different question' }, 'secretary')]);
  assert.equal(result[0].status, 'fulfilled');
  assert.equal(result[1].status, 'rejected'); assert.equal(result[1].reason.code, 'secretary_follow_up_conflict');
  assert.equal(f.calls.filter(call => call.method === 'turn/start').length, 1);
});

test('concurrent first questions create one shared item context', async t => {
  const f = fixture(t), first = f.request('first-context'), second = f.request('second-context');
  const result = await Promise.allSettled([f.advance(first, 'secretary'), f.advance(second, 'secretary')]);
  assert.equal(result.filter(value => value.status === 'fulfilled').length, 1);
  const rejected = result.find(value => value.status === 'rejected');
  assert.ok(['revision_conflict', 'secretary_follow_up_busy'].includes(rejected.reason.code));
  await f.advance(result[0].status === 'rejected' ? first : second, 'secretary');
  assert.equal(f.calls.filter(call => call.method === 'thread/start').length, 1);
  assert.equal(f.calls.filter(call => call.method === 'turn/start').length, 2);
});

test('follow-up admission is durable, returns before native work and rejects changed arguments', async () => {
  const store = new SecretaryStore({}, 'root');
  const engine = { store, settings: { identity: { principalId: 'main' } }, verifyOwner: async () => {}, now: () => new Date() };
  try {
    const follow = new SecretaryFollowUp(engine), request = { operationId: 'original', itemId: 'execution-object', question: 'Explain this result.' };
    const accepted = await follow.run(request, 'main');
    assert.equal(accepted.phase, 'prepared');
    assert.equal(accepted.threadId, null);
    assert.deepEqual(await new SecretaryFollowUp(engine).read('original', 'main'), accepted);
    assert.deepEqual(await follow.run(request, 'main'), accepted);
    await assert.rejects(follow.run({ ...request, question: 'Different' }, 'main'), { code: 'secretary_follow_up_conflict' });
    follow.newContext = async () => { throw Object.assign(new Error('invalid reference'), { code: 'secretary_follow_up_context_missing' }); };
    engine.settings.recordsPerTick = 2; engine.signal = new AbortController().signal; engine.issue = () => {};
    await follow.tick();
    const failed = await follow.read('original', 'main');
    assert.equal(failed.phase, 'outcome_unknown');
    assert.notEqual(failed.errorCode, 'Error');
  } finally { store.close(); }
});

test('background follow-ups page past uncertain work and recover the original turns', async t => {
  const f = fixture(t), worker = f.worker(); f.context(); f.engine.settings.recordsPerTick = 1;
  const first = f.request('a'), second = f.request('b');
  await worker.run(first, 'secretary'); await worker.run(second, 'secretary');
  assert.equal(f.calls.length, 0, 'Admission performs no native work.');
  f.state.readError = true;
  await worker.tick();
  assert.equal(f.record(first).value.phase, 'outcome_unknown');
  await worker.tick();
  assert.equal(f.record(second).value.errorCode, 'secretary_follow_up_busy');
  f.state.readError = false;
  for (let i = 0; i < 3; i++) await worker.tick();
  assert.equal((await worker.read(first.operationId, 'secretary')).phase, 'succeeded');
  assert.equal((await worker.read(second.operationId, 'secretary')).phase, 'succeeded');
  assert.equal(f.calls.filter(call => call.method === 'turn/start').length, 2);
  assert.equal(f.engine.recoveryIssues.size, 0);
});

test('an execution follow-up uses its retained result in a separate read-only context', async t => {
  const f = fixture(t), request = f.request('execution'); f.state.sourceContract = 'secretary/execution';
  f.store.read = async (key, id) => {
    assert.equal(key, 'secretary/execution'); assert.equal(id, request.itemId);
    return { value: { result: 'Retained assignment result', threadId: 'assignment-thread' } };
  };
  await f.advance(request);
  const turn = f.calls.find(call => call.method === 'turn/start');
  assert.match(turn.params.input[0].text, /Retained assignment result/);
  assert.notEqual(turn.params.threadId, 'assignment-thread');
  assert.equal(turn.params.permissions, ':read-only');
});
