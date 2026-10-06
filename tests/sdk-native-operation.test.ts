import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { digest, hashJson } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Agent } from '../packages/contracts/src/generated.js';
import { NativeOwner } from '../packages/sdk/src/native-owner.js';
import type { NativeOperationCall } from '../packages/sdk/src/native-owner.js';
import { reconcileNativeOperation } from '../packages/sdk/src/native-operation.js';
import type { NativeOperationJournal } from '../packages/sdk/src/native-operation.js';
import { validateNativeOperation, validateNativeProgress, validateNativeRead, validateNativeStatus } from '../packages/sdk/src/native-evidence.js';
import { chatNativeCatalog } from '../services/chat-bridge/src/native-evidence.js';
import { chatNativeFixture } from './fixtures/chat-native.js';
import { serviceTools } from '../packages/sdk/src/client.js';

async function fixture(t: TestContext) {
  const f = await chatNativeFixture(t), source = chatNativeCatalog('0.154.0');
  const target = { serviceNodeId: 'native-agent', hostId: 'fixture', nativeVersion: source.catalog.version,
    nativeExecutableHash: source.catalog.nativeExecutableHash, catalogHash: source.catalogHash };
  const owner = new NativeOwner(f.clients.first.client, 'chat-owner', target);
  const call: NativeOperationCall = { operationId: 'original-sdk-operation', method: 'turn/interrupt',
    params: { threadId: 'original-thread', turnId: 'original-turn' }, definitionHash: hashJson(source.definitions.get('turn/interrupt')) };
  let previous: Agent.Operation | null = null, revision = 0, guards = 0;
  const journal = (): NativeOperationJournal<number> => ({ current: revision, previous, wasObserved: previous !== null,
    retain: async observed => { previous = structuredClone(observed); return ++revision; } });
  const onAbsent = async (original: NativeOperationCall, absence: Agent.OperationAbsence) => {
    assert.equal((await owner.status()).epoch, absence.epoch);
    await owner.dispatch(original, async () => { guards++; });
  };
  return { f, source, target, owner, call, journal, onAbsent, previous: () => previous!, guards: () => guards };
}

test('shared native reconciliation observes absence without authority and recovers a lost original dispatch response', async t => {
  const f = await fixture(t);
  const absent = await reconcileNativeOperation({ owner: f.owner, call: f.call, journal: f.journal() });
  assert.equal(absent.kind, 'absent'); assert.equal(f.f.frames.length, 0); assert.equal(f.guards(), 0);
  f.f.loseReply();
  const result = await reconcileNativeOperation({ owner: f.owner, call: f.call, journal: f.journal(), onAbsent: f.onAbsent });
  assert.equal(result.kind, 'observed'); assert.equal(result.value, 1); assert.equal(f.previous().phase, 'succeeded');
  assert.equal(f.guards(), 1); assert.equal(f.f.frames.length, 1);
  f.f.unavailable(true);
  assert.deepEqual(await reconcileNativeOperation({ owner: f.owner, call: f.call, journal: f.journal(), onAbsent: f.onAbsent }), result);
  assert.equal(f.f.frames.length, 1); assert.equal(f.guards(), 1);
});

test('SDK native absence lookup remains fenced to the configured original caller', async t => {
  const f = await fixture(t), wrongCaller = new NativeOwner(f.f.clients.first.client, 'different-original-caller', f.target);
  await assert.rejects(wrongCaller.operation(f.call), { code: 'caller_changed' });
  assert.equal(f.f.frames.length, 0); assert.equal(f.guards(), 0);
});

test('SDK dispatch exposes known non-execution so callers can retry instead of treating it as an uncertain effect',async t=>{
  const f=await fixture(t);let attempts=0;
  f.f.beforeNative(async()=>{if(++attempts===1)throw new IvyError('service_not_ready','Temporary refusal before dispatch.','not_executed');});
  await assert.rejects(f.owner.dispatch(f.call,async()=>{}),{code:'service_not_ready',outcome:'not_executed'});
  assert.equal(f.f.frames.length,0);
  const result=await f.owner.dispatch(f.call,async()=>{});assert.equal(result?.phase,'succeeded');assert.equal(f.f.frames.length,1);
});

test('native dispatch refreshes only its stable envelope and reruns guards with the original request', async t => {
  const f = await fixture(t), tools = serviceTools(f.f.clients.first.client, 'native-agent', [{ namespace: 'agent', interfaceVersion: '1.0.0' }]);
  await tools.binding('agent.invoke');
  const registry = f.f.kernel.registry.getRegistry('native-agent')!;
  registry.namespaces.find(ns => ns.namespace === 'agent')!.tools.find(tool => tool.name === 'invoke')!.description += ' Updated transport.';
  await f.f.clients.agent.client.request('registry.sync', registry);
  const request = f.f.clients.first.client.request.bind(f.f.clients.first.client), attempts: unknown[] = [];
  f.f.clients.first.client.request = async (method, params, options) => {
    if (method === 'tools.call' && 'qualifiedName' in params && params.qualifiedName === 'agent.invoke') attempts.push(structuredClone(params));
    return request(method, params, options);
  };
  let guards = 0;
  assert.equal((await f.owner.dispatch(f.call, async () => { guards++; }))?.phase, 'succeeded');
  assert.equal(guards, 2); assert.equal(f.f.frames.length, 1); assert.equal(attempts.length, 2);
  const [first, second] = attempts as Array<Record<string, unknown>>;
  assert.notEqual(first!['expectedDefinitionHash'], second!['expectedDefinitionHash']);
  assert.deepEqual({ ...first, expectedDefinitionHash: second!['expectedDefinitionHash'] }, second);
  const retained = f.f.journal.get({ callerPrincipalId: 'chat-owner', operationId: f.call.operationId });
  assert.deepEqual(retained?.params, f.call.params);
  const discoveries = f.f.accesses.filter(method => method === 'tools.list').length;
  await f.owner.dispatch({ ...f.call, operationId: 'next-dispatch' }, async () => {});
  assert.equal(f.f.accesses.filter(method => method === 'tools.list').length, discoveries, 'Warm calls retain the refreshed binding.');
});

test('management refresh cannot bypass a changed domain guard or interface version', async t => {
  const f = await fixture(t), tools = serviceTools(f.f.clients.first.client, 'native-agent', [{ namespace: 'agent', interfaceVersion: '1.0.0' }]);
  await tools.binding('agent.invoke');
  const registry = f.f.kernel.registry.getRegistry('native-agent')!;
  const invoke = registry.namespaces.find(ns => ns.namespace === 'agent')!.tools.find(tool => tool.name === 'invoke')!;
  invoke.description += ' Updated transport.';
  await f.f.clients.agent.client.request('registry.sync', registry);
  let guards = 0;
  await assert.rejects(f.owner.dispatch(f.call, async () => {
    if (++guards === 2) throw new IvyError('claim_changed', 'The original claim changed during discovery.', 'unknown');
  }), { code: 'claim_changed' });
  assert.equal(guards, 2); assert.equal(f.f.frames.length, 0);
  const incompatible = structuredClone(registry);
  for (const tool of incompatible.namespaces.find(ns => ns.namespace === 'agent')!.tools) tool.interfaceVersion = '2.0.0';
  await f.f.clients.agent.client.request('registry.sync', incompatible);
  await assert.rejects(f.owner.dispatch(f.call, async () => {}), { code: 'contract_version_conflict' });
  assert.equal(f.f.frames.length, 0);
});

test('envelope recovery freshly checks the pinned native definition before repeating a mutation', async t => {
  const f = await fixture(t), tools = serviceTools(f.f.clients.first.client, 'native-agent', [{ namespace: 'agent', interfaceVersion: '1.0.0' }]);
  await tools.binding('agent.invoke');
  await f.owner.binding(f.call.method, f.call.definitionHash);
  const registry = f.f.kernel.registry.getRegistry('native-agent')!;
  registry.namespaces.find(ns => ns.namespace === 'agent')!.tools.find(tool => tool.name === 'invoke')!.description += ' Updated envelope.';
  registry.namespaces.find(ns => ns.namespace === 'codex')!.tools.find(tool => tool.name === f.call.method)!.description += ' Changed native contract.';
  await f.f.clients.agent.client.request('registry.sync', registry);
  let guards = 0;
  await assert.rejects(f.owner.dispatch(f.call, async () => { guards++; }), { code: 'tool_definition_changed', outcome: 'not_executed' });
  assert.equal(guards, 1);
  assert.equal(f.f.frames.length, 0);
});

test('a failed refresh read preserves the original non-execution evidence for a later retry', async t => {
  const f = await fixture(t), tools = serviceTools(f.f.clients.first.client, 'native-agent', [{ namespace: 'agent', interfaceVersion: '1.0.0' }]);
  await tools.binding('agent.invoke');
  const registry = f.f.kernel.registry.getRegistry('native-agent')!;
  registry.namespaces.find(ns => ns.namespace === 'agent')!.tools.find(tool => tool.name === 'invoke')!.description += ' Updated envelope.';
  await f.f.clients.agent.client.request('registry.sync', registry);
  const request = f.f.clients.first.client.request.bind(f.f.clients.first.client);
  let failed = false;
  f.f.clients.first.client.request = async (method, params, options) => {
    if (!failed && method === 'tools.call' && 'qualifiedName' in params && params.qualifiedName === 'agent.status') {
      failed = true;
      throw new IvyError('outcome_unknown', 'Read response lost.', 'unknown');
    }
    return request(method, params, options);
  };
  await assert.rejects(f.owner.dispatch(f.call, async () => {}), { code: 'outcome_unknown', outcome: 'not_executed' });
  assert.equal(f.f.frames.length, 0);
  assert.equal((await f.owner.dispatch(f.call, async () => {}))?.phase, 'succeeded');
  assert.equal(f.f.frames.length, 1);
});

test('shared native reconciliation retains late pending receipts and refuses disappearance of observed work', async t => {
  const f = await fixture(t); f.f.hold('turn/interrupt');
  await reconcileNativeOperation({ owner: f.owner, call: f.call, journal: f.journal(), onAbsent: f.onAbsent });
  const pending = f.previous(); assert.equal(pending.phase, 'dispatched');
  f.f.absence('owner');
  await assert.rejects(reconcileNativeOperation({ owner: f.owner, call: f.call, journal: f.journal(), onAbsent: f.onAbsent }), { code: 'native_absence_conflict' });
  assert.equal(f.guards(), 1); assert.equal(f.f.frames.length, 1);
  f.f.absence(null);
  f.f.journal.finish({ callerPrincipalId: 'chat-owner', operationId: f.call.operationId }, pending.epoch!, pending.requestId!, { result: {} });
  const late = await reconcileNativeOperation({ owner: f.owner, call: f.call, journal: f.journal() });
  assert.equal(late.kind, 'observed'); assert.equal(late.value, 2); assert.equal(f.previous().phase, 'succeeded');
  assert.equal(f.f.frames.length, 1);
});

test('shared native reconciliation keeps unknown outcomes and compact observed markers from becoming new actions', async t => {
  const f = await fixture(t); f.f.hold('turn/interrupt');
  await reconcileNativeOperation({ owner: f.owner, call: f.call, journal: f.journal(), onAbsent: f.onAbsent });
  f.f.restart();
  await reconcileNativeOperation({ owner: f.owner, call: f.call, journal: f.journal(), onAbsent: f.onAbsent });
  assert.equal(f.previous().phase, 'outcome_unknown'); assert.equal(f.f.frames.length, 1); assert.equal(f.guards(), 1);
  f.f.absence('owner');
  await assert.rejects(reconcileNativeOperation({ owner: f.owner, call: f.call, journal: { ...f.journal(), previous: null }, onAbsent: f.onAbsent }), { code: 'native_absence_conflict' });
  f.f.absence('generic');
  await assert.rejects(reconcileNativeOperation({ owner: f.owner, call: { ...f.call, operationId: 'never-seen' }, journal: { ...f.journal(), previous: null, wasObserved: false }, onAbsent: f.onAbsent }));
  assert.equal(f.f.frames.length, 1); assert.equal(f.guards(), 1);
});

test('two concurrent SDK consumers retain one original owner action and failed guards or changed definitions cannot dispatch', async t => {
  const f = await fixture(t), second = new NativeOwner(f.f.clients.second.client, 'chat-owner', f.target);
  const result = await Promise.all([f.owner, second].map(owner => reconcileNativeOperation({ owner, call: f.call,
    journal: { current: 0, previous: null, wasObserved: false, retain: async () => 1 },
    onAbsent: (call: NativeOperationCall) => owner.dispatch(call, async () => undefined) })));
  assert.ok(result.every(value => value.kind === 'observed')); assert.equal(f.f.frames.length, 1);
  assert.equal(f.f.journal.status().retained, 1);
  const receipt = f.f.journal.get({ callerPrincipalId: 'chat-owner', operationId: f.call.operationId });
  assert.equal(receipt?.phase, 'succeeded'); assert.deepEqual(receipt?.params, f.call.params);
  await assert.rejects(f.owner.dispatch({ ...f.call, operationId: 'forbidden' }, async () => { throw new IvyError('claim_changed', 'Domain claim changed.'); }), { code: 'claim_changed' });
  await assert.rejects(f.owner.dispatch({ ...f.call, operationId: 'changed-schema', definitionHash: digest('changed') }, async () => { assert.fail('Changed schema reached the domain guard.'); }), { code: 'tool_definition_changed' });
  f.f.beforeNative(async () => { throw new IvyError('tool_definition_changed', 'The bound definition changed at dispatch.', 'not_executed'); });
  await assert.rejects(f.owner.dispatch({ ...f.call, operationId: 'changed-at-dispatch' }, async () => undefined), { code: 'tool_definition_changed' });
  assert.equal(f.f.frames.length, 1);
});

test('lost receipt persistence recovers its original owner result without a second native dispatch', async t => {
  const f = await fixture(t); let saves = 0;
  const retain = async (_observed: Agent.Operation) => { if (++saves === 1) throw new IvyError('outcome_unknown', 'Lost store response.', 'unknown'); return 1; };
  const run = () => reconcileNativeOperation({ owner: f.owner, call: f.call, journal: { current: 0, previous: null, wasObserved: false, retain }, onAbsent: f.onAbsent });
  await assert.rejects(run(), { code: 'outcome_unknown' }); assert.equal((await run()).value, 1);
  assert.equal(saves, 2); assert.equal(f.f.frames.length, 1); assert.equal(f.guards(), 1);
});

test('shared evidence gates reject substituted identity, epoch, regression and replacement terminal receipts', async t => {
  const f = await fixture(t);
  await reconcileNativeOperation({ owner: f.owner, call: f.call, journal: f.journal(), onAbsent: f.onAbsent });
  const original = f.previous(), expected = { ...f.target, ...f.call, callerPrincipalId: 'chat-owner' };
  validateNativeOperation(original, expected); validateNativeProgress(original, original);
  for (const change of [{ serviceNodeId: 'other' }, { callerPrincipalId: 'other' }, { operationId: 'other' },
    { nativeExecutableHash: digest('other') }, { method: 'thread/start' }]) {
    assert.throws(() => validateNativeOperation({ ...original, ...change }, expected));
  }
  for (const change of [{ epoch: 'replacement' }, { requestId: 'replacement' },
    { updatedAt: '2020-01-01T00:00:00.000Z' }]) assert.throws(() => validateNativeProgress(original, { ...original, ...change } as Agent.Operation));
  const status = await f.owner.status(); validateNativeStatus(status, f.target);
  assert.throws(() => validateNativeStatus({ ...status, hostId: 'other' }, f.target));
  const readExpected = { ...f.target, callerPrincipalId: 'chat-owner', epoch: status.epoch!, method: 'thread/read', params: { threadId: 'original-thread' } };
  const read = await f.owner.read('thread/read', readExpected.params, readExpected.epoch);
  validateNativeRead(read, readExpected);
  for (const change of [{ epoch: 'replacement' }, { catalogHash: digest('other') }]) {
    assert.throws(() => validateNativeRead({ ...read, ...change }, readExpected));
  }
});
