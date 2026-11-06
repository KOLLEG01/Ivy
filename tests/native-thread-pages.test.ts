import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { nativeThreadPage } from '../packages/ui-client/src/native-thread-pages.js';
import { NativeContract } from '../packages/contracts/src/native-contract.js';
import { SchemaValidators } from '../packages/contracts/src/schema.js';
import { hashJson } from '../packages/contracts/src/canonical.js';
import type { Schema } from '../packages/contracts/src/types.js';
import type { Agent, RpcClient, Wire } from '../packages/sdk/src/client.js';

const backend: Agent.BackendIdentity = { backendId: 'secondary', provider: 'claude', accountId: 'personal' };
const owner = { serviceNodeId: 'host.agent-manager', backend };
const native = JSON.parse(readFileSync('specs/native/codex-0.163.0/catalog.json', 'utf8')) as Agent.Catalog;
const contract = new NativeContract(native, backend);

function fixture(catalog = native) {
  const selected = catalog === native ? contract : new NativeContract(catalog, backend);
  const visits: { method: string; params: Record<string, Wire.Json> }[] = [];
  const state = { epoch: 'original-epoch', nativeVersion: catalog.version, nativeExecutableHash: catalog.nativeExecutableHash,
    catalogHash: hashJson(catalog), hostId: 'host', ready: true, response: undefined as Wire.Json | undefined,
    afterRead: () => {}, supported: true };
  const schemas = new SchemaValidators();
  const binding = (name: string) => ({ serviceNodeId: owner.serviceNodeId, qualifiedName: name, definitionHash: hashJson({ name }),
    definition: name === 'secondary.thread/list' ? { ...selected.definition('thread/list'),
      ...(!state.supported ? { inputSchema: { type: 'object', properties: {} } } : {}) } : {
      namespace: 'agent', name: 'status', interfaceVersion: '1.0.0', description: 'Inert status fixture.',
      inputSchema: true, outputSchema: true, annotations: { readOnlyHint: true },
    } });
  const client = { async request(method: string, params: Record<string, Wire.Json>) {
    visits.push({ method, params: structuredClone(params) });
    if (method === 'serviceNodes.get') return { serviceNodeId: owner.serviceNodeId, serviceName: 'agent-manager', connected: true, synced: true, ready: true, desiredEnabled: true };
    if (method === 'system.status') return { runtimeEpoch: '11111111-1111-4111-8111-111111111111' };
    if (method === 'tools.list') return { provider: { node: { serviceNodeId: params['serviceNodeId'] } },
      items: [binding(params['namespace'] + '.' + params['namePrefix'])], nextCursor: null };
    if (method === 'discovery.describe') return { provider: { serviceNodeId: params['serviceNodeId'] }, items: (params['tools'] as string[]).map(binding) };
    assert.equal(method, 'tools.call');
    const args = params['arguments'] as Record<string, Wire.Json>;
    if (params['qualifiedName'] === 'agent.status') return { backend, serviceNodeId: owner.serviceNodeId, hostId: state.hostId,
      nativeVersion: state.nativeVersion, nativeExecutableHash: state.nativeExecutableHash, catalogHash: state.catalogHash,
      epoch: state.epoch, state: state.ready ? 'ready' : 'failed', pid: null, observedAt: '2026-01-01T00:00:00.000Z', code: null,
      initialized: null, pendingInputs: 0, operations: { retained: 0, maximum: 100, bytes: 0, maximumBytes: 1048576 }, observedMethods: [] };
    assert.equal(params['qualifiedName'], 'secondary.thread/list');
    schemas.validate(selected.inputSchema('thread/list') as Schema, args);
    const offset = args['cursor'] === undefined ? 0 : Number((args['cursor'] as string).split(':').at(-1));
    const total = args['archived'] ? 3 : 5101;
    const result = state.response ?? { data: Array.from({ length: Math.min(50, total - offset) }, (_, i) => ({ id: 'thread-' + (offset + i),
      name: 'Original ' + (offset + i), preview: 'p'.repeat(4096), cwd: '/original/' + 'w'.repeat(4096),
      opaqueProviderBody: 'never-retained-' + i })), nextCursor: offset + 50 < total ? 'opaque:\ud800:' + (offset + 50) : null };
    state.afterRead();
    return result;
  } } as unknown as RpcClient;
  const reads = () => visits.filter(v => v.method === 'tools.call' && v.params['qualifiedName'] === 'secondary.thread/list');
  return { client, state, visits, reads };
}

test('native history requests one page at a time beyond five thousand saved tasks', async () => {
  const f = fixture();
  let cursor: string | undefined, count = 0;
  do {
    const before = f.reads().length;
    const page = await nativeThreadPage(f.client, owner, { archived: false, cursor });
    assert.equal(f.reads().length, before + 1);
    assert(page.items.length <= 50);
    for (const item of page.items) {
      assert.equal(item.id, 'thread-' + count++);
      assert.equal(item.preview.length, 2048); assert.equal(item.cwd.length, 2048);
      assert.deepEqual(Object.keys(item).sort(), ['cwd', 'id', 'name', 'preview']);
    }
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
  assert.equal(count, 5101); assert.equal(f.reads().length, 103);
  const first = f.reads()[0]!.params['arguments'] as Record<string, Wire.Json>;
  assert.equal(first['useStateDbOnly'], true); assert.equal(first['limit'], 50);
  assert.equal(first['sortKey'], 'recency_at'); assert.deepEqual(first['modelProviders'], []);
  assert.deepEqual(first['sourceKinds'], ['cli', 'vscode', 'exec', 'appServer', 'subAgent', 'subAgentReview',
    'subAgentCompact', 'subAgentThreadSpawn', 'subAgentOther', 'unknown']);
  assert.equal((f.reads()[1]!.params['arguments'] as Record<string, Wire.Json>)['cursor'], 'opaque:\ud800:50');
  const archive = await nativeThreadPage(f.client, owner, { archived: true });
  assert.equal(archive.items.length, 3); assert.equal(archive.nextCursor, null);
  const claudeCatalog = JSON.parse(readFileSync('specs/native/codex-0.142.3/catalog.json', 'utf8')) as Agent.Catalog;
  const claude = fixture(claudeCatalog);
  assert.equal((await nativeThreadPage(claude.client, owner, { archived: false })).items.length, 50);
  assert.equal(claude.reads().length, 1, 'The actual Claude catalog must admit its own native page parameters.');
});

test('history continuations reject owner, generation and filter changes before forwarding a native cursor', async () => {
  for (const field of ['epoch', 'nativeVersion', 'nativeExecutableHash', 'catalogHash', 'hostId', 'filter', 'account', 'service'] as const) {
    const f = fixture(), first = await nativeThreadPage(f.client, owner, { archived: false });
    const cursor = first.nextCursor!;
    if (field === 'epoch') f.state.epoch = 'new-epoch';
    if (field === 'nativeVersion') f.state.nativeVersion = '0.159.2';
    if (field === 'nativeExecutableHash') f.state.nativeExecutableHash = 'sha256:' + '2'.repeat(64);
    if (field === 'catalogHash') f.state.catalogHash = 'sha256:' + '2'.repeat(64);
    if (field === 'hostId') f.state.hostId = 'other-host';
    const selected = field === 'account' ? { ...owner, backend: { ...backend, accountId: 'other' } } :
      field === 'service' ? { ...owner, serviceNodeId: 'other.agent-manager' } : owner;
    await assert.rejects(nativeThreadPage(f.client, selected, { archived: field === 'filter', cursor }),
      (error: unknown) => ['invalid_cursor', 'native_owner_mismatch'].includes((error as { code: string }).code), field);
    assert.equal(f.reads().length, 1, field);
  }
});

test('hidden and empty native pages preserve continuation without fetching another page', async () => {
  const f = fixture();
  for (const data of [[], [{ id: 'ephemeral', ephemeral: true }, { id: 'guard', source: { subAgent: { other: 'guardian' } } }]]) {
    f.state.response = { data, nextCursor: 'original-empty-page-cursor' };
    const before = f.reads().length;
    const page = await nativeThreadPage(f.client, owner, { archived: false });
    assert.deepEqual(page.items, []); assert.equal(typeof page.nextCursor, 'string');
    assert.equal(f.reads().length, before + 1);
  }
});

test('malformed history pages and unsupported catalogs cannot become a complete or usable page', async () => {
  for (const response of [{ data: [] }, { data: null, nextCursor: null }, { data: [], nextCursor: '' },
    { data: [], nextCursor: 'x'.repeat(8193) }, { data: [{ id: 'a' }, { id: 'a' }], nextCursor: null },
    { data: [{ id: '' }], nextCursor: null }, { data: [{ id: 'x'.repeat(257) }], nextCursor: null },
    { data: Array.from({ length: 51 }, (_, n) => ({ id: String(n) })), nextCursor: null }]) {
    const f = fixture(); f.state.response = response;
    await assert.rejects(nativeThreadPage(f.client, owner, { archived: false }), { code: 'native_inventory_invalid' });
  }
  const f = fixture(); f.state.supported = false;
  await assert.rejects(nativeThreadPage(f.client, owner, { archived: false }), { code: 'native_method_unsupported' });
  assert.equal(f.reads().length, 0);
  for (const cursor of ['', '{}', 'x'.repeat(65537), '{"version":1,"owner":false,"cursor":"x"}'])
    await assert.rejects(nativeThreadPage(f.client, owner, { archived: false, cursor }), { code: 'invalid_cursor' });
  assert.equal(f.reads().length, 0);
});

test('a repeated native cursor, changed in-flight owner and unavailable account never yield a history page', async () => {
  const f = fixture(), first = await nativeThreadPage(f.client, owner, { archived: false });
  f.state.response = { data: [], nextCursor: JSON.parse(first.nextCursor!).cursor as string };
  await assert.rejects(nativeThreadPage(f.client, owner, { archived: false, cursor: first.nextCursor! }), { code: 'native_inventory_invalid' });
  f.state.response = { data: [], nextCursor: null }; f.state.afterRead = () => { f.state.epoch = 'replaced-during-read'; };
  await assert.rejects(nativeThreadPage(f.client, owner, { archived: false }), { code: 'native_owner_changed' });
  const blocked = fixture(); blocked.state.ready = false;
  await assert.rejects(nativeThreadPage(blocked.client, owner, { archived: false }), { code: 'native_unavailable' });
  assert.equal(blocked.reads().length, 0);
  const cancelled = fixture(), controller = new AbortController(); controller.abort(new Error('Original cancelled read'));
  await assert.rejects(nativeThreadPage(cancelled.client, owner, { archived: false }, controller.signal), /Original cancelled read/);
  assert.equal(cancelled.visits.length, 0);
});
