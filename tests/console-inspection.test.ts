import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import { digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { OperationName, Params as OperationParams, Result as OperationResult, Operation } from '../packages/contracts/src/generated.js';
import { HiveKernel } from '../services/hive/src/kernel.js';
import type { ConnectionContext } from '../services/hive/src/kernel.js';
import type { RegistrySync, ToolDefinition } from '../services/hive/src/registry.js';
import { operationId } from '../packages/contracts/src/operation-id.js';

const credential = { principalId: 'inspection-fixture', digest: digest('synthetic-inspection-fixture') };
const client: ConnectionContext = { credentialDigest: credential.digest, principalId: credential.principalId, transport: 'http' };
const socket: ConnectionContext = { ...client, transport: 'ws' };
const code = (expected: string) => (error: unknown) => error instanceof IvyError && error.code === expected;
const tool: ToolDefinition = { namespace: 'fixture', name: 'read', description: 'Read an isolated fixture.', interfaceVersion: '1.0.0', inputSchema: { type: 'object' }, outputSchema: { type: 'string' } };
const catalog = (requiredContracts: RegistrySync['requiredContracts'] = [], definition = tool): RegistrySync => ({
  namespaces: [{ namespace: 'fixture', description: 'Isolated fixture', guideMarkdown: '', tools: [definition], topics: [], inventoryKinds: [] }], contracts: [], requiredContracts,
});
function fixture(t: TestContext) {
  const kernel = new HiveKernel({ filename: ':memory:', publicBaseUrl: 'https://fixture.test/ivy', version: '0.1.0-test', buildId: digest('fixture'), credentials: [credential] });
  t.after(() => kernel.close());
  const mutationIds = new Map<string, string>(), issuedAt = Date.now();
  const normalize = <T,>(params: T): T => {
    if (!params || typeof params !== 'object' || typeof (params as { mutationId?: unknown }).mutationId !== 'string') return params;
    const source = (params as unknown as { mutationId: string }).mutationId; let id = mutationIds.get(source);
    if (!id) { id = operationId(kernel.store.runtimeEpoch, issuedAt, digest(source).slice(7)); mutationIds.set(source, id); }
    return { ...params, mutationId: id };
  };
  const call = <M extends OperationName>(method: M, params: OperationParams<M>, context = client): OperationResult<M> => {
    const result = kernel.execute(context, { jsonrpc: '2.0', id: randomUUID(), method, params: normalize(params) });
    assert.equal(result.kind, 'result'); if (result.kind !== 'result') throw new Error('Unexpected provider call.');
    return result.value as OperationResult<M>;
  };
  const connect = (serviceNodeId = 'fixture-node') => ({ ...socket, ...call('service.connect', { serviceNodeId, hostId: 'fixture-host', serviceName: 'fixture', version: '1', buildId: digest(serviceNodeId), hiveProtocol: 1 }, socket) });
  const register = (key: string, version = '1.0.0', mediaType = 'text/markdown') => call('contracts.register', { mutationId: randomUUID(), definition: { key, version, mediaType, owner: { kind: 'agent' }, retention: { objects: { mode: 'retain' }, revisions: { mode: 'all' } }, specMarkdown: 'Isolated fixture only.' } });
  const text = (key: string, name: string, parentId: string | null = null) => call('objects.write', { mutationId: randomUUID(), contractVersion: '1.0.0', references: {}, create: { contractKey: key, name, parentId, ownerObjectId: null }, content: { encoding: 'text', value: '<h1>Immutable fixture</h1>' } });
  const stage = (uiId: string, releaseId: string) => {
    const bytes = Buffer.from('<h1>Immutable fixture</h1>');
    const asset = { path: 'index.html', mediaType: 'text/html', contentHash: digest(bytes), byteLength: bytes.length };
    kernel.uiFiles.stage(uiId, releaseId, asset, bytes);
    return asset;
  };
  return { kernel, call, connect, register, text, stage };
}

test('node-specific supported contracts distinguish no catalog, empty declarations, retained state and fenced pagination', t => {
  const { kernel, call, connect, register } = fixture(t);
  for (const key of ['fixture/a', 'fixture/b', 'fixture/global-only']) register(key);
  const owner = connect();
  const fresh = call('serviceNodes.contracts', { serviceNodeId: owner.serviceNodeId });
  assert.equal(fresh.hasCatalog, false); assert.equal(fresh.capturedAt, null); assert.deepEqual(fresh.items, []);
  call('registry.sync', catalog(), owner);
  const empty = call('serviceNodes.contracts', { serviceNodeId: owner.serviceNodeId });
  assert.equal(empty.hasCatalog, true); assert.ok(empty.capturedAt); assert.deepEqual(empty.items, []);
  const required = ['fixture/b', 'fixture/a'].map(key => ({ key, readVersions: ['1.0.0'], writeVersions: [] }));
  call('registry.sync', catalog(required), owner); call('service.heartbeat', { ready: true, diagnostics: [] }, owner);
  const first = call('serviceNodes.contracts', { serviceNodeId: owner.serviceNodeId, limit: 1 });
  assert.equal(first.provider.eligible, true); assert.deepEqual(first.items, [required[0]]); assert.ok(first.nextCursor);
  const second = call('serviceNodes.contracts', { serviceNodeId: owner.serviceNodeId, limit: 1, cursor: first.nextCursor! });
  assert.deepEqual(second.items, [required[1]]); assert.equal(second.nextCursor, null);
  assert.deepEqual(call('serviceNodes.contracts', { serviceNodeId: owner.serviceNodeId, key: 'fixture/global-only' }).items, []);
  kernel.registry.disconnect(owner.serviceNodeId);
  const offline = call('serviceNodes.contracts', { serviceNodeId: owner.serviceNodeId });
  assert.equal(offline.hasCatalog, true); assert.equal(offline.provider.eligible, false); assert.deepEqual(offline.items, required);
  const renewed = connect(); call('registry.sync', catalog(required.slice(1)), renewed);
  assert.throws(() => call('serviceNodes.contracts', { serviceNodeId: renewed.serviceNodeId, limit: 1, cursor: first.nextCursor! }), code('invalid_cursor'));
});

test('ui inspection distinguishes incompatible, unavailable, missing and corrupt assets while old immutable content remains readable', t => {
  const { kernel, call, connect, register, stage } = fixture(t);
  register('fixture/html', '1.0.0', 'text/html'); register('fixture/data');
  const owner = connect(); call('registry.sync', catalog(), owner); call('service.heartbeat', { ready: true, diagnostics: [] }, owner);
  const asset = stage('fixture', 'r1');
  const release: Operation.UiRelease = { releaseId: 'r1', entryPath: 'index.html', requirements: { hiveProtocol: 1,
    contracts: [{ key: 'fixture/data', readVersions: ['1.0.0'], writeVersions: [] }], services: [{ serviceName: 'fixture', namespace: 'fixture', interfaceVersion: '1.0.0' }] },
    assets: [asset] };
  const metadata: Operation.UiMetadata = { uiId: 'fixture', displayName: 'Fixture', description: 'Isolated inspection', iconKey: 'ui' };
  call('uis.deploy', { metadata, release, mutationId: 'deploy-r1', expectedReleaseId: null });
  const retainedNode = JSON.parse(String(kernel.store.get('SELECT status_json FROM service_nodes WHERE id=?', owner.serviceNodeId)!['status_json']));
  retainedNode.lastContactAt = '2000-01-01T00:00:00.000Z';
  kernel.store.run('UPDATE service_nodes SET status_json=? WHERE id=?', JSON.stringify(retainedNode), owner.serviceNodeId);
  const ready = call('uis.inspect', { uiId: 'fixture' }); assert.equal(ready.status, 'ready'); assert.deepEqual(ready.issues, []);
  assert.deepEqual(kernel.uis.entry('fixture'), { releaseId: 'r1', entryPath: 'index.html' });
  const compact = call('uis.catalog', {}).items[0]!;
  assert.equal(compact.releaseCount, 1); assert.equal(compact.currentReleaseId, 'r1'); assert.equal('current' in compact, false); assert.equal('releases' in compact, false);
  assert.equal(call('uis.inspect', { uiId: 'fixture', releaseId: 'missing' }).status, 'missing');
  kernel.registry.disconnect(owner.serviceNodeId);
  assert.equal(call('uis.inspect', { uiId: 'fixture' }).status, 'unavailable');
  assert.throws(() => kernel.uis.entry('fixture'), code('service_not_ready'));
  assert.equal(kernel.uis.asset('fixture', 'r1', 'index.html').asset.contentHash, asset.contentHash);
  const renewed = connect(); call('registry.sync', catalog([], { ...tool, interfaceVersion: '2.0.0', description: 'Changed exact interaction.' }), renewed);
  call('service.heartbeat', { ready: true, diagnostics: [] }, renewed);
  const changed = call('uis.inspect', { uiId: 'fixture' });
  assert.equal(changed.status, 'incompatible'); assert.equal(changed.issues[0]?.code, 'service_interface_changed');
  assert.throws(() => kernel.uis.entry('fixture'), code('service_interface_changed'));
  call('registry.sync', catalog(), renewed); call('service.heartbeat', { ready: true, diagnostics: [] }, renewed);
  register('fixture/data', '2.0.0');
  call('objects.write', { mutationId: 'data-v2', contractVersion: '2.0.0', references: {}, create: { contractKey: 'fixture/data', name: 'Current V2', parentId: null, ownerObjectId: null }, content: { encoding: 'text', value: 'not readable by r1' } });
  assert.equal(call('uis.inspect', { uiId: 'fixture' }).issues[0]?.code, 'contract_version_conflict');
  assert.throws(() => call('uis.rollback', { uiId: 'fixture', releaseId: 'r1', expectedReleaseId: 'r1', mutationId: 'rollback-incompatible' }), code('contract_version_conflict'));
  assert.deepEqual(kernel.uis.entry('fixture'), { releaseId: 'r1', entryPath: 'index.html' });
  const inspection = t.mock.method(kernel.uis, 'inspect', () => { throw new Error('Normal UI reads must not inspect releases.'); });
  const requirements = t.mock.method(kernel.registry, 'checkRequirements', () => { throw new Error('Normal UI reads must not inspect stored contracts.'); });
  const files = t.mock.method(kernel.uiFiles, 'available', () => { throw new Error('Normal UI reads must not enumerate release files.'); });
  assert.equal(call('uis.catalog', { limit: 1 }).items[0]?.currentReleaseId, 'r1');
  assert.equal(kernel.uis.currentAsset('fixture').asset.path, 'index.html');
  inspection.mock.restore(); requirements.mock.restore(); files.mock.restore();
  // Deliberate storage fault injection: the private file no longer matches its release.
  writeFileSync(kernel.uiFiles.path('fixture', 'r1', 'index.html'), 'changed');
  assert.equal(call('uis.inspect', { uiId: 'fixture' }).status, 'invalid');
  kernel.store.run('UPDATE apps SET current_release_id=? WHERE app_id=?', 'lost-release', 'fixture');
  assert.equal(call('uis.catalog', {}).items[0]!.currentReleaseId, 'lost-release');
  assert.throws(() => kernel.uis.entry('fixture'), code('not_found'));
  kernel.store.run('UPDATE apps SET current_release_id=NULL WHERE app_id=?', 'fixture');
  assert.equal(call('uis.inspect', { uiId: 'fixture' }).status, 'unselected');
  assert.equal(call('uis.inspect', { uiId: 'fixture', releaseId: 'r1' }).status, 'invalid');
});

test('retained release and ui pages are bounded and bind ui identity', t => {
  const { kernel, call, register, stage } = fixture(t); register('fixture/html', '1.0.0', 'text/html');
  for (const uiId of ['one', 'two']) for (const releaseId of ['r1', 'r2', 'r3']) {
    const asset = stage(uiId, releaseId);
    call('uis.deploy', { mutationId: uiId + releaseId, metadata: { uiId, displayName: uiId, description: 'Fixture', iconKey: 'ui' },
      expectedReleaseId: releaseId === 'r1' ? null : 'r' + (Number(releaseId[1]) - 1),
      release: { releaseId, entryPath: 'index.html', requirements: { hiveProtocol: 1, contracts: [], services: [] }, assets: [asset] } });
  }
  const first = call('uis.releases', { uiId: 'one', limit: 1 }); assert.equal(first.items.length, 1); assert.ok(first.nextCursor);
  const second = call('uis.releases', { uiId: 'one', limit: 1, cursor: first.nextCursor! });
  assert.deepEqual(second.items.map(item => item.releaseId), ['r3']); assert.equal(second.nextCursor, null);
  assert.throws(() => call('uis.releases', { uiId: 'two', limit: 2, cursor: first.nextCursor! }), code('invalid_cursor'));
  const page = call('uis.catalog', { limit: 1 }); assert.equal(page.items[0]?.metadata.uiId, 'one');
  assert.equal(call('uis.catalog', { limit: 1, cursor: page.nextCursor! }).items[0]?.metadata.uiId, 'two');
  for (const id of ['a', 'b', 'p', 'z']) kernel.store.run('INSERT INTO apps VALUES (?,?,NULL,NULL)', id, '{}');
  const filtered = call('uis.catalog', { limit: 1 });
  assert.equal(filtered.items[0]?.metadata.uiId, 'one'); assert.ok(filtered.nextCursor);
  const last = call('uis.catalog', { limit: 1, cursor: filtered.nextCursor! });
  assert.equal(last.items[0]?.metadata.uiId, 'two'); assert.equal(last.nextCursor, null);
});

test('obsolete ui metadata stays replaceable but is absent from application catalogs', t => {
  const { kernel, call } = fixture(t);
  const releaseId = 'legacy-chat-release';
  kernel.store.run('INSERT INTO apps VALUES (?,?,?,NULL)', 'chat-ui', JSON.stringify({ appId: 'chat-ui', displayName: 'Chat', description: 'Retired browser chat.', iconKey: 'message-circle' }), releaseId);
  kernel.store.run('INSERT INTO app_releases VALUES (?,?,?)', 'chat-ui', releaseId,
    JSON.stringify({ releaseId, entryPath: 'index.html', requirements: { hiveProtocol: 1, contracts: [], tools: [] }, assets: [] }));
  assert.deepEqual(call('uis.catalog', {}).items, []);
  assert.deepEqual(call('uis.list', {}).items, []);
  assert.equal(call('uis.inspect', { uiId: 'chat-ui' }).currentReleaseId, releaseId);
  assert.equal(call('uis.inspect', { uiId: 'chat-ui' }).status, 'invalid');
  assert.throws(() => call('uis.get', { uiId: 'chat-ui' }), code('not_found'));
});

test('typed query exposes inherited archive as a boolean with correct predicates and cursor ordering', t => {
  const { call, register, text } = fixture(t); register('fixture/text');
  const parent = text('fixture/text', 'parent'), child = text('fixture/text', 'child', parent.object.id), live = text('fixture/text', 'live');
  call('objects.archive', { mutationId: 'archive-parent', objectId: parent.object.id, archived: true });
  const params: Operation.ObjectsQueryParams = { contractKey: 'fixture/text', includeArchived: true, select: ['object.archivedAt', 'object.effectivelyArchived'], where: { op: 'eq', field: 'object.effectivelyArchived', value: true } };
  const result = call('objects.query', params); assert.equal(result.items.length, 2);
  assert.equal(result.items.find(item => item.objectId === child.object.id)?.values['object.archivedAt'], null);
  assert.ok(result.items.every(item => item.values['object.effectivelyArchived'] === true));
  assert.throws(() => call('objects.query', { ...params, where: { op: 'eq', field: 'object.effectivelyArchived', value: 1 } }), code('invalid_arguments'));
  const ordering: Operation.ObjectsQueryParams = { contractKey: 'fixture/text', includeArchived: true, select: ['object.effectivelyArchived'], orderBy: [{ field: 'object.effectivelyArchived', direction: 'asc' }], limit: 1 };
  const first = call('objects.query', ordering); assert.equal(first.items[0]?.objectId, live.object.id); assert.equal(first.items[0]?.values['object.effectivelyArchived'], false);
  const next = call('objects.query', { ...ordering, cursor: first.nextCursor! }); assert.equal(next.items[0]?.values['object.effectivelyArchived'], true);
  assert.equal(call('objects.query', { ...params, includeArchived: false }).items.length, 0);
});

test('inspection bounds many independent dependency failures and reports explicit truncation', t => {
  const { kernel, call, connect, register, stage } = fixture(t); register('fixture/html', '1.0.0', 'text/html');
  const owner = connect(), declared = catalog();
  call('registry.sync', declared, owner); call('service.heartbeat', { ready: true, diagnostics: [] }, owner);
  const asset = stage('many', 'r1');
  call('uis.deploy', { mutationId: 'many-dependencies', expectedReleaseId: null, metadata: { uiId: 'many', displayName: 'Many', description: 'Bounded dependency fixture', iconKey: 'ui' },
    release: { releaseId: 'r1', entryPath: 'index.html', requirements: { hiveProtocol: 1, contracts: [], services: Array.from({ length: 33 }, (_, index) => ({ serviceName: 'missing-' + index, namespace: 'fixture', interfaceVersion: '1.0.0' })) },
      assets: [asset] } });
  kernel.registry.disconnect(owner.serviceNodeId);
  const inspection = call('uis.inspect', { uiId: 'many' }); assert.equal(inspection.status, 'unavailable');
  assert.equal(inspection.issues.length, 32); assert.equal(inspection.issuesTruncated, true);
  assert.equal(new Set(inspection.issues.map(value => value.resource.serviceName)).size, 32);
});
