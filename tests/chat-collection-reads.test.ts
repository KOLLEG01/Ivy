import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { hashJson } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Agent, Wire } from '../packages/contracts/src/generated.js';
import { ChatCollectionReads } from '../services/chat-bridge/src/collection-reads.js';
import { chatNativeCatalog, verifyChatNativeRead } from '../services/chat-bridge/src/native-evidence.js';

const catalog = chatNativeCatalog('0.154.0').catalog;
const catalogHash = hashJson(catalog);
const identity = { serviceNodeId: 'agent', callerPrincipalId: 'chat', nativeVersion: '0.154.0' as const, epoch: 'epoch' };
const code = (expected: string) => (error: unknown) => error instanceof IvyError && error.code === expected;
const message = (id: string, text = id) => ({ id, type: 'agentMessage', text });
const turn = (id = 'original', status = 'completed', extra = {}) => ({ id, status, items: [], itemsView: 'notLoaded', ...extra });
type ObservationOverrides = Partial<Pick<Agent.ReadObservation, 'observationId' | 'observedAt'>>;
function observation(state: ChatCollectionReads, reply: Agent.Reply, extra: ObservationOverrides = {}): Agent.ReadObservation {
  const request = state.next(), value: unknown = { schemaVersion: 1, observationId: randomUUID(), requestId: randomUUID(), ...identity,
    nativeExecutableHash: catalog.nativeExecutableHash, catalogHash, ...request,
    requestHash: hashJson(request), observedAt: '2026-09-07T04:00:00.000Z', reply, ...extra };
  verifyChatNativeRead(value, { ...identity, ...request }); return value;
}
function append(state: ChatCollectionReads, data: Wire.Json[], nextCursor: string | null = null, extra: ObservationOverrides = {}) {
  return state.append(observation(state, { result: { data: state.phase === 'items' ? data.map(item => ({ turnId: state.turnId, item })) : data, nextCursor } }, extra));
}
const started = () => { const state = new ChatCollectionReads('main', 'original'); append(state, [turn()]); return state; };

test('original native read ledger resumes exact search/items, preserves text and verifies completion across clock changes', () => {
  const state = new ChatCollectionReads('main', 'original');
  append(state, [turn('manual')], 'older'); assert.equal((state.next().params as Record<string, Wire.Json>)['cursor'], 'older');
  assert.equal(append(state, [turn('original', 'inProgress')]), false); assert.equal(state.count, 1);
  append(state, [turn()], null, { observedAt: '2026-09-07T03:00:00.000Z' });
  assert.equal((state.next().params as Record<string, Wire.Json>)['limit'], 8);
  append(state, [message('a', 'A😀'), message('b', '')], 'next');
  const fork = state.fork(); append(fork, [message('c', 'Complete')]); assert.equal(state.phase, 'items');
  append(fork, [turn()], null, { observedAt: '2026-09-07T02:00:00.000Z' });
  assert.equal(fork.phase, 'done'); assert.equal(fork.nativeState, 'completed'); assert.equal(fork.texts.join('\n\n'), 'A😀\n\n\n\nComplete');
  assert.equal(fork.textBytes, Buffer.byteLength(fork.texts.join('\n\n'))); assert.equal(fork.count, 5); assert.throws(() => fork.next(), code('chat_collection_complete'));
});

test('only the exact first unsupported items reply permits the same complete full-turn body', () => {
  const state = started(); state.append(observation(state, { error: { code: -32601, message: 'Native method unsupported' } }));
  assert.equal(state.phase, 'full'); assert.equal((state.next().params as Record<string, Wire.Json>)['itemsView'], 'full');
  append(state, [turn('original', 'completed', { itemsView: 'full', items: [message('answer')] })]); append(state, [turn()]);
  assert.equal(state.phase, 'done'); assert.deepEqual(state.texts, ['answer']);
  const other = started(); assert.throws(() => other.append(observation(other, { error: { code: -32000, message: 'Owner failure' } })), code('chat_collection_native_error'));
  const late = started(); append(late, [message('first')], 'second');
  assert.throws(() => late.append(observation(late, { error: { code: -32601, message: 'Unsupported after real page' } })), code('chat_collection_native_error'));
});

test('native cursor loops, repeated items, missing turns and nonfinal empty pages never establish completion', () => {
  const loop = new ChatCollectionReads('main', 'original'); append(loop, [turn('a')], 'again');
  assert.throws(() => append(loop, [turn('b')], 'again'), code('chat_collection_gap'));
  const cycle = new ChatCollectionReads('main', 'original'); append(cycle, [turn('a')], 'one'); append(cycle, [turn('b')], 'two');
  assert.throws(() => append(cycle, [turn('c')], 'one'), code('chat_collection_gap'));
  const repeat = started(); append(repeat, [message('same')], 'later'); assert.throws(() => append(repeat, [message('same')]), code('chat_collection_gap'));
  assert.throws(() => append(new ChatCollectionReads('main', 'original'), []), code('chat_collection_gap'));
  assert.throws(() => append(started(), [], 'gap'), code('chat_collection_gap'));
  const foreign = started(); assert.throws(() => foreign.append(observation(foreign, {
    result: { data: [{ turnId: 'another-turn', item: message('foreign') }], nextCursor: null },
  })), code('native_turn_mismatch'));
});

test('a shifted full-turn page, partial items view or changed final terminal state is explicit failure', () => {
  for (const body of [turn('other', 'completed', { itemsView: 'full' }), turn('original', 'completed', { itemsView: 'notLoaded' })]) {
    const state = started(); state.append(observation(state, { error: { code: -32601, message: 'Unsupported' } }));
    assert.throws(() => append(state, [body]), code('chat_collection_changed'));
  }
  const changed = started(); append(changed, [message('answer')]); assert.throws(() => append(changed, [turn('original', 'failed')]), code('chat_collection_changed'));
  for (const status of ['failed', 'interrupted']) {
    const state = new ChatCollectionReads('main', 'original'); append(state, [turn('original', status)]); append(state, []); append(state, [turn('original', status)]);
    assert.equal(state.nativeState, status); assert.equal(state.phase, 'done'); assert.deepEqual(state.texts, []);
  }
});

test('duplicate native observations, reordered queries and lossy UTF-8 text are rejected', () => {
  const state = new ChatCollectionReads('main', 'original'), original = observation(state, { result: { data: [turn()], nextCursor: null } }); state.append(original);
  assert.throws(() => state.append(original), code('chat_collection_gap'));
  const duplicate = observation(state, { result: { data: [], nextCursor: null } }, { observationId: original.observationId });
  assert.throws(() => state.append(duplicate), code('chat_collection_gap'));
  assert.throws(() => append(started(), [message('bad', '\ud800')]), code('chat_result_text_invalid'));
  const omitted = started(); assert.equal(omitted.append(observation(omitted, { result: { data: [] } })), true); assert.equal(omitted.phase, 'verify');
});

test('complete collection enforces its actual 1024-read and 64 MiB evidence bounds without truncation', () => {
  const pages = new ChatCollectionReads('main', 'original');
  for (let i = 0; i < 1024; i++) append(pages, [turn('other-' + i)], 'cursor-' + i);
  assert.throws(() => append(pages, [turn()]), code('chat_collection_limit')); assert.equal(pages.count, 1024); assert.equal(pages.nativeState, null);
  const bytes = started(), text = 'x'.repeat(7 * 1024 * 1024);
  for (let i = 0; i < 9; i++) append(bytes, [message('large-' + i, text)], 'cursor-' + i);
  assert.throws(() => append(bytes, [message('overflow', text)]), code('chat_collection_limit'));
  assert.equal(bytes.phase, 'items'); assert.equal(bytes.texts.length, 9); assert.ok(bytes.nativeBytes <= 64 * 1024 * 1024);
});
