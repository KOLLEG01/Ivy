import { nativeRead } from '../../../packages/ui-client/src/native';
import { client, type TaskBoard } from './runtime';

// Start the ticket read while the user points at its card. Consume each snapshot
// once, so subsequent refreshes always consult the service again.
type Prefetch = { expires: number; controller: AbortController; timer: ReturnType<typeof setTimeout>;
  value?: TaskBoard.TaskView; bytes: number };
const pending = new Map<string, Prefetch>();
const maximumBytes = 4 * 1024 * 1024, maximumEntryBytes = 512 * 1024;
let retainedBytes = 0;

// Charge actual string lengths and container slots without serializing a second
// complete snapshot. This is a conservative payload budget, not a VM heap measurement.
function payloadBytes(value: unknown, remaining: number): number {
  if (typeof value === 'string') return 16 + value.length * 2;
  if (!value || typeof value !== 'object') return 8;
  let bytes = 32;
  if (Array.isArray(value)) {
    for (const item of value) {
      bytes += 8 + payloadBytes(item, remaining - bytes);
      if (bytes > remaining) break;
    }
  } else for (const key in value) {
    bytes += 16 + key.length * 2 + payloadBytes((value as Record<string, unknown>)[key], remaining - bytes);
    if (bytes > remaining) break;
  }
  return bytes;
}
function remove(key: string, entry: Prefetch): void {
  if (pending.get(key) !== entry) return;
  pending.delete(key); retainedBytes -= entry.bytes;
  clearTimeout(entry.timer); entry.controller.abort();
}
export function forgetTask(node: string, task: string): void {
  const key = node + ':' + task, entry = pending.get(key);
  if (entry) remove(key, entry);
}
export function prefetchTask(node: string, task: string): void {
  const key = node + ':' + task;
  if ((pending.get(key)?.expires ?? 0) > Date.now()) return;
  const previous = pending.get(key);
  if (previous) remove(key, previous);
  // Pointer movement must not start a board's worth of simultaneous detail reads.
  if ([...pending.values()].filter(entry => !entry.value).length >= 2) return;
  if (pending.size >= 24) { const [oldest, entry] = pending.entries().next().value!; remove(oldest, entry); }
  const controller = new AbortController();
  const entry: Prefetch = { expires: Date.now() + 5000, controller, bytes: 0,
    timer: setTimeout(() => remove(key, entry), 5000) };
  pending.set(key, entry);
  void (nativeRead(client, node, 'task-board.read', { task }, controller.signal) as Promise<TaskBoard.TaskView>).then(value => {
    if (pending.get(key) !== entry) return;
    const bytes = payloadBytes(value, maximumEntryBytes);
    if (bytes > maximumEntryBytes) { remove(key, entry); return; }
    while (retainedBytes + bytes > maximumBytes) {
      const oldest = pending.entries().next().value!;
      if (oldest[1] === entry) { remove(key, entry); return; }
      remove(...oldest);
    }
    entry.value = value; entry.bytes = bytes; retainedBytes += bytes;
  }, () => remove(key, entry));
}
export async function readTask(node: string, task: string, signal: AbortSignal): Promise<TaskBoard.TaskView> {
  signal.throwIfAborted();
  const key = node + ':' + task, cached = pending.get(key);
  if (cached?.value && cached.expires > Date.now()) { remove(key, cached); return cached.value; }
  // Join the SDK's existing in-flight read before releasing the hover's interest.
  // The consuming panel now owns cancellation; another panel remains independent.
  const read = nativeRead(client, node, 'task-board.read', { task }, signal) as Promise<TaskBoard.TaskView>;
  if (cached) remove(key, cached);
  return read;
}
