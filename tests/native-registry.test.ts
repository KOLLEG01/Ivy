import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { HiveKernel } from '../services/hive/src/kernel.js';
import type { ConnectionContext } from '../services/hive/src/kernel.js';
import { LiveRouting } from '../services/hive/src/live-routing.js';
import { agentRegistry } from '../services/agent-manager/src/registry.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { toolDefinitionHash } from '../packages/contracts/src/tool-definition.js';
import type { Agent, Operation } from '../packages/contracts/src/generated.js';

test('atomic caller fence rejects an old UI intent before tool lookup or operation absence under a new principal', t => {
  const oldDigest = digest('old-account'), nextDigest = digest('new-account');
  const kernel = new HiveKernel({ filename: ':memory:', publicBaseUrl: 'http://127.0.0.1/ivy', version: 'test', buildId: digest('test'),
    credentials: [{ principalId: 'original', digest: oldDigest }, { principalId: 'next', digest: nextDigest }] });
  t.after(() => kernel.close());
  const old = { transport: 'http' as const, credentialDigest: oldDigest, principalId: 'original' }, next = { ...old, credentialDigest: nextDigest, principalId: 'next' };
  const routing = new LiveRouting(false);
  const status = kernel.execute(old, { jsonrpc: '2.0', id: 'status', method: 'system.status', params: {} });
  assert.equal(status.kind, 'result'); if (status.kind === 'result') assert.equal((status.value as Operation.Status).callerPrincipalId, 'original');
  for (const qualifiedName of ['codex.thread/start', 'agent.operation']) {
    assert.throws(() => routing.prepare({ ...next, principalId: 'next' }, {
      qualifiedName, serviceNodeId: 'original-owner', expectedDefinitionHash: digest('binding'), expectedCallerPrincipalId: 'original',
      operationId: 'original-intent', arguments: qualifiedName === 'agent.operation' ? { operationId: 'original-intent' } : {},
    }), { code: 'caller_changed' });
  }
});

test('the complete supported catalog register in the actual Hive kernel with exact paged bindings and native null dispatch', { timeout: 90_000 }, t => {
  const credential = digest('native-catalog-fixture'), kernel = new HiveKernel({ filename: ':memory:', publicBaseUrl: 'https://native.test/ivy', version: 'test', buildId: digest('native-catalog-test'),
    credentials: [{ principalId: 'caller', digest: credential }] });
  const routing = new LiveRouting(false);
  t.after(() => kernel.close());
  const client: ConnectionContext = { transport: 'http', credentialDigest: credential, principalId: 'caller' }, socket: ConnectionContext = { ...client, transport: 'ws' };
  const call = <T>(context: ConnectionContext, method: string, params: unknown): T => {
    const result = kernel.execute(context, { jsonrpc: '2.0', id: randomUUID(), method, params }); assert.equal(result.kind, 'result');
    return (result as { kind: 'result'; value: T }).value;
  };
  for (const version of ['0.154.0']) {
    const catalog = JSON.parse(readFileSync('specs/native/codex-' + version + '/catalog.json', 'utf8')) as Agent.Catalog;
    const registry = agentRegistry(catalog), node = 'native-' + version;
    const connected = call<{ generation: number }>(socket, 'service.connect', { serviceNodeId: node, serviceName: 'agent-manager', hostId: node, nativeVersion: version, version: '0.1.0', buildId: digest('native-test-' + version), hiveProtocol: 1 });
    const context = { ...socket, serviceNodeId: node, generation: connected.generation };
    call(context, 'registry.sync', registry); call(context, 'service.heartbeat', { ready: true, diagnostics: [] });
    const liveCatalog = kernel.registry.liveCatalog(node); routing.update({ node: kernel.registry.node(node), generation: connected.generation, ...(liveCatalog ? { catalog: liveCatalog } : {}) });
    const found: Operation.ToolBinding[] = []; let cursor: string | null = null;
    do {
      const page: Operation.ToolsListResult = call(client, 'tools.list', { namespace: 'codex', serviceNodeId: node, limit: 17, ...(cursor ? { cursor } : {}) });
      found.push(...page.items); cursor = page.nextCursor;
    } while (cursor);
    assert.equal(found.length, catalog.clientRequests.length);
    const actual = new Map(found.map(value => [value.qualifiedName, value]));
    for (const definition of registry.namespaces[0]!.tools) {
      const binding = actual.get('codex.' + definition.name)!; assert.ok(binding);
      assert.deepEqual(binding.definition, definition); assert.equal(binding.definitionHash, toolDefinitionHash(definition));
    }
    const management = call<Operation.ToolsListResult>(client, 'tools.list', { namespace: 'agent', serviceNodeId: node, limit: 25 }); assert.equal(management.items.length, 22);
    assert.deepEqual(management.items.map(value => value.qualifiedName).sort(), ['answer', 'capabilities', 'catalog', 'configureCapabilities', 'discover', 'environmentDefaults', 'frameLimits', 'inputDefinition', 'inputs', 'interact', 'interaction', 'invoke', 'listDirectories', 'notifications', 'operation', 'prevent', 'projects', 'read', 'resolveProject', 'resolveWorkspace', 'stageFile', 'status'].map(name => 'agent.' + name).sort());
    assert.equal(management.items.find(value => value.qualifiedName === 'agent.frameLimits')!.definition.annotations?.readOnlyHint, true);
    assert.equal(management.items.find(value => value.qualifiedName === 'agent.catalog')!.definition.annotations?.readOnlyHint, true);
    const read = management.items.find(value => value.qualifiedName === 'agent.read')!;
    assert.equal(read.definition.annotations?.readOnlyHint, true);
    assert.equal(read.definition.annotations?.idempotentHint, true);
    assert.equal(management.items.find(value => value.qualifiedName === 'agent.interact')!.definition.annotations?.idempotentHint, false);
    assert.equal(management.items.find(value => value.qualifiedName === 'agent.prevent')!.definition.annotations?.readOnlyHint, false);
    assert.equal(management.items.find(value => value.qualifiedName === 'agent.stageFile')!.definition.annotations?.readOnlyHint, false);
    assert.equal(actual.get('codex.thread/list')!.definition.annotations?.readOnlyHint, true);
    for (const method of ['thread/goal/get', 'config/read', 'permissionProfile/list', 'fs/readFile']) {
      const binding = actual.get('codex.' + method);
      if (binding) assert.equal(binding.definition.annotations?.readOnlyHint, true, method + ' must not fill the action journal');
    }
    assert.equal(actual.get('codex.thread/start')!.definition.annotations?.readOnlyHint, undefined);
    const nil = actual.get('codex.account/rateLimits/read')!;
    const dispatch = routing.prepare({ ...client, principalId: 'caller' }, { qualifiedName: nil.qualifiedName, serviceNodeId: node,
      expectedDefinitionHash: nil.definitionHash, operationId: 'native-null-' + version, arguments: null });
    assert.equal(dispatch.arguments, null); assert.equal(dispatch.serviceNodeId, node); assert.equal(dispatch.generation, connected.generation);
    assert.equal(dispatch.operationId, 'native-null-' + version);
  }
});

test('searchable native inventory advances its immutable schema version', t => {
  const credential = digest('native-inventory-version'), kernel = new HiveKernel({ filename: ':memory:', publicBaseUrl: 'https://native.test/ivy', version: 'test', buildId: digest('native-inventory-version'),
    credentials: [{ principalId: 'agent', digest: credential }] });
  t.after(() => kernel.close());
  const socket: ConnectionContext = { transport: 'ws', credentialDigest: credential, principalId: 'agent' };
  const call = <T>(context: ConnectionContext, method: string, params: unknown): T => {
    const result = kernel.execute(context, { jsonrpc: '2.0', id: randomUUID(), method, params }); assert.equal(result.kind, 'result');
    return (result as { kind: 'result'; value: T }).value;
  };
  const catalog = JSON.parse(readFileSync('specs/native/codex-0.154.0/catalog.json', 'utf8')) as Agent.Catalog;
  const current = agentRegistry(catalog), legacy = structuredClone(current);
  for (const kind of legacy.namespaces[0]!.inventoryKinds) {
    kind.version = '1.0.0'; delete kind.searchPointers; delete kind.archivedPointer; delete kind.recencyPointer;
  }
  assert.deepEqual(current.namespaces[0]!.inventoryKinds.map(kind => kind.version), ['1.1.0', '2.0.0']);
  const connected = call<{ generation: number }>(socket, 'service.connect', { serviceNodeId: 'agent', serviceName: 'agent-manager', hostId: 'host', nativeVersion: '0.154.0', version: '0.2.12', buildId: digest('agent'), hiveProtocol: 1 });
  const context = { ...socket, serviceNodeId: 'agent', generation: connected.generation };
  call(context, 'registry.sync', legacy);
  assert.doesNotThrow(() => call(context, 'registry.sync', current));
});
