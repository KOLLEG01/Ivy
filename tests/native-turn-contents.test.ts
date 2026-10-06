import test from 'node:test';
import assert from 'node:assert/strict';
import { checkedNativeContract, hashJson, NativeOwner, readNativeTurnContents, verifyNativeTurnContents } from '../packages/sdk/src/node.js';
import type { Agent, NativeTurnItemPages, Wire } from '../packages/sdk/src/node.js';

function fixture() {
  const selected = checkedNativeContract('0.158.0'), threadId = 'thread', turnId = 'turn';
  const target = { serviceNodeId: 'native', hostId: 'host', nativeVersion: selected.catalog.version,
    nativeExecutableHash: selected.catalog.nativeExecutableHash, catalogHash: selected.catalogHash };
  const owner = new NativeOwner({ request: async () => { throw Error('Retained schemas must verify original pages offline.'); } }, 'reader', target);
  const items: Wire.Json[] = Array.from({ length: 37 }, (_, index) => ({ id: 'item-' + index, type: 'agentMessage', text: 'Complete text ' + index }));
  const queries: { method: string; params: Wire.Json }[] = [];
  const { hostId: _hostId, ...observedOwner } = target;
  const state = { unsupported: false, errorAt: -1, code: -32601, status: 'completed' };
  let sequence = 0;
  const observation = (method: Agent.ReadObservation['method'], params: Wire.Json, reply: Agent.Reply): Agent.ReadObservation => ({
    schemaVersion: 1, ...observedOwner, observationId: 'read-' + sequence, callerPrincipalId: 'reader', epoch: 'epoch', requestId: sequence,
    observedAt: new Date(Date.parse('2026-09-29T12:00:00Z') + sequence++).toISOString(), method, params: params as Agent.ReadObservation['params'], requestHash: hashJson({ method, params }), reply,
  } as Agent.ReadObservation);
  owner.read = async (method, raw, epoch, allowError) => {
    const params = raw as Record<string, Wire.Json>; queries.push({ method, params: structuredClone(params) });
    let reply: Agent.Reply;
    if (method === 'thread/items/list') {
      const offset = Number(params['cursor'] ?? 0);
      reply = state.unsupported || offset === state.errorAt ? { error: { code: state.code, message: 'Original native error.' } }
        : { result: { data: items.slice(offset, offset + 16).map(item => ({ turnId, item })), nextCursor: offset + 16 < items.length ? String(offset + 16) : null } };
    } else reply = { result: { data: [{ id: turnId, status: state.status, itemsView: params['itemsView']!, items: params['itemsView'] === 'full' ? items : [], error: null }], nextCursor: null } };
    const value = observation(method, raw, reply); owner.checkRead(value, method, raw, epoch ?? value.epoch, allowError); return value;
  };
  const snapshot = observation('thread/turns/list', { threadId, cursor: null, limit: 1, sortDirection: 'desc', itemsView: 'notLoaded' },
    { result: { data: [{ id: turnId, status: 'completed', itemsView: 'notLoaded', items: [], error: null }], nextCursor: null } });
  return { owner, threadId, turnId, items, state, snapshot, queries,
    read: (maximum?: number) => readNativeTurnContents(owner, snapshot, threadId, turnId, maximum),
    verify: (value: NativeTurnItemPages) => verifyNativeTurnContents(owner, value, threadId, turnId) };
}

test('complete turn contents use bounded original item pages and remain verifiable without a live owner', async () => {
  const f = fixture(), evidence = await f.read(); assert.ok('format' in evidence);
  assert.deepEqual((await f.verify(evidence)).items, f.items);
  assert.deepEqual(f.queries.map(query => [query.method, (query.params as Record<string, Wire.Json>)['limit']]),
    [['thread/items/list', 16], ['thread/items/list', 16], ['thread/items/list', 16]]);
  assert.equal(evidence.snapshot, f.snapshot); assert.equal(evidence.pages.length, 3);
});

test('only an exact unsupported first item page permits the unchanged complete native turn', async () => {
  const f = fixture(); f.state.unsupported = true; const evidence = await f.read(); assert.ok(!('format' in evidence));
  assert.equal(evidence.params && (evidence.params as Record<string, Wire.Json>)['itemsView'], 'full');
  assert.deepEqual((await verifyNativeTurnContents(f.owner, evidence, f.threadId, f.turnId)).items, f.items);
  for (const config of [{ errorAt: 16 }, { unsupported: true, code: -32603 }]) {
    const other = fixture(); Object.assign(other.state, config); await assert.rejects(other.read(), { code: 'native_read_failed' });
    assert.ok(!other.queries.some(query => query.method === 'thread/turns/list'));
  }
  const changed = fixture(); changed.state.unsupported = true; changed.state.status = 'failed';
  await assert.rejects(changed.read(), { code: 'native_turn_changed' });
});

test('retained item pages reject changed cursors, foreign turns, repeated items and owners', async () => {
  for (const fault of ['cursor', 'turn', 'duplicate', 'epoch', 'observation']) {
    const f = fixture(), evidence = await f.read(); assert.ok('format' in evidence);
    const changed = structuredClone(evidence), page = changed.pages[1]!;
    if (fault === 'cursor') { (page.params as Record<string, Wire.Json>)['cursor'] = 'foreign'; page.requestHash = hashJson({ method: page.method, params: page.params }); }
    if (fault === 'epoch') page.epoch = 'another-epoch';
    if (fault === 'observation') page.observationId = changed.pages[0]!.observationId;
    if ('result' in page.reply) {
      const entries = (page.reply.result as Record<string, Wire.Json>)['data'] as Record<string, Wire.Json>[];
      if (fault === 'turn') entries[0]!['turnId'] = 'foreign';
      if (fault === 'duplicate') (entries[0]!['item'] as Record<string, Wire.Json>)['id'] = 'item-0';
    }
    await assert.rejects(f.verify(changed));
  }
  await assert.rejects(fixture().read(4000), { code: 'native_turn_contents_limit' });
  await assert.rejects(fixture().read(100), { code: 'content_too_large' });
});
