import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { Ajv2020 } from 'ajv/dist/2020.js';
import type { AnySchema } from 'ajv';
import { HiveKernel } from '../services/hive/src/kernel.js';
import type { ConnectionContext } from '../services/hive/src/kernel.js';
import type { RegistrySync, ToolDefinition } from '../services/hive/src/registry.js';
import type { Operation, OperationName, Params, Result } from '../packages/contracts/src/generated.js';
import { digest, hashJson } from '../packages/contracts/src/canonical.js';
import { toolDefinitionHash } from '../packages/contracts/src/tool-definition.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { directDescriptors, directCall } from '../services/hive/src/mcp-catalog.js';
import { toolPresentation } from '../services/hive/src/mcp-presentation.js';
import { SchemaValidators } from '../packages/contracts/src/schema.js';
import { LiveRouting } from '../services/hive/src/live-routing.js';
import type { PreparedCall } from '../services/hive/src/live-routing.js';
import { secretaryRegistry } from '../services/secretary/src/schema.js';
import { taskBoardRegistry } from '../services/task-board/src/runtime/registry.js';
import { chatRegistry } from '../services/chat-bridge/src/registry.js';
import { phoneRegistry } from '../services/phone-bridge/src/runtime/registry.js';
import wikiManifest from '../ui/wiki-ui/ui.json' with { type: 'json' };
import { operationId } from '../packages/contracts/src/operation-id.js';
import type { Wire } from '../packages/contracts/src/generated.js';
import { readFileSync } from 'node:fs';
import { discoverNativeTools } from '../services/agent-manager/src/native-discovery.js';
import { operations } from '../packages/contracts/src/core-validation.js';
import { validateAgent } from '../packages/contracts/src/agent-validation.js';
import { coreMcpNames } from '../services/hive/src/core-mcp.js';
import { agentRegistry } from '../services/agent-manager/src/registry.js';
import { mcpDiscoveryResultBytes } from '../packages/contracts/src/limits.js';

const client: ConnectionContext = { transport: 'http', credentialDigest: digest('discovery-client'), principalId: 'client' };
const socket: ConnectionContext = { transport: 'ws', credentialDigest: digest('discovery-service'), principalId: 'service' };
const errorCode = (code: string) => (error: unknown) => error instanceof IvyError && error.code === code;
function fixture(t: TestContext) {
  const kernel = new HiveKernel({ filename: ':memory:', publicBaseUrl: 'https://hive.test', version: 'test', buildId: digest('test'),
    credentials: [{ principalId: 'client', digest: client.credentialDigest }, { principalId: 'service', digest: socket.credentialDigest }] });
  const routing = new LiveRouting(true), caller = { ...client, principalId: 'client' };
  t.after(() => kernel.close());
  const call = <M extends OperationName>(method: M, params: Params<M>, context = client): Result<M> => {
    const result = kernel.execute(context, { jsonrpc: '2.0', id: 'test', method, params });
    assert.equal(result.kind, 'result');
    if ((method === 'registry.sync' || method === 'service.heartbeat') && context.serviceNodeId) { const catalog = kernel.registry.liveCatalog(context.serviceNodeId); routing.update({ node: kernel.registry.node(context.serviceNodeId), ...(catalog ? { catalog } : {}) }); }
    return result.value as Result<M>;
  };
  const connect = (id: string, serviceName = 'agent-manager') => {
    const connected = call('service.connect', { serviceNodeId: id, hostId: 'host-' + id, serviceName, version: '1', buildId: digest(id), hiveProtocol: 1 }, socket);
    const catalog = kernel.registry.liveCatalog(id); routing.update({ node: kernel.registry.node(id), generation: connected.generation, ...(catalog ? { catalog } : {}) });
    return { ...socket, serviceNodeId: id, generation: connected.generation };
  };
  const sync = (owner: ConnectionContext, definitions: ToolDefinition[], mcpPrefix?: string) => {
    const namespaces = [...new Set(definitions.map(tool => tool.namespace))];
    const registry: RegistrySync = { namespaces: namespaces.map(namespace => ({ namespace, description: 'Fixture ' + namespace,
      guideMarkdown: 'Exact provider guide for ' + namespace, tools: definitions.filter(tool => tool.namespace === namespace),
      discoveryGroups: namespace === 'codex' ? [
        { group: 'codex', description: 'Native Codex operations.' }, { group: 'codex/tasks', description: 'Codex task operations.' },
        { group: 'codex/tasks/content', description: 'Read task history and content.' }, { group: 'codex/tasks/lifecycle', description: 'Create and archive tasks.' },
        { group: 'codex/models-account', description: 'Models and account limits.' }, { group: 'codex/newFeature', description: 'Future provider-owned tools.' },
      ] : [], topics: [], inventoryKinds: [] })), contracts: [], requiredContracts: [], ...(mcpPrefix ? { mcpPrefix } : {}) };
    call('registry.sync', registry, owner); call('service.heartbeat', { ready: true, diagnostics: [] }, owner);
  };
  return { kernel, routing, caller, call, connect, sync };
}
function definition(name: string, namespace = 'codex', size = 0): ToolDefinition {
  const discovery = namespace !== 'codex' ? undefined : name.startsWith('thread/read') ? { group: 'codex/tasks/content', summary: 'Read a Codex task.', keywords: ['task'] }
    : name === 'thread/archive' ? { group: 'codex/tasks/lifecycle', summary: 'Archive a Codex task.', keywords: ['task', 'archivieren'] }
    : name.startsWith('account/') ? { group: 'codex/models-account', summary: 'Read account usage limits.', keywords: ['usage', 'limit'] }
    : { group: 'codex/newFeature', summary: 'Use a future provider-owned feature.' };
  return { namespace, name, interfaceVersion: '1.0.0', description: 'Read synthetic task ' + name,
    inputSchema: { type: 'object', properties: { value: { type: 'string' } }, required: ['value'], additionalProperties: false,
      ...(size ? { description: 'x'.repeat(size) } : {}) }, outputSchema: { type: 'object', additionalProperties: true },
    annotations: { readOnlyHint: true }, discovery: { ...discovery }, ...(namespace === 'codex' ? { nativeMethod: name, nativeSchemaIdentity: 'fixture' } : {}) };
}

test('service roots aggregate hosts and namespaces; large catalogs browse and search without schemas', t => {
  const { call, connect, sync } = fixture(t);
  const definitions = Array.from({ length: 40 }, (_, index) => definition('thread/read' + String(index).padStart(2, '0'), 'codex', 30000));
  definitions.push(definition('thread/archive'), definition('account/rateLimits/read'), definition('status', 'agent'));
  assert.ok(Buffer.byteLength(JSON.stringify(definitions)) > 1024 * 1024);
  sync(connect('host-b'), definitions); sync(connect('host-a'), definitions);
  const roots = call('discovery.list', {});
  assert.deepEqual(roots.items.map(entry => entry.name), ['agent-manager', 'hive']);
  assert.equal(roots.items[0]!.providerCount, 2); assert.equal(roots.items[0]!.toolCount, 43);
  assert.match(roots.items[0]!.description, /Fixture codex/);
  const groups = call('discovery.list', { serviceName: 'agent-manager' });
  assert.deepEqual(groups.items.map(item => item.name), ['agent', 'codex']);
  assert.deepEqual(call('discovery.list', { serviceName: 'agent-manager', group: 'agent' }).items.map(item => item.name), ['agent.status']);
  const nested = call('discovery.list', { serviceName: 'agent-manager', group: 'codex' });
  assert.ok(nested.items.some(item => item.name === 'codex/tasks'));
  const selected = call('discovery.list', { serviceName: 'agent-manager', query: 'Task archivieren' });
  assert.deepEqual(selected.items.map(item => item.name), ['codex.thread/archive']);
  const usage = call('discovery.list', { query: 'account limits' });
  assert.ok(usage.items.some(item => item.name === 'codex.account/rateLimits/read'));
  assert.equal(call('discovery.list', { query: 'Codex usage' }).items[0]!.name, 'codex.account/rateLimits/read');
  let cursor: string | undefined; const names = new Set<string>();
  do {
    const page = call('discovery.list', { serviceName: 'agent-manager', group: 'codex/tasks/content', ...(cursor ? { cursor } : {}) });
    assert.ok(Buffer.byteLength(JSON.stringify(page)) <= 8192); assert.ok(page.items.length <= 20);
    assert.doesNotMatch(JSON.stringify(page), /inputSchema|outputSchema|nativeSchemaIdentity|xxxx/);
    for (const item of page.items) { assert.equal(item.kind, 'tool'); assert.ok(!names.has(item.name)); names.add(item.name); }
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
  assert.equal(names.size, 40);
  const providers = call('discovery.list', { serviceName: 'agent-manager', providers: true });
  assert.deepEqual(providers.items.map(item => item.hostId), ['host-host-a', 'host-host-b']);
});

test('capability searches find real service tools and Wiki data entry points without loading schemas', t => {
  const { call, connect } = fixture(t);
  const services: Array<[string, RegistrySync]> = [
    ['secretary', secretaryRegistry()], ['task-board', taskBoardRegistry()],
    ['chat-bridge', chatRegistry()], ['phone-bridge', phoneRegistry()],
  ];
  for (const [name, registry] of services) {
    const owner = connect(name, name);
    call('registry.sync', { ...registry, contracts: [], requiredContracts: [] }, owner);
    call('service.heartbeat', { ready: true, diagnostics: [] }, owner);
  }
  const searches: Array<[string, string[]]> = [
    ['wiki', ['hive.objects.search', 'hive.objects.read']],
    ['kanban', ['task-board.workspace', 'task-board.create']],
    ['secretary schedule', ['secretary.createAssignment']],
    ['secretary reminder', ['secretary.createAssignment']],
    ['secretary Zeitplan', ['secretary.createAssignment']],
    ['WhatsApp message', ['chat.notify', 'chat.history']],
    ['voice model', ['phone.request']],
    ['event trigger', ['hive.topics.list']],
  ];
  for (const [query, expected] of searches) {
    const page = call('discovery.list', { query });
    for (const name of expected) assert.ok(page.items.some(item => item.name === name), `${query} must find ${name}`);
    assert.ok(Buffer.byteLength(JSON.stringify(page)) <= 8192);
    assert.doesNotMatch(JSON.stringify(page), /inputSchema|outputSchema|guideMarkdown/);
  }
  const roots = call('discovery.list', {}).items;
  for (const [name, registry] of services) assert.equal(roots.find(item => item.name === name)?.description, registry.discoveryHint);
  const groups = call('discovery.list', { serviceName: 'hive' }).items;
  assert.match(groups.find(item => item.group === 'objects')!.description, /Wiki/);
  const detail = call('discovery.describe', { serviceName: 'secretary', tools: ['secretary.createAssignment'] });
  assert.equal(detail.provider!.serviceNodeId, 'secretary');
  assert.match(detail.guides[0]!.guideMarkdown, /secretary_create_assignment/);
  assert.match(detail.guides[0]!.guideMarkdown, /ivy_dev/);
  assert.equal(detail.items[0]!.definition.annotations!.readOnlyHint, false);
  assert.equal(detail.items[0]!.definitionHash, toolDefinitionHash(detail.items[0]!.definition));
});

test('service hints update root discovery while empty hints retain namespace fallback', t => {
  const { call, connect } = fixture(t), owner = connect('custom', 'custom-service');
  const registry: RegistrySync = { discoveryHint: '  A custom service helps organize research.  ',
    namespaces: [{ namespace: 'research', description: 'Research collections', guideMarkdown: 'Start by listing collections.',
      tools: [definition('list', 'research')], topics: [], inventoryKinds: [] }], contracts: [], requiredContracts: [] };
  call('registry.sync', registry, owner);
  const description = () => call('discovery.list', {}).items.find(item => item.name === 'custom-service')!.description;
  assert.equal(description(), registry.discoveryHint!.trim());
  call('registry.sync', { ...registry, discoveryHint: ' ' }, owner);
  assert.equal(description(), 'Research collections');
  call('registry.sync', { ...registry, discoveryHint: 'é'.repeat(300) }, owner);
  assert.ok(Buffer.byteLength(description()) <= 220);
});

test('describe binds exact owner and schemas; cursors survive heartbeat but reject changed catalogs and selectors', t => {
  const { kernel, call, connect, sync } = fixture(t), a = connect('a'), b = connect('b');
  const tools = [definition('thread/archive'), definition('thread/read')];
  sync(a, tools); sync(b, [definition('thread/archive', 'codex', 32)]);
  assert.throws(() => call('discovery.describe', { serviceName: 'agent-manager', tools: ['codex.thread/archive'] }), errorCode('ambiguous_service_node'));
  const described = call('discovery.describe', { serviceName: 'agent-manager', serviceNodeId: 'a', tools: ['codex.thread/archive', 'codex.thread/read'] });
  assert.equal(described.provider!.serviceNodeId, 'a'); assert.equal(described.guides.length, 1);
  assert.deepEqual(described.items[0]!.definition, tools[0]); assert.equal(described.items[0]!.definitionHash, toolDefinitionHash(tools[0]!));
  const args = { serviceName: 'agent-manager', serviceNodeId: 'a', limit: 1 };
  const page = call('discovery.list', args); assert.ok(page.nextCursor);
  call('service.heartbeat', { ready: true, diagnostics: [] }, a);
  assert.equal(call('discovery.list', { ...args, cursor: page.nextCursor! }).items.length, 1);
  assert.throws(() => call('discovery.list', { ...args, query: 'archive', cursor: page.nextCursor! }), errorCode('invalid_cursor'));
  sync(a, [definition('thread/archive', 'codex', 40), tools[1]!]);
  assert.throws(() => call('discovery.list', { ...args, cursor: page.nextCursor! }), errorCode('invalid_cursor'));
  assert.throws(() => call('discovery.list', { serviceName: 'task-board', serviceNodeId: 'a' }), errorCode('target_conflict'));
  kernel.registry.disconnect('a');
  assert.equal(call('discovery.describe', { serviceName: 'agent-manager', serviceNodeId: 'a', tools: ['codex.thread/archive'] }).provider!.available, false);
  assert.equal(call('discovery.list', { serviceName: 'agent-manager', providers: true }).items.find(item => item.serviceNodeId === 'a')!.available, false);
});

test('generic calls retain validation, original identity, owner fencing and definition hashes', t => {
  const { kernel, routing, caller, call, connect, sync } = fixture(t), a = connect('a'); sync(a, [definition('thread/archive')]);
  const binding = call('discovery.describe', { serviceName: 'agent-manager', serviceNodeId: 'a', tools: ['codex.thread/archive'] }).items[0]!;
  const params: Operation.DiscoveryCallParams = { serviceName: 'agent-manager', serviceNodeId: 'a', qualifiedName: binding.qualifiedName,
    expectedDefinitionHash: binding.definitionHash, operationId: 'original-intent', arguments: { value: 'exact-case' } };
  const invoke = (value: unknown): unknown => {
    if ((value as { serviceName?: string } | null)?.serviceName === 'hive') {
      const result = kernel.execute(client, { jsonrpc: '2.0', id: 'test', method: 'discovery.call', params: value });
      assert.equal(result.kind, 'result'); return result.value;
    }
    const params = value as Parameters<typeof routing.prepare>[1];
    requireExactServiceNode(params);
    return routing.prepare(caller, params);
  };
  const result = invoke(params) as PreparedCall;
  assert.equal(result.operationId, 'original-intent'); assert.deepEqual(result.arguments, params.arguments);
  assert.throws(() => invoke({ ...params, serviceName: 'task-board' }), errorCode('target_conflict'));
  const { serviceNodeId: _node, ...withoutNode } = params;
  assert.throws(() => invoke(withoutNode), errorCode('invalid_arguments'));
  assert.throws(() => invoke({ ...params, expectedDefinitionHash: digest('wrong') }), errorCode('tool_definition_changed'));
  assert.throws(() => invoke({ ...params, arguments: { unexpected: true } }), errorCode('invalid_arguments'));
  assert.throws(() => invoke({ ...params, expectedCallerPrincipalId: 'another' }), errorCode('caller_changed'));
  const core = call('discovery.describe', { serviceName: 'hive', tools: ['hive.system.status'] }).items[0]!;
  const coreParams = { serviceName: 'hive', qualifiedName: core.qualifiedName, expectedDefinitionHash: core.definitionHash, arguments: {} };
  assert.equal((call('discovery.call', coreParams) as { callerPrincipalId: string }).callerPrincipalId, call('system.status', {}).callerPrincipalId);
  assert.throws(() => invoke({ ...coreParams, arguments: { extra: true } }), errorCode('invalid_arguments'));
  assert.throws(() => invoke({ ...coreParams, expectedDefinitionHash: digest('stale') }), errorCode('tool_definition_changed'));
  assert.throws(() => invoke({ ...coreParams, expectedCallerPrincipalId: 'another' }), errorCode('caller_changed'));
  for (const name of ['hive.service.connect', 'hive.registry.sync', 'hive.discovery.call', 'hive.tools.call'])
    assert.throws(() => call('discovery.describe', { serviceName: 'hive', tools: [name] }), errorCode('not_found'));
  assert.throws(() => connect('fake', 'hive'), errorCode('invalid_arguments'));
  assert.throws(() => kernel.store.authenticate({ credentialDigest: digest('revoked') }), errorCode('unauthenticated'));
  kernel.registry.disconnect('a'); routing.disconnect('a', a.generation);
  assert.throws(() => invoke(params), errorCode('service_unavailable'));
});

function requireExactServiceNode(params: Parameters<LiveRouting['prepare']>[1]): void {
  if (params.serviceName !== 'hive' && !params.serviceNodeId) throw new IvyError('invalid_arguments', 'Service calls use the exact described Service Node.');
}

test('unknown services/methods remain reachable and oversized definitions fail explicitly', t => {
  const { call, connect, sync } = fixture(t);
  const service = 'custom-service'; sync(connect('custom', service), [definition('newFeature/DoExactCASE')]);
  assert.equal(call('discovery.list', { serviceName: service }).items[0]!.name, 'codex.newFeature/DoExactCASE');
  assert.equal(call('discovery.describe', { serviceName: service, tools: ['codex.newFeature/DoExactCASE'] }).items[0]!.definition.nativeMethod, 'newFeature/DoExactCASE');
  sync(connect('prototype-word', 'constructor'), Array.from({ length: 21 }, (_, index) => definition('read' + index, 'constructor')));
  assert.ok(call('discovery.list', {}).items.some(item => item.serviceName === 'constructor'));
  assert.equal(call('discovery.list', { serviceName: 'constructor' }).items[0]!.name, 'constructor');
  assert.equal(call('discovery.list', { serviceName: 'constructor', group: 'constructor', query: 'constructor', limit: 20 }).items.length, 20);
  const large = connect('large', 'large-service'); sync(large, ['one', 'two', 'three'].map(name => definition(name, 'test', 180000)));
  assert.equal(call('discovery.list', { serviceName: 'large-service' }).items.length, 3);
  assert.throws(() => call('discovery.describe', { serviceName: 'large-service', tools: ['test.one', 'test.two', 'test.three'] }), errorCode('result_too_large'));
  assert.equal(call('discovery.describe', { serviceName: 'large-service', tools: ['test.one'] }).items.length, 1);
});

test('MCP publication is opt-in and internal tools cannot be reached through fallback dispatch', t => {
  const { kernel, call, connect, sync, routing, caller } = fixture(t);
  const mcpCall = <M extends OperationName>(method: M, params: Params<M>) => call(method, params, { ...client, transport: 'mcp' });
  const internal = definition('internal', 'sample'); delete internal.discovery;
  const publicTool = { ...definition('read', 'sample'), discovery: { mcp: { name: 'custom_service_read' } } };
  const owner = connect('public', 'custom-service'); sync(owner, [publicTool, internal]);
  sync(connect('private', 'internal-service'), [internal]);
  assert.deepEqual(mcpCall('discovery.list', {}).items.map(item => item.name), ['custom-service', 'hive']);
  assert.equal(mcpCall('discovery.list', { query: 'internal' }).items.length, 0);
  assert.equal(mcpCall('discovery.list', { serviceName: 'custom-service' }).items[0]!.mcpName, 'custom_service_read');
  assert.throws(() => mcpCall('discovery.describe', { serviceName: 'custom-service', serviceNodeId: 'public', tools: ['sample.internal'] }), errorCode('not_found'));
  const retained = kernel.discovery.describe({ serviceName: 'custom-service', serviceNodeId: 'public', tools: ['sample.internal'] }).items[0]!;
  assert.throws(() => routing.prepare({ ...caller, transport: 'mcp' }, { serviceName: 'custom-service', serviceNodeId: 'public', qualifiedName: retained.qualifiedName,
    expectedDefinitionHash: retained.definitionHash, arguments: { value: 'hidden' } }), errorCode('not_found'));
  assert.equal(routing.prepare({ ...caller, transport: 'http' }, { serviceName: 'custom-service', serviceNodeId: 'public', qualifiedName: retained.qualifiedName,
    expectedDefinitionHash: retained.definitionHash, arguments: { value: 'internal client' } }).serviceNodeId, 'public');
  assert.throws(() => sync(owner, [{ ...publicTool, discovery: { mcp: { name: 'wiki_spoof' } } }]), errorCode('registry_invalid'));
  assert.throws(() => sync(connect('fake-wiki', 'wiki'), [{ ...publicTool, discovery: { mcp: { name: 'wiki_read' } } }]), errorCode('registry_invalid'));
  sync(owner, [publicTool]);
  assert.throws(() => sync(connect('overlapping-prefix', 'custom'), [publicTool]), errorCode('registry_invalid'));
});

test('services can declare a distinct MCP prefix without claiming reserved or duplicate names', t => {
  const { kernel, connect, sync } = fixture(t);
  const owner = connect('task-owner', 'task-board');
  const publicTool = { ...definition('read', 'task'), discovery: { mcp: { name: 'task_read' } } };
  assert.throws(() => sync(owner, [publicTool]), errorCode('registry_invalid'));
  sync(owner, [publicTool], 'task');
  assert.equal(kernel.discovery.mcpCatalog('task_read').length, 1);
  assert.throws(() => sync(owner, [publicTool], 'hive'), errorCode('registry_invalid'));
  assert.throws(() => sync(connect('other-task', 'other-service'), [publicTool], 'task'), errorCode('registry_invalid'));
});

test('available service publication policy suppresses obsolete MCP aliases from unavailable hosts', t => {
  const { kernel, call, connect, sync } = fixture(t);
  const published = (name: string, mcpName: string, surface?: 'ivy' | 'ivy_dev') => ({
    ...definition(name, 'sample'), discovery: { mcp: { name: mcpName, ...(surface ? { surface } : {}) } },
  });
  const currentInternal = definition('prevent', 'sample'); delete currentInternal.discovery;
  const current = connect('current', 'custom-service'), stale = connect('stale', 'custom-service');
  sync(current, [published('capabilities', 'custom_status', 'ivy'), published('read', 'custom_read', 'ivy'), currentInternal], 'custom');
  sync(stale, [published('capabilities', 'custom_capabilities'), published('read', 'custom_read', 'ivy'), published('prevent', 'custom_prevent')], 'custom');
  call('service.heartbeat', { ready: false, diagnostics: [] }, stale);
  const bindings = kernel.discovery.mcpCatalog();
  assert.ok(!bindings.some(binding => binding.definition.discovery?.mcp?.name === 'custom_capabilities'));
  assert.ok(!bindings.some(binding => binding.definition.discovery?.mcp?.name === 'custom_prevent'));
  assert.deepEqual(bindings.filter(binding => binding.definition.discovery?.mcp?.name === 'custom_read')
    .map(binding => binding.provider?.serviceNodeId), ['current', 'stale']);
});

test('the main service surfaces publish bounded complete direct schemas and keep native Codex discovery inside AgentManager', t => {
  const { kernel, call, connect } = fixture(t);
  const catalog = JSON.parse(readFileSync('specs/native/codex-0.154.0/catalog.json', 'utf8'));
  for (const [name, registry] of [['agent-manager', agentRegistry(catalog)], ['task-board', taskBoardRegistry()], ['chat-bridge', chatRegistry()],
    ['phone-bridge', phoneRegistry()], ['secretary', secretaryRegistry()]] as const) {
    const owner = connect(name, name);
    call('registry.sync', { ...registry, contracts: [], requiredContracts: [] }, owner);
    call('service.heartbeat', { ready: true, diagnostics: [] }, owner);
  }
  const descriptors = directDescriptors(kernel.discovery.mcpCatalog());
  assert.equal(kernel.discovery.toolSchema({ name: 'secretary_operation_read' }).name, 'secretary_operation_read');
  assert.throws(() => kernel.discovery.toolSchema({ name: 'secretary_history' }), errorCode('not_found'));
  const schemas = new SchemaValidators();
  const clientSchemas = new Ajv2020({ strict: false, validateFormats: false, inlineRefs: false });
  const examples = readFileSync('instructions/hive-mcp-examples.md', 'utf8');
  const samples = [...examples.matchAll(/<!-- mcp-example: ([a-z_]+) -->\s*```json\r?\n([\s\S]*?)\r?\n```/g)];
  assert.equal(samples.length, 12);
  for (const [, name, source] of samples) {
    const descriptor = descriptors.find(tool => tool.name === name);
    assert.ok(descriptor, name);
    const args = JSON.parse(source!) as Record<string, unknown>;
    if (typeof args['serviceNodeId'] === 'string') args['serviceNodeId'] = name!.startsWith('agent_') ? 'agent-manager' : name!.startsWith('secretary_') ? 'secretary' : 'task-board';
    try { schemas.validate(descriptor.inputSchema, args); }
    catch { const validate = clientSchemas.compile(descriptor.inputSchema); validate(args); assert.fail(`${name}: ${JSON.stringify(validate.errors)}`); }
    if (args['operationId']) assert.equal(args['operationId'], (args['input'] as Record<string, unknown>)['operationId'], name);
  }
  const wikiUpdate = descriptors.find(tool => tool.name === 'wiki_update')!;
  const taskUpdate = descriptors.find(tool => tool.name === 'task_update')!;
  const alternatives = (schema: Record<string, unknown>) => (schema['allOf'] as { oneOf: unknown[] }[])[0]!.oneOf;
  assert.equal(alternatives(wikiUpdate.inputSchema).length, 3);
  assert.equal(alternatives(taskUpdate.inputSchema).length, 7);
  assert.ok((wikiUpdate.inputSchema['properties'] as Record<string, unknown>)['objectId']);
  assert.ok((taskUpdate.inputSchema['properties'] as Record<string, unknown>)['input']);
  const taskCreateInput = ((descriptors.find(tool => tool.name === 'task_create')!.inputSchema['properties'] as Record<string, unknown>)['input'] as Record<string, unknown>);
  assert.ok((taskCreateInput['properties'] as Record<string, unknown>)['fields']);
  for (const descriptor of [descriptors.find(tool => tool.name === 'task_create')!, taskUpdate]) {
    const definitions = descriptor.inputSchema['$defs'] as Record<string, { properties: Record<string, Record<string, unknown>>; required: string[] }>;
    const taskFields = definitions['TaskBoardTaskFieldsInput']!;
    assert.equal(Object.hasOwn(taskFields.properties['userContact']!, 'default'), false);
    assert.match(String(taskFields.properties['userContact']!['description']), /task_configuration/);
    assert.ok(!taskFields.required.includes('userContact'));
  }
  assert.match(String(descriptors.find(tool => tool.name === 'task_create')!.description), /task_configuration/);
  const defaultingClient = new Ajv2020({ strict: false, validateFormats: false, inlineRefs: false, useDefaults: true });
  const createArgs = JSON.parse(samples.find(([, name]) => name === 'task_create')![2]!);
  createArgs.serviceNodeId = 'task-board';
  assert.equal(defaultingClient.compile(descriptors.find(tool => tool.name === 'task_create')!.inputSchema)(createArgs), true);
  assert.equal(Object.hasOwn(createArgs.input.fields, 'userContact'), false, 'Client schema defaults must not override the live board setting.');
  assert.equal(directCall(kernel.discovery.mcpCatalog('wiki_update'), {
    objectId: 'page', parentId: null, name: 'Renamed', mutationId: operationId(kernel.store.runtimeEpoch),
  }).qualifiedName, 'hive.wiki.move');
  assert.equal(directCall(kernel.discovery.mcpCatalog('wiki_update'), {
    objectId: 'page', archived: true, mutationId: operationId(kernel.store.runtimeEpoch),
  }).qualifiedName, 'hive.wiki.archive');
  assert.equal(directCall(kernel.discovery.mcpCatalog('task_update'), {
    serviceNodeId: 'task-board', input: { action: 'transition', operationId: 'transition-1', taskId: 'task-1', expectedRevision: 1, workflowState: 'todo', detail: null },
  }).qualifiedName, 'task-board.transition');
  assert.equal(directCall(kernel.discovery.mcpCatalog('task_list'), {
    serviceNodeId: 'task-board', input: {},
  }).qualifiedName, 'task-board.list');
  assert.equal(directCall(kernel.discovery.mcpCatalog('task_list'), {
    serviceNodeId: 'task-board', input: { categories: true },
  }).qualifiedName, 'task-board.categories');
  assert.equal(directCall(kernel.discovery.mcpCatalog('hive_object_read'), {
    objectId: 'object-1',
  }).qualifiedName, 'hive.objects.read');
  assert.equal(directCall(kernel.discovery.mcpCatalog('hive_object_read'), {
    objectId: 'object-1', view: 'metadata',
  }).qualifiedName, 'hive.objects.stat');
  assert.equal(directCall(kernel.discovery.mcpCatalog('hive_object_read'), {
    objectId: 'object-1', view: 'history',
  }).qualifiedName, 'hive.objects.history');
  assert.equal(directCall(kernel.discovery.mcpCatalog('wiki_read'), {
    objectId: 'object-1',
  }).qualifiedName, 'hive.wiki.read');
  assert.equal(directCall(kernel.discovery.mcpCatalog('wiki_read'), {
    objectId: 'object-1', view: 'history',
  }).qualifiedName, 'hive.wiki.history');
  assert.equal(directCall(kernel.discovery.mcpCatalog('agent_manager_status'), {
    serviceNodeId: 'agent-manager', input: {},
  }).qualifiedName, 'agent.status');
  assert.equal(directCall(kernel.discovery.mcpCatalog('agent_manager_status'), {
    serviceNodeId: 'agent-manager', input: {}, view: 'capabilities',
  }).qualifiedName, 'agent.capabilities');
  const inputIdentity = { serviceNodeId: 'agent-manager', epoch: 'epoch-1', requestId: 1 };
  assert.equal(directCall(kernel.discovery.mcpCatalog('agent_manager_inputs'), {
    serviceNodeId: 'agent-manager', input: { identity: inputIdentity },
  }).qualifiedName, 'agent.inputs');
  assert.equal(directCall(kernel.discovery.mcpCatalog('agent_manager_inputs'), {
    serviceNodeId: 'agent-manager', input: { identity: inputIdentity }, view: 'definition',
  }).qualifiedName, 'agent.inputDefinition');
  const expectedBridge = { principalId: 'bridge', callerPrincipalId: 'caller', workspaceId: 'workspace', definitionHash: 'sha256:' + '0'.repeat(64) };
  assert.equal(directCall(kernel.discovery.mcpCatalog('chat_bridge_read'), {
    serviceNodeId: 'chat-bridge', input: { operationId: 'operation-1', expectedBridge },
  }).qualifiedName, 'chat.operation');
  assert.equal(directCall(kernel.discovery.mcpCatalog('chat_bridge_read'), {
    serviceNodeId: 'chat-bridge', input: { operationId: 'operation-1' }, view: 'notice',
  }).qualifiedName, 'chat.notice');
  for (const [name, input, primary, view, selected] of [
    ['hive_hosts', {}, 'hive.hosts.list', 'observations', 'hive.hosts.observations'],
    ['hive_host_configuration_read', { hostId: 'host' }, 'hive.hostConfigurations.edit', 'history', 'hive.hostConfigurations.history'],
    ['hive_retention_status', {}, 'hive.retention.status', 'preview', 'hive.retention.preview'],
    ['hive_service_node_get', { serviceNodeId: 'node' }, 'hive.serviceNodes.get', 'contracts', 'hive.serviceNodes.contracts'],
    ['hive_ui_get', { uiId: 'app' }, 'hive.uis.get', 'releases', 'hive.uis.releases'],
  ] as const) {
    const bindings = kernel.discovery.mcpCatalog(name), descriptor = descriptors.find(tool => tool.name === name)!;
    for (const [args, operation] of [[input, primary], [{ ...input, view }, selected]] as const) {
      schemas.validate(descriptor.inputSchema, args);
      const call = directCall(bindings, args);
      assert.equal(call.qualifiedName, operation);
      assert.deepEqual(call.arguments, input, 'facade selectors do not reach core operations');
    }
    assert.throws(() => directCall(bindings, { ...input, view: 'unknown' }), errorCode('invalid_arguments'));
  }
  const phoneBindings = kernel.discovery.mcpCatalog('phone_bridge_diagnostics');
  for (const [view, operation, input, write] of [
    ['loopback', 'probeLoopback', {}, false], ['audio', 'audioSetup', {}, false],
    ['codecs', 'codecTest', {}, false], ['inventory', 'inventory', {}, false],
    ['logs', 'logs', { callId: '10000000-0000-4000-8000-000000000001' }, false],
    ['archive', 'reconcileArchive', { callId: '10000000-0000-4000-8000-000000000001' }, true],
  ] as const) {
    const args = { serviceNodeId: 'phone-bridge', input, view, ...(write ? { operationId: 'archive-1' } : {}) };
    schemas.validate(descriptors.find(tool => tool.name === 'phone_bridge_diagnostics')!.inputSchema, args);
    const call = directCall(phoneBindings, args);
    assert.equal(call.qualifiedName, 'phone.' + operation);
    assert.deepEqual(call.arguments, input);
    assert.equal(call.operationId, write ? 'archive-1' : undefined);
  }
  assert.throws(() => directCall(phoneBindings, { serviceNodeId: 'phone-bridge', input: {} }), errorCode('invalid_arguments'));
  for (const tool of descriptors) {
    assert.ok(Buffer.byteLength(JSON.stringify(tool)) < mcpDiscoveryResultBytes - 4096, tool.name);
    assert.ok(tool.inputSchema); assert.ok(tool.outputSchema);
    assert.ok(tool.title && tool.title.length <= 50, tool.name);
    assert.ok(tool.description && tool.description.length <= 320, tool.name);
    assert.ok(typeof tool._meta?.["ivy/serviceName"] === "string");
    clientSchemas.compile(tool.inputSchema);
    clientSchemas.compile(tool.outputSchema as AnySchema);
  }
  assert.ok(toolPresentation('hive', 'hive_event_topics'));
  const eventTopics = descriptors.find(tool => tool.name === 'hive_event_topics')!;
  assert.equal(eventTopics._meta?.['ivy/serviceName'], 'hive');
  schemas.validate(eventTopics.inputSchema, {});
  assert.equal(directCall(kernel.discovery.mcpCatalog('hive_event_topics'), {}).qualifiedName, 'hive.topics.list');
  const openWorld = new Set(['agent_manager_answer', 'agent_manager_invoke', 'agent_manager_interact',
    'phone_bridge_call', 'phone_bridge_screen', 'phone_bridge_bridge_screening', 'phone_bridge_accept']);
  for (const tool of descriptors.filter(tool => toolPresentation(String(tool._meta?.['ivy/serviceName']), tool.name)))
    assert.equal(tool.annotations?.openWorldHint, openWorld.has(tool.name), tool.name);
  for (const name of ['hive_schema_register', 'wiki_create', 'task_create', 'task_comment', 'task_upload_attachment', 'chat_bridge_send',
    'secretary_create_assignment', 'agent_manager_resolve_project', 'hive_authorize_package_upload'])
    assert.equal(descriptors.find(tool => tool.name === name)!.annotations?.destructiveHint, false, name);
  for (const name of ['hive_object_write', 'wiki_update', 'task_update', 'agent_manager_invoke', 'chat_bridge_create_main'])
    assert.equal(descriptors.find(tool => tool.name === name)!.annotations?.destructiveHint, true, name);
  for (const name of ['hive_schema_register', 'hive_object_write', 'wiki_create', 'wiki_update', 'task_create', 'task_upload_attachment', 'agent_manager_invoke'])
    assert.equal(descriptors.find(tool => tool.name === name)!.annotations?.idempotentHint, true, name);
  for (const name of ['agent_manager_interact', 'hive_authorize_package_upload'])
    assert.equal(descriptors.find(tool => tool.name === name)!.annotations?.idempotentHint, false, name);
  assert.deepEqual(descriptors.find(tool => tool.name === 'hive_status')!.inputSchema, { type: 'object', additionalProperties: false });
  for (const name of ['wiki_search', 'hive_object_read', 'task_create', 'chat_bridge_send', 'phone_bridge_call', 'secretary_create_assignment', 'secretary_update_assignment', 'agent_manager_status'])
    assert.ok(descriptors.some(tool => tool.name === name), name);
  assert.ok(!descriptors.some(tool => tool.name.includes('codex_thread')));
  const answer = descriptors.find(tool => tool.name === 'agent_manager_answer')!;
  assert.equal(answer.title, 'Answer Codex request');
  assert.equal(answer.description, 'Answer a pending Codex question or approval request.');
  assert.doesNotMatch(answer.description, /serviceNodeId|native Codex function/);
  assert.match(kernel.discovery.instructions().instructions, /Use a direct native Codex function first/);
  assert.match(kernel.discovery.instructions().instructions, /agent_manager_inputs/);
  assert.match(kernel.discovery.instructions().instructions, /phone_bridge_screen \(ivy_dev only\)/);
  assert.equal(descriptors.find(tool => tool.name === 'wiki_read')!.title, 'Read wiki page');
  assert.ok(call('discovery.list', { serviceName: 'agent-manager', query: 'thread read' }).items.some(item => item.name === 'codex.thread/read'));
  assert.equal(call('discovery.list', { serviceName: 'agent-manager', query: 'thread read' }, { ...client, transport: 'mcp' }).items.some(item => item.name === 'codex.thread/read'), false);
  assert.ok(Object.keys(coreMcpNames).every(name => operations[name]?.access === 'client' && operations[name]?.discoverable !== false && !/^(discovery|namespaces|tools)\./.test(name)));
  for (const name of ['task_save_plan', 'task_upload_attachment', 'chat_bridge_read', 'phone_bridge_diagnostics', 'agent_manager_discover', 'agent_manager_invoke', 'hive_package_catalog']) assert.ok(descriptors.some(tool => tool.name === name), name);
  for (const name of ['chat_bridge_receive', 'phone_bridge_read', 'phone_bridge_probe_loopback', 'phone_bridge_audio_setup', 'phone_bridge_codec_test', 'phone_bridge_inventory', 'phone_bridge_logs', 'phone_bridge_reconcile_archive',
    'agent_manager_catalog', 'agent_manager_frame_limits', 'agent_manager_environment_defaults', 'agent_manager_resolve_workspace']) assert.ok(!descriptors.some(tool => tool.name === name), name);
  const native = agentRegistry(catalog).namespaces[0]!.tools;
  const search = discoverNativeTools(catalog, native, { query: 'read task' });
  validateAgent('NativeDiscovery', search);
  assert.ok(search.items.some(item => item.method === 'thread/read'));
  assert.doesNotMatch(JSON.stringify(search), /inputSchema|outputSchema/);
  const exact = discoverNativeTools(catalog, native, { method: 'thread/read' });
  validateAgent('NativeDiscovery', exact);
  assert.equal(exact.nativeVersion, catalog.version);
  assert.equal(exact.catalogHash, hashJson(catalog));
  assert.equal(exact.items.length, 1);
  assert.equal(exact.items[0]!.expectedDefinitionHash, toolDefinitionHash(native.find(tool => tool.name === 'thread/read')!));
  assert.deepEqual(exact.items[0]!.inputSchema, catalog.clientRequests.find((method: { method: string }) => method.method === 'thread/read').inputSchema);
  const windowsExec = discoverNativeTools(catalog, native, { method: 'command/exec' }, 'win32');
  assert.equal(windowsExec.items[0]!.runtimeConstraints?.[0]?.condition, 'Windows sandbox is active');
  assert.deepEqual(windowsExec.items[0]!.runtimeConstraints?.[0]?.unsupportedParameters, ['outputBytesCap']);
  assert.deepEqual(windowsExec.items[0]!.inputSchema, catalog.clientRequests.find((method: { method: string }) => method.method === 'command/exec').inputSchema);
  assert.equal(discoverNativeTools(catalog, native, { method: 'command/exec' }, 'linux').items[0]!.runtimeConstraints, undefined);
  assert.throws(() => discoverNativeTools(catalog, native, { method: 'unknown' }), errorCode('native_method_unsupported'));
  assert.throws(() => discoverNativeTools(catalog, native, { method: 'thread/read', query: 'read' }), errorCode('invalid_arguments'));
});

test('public documentation, Secretary binding and schema summaries are reachable without hidden prerequisites', t => {
  const { kernel, call, connect } = fixture(t);
  const owner = connect('secretary', 'secretary');
  const registry = secretaryRegistry();
  registry.namespaces[0]!.guideMarkdown += ' ' + 'Complete documentation segment. '.repeat(80);
  call('registry.sync', { ...registry, contracts: [], requiredContracts: [] }, owner);
  call('service.heartbeat', { ready: true, diagnostics: [] }, owner);
  const instructions = call('discovery.instructions', {}).instructions;
  let cursor: string | undefined, combined = '', digestValue = '';
  do {
    const page = call('system.instructions', cursor ? { cursor } : {});
    assert.equal(page.startByte, Buffer.byteLength(combined));
    combined += page.text;
    digestValue = page.sha256;
    cursor = page.nextCursor ?? undefined;
    if (cursor) assert.equal(page.complete, false);
  } while (cursor);
  assert.equal(combined, instructions);
  assert.equal(digestValue, digest(combined));
  assert.equal(call('system.instructions', { view: 'examples' }).view, 'examples');
  assert.match(combined, /ivy_tool_schema/);
  assert.match(combined, /secretary_binding/);
  assert.throws(() => call('system.instructions', { cursor: digest('old') + ':8192' }), errorCode('invalid_cursor'));

  const binding = kernel.discovery.toolSchema({ name: 'secretary_binding' });
  const list = kernel.discovery.toolSchema({ name: 'secretary_list_assignments' });
  const listInput = ((list.inputSchema as Record<string, unknown>)['properties'] as Record<string, unknown>)['input'] as Record<string, unknown>;
  assert.ok((listInput['properties'] as Record<string, unknown>)['expectedScope']);
  const validators = new SchemaValidators();
  validators.validate(binding.inputSchema as Record<string, unknown>, { serviceNodeId: 'secretary', input: {} });
  assert.throws(() => validators.validate(list.inputSchema as Record<string, unknown>, { serviceNodeId: 'secretary', input: {} }));
  assert.equal(binding.complete, true);
  assert.equal(binding.schemaHash, hashJson({ inputSchema: binding.inputSchema, outputSchema: binding.outputSchema }));
  assert.throws(() => call('system.toolSchema', { name: 'secretary_status' }), errorCode('not_found'));

  const definition: Wire.DataContract = { key: 'sample/compact-list', version: '1.0.0', owner: { kind: 'agent' }, mediaType: 'application/json',
    retention: { objects: { mode: 'retain' }, revisions: { mode: 'all' } }, specMarkdown: 'Short purpose. Full guidance follows.', jsonSchema: { type: 'object', properties: { value: { type: 'string' } } } };
  call('contracts.register', { mutationId: operationId(kernel.store.runtimeEpoch), definition });
  const page = call('contracts.summaries', { key: definition.key });
  assert.equal(page.items.length, 1);
  assert.equal(page.items[0]!.description, definition.specMarkdown);
  assert.equal(page.items[0]!.hasJsonSchema, true);
  assert.equal('jsonSchema' in page.items[0]!, false);
  assert.deepEqual(call('contracts.get', { key: definition.key }).jsonSchema, definition.jsonSchema);
});

test('MCP combines provider risk hints and keeps conservative defaults for unknown tools', t => {
  const { kernel, connect, sync } = fixture(t);
  const published = { ...definition('status', 'agent'), discovery: { summary: 'Read provider-specific host status.', mcp: { name: 'agent_manager_status' } } };
  const a = connect('a'), b = connect('b');
  sync(a, [published]);
  const status = directDescriptors(kernel.discovery.mcpCatalog('agent_manager_status'))[0]!;
  assert.equal(status.annotations?.openWorldHint, false);
  assert.equal(status.description, 'Read provider-specific host status.');
  sync(b, [{ ...published, annotations: { readOnlyHint: true, openWorldHint: true } }]);
  assert.equal(directDescriptors(kernel.discovery.mcpCatalog('agent_manager_status'))[0]!.annotations?.openWorldHint, true);
  const custom = { ...definition('search', 'custom'), discovery: { mcp: { name: 'custom_search' } } };
  const other = connect('custom', 'custom'); sync(other, [custom]);
  assert.equal(directDescriptors(kernel.discovery.mcpCatalog('custom_search'))[0]!.annotations?.openWorldHint, true);
  sync(other, [{ ...custom, annotations: { readOnlyHint: true, openWorldHint: false } }]);
  assert.equal(directDescriptors(kernel.discovery.mcpCatalog('custom_search'))[0]!.annotations?.openWorldHint, false);
  assert.equal(toolPresentation('custom', 'agent_manager_status'), undefined);
  assert.equal(toolPresentation('constructor', 'name'), undefined);
  assert.equal(toolPresentation('hive', 'constructor'), undefined);
  const create = { ...definition('create', 'task-board'), discovery: { mcp: { name: 'task_create' } }, annotations: { readOnlyHint: false } };
  const board = connect('board', 'task-board'); sync(board, [create], 'task');
  assert.equal(directDescriptors(kernel.discovery.mcpCatalog('task_create'))[0]!.annotations?.destructiveHint, false);
  sync(board, [{ ...create, annotations: { readOnlyHint: false, destructiveHint: true } }], 'task');
  assert.equal(directDescriptors(kernel.discovery.mcpCatalog('task_create'))[0]!.annotations?.destructiveHint, true);
});

test('direct schemas keep provider-specific definitions and local references, with no provider substitution', t => {
  const { kernel, connect, sync, call, routing, caller } = fixture(t);
  const source = { ...definition('echo', 'sample'), discovery: { mcp: { name: 'custom_service_echo' } },
    inputSchema: { type: 'object', properties: { value: { $ref: '#/$defs/Value' }, $id: { type: 'string' } }, required: ['value', '$id'], additionalProperties: false,
      $defs: { Value: { type: 'string' } } }, outputSchema: { $ref: '#/$defs/Value', $defs: { Value: { type: 'string' } } } };
  const a = connect('a', 'custom-service'), b = connect('b', 'custom-service'); sync(a, [source]);
  sync(b, [{ ...source, inputSchema: { ...source.inputSchema, $defs: { Value: { type: 'number' } } }, outputSchema: { type: 'null' } }]);
  const bindings = kernel.discovery.mcpCatalog('custom_service_echo'), tools = directDescriptors(bindings), validator = new SchemaValidators();
  assert.equal(tools.length, 1);
  for (const [serviceNodeId, value] of [['a', 'text'], ['b', 42]] as const) {
    const args = { serviceNodeId, input: { value, $id: 'domain field' } };
    validator.validate(tools[0]!.inputSchema, args);
    const selected = directCall(bindings, args); assert.equal(selected.serviceNodeId, serviceNodeId);
    assert.deepEqual(selected.arguments, args.input);
    assert.equal(routing.prepare(caller, selected).serviceNodeId, serviceNodeId);
  }
  validator.validate(tools[0]!.outputSchema!, { result: 'valid' });
  validator.validate(tools[0]!.outputSchema!, { result: null });
  assert.throws(() => directCall(bindings, { input: { value: 'missing owner' } }), errorCode('invalid_arguments'));
  assert.throws(() => directCall(bindings, { serviceNodeId: 'b', input: { value: 'wrong schema', $id: 'field' } }), errorCode('invalid_arguments'));
  call('service.heartbeat', { ready: false, diagnostics: [] }, a);
  assert.throws(() => routing.prepare(caller, directCall(kernel.discovery.mcpCatalog('custom_service_echo'), { serviceNodeId: 'a', input: { value: 'text', $id: 'field' } })), errorCode('service_not_ready'));
  const privateTool = { ...source }; delete (privateTool as ToolDefinition).discovery; sync(a, [privateTool]);
  assert.equal(kernel.discovery.mcpCatalog('custom_service_echo').length, 1);
});

test('MCP shares definitions without crossing recursive provider contracts or rewriting literal data', t => {
  const { kernel, connect, sync } = fixture(t), validator = new SchemaValidators();
  const examples = [{ $id: 'literal identifier', $ref: '#/literal/reference' }];
  const schema = {
    type: 'object', properties: { root: { $ref: '#/$defs/Node~1~0' }, label: { $ref: '#/$defs/Label' } },
    required: ['root', 'label'], additionalProperties: false, examples,
    $defs: {
      'Node/~': { type: 'object', properties: {
        value: { $ref: '#/$defs/Value' }, child: { $ref: '#/$defs/Node~1~0' }, label: { $ref: '#/properties/label' },
      }, required: ['value'], additionalProperties: false },
      Value: { type: 'string' }, Label: { type: 'string', maxLength: 12 },
    },
  };
  const source = { ...definition('echo', 'sample'), discovery: { mcp: { name: 'recursive_echo' } }, inputSchema: schema, outputSchema: schema };
  const numeric = { ...schema, $defs: { ...schema.$defs, Value: { type: 'number' } } };
  const original = structuredClone(schema);
  sync(connect('a', 'recursive'), [source]);
  sync(connect('b', 'recursive'), [{ ...source, inputSchema: numeric, outputSchema: numeric }]);
  const bindings = kernel.discovery.mcpCatalog('recursive_echo'), tool = directDescriptors(bindings)[0]!;
  for (const [owner, value, wrong] of [['a', 'text', 42], ['b', 42, 'text']] as const) {
    const input = { root: { value, child: { value, label: 'child' } }, label: 'root' };
    validator.validate(tool.inputSchema, { serviceNodeId: owner, input });
    validator.validate(tool.outputSchema!, { result: input });
    assert.equal(directCall(bindings, { serviceNodeId: owner, input }).serviceNodeId, owner);
    assert.throws(() => validator.validate(tool.inputSchema, { serviceNodeId: owner, input: { ...input, root: { value, child: { value: wrong } } } }));
  }
  assert.throws(() => validator.validate(tool.outputSchema!, { result: { root: { value: 'text', child: { value: 42 } }, label: 'root' } }));
  assert.throws(() => validator.validate(tool.outputSchema!, { result: { root: { value: 'text', label: 42 }, label: 'root' } }));
  // Each contract retains its own Node and Value; the common Label is sent once.
  assert.equal(Object.keys(tool.inputSchema['$defs'] as object).length, 5);
  assert.equal(Object.keys(tool.outputSchema!['$defs'] as object).length, 5);
  const variants = (tool.inputSchema['allOf'] as { oneOf: { properties: { input: { examples: unknown } } }[] }[])[0]!.oneOf;
  for (const variant of variants) assert.deepEqual(variant.properties.input.examples, examples);
  assert.deepEqual(source.inputSchema, original, 'schema composition does not mutate the published source');
});

test('Wiki tools constrain page scope and retain object CAS, history and mutation replay', t => {
  const { call, kernel } = fixture(t);
  const ids = new Map<string, string>();
  const mutation = (nonce: string) => { if (!ids.has(nonce)) ids.set(nonce, operationId(kernel.store.runtimeEpoch)); return ids.get(nonce)!; };
  for (const definition of wikiManifest.dataContracts) call('contracts.register', { mutationId: mutation('contract-' + definition.key), definition: definition as Wire.DataContract });
  const root = call('wiki.create', { mutationId: mutation('root'), title: 'Knowledge', markdown: 'First version' });
  assert.deepEqual(call('wiki.create', { mutationId: mutation('root'), title: 'Knowledge', markdown: 'First version' }), root);
  const objectId = root.object.id;
  const child = call('wiki.create', { mutationId: mutation('child'), title: 'Context', parentId: objectId, markdown: 'Searchable knowledge' });
  assert.equal(call('wiki.list', { parentId: objectId }).items[0]!.objectId, child.object.id);
  assert.equal(call('wiki.search', { text: 'Searchable' }).items[0]!.id, child.object.id);
  const update = { mutationId: mutation('update'), objectId, expectedRevision: 1, markdown: 'Second version' };
  const saved = call('wiki.update', update); assert.equal(saved.revision.revision, 2);
  assert.deepEqual(call('wiki.update', update), saved);
  assert.throws(() => call('wiki.update', { ...update, mutationId: mutation('conflict') }), errorCode('revision_conflict'));
  assert.deepEqual(call('wiki.read', { objectId, revision: 1 }).content, { encoding: 'text', value: 'First version' });
  assert.equal(call('wiki.history', { objectId }).items.length, 2);
  const attachment = call('objects.write', { mutationId: mutation('attachment'), contractVersion: '1.0.0', references: {},
    create: { contractKey: 'wiki/attachment', name: 'file', parentId: objectId, ownerObjectId: objectId }, content: { encoding: 'base64', value: 'aGk=' } });
  const references = { attachment: { objectId: attachment.object.id, revision: 1 } };
  call('objects.write', { mutationId: mutation('attach'), objectId, expectedRevision: 2, contractVersion: '1.0.0', references,
    content: { encoding: 'text', value: 'Second version' } });
  const markdownEdit = { mutationId: mutation('markdown-only'), objectId, expectedRevision: 3, markdown: 'With attachment' };
  const withAttachment = call('wiki.update', markdownEdit);
  assert.deepEqual(withAttachment.revision.references, references);
  call('objects.write', { mutationId: mutation('detach'), objectId, expectedRevision: 4, contractVersion: '1.0.0', references: {},
    content: { encoding: 'text', value: 'Later edit' } });
  assert.deepEqual(call('wiki.update', markdownEdit), withAttachment, 'replay retains original references after later edits');
  assert.throws(() => call('wiki.read', { objectId: attachment.object.id }), errorCode('invalid_arguments'));
  assert.throws(() => call('wiki.create', { mutationId: mutation('bad-parent'), title: 'Invalid', parentId: attachment.object.id, markdown: '' }), errorCode('invalid_arguments'));
  call('wiki.archive', { objectId, archived: true, mutationId: mutation('archive') });
  assert.equal(call('wiki.search', { text: 'Searchable' }).items.length, 0);
  assert.equal(call('wiki.search', { text: 'Searchable', includeArchived: true }).items.length, 1);
});
