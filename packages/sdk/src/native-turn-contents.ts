import { canonical } from '../../contracts/src/canonical-json.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { checkedNativeContract } from '../../contracts/src/checked-native-contract.js';
import type { Agent, Wire } from '../../contracts/src/generated.js';
import { nativeTurnItems } from './native-observations.js';
import type { NativeOwner } from './native-owner.js';

type Record = { [key: string]: Wire.Json };
const record = (value: unknown): Record => value && typeof value === 'object' && !Array.isArray(value) ? value as Record : {};
export interface NativeTurnItemPages {
  format: 'native-turn-item-pages-1';
  snapshot: Agent.ReadObservation;
  pages: Agent.ReadObservation[];
}
/** Complete original evidence: native item pages, or the supported full-turn fallback. */
export type NativeTurnContents = Agent.ReadObservation | NativeTurnItemPages;
export const nativeTurnSnapshot = (value: NativeTurnContents): Agent.ReadObservation =>
  'format' in value ? value.snapshot : value;
const itemParams = (threadId: string, turnId: string, cursor: string | null) =>
  ({ threadId, turnId, cursor, limit: 16, sortDirection: 'asc' });
function turnOf(read: Agent.ReadObservation, threadId: string, turnId: string, view: 'full' | 'notLoaded'): Record {
  requireThat(read.method === 'thread/turns/list' && 'result' in read.reply && record(read.params)['threadId'] === threadId &&
    record(read.params)['itemsView'] === view && record(read.params)['limit'] === 1 && record(read.params)['sortDirection'] === 'desc',
    'native_turn_mismatch', 'Turn contents require the exact one-turn query.');
  const data = record(read.reply.result)['data'];
  requireThat(Array.isArray(data) && data.length === 1, 'native_turn_mismatch', 'Turn contents require one original turn.');
  const turn = record(data[0]);
  requireThat(turn['id'] === turnId && ['inProgress', 'completed', 'failed', 'interrupted'].includes(String(turn['status'])) &&
    turn['itemsView'] === view && Array.isArray(turn['items']) && (view !== 'notLoaded' || turn['items'].length === 0),
    'native_turn_mismatch', 'Turn contents must retain their original turn identity and representation.');
  return turn;
}

/** Verify original observations without synthesizing a native full-history reply. */
export async function verifyNativeTurnContents(owner: NativeOwner, value: NativeTurnContents, threadId: string, turnId: string) {
  const snapshot = nativeTurnSnapshot(value), paged = 'format' in value;
  requireThat(!paged || value.format === 'native-turn-item-pages-1' && Array.isArray(value.pages),
    'native_turn_mismatch', 'Unknown native contents representation.');
  owner.checkRead(snapshot, 'thread/turns/list', snapshot.params, snapshot.epoch);
  const turn = turnOf(snapshot, threadId, turnId, paged ? 'notLoaded' : 'full');
  if (!paged) {
    const items = (turn['items'] as Wire.Json[]).map(record), seen = new Set<string>();
    for (const item of items) {
      const id = item['id']; requireThat(typeof id === 'string' && id.length > 0 && !seen.has(id), 'native_turn_incomplete', 'Complete turn items need distinct original identities.'); seen.add(id);
    }
    return { snapshot, turn, items };
  }
  let contract;
  try { contract = checkedNativeContract(snapshot.nativeVersion, { catalogHash: snapshot.catalogHash, nativeExecutableHash: snapshot.nativeExecutableHash }).contract; }
  catch (error) {
    if (!(error instanceof IvyError) || !['native_catalog_mismatch', 'native_version_unsupported'].includes(error.code)) throw error;
    contract = await owner.selectedContract();
  }
  const items: Record[] = [], seen = new Set<string>(), cursors = new Set<string>(), observations = new Set([snapshot.observationId]);
  let cursor: string | null = null;
  requireThat(value.pages.length > 0, 'native_turn_incomplete', 'A paginated turn needs its original item observations.');
  for (let index = 0; index < value.pages.length; index++) {
    const page = value.pages[index]!;
    owner.checkRead(page, 'thread/items/list', itemParams(threadId, turnId, cursor), snapshot.epoch);
    requireThat(!observations.has(page.observationId), 'native_turn_incomplete', 'Original item observations must have distinct identities.');
    observations.add(page.observationId);
    requireThat('result' in page.reply, 'native_turn_incomplete', 'Complete turn contents cannot omit a failed item page.');
    const pageItems = nativeTurnItems(contract, page.reply.result, turnId), next = record(page.reply.result)['nextCursor'];
    requireThat(pageItems.length <= 16 && (pageItems.length > 0 || next === null) &&
      (next === null || typeof next === 'string' && next.length > 0 && next !== cursor && !cursors.has(next)) &&
      (index === value.pages.length - 1 ? next === null : next !== null),
      'native_turn_incomplete', 'Original item pages must advance to their final cursor.');
    for (const item of pageItems) {
      const id = item['id'];
      requireThat(typeof id === 'string' && id.length > 0 && !seen.has(id), 'native_turn_incomplete', 'Turn items need distinct original identities.');
      seen.add(id); items.push(item);
    }
    if (cursor !== null) cursors.add(cursor);
    cursor = next as string | null;
  }
  return { snapshot, turn, items };
}

/** Read contents only when requested; an exact first-page -32601 retains full-turn support. */
export async function readNativeTurnContents(owner: NativeOwner, snapshot: Agent.ReadObservation, threadId: string, turnId: string,
  maximumBytes = 64 * 1024 * 1024): Promise<NativeTurnContents> {
  owner.checkRead(snapshot, 'thread/turns/list', snapshot.params, snapshot.epoch);
  const turn = turnOf(snapshot, threadId, turnId, 'notLoaded'), pages: Agent.ReadObservation[] = [], visited = new Set<string>();
  let cursor: string | null = null, bytes = Buffer.byteLength(canonical(snapshot, maximumBytes));
  while (true) {
    const observed = await owner.read('thread/items/list', itemParams(threadId, turnId, cursor), snapshot.epoch, true);
    if ('error' in observed.reply) {
      requireThat(pages.length === 0 && cursor === null && observed.reply.error.code === -32601,
        'native_read_failed', 'Only an unsupported first item page permits the full-turn fallback.');
      const params = { ...record(snapshot.params), itemsView: 'full' };
      const full = await owner.read('thread/turns/list', params, snapshot.epoch);
      requireThat(turnOf(full, threadId, turnId, 'full')['status'] === turn['status'], 'native_turn_changed', 'The native turn changed while reading its contents.');
      canonical(full, maximumBytes); return full;
    }
    bytes += Buffer.byteLength(canonical(observed, maximumBytes));
    requireThat(bytes <= maximumBytes, 'native_turn_contents_limit', 'Complete native contents exceed their original byte budget.');
    pages.push(observed);
    const next = record(observed.reply.result)['nextCursor'];
    requireThat(next === null || typeof next === 'string' && next.length > 0 && next !== cursor && !visited.has(next),
      'native_turn_incomplete', 'Native item cursors must advance.');
    if (next === null) break;
    visited.add(next); cursor = next;
  }
  const value: NativeTurnItemPages = { format: 'native-turn-item-pages-1', snapshot, pages };
  await verifyNativeTurnContents(owner, value, threadId, turnId);
  return value;
}
