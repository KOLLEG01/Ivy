import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { createRenderer } from 'vue';
import { digest, hashJson } from '../packages/contracts/src/canonical.js';
import type { Agent, Wire } from '../packages/contracts/src/generated.js';
import { IvyError } from '../packages/sdk/src/client.js';
import type { BoundTool, RpcClient } from '../packages/sdk/src/client.js';
import { useNativeAction } from '../packages/ui-client/src/native-action.js';
import { checkNativeActionReceipt } from '../packages/ui-client/src/native-action-evidence.js';

const backend: Agent.BackendIdentity = { backendId: 'secondary', provider: 'claude', accountId: 'personal' };
const foreign: Agent.BackendIdentity = { backendId: 'primary', provider: 'codex', accountId: 'work' };
const serviceNodeId = 'host.agent-manager', time = '2026-01-01T00:00:00.000Z';
const executable = digest('native'), catalogHash = digest('catalog');
const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;
function tool(qualifiedName: string): BoundTool {
  const [namespace, ...name] = qualifiedName.split('.');
  const native = namespace !== 'agent';
  return { serviceNodeId, qualifiedName, definitionHash: digest(qualifiedName), definition: { namespace: namespace!, name: name.join('.'),
    interfaceVersion: native ? '0.163.0' : '1.0.0', description: 'Inert action fixture.', inputSchema: true, outputSchema: true,
    ...(native ? { nativeMethod: name.join('.'), nativeSchemaIdentity: catalogHash } : {}) } };
}
function receipt(operationId: string, method: string, params: Wire.Json): Agent.Operation {
  return { schemaVersion: 1, serviceNodeId, backend, nativeVersion: '0.163.0', nativeExecutableHash: executable,
    operationId, callerPrincipalId: 'caller', method, params, requestHash: hashJson({ method, params }), phase: 'succeeded',
    createdAt: time, updatedAt: time, epoch: 'epoch', requestId: 'native-request', reply: { result: { original: true } }, code: null };
}
const renderer = createRenderer<object, object>({ insert() {}, remove() {}, createElement: () => ({}), createText: () => ({}), createComment: () => ({}),
  setText() {}, setElementText() {}, parentNode: () => null, nextSibling: () => null, patchProp() {} });
function fixture(t: TestContext) {
  const storage = new Map<string, string>(), originals = new Map<string, Agent.Operation>();
  const globals = ['window', 'document', 'sessionStorage'] as const;
  const descriptors = globals.map(key => Object.getOwnPropertyDescriptor(globalThis, key));
  Object.defineProperty(globalThis, 'window', { configurable: true, value: new EventTarget() });
  Object.defineProperty(globalThis, 'document', { configurable: true, value: Object.assign(new EventTarget(), { hidden: false }) });
  Object.defineProperty(globalThis, 'sessionStorage', { configurable: true, value: { getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value), removeItem: (key: string) => storage.delete(key) } });
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const calls: Wire.ToolCall[] = [], reads: Wire.ToolCall[] = [];
  const behavior: { statusBackend: Agent.BackendIdentity; holdPreparation?: Promise<void>; lost: boolean; absence?: Agent.BackendIdentity; rejectRetry: boolean; wrongReceipt: boolean; invalidReply: boolean; selected: Agent.BackendIdentity } =
    { statusBackend: backend, lost: false, rejectRetry: false, wrongReceipt: false, invalidReply: false, selected: backend };
  const client = { async request(method: string, raw: Record<string, unknown>) {
    const input = clone(raw);
    if (method === 'system.status') { await behavior.holdPreparation; return { callerPrincipalId: 'caller', runtimeEpoch: '11111111-1111-4111-8111-111111111111' }; }
    if (method === 'serviceNodes.get') return { connected: true, synced: true, ready: true, desiredEnabled: true, serviceName: 'agent-manager' };
    if (method === 'tools.list') return { provider: { node: { serviceNodeId } }, items: [tool(input['namespace'] + '.' + input['namePrefix'])], nextCursor: null };
    assert.equal(method, 'tools.call');
    const call = input as unknown as Wire.ToolCall, args = call.arguments as Record<string, Wire.Json>;
    if (call.qualifiedName === 'agent.status') return { serviceNodeId, hostId: 'host', backend: behavior.statusBackend, state: 'ready', epoch: 'epoch', nativeVersion: '0.163.0',
      nativeExecutableHash: executable, catalogHash, observedAt: time, pid: null, code: null, initialized: null, pendingInputs: 0,
      operations: { retained: 0, maximum: 100, bytes: 0, maximumBytes: 1048576 }, observedMethods: [] };
    assert.deepEqual(args['backend'], backend, 'Every journal/action call selects the original complete account.');
    assert.equal(call.expectedCallerPrincipalId, 'caller');
    if (call.qualifiedName === 'agent.operation') {
      reads.push(call);
      if (behavior.absence) throw new IvyError('not_found', 'Inert owner absence.', 'not_executed', { kind: 'agent_operation_absent',
        serviceNodeId, operationId: args['operationId']!, epoch: 'epoch', backend: behavior.absence });
      const value = clone(originals.get(String(args['operationId']))!);
      return behavior.wrongReceipt ? { ...value, backend: foreign } : value;
    }
    assert(['agent.invoke', 'agent.answer'].includes(call.qualifiedName));
    calls.push(call);
    const nativeMethod = call.qualifiedName === 'agent.answer' ? 'agent.answer' : String(args['method']);
    const params = call.qualifiedName === 'agent.answer' ? { identity: args['identity']!, reply: args['reply']! } : args['params']!;
    const value = receipt(call.operationId!, nativeMethod, params);
    originals.set(call.operationId!, value);
    if (behavior.rejectRetry && calls.length > 1) throw new IvyError('tool_definition_changed', 'Inert rejected retry.', 'not_executed');
    if (behavior.lost) throw new IvyError('service_unavailable', 'Inert lost reply.', 'unknown');
    return behavior.invalidReply ? {} : behavior.wrongReceipt ? { ...value, backend: foreign } : value;
  } } as unknown as RpcClient;
  const apps = new Set<ReturnType<typeof renderer.createApp>>();
  const mount = (key = 'action', selected: () => Agent.BackendIdentity | undefined = () => behavior.selected) => {
    let action!: ReturnType<typeof useNativeAction>;
    const app = renderer.createApp({ setup() { action = useNativeAction(client, key, selected); return () => null; } });
    app.mount({}); apps.add(app);
    return { action, unmount: () => { if (apps.delete(app)) app.unmount(); } };
  };
  t.after(() => { for (const app of apps) app.unmount(); for (const [index, key] of globals.entries()) {
    const previous = descriptors[index]; if (previous) Object.defineProperty(globalThis, key, previous); else Reflect.deleteProperty(globalThis, key);
  } });
  return { client, mount, storage, behavior, calls, reads, originals };
}

test('leaving an action view during preparation does not publish a new retained action or dispatch it', async t => {
  const f = fixture(t), mounted = f.mount(); let resume!: () => void;
  f.behavior.holdPreparation = new Promise<void>(resolve => { resume = resolve; });
  const pending = mounted.action.start('Send', tool(backend.backendId + '.turn/start'), { threadId: 'same-thread' });
  mounted.unmount(); resume(); await pending;
  assert.equal(mounted.action.saved.value, null); assert.equal(f.calls.length, 0); assert.equal(f.storage.has('action'), false);
});

test('unscoped routes cannot adopt a currently scoped account even when its backend namespace is codex', async t => {
  const f = fixture(t), { action } = f.mount('legacy', () => undefined);
  f.behavior.statusBackend = { backendId: 'codex', provider: 'codex', accountId: 'different' };
  await action.start('Rename', tool('codex.thread/name/set'), { threadId: 'same-thread', name: 'Wrong route' });
  assert.match(action.error.value!, /selected service and account/);
  assert.equal(action.saved.value, null); assert.equal(f.calls.length, 0);
});

test('scoped native actions retain an Agent envelope, untouched provider params and original receipt identity', async t => {
  const f = fixture(t), { action } = f.mount();
  const params = { threadId: 'same-thread', input: [{ type: 'text', text: 'Original input' }], backend: { opaque: 'native data' } };
  const binding = tool(backend.backendId + '.turn/start');
  const started = action.start('Send', binding, params);
  params.input[0]!.text = 'Changed caller object';
  binding.qualifiedName = backend.backendId + '.thread/delete'; binding.definition.nativeMethod = 'thread/delete';
  await started;
  assert.equal(action.saved.value?.phase, 'succeeded');
  assert.deepEqual(action.saved.value?.result, { original: true });
  assert.equal(f.calls.length, 1);
  assert.deepEqual((f.calls[0]!.arguments as Record<string, Wire.Json>)['params'], { threadId: 'same-thread', input: [{ type: 'text', text: 'Original input' }], backend: { opaque: 'native data' } });
  assert.equal((f.calls[0]!.arguments as Record<string, Wire.Json>)['operationId'], f.calls[0]!.operationId);
  assert.equal(f.calls[0]!.qualifiedName, 'agent.invoke');
  assert.equal(action.saved.value?.original?.method, 'turn/start');
  await assert.doesNotReject(action.reconcile());
  assert.equal(f.calls.length, 1, 'Reconciliation never repeats a mutation.');
});

test('lost replies reload against their original account and reject foreign receipts without dispatch', async t => {
  const f = fixture(t); f.behavior.lost = true;
  const first = f.mount(); await first.action.start('Send', tool(backend.backendId + '.turn/start'), { threadId: 'same-thread' });
  const originalId = first.action.saved.value!.operationId; first.unmount();
  f.behavior.selected = foreign;
  const { action } = f.mount(); f.behavior.wrongReceipt = true;
  await action.reconcile(); assert.equal(action.saved.value?.phase, 'unknown'); assert.match(action.error.value!, /original account/);
  assert.equal(f.calls.length, 1);
  f.behavior.wrongReceipt = false; await action.reconcile();
  assert.equal(action.saved.value?.phase, 'succeeded');
  assert.equal(action.saved.value?.operationId, originalId);
  assert.equal(f.reads.at(-1)?.expectedCallerPrincipalId, 'caller');
});

test('only exact account absence permits retry; a rejected retry cannot erase an earlier unknown dispatch', async t => {
  const f = fixture(t); f.behavior.lost = true;
  const { action } = f.mount(); await action.start('Send', tool(backend.backendId + '.turn/start'), { threadId: 'thread' });
  f.behavior.absence = foreign; await action.reconcile();
  assert.equal(action.saved.value?.phase, 'unknown');
  f.behavior.absence = backend; await action.reconcile(); assert.equal(action.saved.value?.phase, 'not_found');
  f.behavior.selected = foreign; await action.retry(); assert.equal(f.calls.length, 1);
  assert.match(action.error.value!, /original account/);
  f.behavior.selected = backend; f.behavior.rejectRetry = true; await action.retry();
  assert.equal(action.saved.value?.phase, 'unknown');
  assert.deepEqual(f.calls[1], f.calls[0], 'An explicit retry keeps the exact original call and operation ID.');
});

test('invalid immediate receipts remain unknown and cannot expose a result or permit another action', async t => {
  const f = fixture(t); f.behavior.wrongReceipt = true;
  const { action } = f.mount(); await action.start('Send', tool(backend.backendId + '.turn/start'), {});
  assert.equal(action.saved.value?.phase, 'unknown'); assert.equal(action.saved.value?.result, undefined);
  await action.start('Another send', tool(backend.backendId + '.turn/start'), {});
  assert.equal(f.calls.length, 1);
  f.behavior.wrongReceipt = false; f.behavior.invalidReply = true;
  const malformed = f.mount('malformed').action;
  await malformed.start('Malformed reply', tool(backend.backendId + '.turn/start'), {});
  assert.equal(malformed.saved.value?.phase, 'unknown'); assert.equal(malformed.saved.value?.result, undefined);
  await malformed.start('Another send', tool(backend.backendId + '.turn/start'), {});
  assert.equal(f.calls.length, 2);
});

test('preparation rejects another namespace or a changing selection and does not dispatch modified retained params', async t => {
  const f = fixture(t), { action } = f.mount();
  await action.start('Wrong namespace', tool(foreign.backendId + '.turn/start'), {});
  assert.equal(f.calls.length, 0); assert.match(action.error.value!, /another backend/);
  const preparing = action.start('Changing selection', tool(backend.backendId + '.turn/start'), {});
  f.behavior.selected = foreign; await preparing;
  assert.equal(f.calls.length, 0); assert.match(action.error.value!, /account changed/);
  f.behavior.selected = backend; f.behavior.lost = true;
  await action.start('Original', tool(backend.backendId + '.turn/start'), { threadId: 'original-thread' });
  f.behavior.absence = backend; await action.reconcile();
  const args = action.saved.value!.call.arguments as Record<string, Wire.Json>; args['params'] = { threadId: 'replacement' };
  await action.retry();
  assert.equal(f.calls.length, 1); assert.equal(action.saved.value?.phase, 'unknown');
  assert.match(action.saved.value!.detail, /retained call differs/);
  args['params'] = { threadId: 'original-thread' }; args['expectedDefinitionHash'] = digest('replacement definition');
  await action.reconcile(); assert.equal(action.saved.value?.phase, 'not_found'); await action.retry();
  assert.equal(f.calls.length, 1); assert.equal(action.saved.value?.phase, 'unknown');
});

test('secret responses retain only identity and hashes, and re-entry must match the original answer', async t => {
  const f = fixture(t); f.behavior.lost = true;
  const first = f.mount();
  const input = { identity: { serviceNodeId, backend, epoch: 'epoch', requestId: 7 }, reply: { result: { answer: 'private-fixture-answer' } } };
  await first.action.start('Answer', tool('agent.answer'), input, true);
  const originalId = first.action.saved.value!.operationId;
  assert.equal(f.storage.get('action')!.includes('private-fixture-answer'), false);
  first.unmount();
  const { action } = f.mount(); f.behavior.absence = backend; await action.reconcile();
  await action.reenterSecret({ ...input, reply: { result: { answer: 'changed-answer' } } });
  assert.equal(f.calls.length, 1); assert.match(action.error.value!, /differs/);
  f.behavior.lost = false; await action.reenterSecret(input);
  assert.equal(action.saved.value?.phase, 'succeeded'); assert.equal(action.saved.value?.operationId, originalId);
  assert.equal(f.storage.get('action')!.includes('private-fixture-answer'), false);
  assert.deepEqual(f.calls[1], f.calls[0]);
});

test('receipt checkpoints reject identity substitutions, regressions and changed terminal payloads', async () => {
  const value = receipt('original', 'turn/start', { threadId: 'thread' });
  const original = { backend, serviceNodeId, nativeVersion: value.nativeVersion, nativeExecutableHash: executable, catalogHash,
    method: value.method, requestHash: value.requestHash };
  const accepted: Agent.Operation = { ...value, phase: 'accepted', epoch: null, requestId: null, reply: null };
  const first = await checkNativeActionReceipt(accepted, original, 'caller', 'original');
  const terminal = await checkNativeActionReceipt(value, original, 'caller', 'original', first);
  for (const change of [{ backend: foreign }, { backend: { ...backend, provider: 'codex' as const } }, { backend: { ...backend, accountId: 'other' } },
    { serviceNodeId: 'other' }, { callerPrincipalId: 'other' }, { operationId: 'other' }, { nativeVersion: '0.154.0' },
    { nativeExecutableHash: digest('other') }, { method: 'thread/read' }, { requestHash: digest('other') }, { params: { threadId: 'other' } }])
    await assert.rejects(checkNativeActionReceipt({ ...value, ...change }, original, 'caller', 'original'), /original account, caller and request/);
  for (const next of [accepted, { ...value, requestId: 'changed' }, { ...value, epoch: 'changed' },
    { ...value, reply: { result: { replaced: true } } }, { ...value, updatedAt: '2025-12-31T00:00:00.000Z' }])
    await assert.rejects(checkNativeActionReceipt(next, original, 'caller', 'original', terminal));
});
