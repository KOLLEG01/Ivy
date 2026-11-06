import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { digest } from '../packages/contracts/src/canonical.js';
import type { Agent, Wire } from '../packages/contracts/src/generated.js';
import { IvyError } from '../packages/sdk/src/client.js';
import type { RpcClient, BoundTool } from '../packages/sdk/src/client.js';
import { hostProject } from '../packages/sdk/src/project-inventory.js';
import { backendRead, nativeBackendKey, nativeBackendModels, nativeBackendScopes, nativeModelKey } from '../packages/ui-client/src/native-backends.js';
import { executionBackendChoices, executionModels, executionSpeedAvailable, modelAvailability, nativeModelSpeedAvailable, nativeSpeedAvailable } from '../packages/ui-client/src/execution-models.js';
import type { ExecutionModels } from '../packages/ui-client/src/execution-models.js';
import { nativeRead, optionalTool } from '../packages/ui-client/src/native.js';

const a: Agent.BackendIdentity = { backendId: 'primary', provider: 'codex', accountId: 'work' };
const b: Agent.BackendIdentity = { backendId: 'secondary', provider: 'claude', accountId: 'personal' };
const c: Agent.BackendIdentity = { backendId: 'other', provider: 'codex', accountId: 'personal' };
const hostId = 'host', serviceNodeId = 'host.agent-manager', catalogHash = digest('catalog');
function status(backend: Agent.BackendIdentity): Agent.Status {
  return { backend, serviceNodeId, hostId, state: 'ready', epoch: 'epoch', nativeVersion: '0.163.0',
    nativeExecutableHash: digest('native'), catalogHash, pid: null, observedAt: '2026-01-01T00:00:00.000Z', code: null,
    initialized: null, pendingInputs: 0, operations: { retained: 0, maximum: 100, bytes: 0, maximumBytes: 1048576 }, observedMethods: [] };
}
function tool(qualifiedName: string, tier: unknown = { type: ['string', 'null'] }): BoundTool {
  const split = qualifiedName.indexOf('.'), namespace = qualifiedName.slice(0, split), name = qualifiedName.slice(split + 1);
  return { serviceNodeId, qualifiedName, definitionHash: digest(qualifiedName), definition: {
    namespace, name, interfaceVersion: '1.0.0', description: 'Inert account fixture.',
    annotations: { readOnlyHint: namespace === 'agent' },
    inputSchema: { type: 'object', properties: { serviceTier: tier } }, outputSchema: true,
    ...(namespace === 'agent' ? {} : { nativeMethod: name, nativeSchemaIdentity: catalogHash }),
  } };
}
function agent(backend: Agent.BackendIdentity, host = hostId, efforts = ['low', 'high'], tier: unknown = { type: ['string', 'null'] }): ExecutionModels {
  return { node: { hostId: host, serviceNodeId: host + '.agent-manager' }, backend, status: { ...status(backend), hostId: host, serviceNodeId: host + '.agent-manager' },
    models: [{ id: 'same-id', model: 'same-model', name: 'Same model', description: '', efforts, defaultEffort: efforts[0]!, isDefault: true, hidden: false, serviceTiers: ['fast', 'flex'] }],
    bindings: ['thread/start', 'turn/start'].map(method => tool(backend.backendId + '.' + method, tier)) };
}
function transport() {
  const calls: Record<string, unknown>[] = [], definitions: string[] = [];
  let statuses: Agent.BackendsResult['backends'] = [status(a), status(b), status(c)];
  const behavior = { outage: '', changedEpoch: false, wrongBinding: false, wrongCatalog: false };
  const binding = (name: string) => {
    definitions.push(name);
    const value = tool(name);
    if (behavior.wrongBinding && name.endsWith('turn/start')) value.definition.namespace = 'foreign';
    if (behavior.wrongCatalog && name.endsWith('turn/start')) value.definition.nativeSchemaIdentity = digest('other');
    return value;
  };
  const client = { async request(method: string, params: Record<string, unknown>) {
    if (method === 'serviceNodes.get') return { connected: true, synced: true, ready: true, desiredEnabled: true, serviceName: 'agent-manager' };
    if (method === 'system.status') return { runtimeEpoch: '11111111-1111-4111-8111-111111111111' };
    if (method === 'tools.list') return { provider: { node: { serviceNodeId } }, items: [binding(params['namespace'] + '.' + params['namePrefix'])], nextCursor: null };
    if (method === 'discovery.describe') return { provider: { serviceNodeId }, items: (params['tools'] as string[]).map(binding) };
    assert.equal(method, 'tools.call'); calls.push(params);
    const name = params['qualifiedName'] as string, args = params['arguments'] as Record<string, Wire.Json>;
    const namespace = name.split('.')[0];
    if (namespace === behavior.outage) throw new IvyError('service_unavailable', 'Only this account is down.');
    if (name === 'agent.backends') return { backends: statuses };
    if (name === 'agent.status') {
      const selected = statuses.find(value => nativeBackendKey(value.backend) === nativeBackendKey(args['backend'] as Agent.BackendIdentity));
      assert(selected, 'Management observations require the full account selector.');
      return { ...selected, ...(behavior.changedEpoch ? { epoch: 'reconnected' } : {}) };
    }
    if (name.endsWith('model/list')) return { data: [{ id: 'same-id', model: 'same-model', displayName: 'Same model',
      supportedReasoningEfforts: [{ reasoningEffort: namespace === b.backendId ? 'max' : 'low' }], defaultReasoningEffort: 'low' }], nextCursor: null };
    return {};
  } } as unknown as RpcClient;
  return { client, calls, definitions, behavior, setStatuses: (value: Agent.BackendsResult['backends']) => { statuses = value; } };
}

test('equal model names retain provider and account identity across hosts and intersect only their own options', () => {
  const agents = [agent(a), agent(b, hostId, ['max']), agent(c, hostId, ['minimal']), agent(a, 'second', ['high']), agent(b, 'second', ['max'])];
  const models = executionModels(agents);
  assert.equal(models.length, 2, 'An account absent on an eligible host cannot provide a default.');
  assert.deepEqual(models.find(model => model.backend?.backendId === a.backendId)?.efforts, ['high']);
  assert.deepEqual(models.find(model => model.backend?.backendId === b.backendId)?.efforts, ['max']);
  assert.equal(new Set(models.map(model => model.key)).size, 2);
  assert.equal(modelAvailability(agents).find(model => model.backend?.backendId === c.backendId)?.missing[0], 'second');
  assert.notEqual(nativeModelKey(a, 'same-model'), nativeModelKey({ ...a, accountId: 'personal' }, 'same-model'));
  assert.notEqual(nativeModelKey(a, 'same-model'), nativeModelKey({ ...a, provider: 'claude' }, 'same-model'));
  const choices = executionBackendChoices(agents);
  assert.equal(choices.length, 3);
  assert.match(choices.find(choice => choice.key === nativeBackendKey(b))!.label, /Claude.*personal/);
  const unscoped = agents.slice(0, 2).map(({ backend: _, ...value }, index) => ({ ...value, node: { ...value.node, serviceNodeId: 'legacy-' + index } }));
  assert.deepEqual(executionModels(unscoped), [], 'Two unscoped owners on one host are ambiguous.');
});

test('speed choices use the selected account and exact native schemas, including nullable strings and enum restrictions', () => {
  const agents = [agent(a), agent(b, hostId, ['max'], { enum: ['flex', null] }), agent(a, 'second', ['high'], { anyOf: [{ enum: ['fast'] }, { type: 'null' }] })];
  assert.equal(executionSpeedAvailable(agents, 'same-model', 'high', a, 'fast'), true);
  assert.equal(executionSpeedAvailable(agents, 'same-model', 'high', a, 'flex'), false);
  assert.equal(executionSpeedAvailable(agents.slice(0, 2), 'same-model', 'max', b, 'fast'), false);
  assert.equal(executionSpeedAvailable(agents.slice(0, 2), 'same-model', 'max', b, 'flex'), false, 'The pinned Claude adapter does not implement service tiers, even with a broad schema.');
  assert.equal(executionSpeedAvailable(agents, 'same-model', 'max', a, 'fast'), false);
  const catalog = JSON.parse(readFileSync('specs/native/codex-0.163.0/catalog.json', 'utf8')) as Agent.Catalog;
  const actual = agent(a);
  actual.bindings = ['thread/start', 'turn/start'].map(method => ({ ...tool(a.backendId + '.' + method),
    definition: { ...tool(a.backendId + '.' + method).definition, inputSchema: catalog.clientRequests.find(value => value.method === method)!.inputSchema } }));
  assert.equal(executionSpeedAvailable([actual], 'same-model', 'high', a, 'fast'), true, 'The packaged Codex catalog permits nullable string tiers.');
  assert.equal(executionSpeedAvailable([actual], 'same-model', 'high', a, 'flex'), true);
  assert.equal(nativeSpeedAvailable(actual.bindings, 'standard', 'codex'), true, 'Standard is an explicit native null.');
  assert.equal(nativeSpeedAvailable(actual.bindings, 'fast', 'claude'), false);
  assert.equal(nativeSpeedAvailable(actual.bindings, 'fast', undefined), false);
  actual.models[0]!.serviceTiers = ['fast'];
  assert.equal(executionSpeedAvailable([actual], 'same-model', 'high', a, 'flex'), false, 'Native method support alone does not advertise a model tier.');
  assert.equal(nativeModelSpeedAvailable(actual.models, 'same-model', 'fast'), true);
  assert.equal(nativeModelSpeedAvailable(actual.models, 'unavailable', 'fast'), false);
});

test('model discovery addresses each backend namespace and leaves provider JSON unchanged', async () => {
  const f = transport(), scopes = await nativeBackendScopes(f.client, { serviceNodeId, hostId });
  assert.equal(scopes.length, 3);
  const results = await Promise.all(scopes.map(scope => nativeBackendModels(f.client, scope)));
  assert.deepEqual(results.map(result => result.models[0]!.efforts), [['low'], ['max'], ['low']]);
  for (const backend of [a, b, c]) {
    const call = f.calls.find(call => call['qualifiedName'] === backend.backendId + '.model/list')!;
    assert.deepEqual(call['arguments'], { limit: 100, includeHidden: true });
    assert.equal(typeof call['operationId'], 'string', 'Native methods outside the codex namespace still carry an operation identity.');
    assert(f.calls.some(call => call['qualifiedName'] === 'agent.status' && nativeBackendKey((call['arguments'] as { backend: Agent.BackendIdentity }).backend) === nativeBackendKey(backend)));
  }
});

test('discovery rejects duplicate owners, foreign hosts, reconnects and foreign native definitions', async () => {
  for (const values of [[status(a), status(a)], [{ ...status(a), hostId: 'foreign' }], [{ ...status(a), serviceNodeId: 'foreign' }]]) {
    const f = transport(); f.setStatuses(values);
    await assert.rejects(nativeBackendScopes(f.client, { serviceNodeId, hostId }), { code: 'native_owner_mismatch' });
  }
  for (const behavior of ['changedEpoch', 'wrongBinding', 'wrongCatalog'] as const) {
    const f = transport(), scope = (await nativeBackendScopes(f.client, { serviceNodeId, hostId }))[0]!;
    f.behavior[behavior] = true;
    await assert.rejects(nativeBackendModels(f.client, scope), { code: 'native_owner_changed' });
  }
});

test('one account outage does not suppress sibling accounts or the host backend listing', async () => {
  const f = transport(); f.behavior.outage = a.backendId;
  await assert.rejects(nativeRead(f.client, serviceNodeId, a.backendId + '.model/list', {}), { code: 'service_unavailable' });
  const sent = f.calls.length;
  await assert.rejects(backendRead(f.client, { serviceNodeId, backend: a }, 'agent.status', {}), { code: 'service_unavailable' });
  assert.equal(f.calls.length, sent, 'The same account shares its short outage observation.');
  await nativeRead(f.client, serviceNodeId, b.backendId + '.model/list', {});
  await backendRead(f.client, { serviceNodeId, backend: b }, 'agent.status', {});
  assert.equal((await nativeBackendScopes(f.client, { serviceNodeId, hostId })).length, 3);
  const legacy = transport(); legacy.behavior.outage = 'codex';
  await assert.rejects(nativeRead(legacy.client, serviceNodeId, 'codex.model/list', {}));
  assert.equal((await nativeBackendScopes(legacy.client, { serviceNodeId, hostId })).length, 3, 'Unscoped Codex failure cannot hide the host listing.');
});

test('preparation failures stay visible without fabricated catalog data and do not prevent healthy model discovery', async () => {
  const f = transport();
  f.setStatuses([status(a), { backend: b, serviceNodeId, hostId, phase: 'preparation', state: 'failed', nativeVersion: '0.163.0',
    nativeExecutableHash: digest('native'), observedAt: '2026-01-01T00:00:00.000Z', code: 'storage_account_mismatch', operationReadsAvailable: false }]);
  const scopes = await nativeBackendScopes(f.client, { serviceNodeId, hostId });
  assert.equal(executionBackendChoices(scopes.map(scope => ({ ...scope, node: { serviceNodeId, hostId } }))).length, 2);
  await assert.rejects(nativeBackendModels(f.client, scopes[1]!), { code: 'native_unavailable' });
  assert.equal((await nativeBackendModels(f.client, scopes[0]!)).models[0]!.model, 'same-model');
});

test('a catalog or account change expires only the affected native namespace', async () => {
  const f = transport();
  await Promise.all([a, b].map(backend => backendRead(f.client, { serviceNodeId, backend }, 'agent.status', {})));
  await Promise.all([a, b].map(backend => optionalTool(f.client, serviceNodeId, backend.backendId + '.thread/start')));
  const before = f.definitions.length;
  f.setStatuses([{ ...status(a), backend: { ...a, accountId: 'replacement' }, epoch: 'next' }, status(b)]);
  await backendRead(f.client, { serviceNodeId, backend: { ...a, accountId: 'replacement' } }, 'agent.status', {});
  await Promise.all([a, b].map(backend => optionalTool(f.client, serviceNodeId, backend.backendId + '.thread/start')));
  assert.deepEqual(f.definitions.slice(before), [a.backendId + '.thread/start']);
});

test('project inventory retains account namespace, service and path and rejects ambiguous native IDs or cyclic pages', async () => {
  const requests: Record<string, unknown>[] = [];
  const item = (service: string, namespace: string, path: string) => ({ resourceRef: { serviceNodeId: service, namespace, kind: 'project', nativeId: 'same-project' },
    summary: { nativeId: 'same-project', source: 'native', name: namespace + ' project', paths: [path] } });
  let items = [item('foreign', b.backendId, '/selected'), item(serviceNodeId, a.backendId, '/selected'), item(serviceNodeId, b.backendId, '/wrong'), item(serviceNodeId, b.backendId, '/selected')];
  let cursor: string | null = null;
  let observedBackend = b;
  const client = { async request(method: string, params: Record<string, unknown>) {
    requests.push(params);
    if (method === 'tools.list') return { provider: { node: { serviceNodeId } }, items: [tool('agent.projects')], nextCursor: null };
    if (method === 'tools.call') {
      assert.deepEqual(params['arguments'], { backend: b });
      return { backend: observedBackend, source: 'native', observedAt: '2026-01-01T00:00:00.000Z',
        projects: [{ nativeId: 'same-project', source: 'native', name: 'Current personal project', paths: ['/selected'] }] };
    }
    assert.equal(method, 'inventory.list'); return { items, nextCursor: cursor };
  } } as unknown as RpcClient;
  const selection = { backend: b, serviceNodeId, path: '/selected' };
  assert.equal((await hostProject(client, hostId, 'same-project', undefined, selection))?.name, 'Current personal project');
  assert.deepEqual(requests[0], { serviceName: 'agent-manager', namespace: b.backendId, serviceNodeId, kind: 'project', hostId, limit: 100 });
  observedBackend = { ...b, accountId: 'replacement' };
  await assert.rejects(hostProject(client, hostId, 'same-project', undefined, selection), { code: 'native_owner_mismatch' });
  items = [item(serviceNodeId, b.backendId, '/selected'), item(serviceNodeId, b.backendId, '/selected')];
  assert.equal(await hostProject(client, hostId, 'same-project', undefined, selection), null);
  items = []; cursor = 'repeat';
  await assert.rejects(hostProject(client, hostId, 'same-project', undefined, selection), /bounded snapshot/);
});
