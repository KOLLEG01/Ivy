import { canonical } from '../../../packages/sdk/src/node.js';
import { requireThat } from '../../../packages/sdk/src/node.js';
import type { Agent, Chat, Wire } from '../../../packages/sdk/src/node.js';
import { nativeTurnItems } from '../../../packages/sdk/src/native-observations.js';
import { chatNativeCatalog } from './native-evidence.js';
import { maximumEvidenceBytes } from './evidence-bytes.js';

export const collectionBytes = 64 * 1024 * 1024, collectionReads = 1024;
const record = (value: Wire.Json | undefined) => value as Record<string, Wire.Json>;
const cursorOf = (value: Wire.Json | undefined): string | null => {
  requireThat(value === undefined || value === null || typeof value === 'string' && value.length > 0, 'chat_collection_gap', 'A native page needs an omitted/null final cursor or nonempty next cursor.');
  return value ?? null;
};
const terminal = (value: unknown): value is Chat.Result['nativeState'] => ['completed', 'failed', 'interrupted'].includes(String(value));

/** Ordered original observations determine every next request. Native schemas/authority are
 * verified by ChatNativeEvidence before entering this reducer; times are never ordering proof. */
export class ChatCollectionReads {
  phase: 'search' | 'items' | 'full' | 'verify' | 'done' = 'search';
  cursor: string | null = null;
  foundCursor: string | null = null;
  nativeState: Chat.Result['nativeState'] | null = null;
  nativeBytes = 0;
  count = 0;
  textBytes = 0;
  lastObservedAt: string | null = null;
  readonly texts: string[] = [];
  private readonly observations = new Set<string>();
  private readonly cursors = new Set<string>();
  private readonly turns = new Set<string>();
  private readonly items = new Set<string>();
  constructor(readonly threadId: string, readonly turnId: string) {}
  fork(): ChatCollectionReads {
    const next = new ChatCollectionReads(this.threadId, this.turnId);
    next.phase = this.phase; next.cursor = this.cursor; next.foundCursor = this.foundCursor; next.nativeState = this.nativeState;
    next.nativeBytes = this.nativeBytes; next.count = this.count; next.textBytes = this.textBytes; next.lastObservedAt = this.lastObservedAt;
    for (const text of this.texts) next.texts.push(text);
    for (const key of ['observations', 'cursors', 'turns', 'items'] as const) for (const value of this[key]) next[key].add(value);
    return next;
  }
  next(): { method: Agent.ReadObservation['method']; params: Wire.Json } {
    requireThat(this.phase !== 'done', 'chat_collection_complete', 'Complete native evidence cannot append another read.');
    if (this.phase === 'items') return { method: 'thread/items/list', params: { threadId: this.threadId, turnId: this.turnId, cursor: this.cursor, limit: 8, sortDirection: 'asc' } };
    return { method: 'thread/turns/list', params: { threadId: this.threadId, cursor: this.phase === 'full' ? this.foundCursor : this.cursor,
      limit: 1, itemsView: this.phase === 'full' ? 'full' : 'notLoaded', sortDirection: 'desc' } };
  }
  private advance(next: string | null): void {
    requireThat(next === null || next !== this.cursor && !this.cursors.has(next), 'chat_collection_gap', 'Native page cursors must advance without repeating.');
    if (this.cursor !== null) this.cursors.add(this.cursor);
    this.cursor = next;
  }
  private verifyNext(): void { this.phase = 'verify'; this.cursor = null; this.cursors.clear(); this.turns.clear(); }
  private appendItems(items: Wire.Json[]): void {
    for (const raw of items) {
      const item = record(raw), id = item['id'];
      requireThat(typeof id === 'string' && id.length && !this.items.has(id), 'chat_collection_gap', 'Complete native items must have distinct original identities.');
      this.items.add(id);
      if (item['type'] === 'agentMessage' && item['phase'] !== 'commentary') {
        const text = item['text'];
        requireThat(typeof text === 'string' && Buffer.from(text).toString('utf8') === text, 'chat_result_text_invalid', 'Original reply text must encode losslessly as UTF-8.');
        this.textBytes += Buffer.byteLength(text) + (this.texts.length ? 2 : 0);
        requireThat(this.textBytes <= collectionBytes, 'chat_collection_limit', 'Complete native reply text exceeds 64 MiB.');
        this.texts.push(text);
      }
    }
  }
  /** False means a current in-progress observation: wait without extending the saved ledger. */
  append(observed: Agent.ReadObservation): boolean {
    const next = this.next();
    requireThat(observed.method === next.method && canonical(observed.params) === canonical(next.params), 'chat_collection_gap', 'Retained observations must answer the exact next original query.');
    requireThat(!this.observations.has(observed.observationId), 'chat_collection_gap', 'Native completion requires distinct original read observations.');
    const bytes = Buffer.byteLength(canonical(observed, maximumEvidenceBytes));
    requireThat(this.count < collectionReads && this.nativeBytes + bytes <= collectionBytes, 'chat_collection_limit', 'Complete native collection exceeds its read or byte bound.');
    if ('error' in observed.reply) {
      requireThat(this.phase === 'items' && this.cursor === null && this.items.size === 0 && observed.reply.error.code === -32601,
        'chat_collection_native_error', 'The original native result read failed; only first-page unsupported items permit full-turn collection.');
      this.phase = 'full';
    } else {
      const body = record(observed.reply.result), data = body['data'] as Wire.Json[], nextCursor = cursorOf(body['nextCursor']);
      requireThat(Array.isArray(data), 'chat_collection_gap', 'A native page must retain its original data array.');
      if (this.phase === 'items') {
        requireThat(data.length > 0 || nextCursor === null, 'chat_collection_gap', 'An empty nonfinal item page does not prove a complete native result.');
        const items = nativeTurnItems(chatNativeCatalog(observed.nativeVersion, { catalogHash: observed.catalogHash }).contract, observed.reply.result, this.turnId);
        this.appendItems(items); this.advance(nextCursor); if (nextCursor === null) this.verifyNext();
      } else {
        requireThat(data.length <= 1, 'chat_collection_gap', 'A one-turn query must return at most its requested one turn.');
        const turn = data[0] === undefined ? null : record(data[0]);
        if (this.phase === 'full') {
          requireThat(turn?.['id'] === this.turnId && turn['status'] === this.nativeState && turn['itemsView'] === 'full' && Array.isArray(turn['items']),
            'chat_collection_changed', 'The original full-turn page must still name the same terminal turn with complete items.');
          this.appendItems(turn['items']); this.verifyNext();
        } else if (turn?.['id'] === this.turnId) {
          if (this.phase === 'search' && turn['status'] === 'inProgress') return false;
          requireThat(terminal(turn['status']) && (this.phase === 'search' || turn['status'] === this.nativeState),
            'chat_collection_changed', 'The original turn must retain its terminal status through final verification.');
          if (this.phase === 'search') {
            this.nativeState = turn['status']; this.foundCursor = this.cursor; this.phase = 'items'; this.cursor = null; this.cursors.clear();
          } else this.phase = 'done';
        } else {
          const id = turn?.['id'];
          requireThat(typeof id === 'string' && !this.turns.has(id) && nextCursor !== null, 'chat_collection_gap', 'The original turn is missing or its search contains repeated turns.');
          this.turns.add(id); this.advance(nextCursor);
        }
      }
    }
    this.observations.add(observed.observationId); this.nativeBytes += bytes; this.count++; this.lastObservedAt = observed.observedAt;
    return true;
  }
}
