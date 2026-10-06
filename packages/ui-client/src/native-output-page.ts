import { callBound, canonical, discover, IvyError, nativeVersions } from '../../sdk/src/client.js';
import type { RpcClient, Wire } from '../../sdk/src/client.js';
import { nativeRead, record, text } from './native.js';

interface Cursor {
  format: 'ivy-native-output-1'; scope: string; mode: 'items' | 'search' | 'full'; nativeCursor: string | null;
  epoch: string | null; offset: number; turnId: string | null; turnHash: string | null;
}
export interface NativeOutputPage {
  items: Wire.Json[]; nextCursor: string | null; mode: 'items' | 'full' | 'search'; detail: string;
  download: { name: string; raw: string } | null;
}
interface CompleteTurnCache {
  scope: string; epoch: string; cursor: string | null; turnHash: string; stamp: string;
  page: { turn: Record<string, Wire.Json>; next: string | null; observed: Record<string, Wire.Json>; unmaterialized: boolean };
}
// One original terminal snapshot per connection; a new view starts a fresh native read.
const completeTurns = new WeakMap<RpcClient, CompleteTurnCache>();
const turnStamp = (turn: Record<string, Wire.Json>) => {
  const { items: _items, itemsView: _view, ...metadata } = turn; return canonical(metadata);
};
const fail = (message: string): never => { throw new IvyError('native_output_unavailable', message); };
export function isUnmaterializedNativeHistory(error: unknown, threadId: string): boolean {
  const original = error instanceof IvyError ? record(record(error.details).nativeError) : {};
  return error instanceof IvyError && error.code === 'native_error' && error.outcome === 'completed' && original.code === -32600 &&
    original.message === `thread ${threadId} is not materialized yet; thread/turns/list is unavailable before first user message`;
}
const hash = async (value: unknown): Promise<string> => 'sha256:' + Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(canonical(value)))), byte => byte.toString(16).padStart(2, '0')).join('');
function parseCursor(value: string | undefined, scope: string): Cursor | null {
  if (value === undefined) return null;
  if (value.length > 32768) return fail('Saved output cursor is too large. Reload saved output.');
  const parsed = record(JSON.parse(value));
  if (parsed.format !== 'ivy-native-output-1' || parsed.scope !== scope || !['items', 'search', 'full'].includes(text(parsed.mode)) ||
    !(parsed.nativeCursor === null || typeof parsed.nativeCursor === 'string') || !(parsed.epoch === null || typeof parsed.epoch === 'string') ||
    !Number.isSafeInteger(parsed.offset) || Number(parsed.offset) < 0 || Number(parsed.offset) > 400000 || Number(parsed.offset) % 50 !== 0 ||
    !(parsed.turnId === null || typeof parsed.turnId === 'string') || !(parsed.turnHash === null || typeof parsed.turnHash === 'string'))
    return fail('Saved output cursor belongs to another view or has changed. Reload saved output.');
  return parsed as unknown as Cursor;
}
const nextNative = (value: unknown): string | null => {
  if (value === null) return null;
  if (typeof value !== 'string' || !value || value.length > 16384) return fail('Native output returned an invalid continuation.');
  return value;
};
function nativeItemEntry(value: Wire.Json, selectedTurn: string): Wire.Json {
  const entry = record(value), wrapped = 'item' in entry, item = wrapped ? record(entry.item) : entry;
  if (!text(item.id) || !text(item.type)) return fail('Native output returned an invalid saved item.');
  if (wrapped && (!text(entry.turnId) || selectedTurn && entry.turnId !== selectedTurn)) return fail('Native output returned an item from another or unknown turn.');
  return wrapped ? value : { item: value, turnId: selectedTurn || null };
}

/** Existing item pagination first; only an exact first-page native -32601 permits full-turn reads. */
export async function readNativeOutputPage(client: RpcClient, node: string, threadId: string, selectedTurn: string, cursor: string | undefined, signal: AbortSignal): Promise<NativeOutputPage> {
  const scope = canonical([node, threadId, selectedTurn]), prior = parseCursor(cursor, scope);
  if (!prior) completeTurns.delete(client);
  const bounded = AbortSignal.any([signal, AbortSignal.timeout(35000)]);
  const encode = (value: Omit<Cursor, 'format' | 'scope'>) => canonical({ format: 'ivy-native-output-1', scope, ...value });
  if (!prior || prior.mode === 'items') {
    try {
      const page = record(await nativeRead(client, node, 'codex.thread/items/list', { threadId, limit: 50, sortDirection: 'desc',
        ...(selectedTurn ? { turnId: selectedTurn } : {}), ...(prior?.nativeCursor ? { cursor: prior.nativeCursor } : {}) }, bounded));
      if (!Array.isArray(page.data)) return fail('Native item response is missing its data.');
      const next = nextNative(page.nextCursor);
      if (next !== null && next === prior?.nativeCursor) return fail('Native item continuation did not advance.');
      return { items: page.data.map(item => nativeItemEntry(item, selectedTurn)), nextCursor: next === null ? null : encode({ mode: 'items', nativeCursor: next, epoch: null, offset: 0, turnId: null, turnHash: null }), mode: 'items', detail: '', download: null };
    } catch (error) {
      if (prior || !(error instanceof IvyError) || error.code !== 'native_error' || error.outcome !== 'completed' || record(record(error.details).nativeError).code !== -32601) throw error;
    }
  }

  const before = record(await nativeRead(client, node, 'agent.status', {}, bounded));
  if (before.serviceNodeId !== node || before.state !== 'ready' || !text(before.epoch) || !nativeVersions.includes(text(before.nativeVersion)) ||
    !/^sha256:[a-f0-9]{64}$/.test(text(before.nativeExecutableHash)) || !/^sha256:[a-f0-9]{64}$/.test(text(before.catalogHash)) ||
    prior && prior.epoch !== before.epoch) return fail('The native connection changed. Reload saved output from its current owner.');
  const epoch = text(before.epoch), binding = await discover(client, 'agent.read', { serviceNodeId: node });
  if (binding.serviceNodeId !== node) return fail('Native output discovery returned another owner.');
  const checkOwner = async () => {
    const after = record(await nativeRead(client, node, 'agent.status', {}, bounded));
    if (after.state !== 'ready' || ['serviceNodeId', 'epoch', 'nativeVersion', 'nativeExecutableHash', 'catalogHash'].some(key => after[key] !== before[key]))
      fail('The native connection changed while reading saved output. Reload this view.');
  };
  const read = async (nativeCursor: string | null, itemsView: 'notLoaded' | 'full') => {
    const method = 'thread/turns/list', params = { threadId, cursor: nativeCursor, limit: 1, sortDirection: 'desc', itemsView };
    const observed = record(await callBound(client, binding, { nativeVersion: before.nativeVersion!, method, params }, undefined, { signal: bounded }));
    if (observed.schemaVersion !== 1 || !text(observed.observationId) || !text(observed.callerPrincipalId) || !Number.isFinite(Date.parse(text(observed.observedAt))) ||
      !(typeof observed.requestId === 'string' || typeof observed.requestId === 'number' && Number.isSafeInteger(observed.requestId)) ||
      observed.method !== method || canonical(observed.params) !== canonical(params) || observed.requestHash !== await hash({ method, params }) ||
      ['serviceNodeId', 'epoch', 'nativeVersion', 'nativeExecutableHash', 'catalogHash'].some(key => observed[key] !== before[key]))
      return fail('The native read observation does not match the requested owner, connection or turn page.');
    const reply = record(observed.reply);
    if ('error' in reply) {
      const error = new IvyError('native_error', 'The native owner could not read this saved turn.', 'completed', { nativeError: reply.error! });
      if (!selectedTurn && nativeCursor === null && isUnmaterializedNativeHistory(error, threadId)) return { turn: null, next: null, observed, unmaterialized: true };
      throw error;
    }
    if (!('result' in reply)) return fail('Native read observation is missing its original result.');
    const result = record(reply.result);
    if (!Array.isArray(result.data) || result.data.length > 1) return fail('Native turn response is not the requested single-turn page.');
    const next = nextNative(result.nextCursor);
    if (next !== null && next === nativeCursor) return fail('Native turn continuation did not advance.');
    const turn = result.data.length ? record(result.data[0]) : null;
    if (turn && (!text(turn.id) || turn.itemsView !== itemsView || !Array.isArray(turn.items))) return fail('Native turn did not return the requested complete representation.');
    if (!turn && next !== null) return fail('Empty native turn page cannot carry a continuation.');
    return { turn, next, observed, unmaterialized: false };
  };
  let nativeCursor = prior?.nativeCursor ?? null;
  if (selectedTurn && prior?.mode !== 'full') {
    const seen = new Set<string | null>(), began = Date.now(); let found = false;
    for (let count = 0; count < 32; count++) {
      if (seen.has(nativeCursor)) return fail('Native saved-turn search repeated a page.'); seen.add(nativeCursor);
      const page = await read(nativeCursor, 'notLoaded');
      if (page.turn?.id === selectedTurn) { found = true; break; }
      if (page.next === null) return fail('The selected saved turn was not found on this native owner.');
      nativeCursor = page.next;
      if (Date.now() - began >= 12000) break;
    }
    if (!found) {
      await checkOwner();
      return { items: [], nextCursor: encode({ mode: 'search', nativeCursor, epoch, offset: 0, turnId: null, turnHash: null }), mode: 'search',
        detail: 'The selected turn is further back in saved history. Continue loading earlier messages.', download: null };
    }
  }
  const cached = completeTurns.get(client);
  let page;
  if (prior?.mode === 'full' && prior.offset > 0 && cached?.scope === scope && cached.epoch === epoch && cached.cursor === nativeCursor && cached.turnHash === prior.turnHash) {
    const metadata = await read(nativeCursor, 'notLoaded');
    if (!metadata.turn || turnStamp(metadata.turn) !== cached.stamp || metadata.next !== cached.page.next)
      return fail('Saved output changed or moved between item groups. Reload saved output.');
    page = cached.page;
  } else page = await read(nativeCursor, 'full');
  await checkOwner();
  if (!page.turn) {
    if (selectedTurn || prior?.offset) return fail('The requested saved turn is no longer on this page. Reload saved output.');
    return { items: [], nextCursor: null, mode: 'full', detail: page.unmaterialized ? 'Saved history becomes available after the first message.' : 'No saved turn is available.', download: null };
  }
  const turn = page.turn, items = turn.items as Wire.Json[], ids = new Set<string>();
  for (const item of items) { const id = text(record(item).id); if (!id || ids.has(id)) return fail('The complete turn has missing or duplicate item identities.'); ids.add(id); }
  const turnHash = await hash(turn), offset = prior?.mode === 'full' ? prior.offset : 0;
  if (selectedTurn && turn.id !== selectedTurn || prior?.mode === 'full' && prior.turnId !== null && (turn.id !== prior.turnId || turnHash !== prior.turnHash) || offset > 0 && offset >= items.length)
    return fail('Saved output changed or moved between item groups. Reload saved output.');
  const end = Math.min(offset + 50, items.length);
  if (['completed', 'failed', 'interrupted'].includes(text(turn.status)) && typeof turn.completedAt === 'number' && Number.isFinite(turn.completedAt))
    completeTurns.set(client, { scope, epoch, cursor: nativeCursor, turnHash, stamp: turnStamp(turn), page: { ...page, turn } });
  const next = end < items.length ? encode({ mode: 'full', nativeCursor, epoch, offset: end, turnId: text(turn.id), turnHash })
    : !selectedTurn && page.next !== null ? encode({ mode: 'full', nativeCursor: page.next, epoch, offset: 0, turnId: null, turnHash: null }) : null;
  return { items: items.slice(offset, end).map(item => ({ item, turnId: turn.id! })), nextCursor: next, mode: 'full',
    detail: `Complete native turn · showing ${items.length ? offset + 1 : 0}–${end} of ${items.length} items.`,
    download: { name: 'native-turn-' + text(turn.id).replace(/[^a-zA-Z0-9_-]/g, '_') + '.json', raw: canonical(page.observed) } };
}
