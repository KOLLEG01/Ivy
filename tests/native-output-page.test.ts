import test from 'node:test';
import assert from 'node:assert/strict';
import { isUnmaterializedNativeHistory, readNativeOutputPage } from '../packages/ui-client/src/native-output-page.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { hashJson, digest } from '../packages/contracts/src/canonical.js';
import type { RpcClient, Wire } from '../packages/sdk/src/client.js';

function fixture(version = '0.154.0') {
  const node = 'selected-owner', threadId = 'selected-thread', calls: string[] = [];
  const status = { serviceNodeId: node, state: 'ready', nativeVersion: version, epoch: 'native-epoch', nativeExecutableHash: digest('executable'), catalogHash: digest('catalog') };
  const turns: { id: string; status: string; items: Wire.Json[]; completedAt?: number }[] = [{ id: 'turn-one', status: 'completed', items: [{ id: 'item-one', type: 'agentMessage', text: 'Exact saved output.' }] }];
  const state = { supported: false, nativeCode: -32601, outcome: 'completed' as 'completed' | 'unknown', tamper: (value: Record<string, unknown>) => value, itemPage: null as Wire.Json[] | null, statusReads: 0, changeEpoch: false };
  const client = { async request(method: string, raw: Record<string, unknown>) {
    if (method === 'system.status') return { runtimeEpoch: '11111111-1111-4111-8111-111111111111' };
    if (method === 'tools.list') return { provider: { node: { serviceNodeId: node } }, items: [{ qualifiedName: raw.namespace + '.' + raw.namePrefix, definitionHash: digest('definition'), definition: {} }], nextCursor: null };
    assert.equal(method, 'tools.call'); const name = raw.qualifiedName as string, args = raw.arguments as Record<string, unknown>; calls.push(name);
    if (name === 'agent.status') { state.statusReads++; return { ...status, ...(state.changeEpoch && state.statusReads > 1 ? { epoch: 'changed-epoch' } : {}) }; }
    if (name === 'codex.thread/items/list') {
      if (!state.supported) throw new IvyError('native_error', 'Native original error.', state.outcome, { nativeError: { code: state.nativeCode, message: 'Unsupported.' } });
      return { data: state.itemPage ?? [{ turnId: 'turn-one', item: turns[0]!.items[0] }], nextCursor: args.cursor ? null : 'native-items-next' };
    }
    assert.equal(name, 'agent.read'); assert.equal(args.method, 'thread/turns/list'); assert.equal(args.nativeVersion, version);
    assert.equal(raw.operationId, undefined, 'fallback observations must not create native mutation journal operations');
    const params = args.params as Record<string, unknown>, offset = params.cursor ? Number(String(params.cursor).slice('turn-page-'.length)) : 0;
    assert.equal(params.threadId, threadId); assert.equal(params.limit, 1); assert.equal(params.sortDirection, 'desc');
    const turn = turns[offset], result = { data: turn ? [{ ...turn, items: params.itemsView === 'full' ? structuredClone(turn.items) : [], itemsView: params.itemsView }] : [], nextCursor: offset + 1 < turns.length ? 'turn-page-' + (offset + 1) : null };
    return state.tamper({ schemaVersion: 1, observationId: 'observation-' + calls.length, callerPrincipalId: 'user', serviceNodeId: node, nativeVersion: version,
      nativeExecutableHash: status.nativeExecutableHash, catalogHash: status.catalogHash, epoch: status.epoch, requestId: calls.length,
      method: args.method, params, requestHash: hashJson({ method: args.method, params }), observedAt: new Date().toISOString(), reply: { result } });
  } } as unknown as RpcClient;
  const read = (turn = '', cursor?: string) => readNativeOutputPage(client, node, threadId, turn, cursor, new AbortController().signal);
  return { node, threadId, status, turns, calls, state, client, read };
}

for (const version of ['0.154.0', '0.158.0', '0.159.2']) test('complete native UI output uses the original full observation and bounded item groups for ' + version, async () => {
  const f = fixture(version), big = 'Original large output '.repeat(100000);
  f.turns[0]!.items = Array.from({ length: 51 }, (_, i) => ({ id: 'item-' + i, type: 'agentMessage', text: i === 50 ? big : 'Item ' + i }));
  const first = await f.read('turn-one'); assert.equal(first.mode, 'full'); assert.equal(first.items.length, 50); assert.ok(first.nextCursor);
  assert.equal(JSON.parse(first.download!.raw).reply.result.data[0].items[50].text, big);
  const second = await f.read('turn-one', first.nextCursor); assert.equal(second.items.length, 1); assert.equal(second.nextCursor, null);
  assert.equal((second.items[0] as { item: { text: string } }).item.text, big);
  assert.equal(f.calls.filter(name => name === 'codex.thread/items/list').length, 1);
});

test('terminal output groups reuse their original snapshot after checking current metadata; a new view reads afresh', async () => {
  const f = fixture(); f.turns[0]!.completedAt = 100;
  f.turns[0]!.items = Array.from({ length: 101 }, (_, i) => ({ id: 'item-' + i, type: 'agentMessage', text: 'Complete item ' + i }));
  let fullReads = 0; f.state.tamper = value => { if ((value.params as Record<string, unknown>)['itemsView'] === 'full') fullReads++; return value; };
  const first = await f.read('turn-one'), second = await f.read('turn-one', first.nextCursor!), third = await f.read('turn-one', second.nextCursor!);
  assert.equal(fullReads, 1); assert.equal(third.items.length, 1); assert.equal(third.download!.raw, first.download!.raw);
  f.turns[0]!.completedAt = 101;
  await assert.rejects(f.read('turn-one', first.nextCursor!), { code: 'native_output_unavailable' });
  await f.read('turn-one'); assert.equal(fullReads, 2);
});

test('ordinary native item paging is preserved and no mid-stream or uncertain error enables fallback', async () => {
  const f = fixture(); f.state.supported = true;
  const first = await f.read(); assert.equal(first.mode, 'items'); assert.ok(first.nextCursor);
  const second = await f.read('', first.nextCursor); assert.equal(second.mode, 'items'); assert.equal(second.nextCursor, null);
  f.state.supported = false;
  await assert.rejects(f.read('', first.nextCursor), { code: 'native_error' }); assert.equal(f.calls.includes('agent.read'), false);
  for (const scenario of [{ nativeCode: -32000, outcome: 'completed' }, { nativeCode: -32601, outcome: 'unknown' }] as const) {
    const other = fixture(); Object.assign(other.state, scenario); await assert.rejects(other.read(), { code: 'native_error' }); assert.equal(other.calls.includes('agent.read'), false);
  }
});

test('native direct items retain exact content without inventing a turn; wrapped items retain their observed turn', async () => {
  const f = fixture('0.154.0'); f.state.supported = true;
  const item = { id: 'old-item', type: 'agentMessage', text: 'Exact legacy output.', phase: null, memoryCitation: null };
  f.state.itemPage = [item];
  assert.deepEqual((await f.read()).items, [{ item, turnId: null }]);
  assert.deepEqual((await f.read('explicit-turn')).items, [{ item, turnId: 'explicit-turn' }]);
  f.state.itemPage = [{ item, turnId: 'observed-turn' }];
  assert.deepEqual((await f.read()).items, f.state.itemPage);
  await assert.rejects(f.read('another-turn'), /another or unknown turn/);
  for (const malformed of [{ item }, { id: 'missing-type' }, { type: 'agentMessage' }, null]) {
    f.state.itemPage = [malformed]; await assert.rejects(f.read(), { code: 'native_output_unavailable' });
  }
  assert.equal(f.calls.includes('agent.read'), false);
});

test('selected old-turn search resumes after its bounded pass, and all-turn output advances independently', async () => {
  const f = fixture(); f.turns.splice(0, 1, ...Array.from({ length: 40 }, (_, i) => ({ id: 'turn-' + i, status: 'completed', items: [{ id: 'item-' + i, type: 'agentMessage', text: 'Output ' + i }] })));
  const search = await f.read('turn-39'); assert.equal(search.mode, 'search'); assert.equal(search.download, null); assert.ok(search.nextCursor);
  const found = await f.read('turn-39', search.nextCursor); assert.equal(found.mode, 'full'); assert.equal((found.items[0] as { turnId: string }).turnId, 'turn-39'); assert.equal(found.nextCursor, null);
  const all = await f.read(); assert.ok(all.nextCursor);
  const next = await f.read('', all.nextCursor); assert.equal((next.items[0] as { turnId: string }).turnId, 'turn-1');
});

test('complete native UI reads reject missing, summary, foreign, repeated and mismatched observations', async () => {
  const mutations = [
    (value: Record<string, unknown>) => { value.epoch = 'foreign'; },
    (value: Record<string, unknown>) => { value.serviceNodeId = 'foreign'; },
    (value: Record<string, unknown>) => { value.catalogHash = digest('foreign'); },
    (value: Record<string, unknown>) => { value.requestHash = digest('wrong'); },
    (value: Record<string, unknown>) => { value.params = { wrong: true }; },
    (value: Record<string, unknown>) => { delete value.requestId; },
    (value: Record<string, unknown>) => { value.observedAt = 'not-a-time'; },
    (value: Record<string, unknown>) => { const result = (value.reply as { result: { data: Record<string, unknown>[] } }).result; result.data[0]!.itemsView = 'notLoaded'; },
    (value: Record<string, unknown>) => { const result = (value.reply as { result: { data: Record<string, unknown>[] } }).result; result.data[0]!.items = null; },
    (value: Record<string, unknown>) => { const result = (value.reply as { result: { data: Record<string, unknown>[] } }).result; result.data.push(result.data[0]!); },
  ];
  for (const change of mutations) {
    const f = fixture(); f.state.tamper = value => { change(value); return value; }; await assert.rejects(f.read());
  }
  const duplicate = fixture(); duplicate.turns[0]!.items.push(duplicate.turns[0]!.items[0]!); await assert.rejects(duplicate.read());
  const missing = fixture(); await assert.rejects(missing.read('missing-turn'));
  const changedEpoch = fixture(); changedEpoch.state.changeEpoch = true; await assert.rejects(changedEpoch.read());
});

test('a full-turn continuation cannot cross a view or mix changed/shifted item groups', async () => {
  const f = fixture(); f.turns[0]!.items = Array.from({ length: 51 }, (_, i) => ({ id: 'item-' + i, type: 'agentMessage', text: 'Output ' + i }));
  const first = await f.read('turn-one'); assert.ok(first.nextCursor);
  await assert.rejects(f.read('different-turn', first.nextCursor));
  f.turns[0]!.items.push({ id: 'new-item', type: 'agentMessage', text: 'New output.' });
  await assert.rejects(f.read('turn-one', first.nextCursor)); f.turns[0]!.items.pop();
  f.turns.unshift({ id: 'new-turn', status: 'completed', items: [] }); await assert.rejects(f.read('turn-one', first.nextCursor));
  f.turns.shift(); f.status.epoch = 'new-native-owner'; await assert.rejects(f.read('turn-one', first.nextCursor));
});

test('only the exact unmaterialized native history error explains a new task without granting control or hiding a selected turn', async () => {
  const f = fixture(), nativeError = { code: -32600, message: 'thread selected-thread is not materialized yet; thread/turns/list is unavailable before first user message' };
  f.state.tamper = value => { value.reply = { error: nativeError }; return value; };
  const page = await f.read(); assert.deepEqual(page.items, []); assert.equal(page.download, null); assert.equal(page.nextCursor, null);
  assert.equal(page.detail, 'Saved history becomes available after the first message.'); assert.equal(f.state.statusReads, 2);
  await assert.rejects(f.read('selected-saved-turn'));
  for (const [code, message, outcome] of [[-32601, nativeError.message, 'completed'], [-32600, nativeError.message.replace('selected-thread', 'another-thread'), 'completed'], [-32600, nativeError.message, 'unknown']])
    assert.equal(isUnmaterializedNativeHistory(new IvyError('native_error', 'Native original error', outcome as 'completed' | 'unknown', { nativeError: { code, message } }), 'selected-thread'), false);
  nativeError.message = 'Another failure'; await assert.rejects(f.read());
});
