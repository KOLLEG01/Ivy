import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { randomUUID } from 'node:crypto';
import { NativeInventory, nativeSourceKinds, publishNativeSnapshot } from '../services/agent-manager/src/inventory.js';
import { NativeJournal } from '../services/agent-manager/src/journal.js';
import { agentRegistry, nativeInventorySchemaVersion, nativeProjectSchemaVersion } from '../services/agent-manager/src/registry.js';
import { HiveKernel } from '../services/hive/src/kernel.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Agent, Wire } from '../packages/contracts/src/generated.js';
import type { RpcClient } from '../packages/sdk/src/client.js';

const catalog = (version: string) => JSON.parse(readFileSync('specs/native/codex-' + version + '/catalog.json', 'utf8')) as Agent.Catalog;
const thread = (id: string) => ({ id, name: null, cwd: '/fixture', preview: 'Original ' + id, status: { type: 'idle' }, source: 'appServer', updatedAt: 1788600000, ephemeral: false });

test('slow inventory retains the full transport watchdog and never publishes an expired snapshot',async t=>{
  let now=1788600000000,advance=30000,calls=0;t.mock.method(Date,'now',()=>now);
  const native=new NativeInventory({catalog:catalog('0.154.0'),request:async(_method,_params,_hooks,timeout)=>{
    assert.equal(timeout,120000);calls++;if(calls===1)now+=advance;return {result:{data:[],nextCursor:null}};
  }},'epoch');
  assert.equal((await native.collectThreads()).threads.length,0);
  calls=0;advance=130000;await assert.rejects(native.collectThreads(),{code:'native_inventory_deadline'});assert.equal(calls,1);
});

test('native inventory includes every schema source, archived history, loaded ephemeral ownership and bounded project roots', async () => {
  for (const version of ['0.154.0']) {
    const c = catalog(version), calls: { method: string; params: Wire.Json }[] = [];
    const native = new NativeInventory({ catalog: c, request: async (method, params) => {
      calls.push({ method, params }); const args = params as Record<string, Wire.Json>;
      if (method === 'thread/loaded/list') return { result: { data: ['loaded', 'ephemeral'], nextCursor: null } };
      if (method === 'thread/read') return { result: { thread: { ...thread('ephemeral'), ephemeral: true, canAcceptDirectInput: false } } };
      if (method === 'project/list') return { result: { data: [{ id: 'native-project', name: 'Desktop project', roots: [{ path: '/configured' }], position: 0, recencyAt: 1788600000000 }], nextCursor: null } };
      assert.equal(method, 'thread/list'); assert.deepEqual(args['sourceKinds'], nativeSourceKinds(c)); assert.deepEqual(args['modelProviders'], []);
      assert.equal(args['useStateDbOnly'], true);
      assert.equal(args['sortKey'], 'recency_at'); assert.equal(args['sortDirection'], 'desc');
      if (args['archived']) return { result: { data: [thread('archived')], nextCursor: null } };
      return { result: { data: [thread(args['cursor'] ? 'historical' : 'loaded')], nextCursor: args['cursor'] ? null : 'next-page' } };
    } }, 'current-epoch');
    const threadSnapshot = await native.collectThreads(), summaries = new Map(threadSnapshot.threads.map(value => [value.nativeId, value.summary as Agent.ThreadSummary]));
    assert.equal(summaries.size, 4); assert.equal(summaries.get('archived')?.archived, true);
    assert.equal(summaries.get('historical')?.owner, 'historical-unattached'); assert.equal(summaries.get('historical')?.epoch, null);
    assert.equal(summaries.get('loaded')?.owner, 'this-native-connection'); assert.equal(summaries.get('loaded')?.epoch, 'current-epoch');
    assert.equal(summaries.get('loaded')?.canAcceptDirectInput, null); assert.equal(summaries.get('ephemeral')?.canAcceptDirectInput, false);
    assert.equal(summaries.get('ephemeral')?.ephemeral, true);
    assert.equal(summaries.get('loaded')?.threadSource, null);
    assert.equal(calls.some(value => value.method === 'project/list'), false);
    const projectSnapshot = await native.collectProjects();
    assert.deepEqual(projectSnapshot.projects.projects.map(value => value.nativeId), ['native-project']);
    assert.deepEqual(projectSnapshot.projects.projects[0], { nativeId: 'native-project', source: 'native', name: 'Desktop project', paths: ['/configured'], position: 0, recencyAt: 1788600000000 });
    assert.equal(projectSnapshot.projects.source, 'native');
    assert.equal(calls.filter(value => value.method === 'thread/loaded/list').length, 2);
  }
});

test('a successful empty native project list replaces earlier projects and a new instance starts empty', async () => {
  let removed = false;
  const rpc = { catalog: catalog('0.154.0'), request: async () => ({ result: { data: removed ? [] : [
    { id: 'project', name: 'Native project', roots: [{ path: '/project' }], position: 0 },
  ], nextCursor: null } }) };
  const inventory = new NativeInventory(rpc, 'epoch');
  assert.equal((await inventory.collectProjects()).projects.projects.length, 1);
  removed = true;
  assert.deepEqual((await inventory.collectProjects()).projects.projects, []);
  assert.deepEqual(inventory.currentProjects().projects.projects, []);
  assert.deepEqual(new NativeInventory(rpc, 'new-home').currentProjects().projects.projects, []);
});

test('native inventory preserves explicit Voice classification without inferring it from titles or process source', async () => {
  const values = [
    { ...thread('voice'), threadSource: 'realtime_voice' },
    { ...thread('named-like-voice'), name: 'Voice chat', source: 'appServer' },
    { ...thread('other'), threadSource: 'another_source' },
  ];
  const native = new NativeInventory({ catalog: catalog('0.154.0'), request: async (method, params) => {
    if (method === 'thread/loaded/list') return { result: { data: [], nextCursor: null } };
    if (method === 'project/list') return { result: { data: [], nextCursor: null } };
    assert.equal(method, 'thread/list');
    return { result: { data: (params as Record<string, Wire.Json>)['archived'] ? [] : values, nextCursor: null } };
  } }, 'epoch');
  const snapshot = await native.collect();
  assert.deepEqual(snapshot.threads.map(entry => (entry.summary as Agent.ThreadSummary).threadSource), ['realtime_voice', null, 'another_source']);
  assert.ok(snapshot.threads.every(entry => (entry.summary as Agent.ThreadSummary).owner === 'historical-unattached'));
});

test('incomplete native pages, duplicate IDs, changing ownership, oversized snapshots and cancelled reads never produce a replacement', async () => {
  for (const failure of ['error', 'cursor-loop', 'duplicate', 'changed', 'overflow', 'byte-overflow', 'cancelled']) {
    let loads = 0; const abort = new AbortController();
    const native = new NativeInventory({ catalog: catalog('0.154.0'), request: async (method, params) => {
      if (method === 'thread/loaded/list') return { result: { data: failure === 'changed' && loads++ ? ['new-owner'] : [], nextCursor: null } };
      if (method === 'project/list') return { result: { data: [], nextCursor: null } };
      const args = params as Record<string, Wire.Json>;
      if (failure === 'error') return { error: { code: -32000, message: 'fixture read failed' } };
      if (failure === 'cancelled') abort.abort();
      if (failure === 'overflow') return { result: { data: Array.from({ length: 5001 }, (_, n) => thread('thread-' + n)), nextCursor: null } };
      if (failure === 'byte-overflow') {
        const page = Number(args['cursor'] ?? 0);
        return { result: { data: [{ ...thread('large-' + page), source: { subAgent: { other: 'x'.repeat(3 * 1024 * 1024) } } }], nextCursor: page < 3 ? String(page + 1) : null } };
      }
      if (failure === 'cursor-loop') return { result: { data: [], nextCursor: 'unchanged' } };
      return { result: { data: args['archived'] && failure !== 'duplicate' ? [] : [thread('same-id')], nextCursor: null } };
    } }, 'epoch');
    await assert.rejects(native.collect(abort.signal), failure);
  }
});

test('restored native revision counters recover from authoritative empty Hive inventory without guessing or replaying uncertain writes', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-native-inventory-')), c = catalog('0.154.0');
  const kernel = new HiveKernel({ filename: ':memory:', publicBaseUrl: 'https://inventory.test/ivy', version: 'test', buildId: digest('inventory'), credentials: [{ principalId: 'owner', digest: digest('credential') }] });
  const base = { transport: 'ws' as const, credentialDigest: digest('credential'), principalId: 'owner' }, node = 'native-owner';
  const result = kernel.execute(base, { jsonrpc: '2.0', id: randomUUID(), method: 'service.connect', params: { serviceNodeId: node, serviceName: 'agent-manager', hostId: 'host', version: 'test', buildId: digest('service'), hiveProtocol: 1 } });
  assert.equal(result.kind, 'result'); const generation = (result as { value: { generation: number } }).value.generation;
  const context = { ...base, serviceNodeId: node, generation };
  const registry = agentRegistry({ ...c, clientRequests: [] });
  const prior = structuredClone(registry);
  const projectKind = prior.namespaces[0]!.inventoryKinds.find(kind => kind.kind === 'project')!;
  projectKind.version = '1.1.0';
  projectKind.summarySchema = { type: 'object', properties: { name: { type: 'string' }, recencyAt: { type: ['integer', 'null'] }, source: { enum: ['native', 'host-configuration'] } } };
  assert.equal(kernel.execute(context, { jsonrpc: '2.0', id: randomUUID(), method: 'registry.sync', params: prior }).kind, 'result');
  assert.equal(kernel.execute(context, { jsonrpc: '2.0', id: randomUUID(), method: 'registry.sync', params: registry }).kind, 'result');
  const client: RpcClient = { request: async (method, params) => {
    const response = kernel.execute(context, { jsonrpc: '2.0', id: randomUUID(), method, params }); assert.equal(response.kind, 'result'); return (response as { value: unknown }).value as never;
  } };
  const owner = { serviceNodeId: node, hostId: 'host', nativeVersion: c.version, nativeExecutableHash: c.nativeExecutableHash };
  const limits = { maxOperations: 100, maxJournalBytes: 32 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 };
  let journal = new NativeJournal(owner, limits);
  t.after(() => { journal.close(); kernel.close(); assert.ok(relative(tmpdir(), root).startsWith('ivy-native-inventory-')); rmSync(root, { recursive: true, force: true }); });
  for (const kind of ['thread', 'project']) await client.request('inventory.sync', { namespace: 'codex', kind, schemaVersion: kind === 'project' ? nativeProjectSchemaVersion : nativeInventorySchemaVersion, mode: 'snapshot', snapshotRevision: 100, entries: [] });
  assert.deepEqual(await client.request('inventory.list', { serviceNodeId: node }), { items: [], nextCursor: null });
  const snapshot = { observedAt: new Date().toISOString(), threads: [], projects: { source: 'native' as const, observedAt: new Date().toISOString(), projects: [] } };
  await publishNativeSnapshot(client, journal, snapshot);
  journal.close(); journal = new NativeJournal(owner, limits);
  await publishNativeSnapshot(client, journal, snapshot);
  assert.equal(journal.inventoryRevision('thread'), 103); assert.equal(journal.inventoryRevision('project'), 103);
  await assert.rejects(client.request('inventory.sync', { namespace: 'codex', kind: 'thread', schemaVersion: nativeInventorySchemaVersion, mode: 'delta', snapshotRevision: 103, expectedRevision: 99, entries: [], removedNativeIds: [] }),
    (error: unknown) => error instanceof IvyError && error.outcome === 'not_executed' && (error.details as Wire.InventoryRevisionConflictDetails).currentRevision === 102);
  for (const failure of ['uncertain', 'wrong-owner', 'no-metadata']) {
    let calls = 0;
    const failed: RpcClient = { request: async () => {
      calls++; throw new IvyError('revision_conflict', 'fixture', failure === 'uncertain' ? 'unknown' : 'not_executed', failure === 'no-metadata' ? undefined :
        { serviceNodeId: 'different-owner', namespace: 'codex', kind: 'thread', currentRevision: 500 });
    } };
    await assert.rejects(publishNativeSnapshot(failed, journal, snapshot)); assert.equal(calls, 1);
  }
});
