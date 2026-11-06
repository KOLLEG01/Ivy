import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { validateAgent } from '../packages/contracts/src/agent-validation.js';
import { backendArguments, sameNativeBackend } from '../packages/contracts/src/native-backend.js';
import { NativeContract } from '../packages/contracts/src/native-contract.js';
import { hashJson } from '../packages/contracts/src/canonical.js';
import { toolDefinitionHash } from '../packages/contracts/src/tool-definition.js';
import type { Agent, Wire } from '../packages/contracts/src/generated.js';
import type { RpcClient } from '../packages/sdk/src/client.js';
import { NativeOwner } from '../packages/sdk/src/native-owner.js';
import { validateNativeOperation, validateNativeProgress, validateNativeRead, validateNativeStatus } from '../packages/sdk/src/native-evidence.js';
import { readNativePlan, saveNativePlan, validateNativeInvocation } from '../packages/sdk/src/native-plan.js';

const a: Agent.BackendIdentity = { backendId: 'codex', provider: 'codex', accountId: 'work' };
const b: Agent.BackendIdentity = { backendId: 'claude', provider: 'claude', accountId: 'personal' };
const time = '2026-11-01T00:00:00.000Z';
function catalog(): Agent.Catalog {
  const value = JSON.parse(readFileSync('specs/native/codex-0.163.0/catalog.json', 'utf8')) as Agent.Catalog;
  value.clientRequests = value.clientRequests.filter(method => method.method === 'thread/read').map(method => ({ ...method, inputSchema: true, outputSchema: true }));
  value.serverRequests = []; value.serverNotifications = []; value.clientNotifications = []; value.derivedLegacyTypes = [];
  return value;
}

test('backend selectors preserve native JSON and reject account or provider substitution', () => {
  const params = { backend: { opaque: '\ud800' }, model: 'same-model' };
  const args = backendArguments({ params }, a);
  assert.equal(args['params'], params);
  assert.deepEqual(args['backend'], a);
  assert.throws(() => backendArguments({ backend: a }, b), { code: 'native_backend_mismatch' });
  assert.throws(() => backendArguments({ backend: a }), { code: 'native_backend_mismatch' });
  for (const backend of [{ ...a, backendId: '../account' }, { ...a, accountId: '' }, { ...a, provider: 'other' }])
    assert.throws(() => validateAgent('BackendIdentity', backend));
  for (const change of [{ backendId: 'other' }, { accountId: 'other' }, { provider: 'claude' }])
    assert.equal(sameNativeBackend({ ...a, ...change } as Agent.BackendIdentity, a), false);
  assert.equal(sameNativeBackend(undefined, a), false);
  const draft = { nativeVersion: '0.163.0', catalogSourceHash: hashJson({}), method: 'thread/start', params: {} };
  validateAgent('InvocationDraft', draft);
  validateAgent('InvocationDraft', { ...draft, backend: a });
});

test('retained plans and invocations bind backend, provider and account before parameter storage', async () => {
  const source = new NativeContract(JSON.parse(readFileSync('specs/native/codex-0.163.0/catalog.json', 'utf8')) as Agent.Catalog);
  const client = { request: async () => { throw Error('Inline plan storage must not use RPC.'); } } as RpcClient;
  for (const backend of [a, b]) {
    const selected = source.withBackend(backend);
    const definitions = Object.fromEntries(Object.entries({ threadStart: 'thread/start', threadResume: 'thread/resume', turnStart: 'turn/start', turnInterrupt: 'turn/interrupt' })
      .map(([key, method]) => [key, selected.definitionHash(method)])) as Agent.PlanDefinitions;
    const draft: Agent.PlanDraft = { backend, nativeVersion: source.catalog.version, catalogSourceHash: source.catalog.sourceHash,
      definitions, threadStart: {}, threadResume: {}, turnStart: {} };
    const plan = await saveNativePlan(client, selected, { draft, kind: 'chat', parentId: null, mutationId: 'operation' });
    assert.deepEqual(await readNativePlan(client, selected, { plan, kind: 'chat', parentId: null }), draft);
    const invocation = { backend, nativeVersion: draft.nativeVersion, catalogSourceHash: draft.catalogSourceHash,
      method: 'turn/interrupt', params: { threadId: 'thread', turnId: 'turn' } };
    validateNativeInvocation(selected, invocation);
    for (const wrong of [undefined, { ...backend, accountId: 'other' }, { ...backend, backendId: 'other' },
      { ...backend, provider: backend.provider === 'codex' ? 'claude' as const : 'codex' as const }]) {
      const other = source.withBackend(wrong);
      await assert.rejects(readNativePlan(client, other, { plan, kind: 'chat', parentId: null }), { code: 'native_plan_backend_mismatch' });
      assert.throws(() => validateNativeInvocation(other, invocation), { code: 'native_request_backend_mismatch' });
    }
  }
});

test('backend projections share immutable catalog and schema owners and separate equal method names across accounts', () => {
  const native = catalog(), first = new NativeContract(native, a), second = first.withBackend(b);
  const otherAccount = first.withBackend({ ...a, accountId: 'personal' });
  assert.equal(first.catalog, second.catalog);
  assert.equal(first.catalog, otherAccount.catalog);
  assert.equal(first.inputSchema('thread/read'), second.inputSchema('thread/read'));
  assert.equal(first.inputSchema('thread/read'), otherAccount.inputSchema('thread/read'));
  native.clientRequests = [];
  assert.equal(first.catalog.clientRequests.length, 1);
  assert(Object.isFrozen(first.backend));
  assert.equal(first.catalogHash, second.catalogHash);
  assert.equal(first.definition('thread/read').namespace, a.backendId);
  assert.equal(second.definition('thread/read').namespace, b.backendId);
  assert.deepEqual(first.definition('thread/read').inputSchema, second.definition('thread/read').inputSchema);
  assert.notEqual(first.definitionHash('thread/read'), second.definitionHash('thread/read'));
  assert.notEqual(first.definitionHash('thread/read'), otherAccount.definitionHash('thread/read'));
  assert.throws(() => first.verifyDefinition('thread/read', second.definition('thread/read'), second.definitionHash('thread/read')),
    { code: 'native_definition_changed' });
});

test('one SDK connection scopes management, catalog cache and original evidence to the selected backend', async () => {
  const native = catalog();
  const targets = [a, b].map(backend => ({ backend, serviceNodeId: 'host.agent-manager', hostId: 'host', nativeVersion: native.version,
    nativeExecutableHash: native.nativeExecutableHash, catalogHash: hashJson(native) }));
  const visits: { method: string; args: Record<string, Wire.Json> }[] = [];
  let wrongReply = false;
  const status = (target: typeof targets[number]) => ({ ...target, state: 'ready', epoch: 'epoch', pid: null, observedAt: time, code: null,
    initialized: null, pendingInputs: 0, operations: { retained: 0, maximum: 100, bytes: 0, maximumBytes: 1048576 }, observedMethods: [] });
  const params = { threadId: 'saved-thread' };
  const operation = (target: typeof targets[number]) => ({ schemaVersion: 1, backend: target.backend, serviceNodeId: target.serviceNodeId,
    nativeVersion: target.nativeVersion, nativeExecutableHash: target.nativeExecutableHash, callerPrincipalId: 'caller', operationId: 'original',
    method: 'thread/read', params, requestHash: hashJson({ method: 'thread/read', params }), phase: 'succeeded', createdAt: time, updatedAt: time,
    epoch: 'epoch', requestId: 'native-id', reply: { result: { original: true } }, code: null });
  const client = { async request(method: string, input: Record<string, Wire.Json>) {
    if (method === 'tools.list') {
      const namespace = input['namespace'] as string, name = input['namePrefix'] as string;
      const target = targets.find(target => target.backend.backendId === namespace);
      const definition = target ? new NativeContract(native, target.backend).definition(name) : {
        namespace, name, interfaceVersion: '1.0.0', description: 'Fixture management envelope.', inputSchema: true, outputSchema: true,
      };
      return { provider: { node: { serviceNodeId: targets[0]!.serviceNodeId } }, items: [{ qualifiedName: namespace + '.' + name,
        definition, definitionHash: toolDefinitionHash(definition), catalogHash: hashJson(native) }], nextCursor: null };
    }
    assert.equal(method, 'tools.call');
    const args = input['arguments'] as Record<string, Wire.Json>;
    const target = targets.find(target => sameNativeBackend(args['backend'] as Agent.BackendIdentity, target.backend));
    assert(target, 'Every management call must select a complete backend identity.');
    const name = input['qualifiedName'] as string;
    visits.push({ method: name, args });
    const observed = wrongReply ? targets[1]! : target;
    if (name === 'agent.status') return status(observed);
    if (name === 'agent.catalog') return native;
    if (name === 'agent.invoke' || name === 'agent.operation') return operation(observed);
    if (name === 'agent.interact' || name === 'agent.interaction') return { backend: observed.backend, operationId: 'original', epoch: 'epoch', reply: { result: null } };
    if (name === 'agent.read') return { schemaVersion: 1, backend: observed.backend, serviceNodeId: observed.serviceNodeId,
      nativeVersion: observed.nativeVersion, nativeExecutableHash: observed.nativeExecutableHash, catalogHash: observed.catalogHash,
      observationId: 'read', callerPrincipalId: 'caller', epoch: 'epoch',
      requestId: 'native-id', observedAt: time, requestHash: hashJson({ method: 'thread/read', params }), method: 'thread/read', params, reply: { result: null } };
    throw new Error(name);
  } } as unknown as RpcClient;
  const ownerA = new NativeOwner(client, 'caller', targets[0]!), ownerB = new NativeOwner(client, 'caller', targets[1]!);
  assert(Object.isFrozen(ownerA.target.backend));
  assert.throws(() => { ownerA.target.backend!.accountId = 'other'; }, TypeError);
  const contractA = await ownerA.selectedContract(), contractB = await ownerB.selectedContract();
  assert.notEqual(contractA, contractB);
  assert.equal(contractA.catalog, contractB.catalog);
  assert.equal(contractA.inputSchema('thread/read'), contractB.inputSchema('thread/read'));
  assert.equal(await new NativeOwner(client, 'other-caller', targets[0]!).selectedContract(), contractA);
  assert.equal(visits.filter(visit => visit.method === 'agent.catalog').length, 2);
  const call = { operationId: 'original', method: 'thread/read', params, definitionHash: contractA.definitionHash('thread/read') };
  const original = await ownerA.dispatch(call, async () => {});
  assert(original);
  ownerA.checkOperation(original, call);
  const callB = {...call,definitionHash:contractB.definitionHash('thread/read')};
  const originalB = await ownerB.dispatch(callB,async()=>{});
  assert(originalB);
  ownerB.checkOperation(originalB,callB);
  assert.deepEqual(visits.find(visit => visit.method === 'agent.invoke')!.args['params'], params);
  const observed = await ownerA.read('thread/read', params, 'epoch');
  validateNativeRead(observed, { ...targets[0]!, callerPrincipalId: 'caller', epoch: 'epoch', method: 'thread/read', params });
  const before = visits.length;
  await assert.rejects(ownerA.management('status', { backend: b }), { code: 'native_backend_mismatch' });
  assert.equal(visits.length, before);
  wrongReply = true;
  await assert.rejects(ownerA.status(), { code: 'native_owner_mismatch' });
  await assert.rejects(ownerA.operation(call), { code: 'native_evidence_mismatch' });
  await assert.rejects(ownerA.interaction('original'), { code: 'native_observation_invalid' });
  assert.throws(() => validateNativeStatus(status(targets[1]!), targets[0]!), { code: 'native_owner_mismatch' });
  assert.throws(() => validateNativeOperation({ ...original, backend: b }, { ...targets[0]!, callerPrincipalId: 'caller', ...call }), { code: 'native_evidence_mismatch' });
  assert.throws(() => validateNativeProgress(original, { ...original, backend: b }), { code: 'native_evidence_regressed' });
});
