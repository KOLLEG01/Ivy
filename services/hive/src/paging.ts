import { canonical } from '../../../packages/contracts/src/canonical.js';
import { requireThat } from '../../../packages/contracts/src/errors.js';
import type { Page } from '../../../packages/contracts/src/types.js';
import type { HiveStore } from './store.js';

/** For bounded registry catalogs; Object/query/event scans use database keysets. */
export function catalogPage<T>(store: HiveStore, records: T[], identity: unknown, key: (record: T) => string, params: { limit?: number; cursor?: string }): Page<T> {
  const after = params.cursor ? String(store.readCursor(identity, params.cursor)) : '';
  const sorted = records.sort((a, b) => Buffer.compare(Buffer.from(key(a)), Buffer.from(key(b))));
  const remaining = sorted.filter(record => Buffer.compare(Buffer.from(key(record)), Buffer.from(after)) > 0);
  const items: T[] = []; let bytes = 128;
  for (const record of remaining) {
    if (items.length === (params.limit ?? 50)) break;
    const size = Buffer.byteLength(canonical(record));
    if (bytes + size > 2 * 1024 * 1024 - 8192) {
      requireThat(items.length, 'result_too_large', 'A catalog entry exceeds the response limit.'); break;
    }
    bytes += size; items.push(record);
  }
  return { items, nextCursor: items.length && remaining.length > items.length ? store.cursor(identity, key(items.at(-1)!)) : null };
}
export function utc(value: string): string {
  requireThat(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{3})?Z$/.test(value), 'invalid_arguments', 'Expected a UTC RFC 3339 timestamp.');
  const date = new Date(value);
  requireThat(Number.isFinite(date.valueOf()) && date.toISOString().replace('.000Z', 'Z') === value.replace('.000Z', 'Z'), 'invalid_arguments', 'Invalid UTC calendar date.');
  return value;
}
