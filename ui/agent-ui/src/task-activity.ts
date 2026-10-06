import type { Operation } from '../../../packages/sdk/src/client.js';
import { record, text } from '../../../packages/ui-client/src/native.js';

export type TaskActivity = 'working' | 'unread' | null;
/** A lifecycle event observed live; it outranks the inventory until the inventory can have caught up. */
export interface LiveTaskState { working: boolean; at: number }
/** Browser-local read state: when each task was last seen, and when tracking began. */
export interface SeenTasks { since: number; tasks: Record<string, number> }

export const liveStateMs = 90_000;
const seenLimit = 500;
export const taskKey = (serviceNodeId: string, threadId: string) => serviceNodeId + ':' + threadId;

export function taskActivity(item: Operation.InventoryItem, live: Map<string, LiveTaskState>, seen: SeenTasks, openKey: string, now = Date.now()): TaskActivity {
  const key = taskKey(item.resourceRef.serviceNodeId, item.resourceRef.nativeId), summary = record(item.summary), event = live.get(key);
  const fresh = event && now - event.at < liveStateMs ? event : undefined;
  if (fresh ? fresh.working : text(record(summary.status).type) === 'active') return 'working';
  if (key === openKey) return null;
  const lastSeen = seen.tasks[key] ?? seen.since, updatedAt = Number(summary.updatedAt) * 1000;
  return (fresh && fresh.at > lastSeen) || (Number.isFinite(updatedAt) && updatedAt > lastSeen) ? 'unread' : null;
}

/** Marks a task seen at the later of browser time and the host's own update time, so clock skew cannot resurrect it. */
export function markSeen(seen: SeenTasks, key: string, updatedAtSeconds: unknown, now = Date.now()): SeenTasks {
  const updatedAt = Number(updatedAtSeconds) * 1000;
  const tasks = { ...seen.tasks, [key]: Math.max(now, Number.isFinite(updatedAt) ? updatedAt : 0) };
  const kept = Object.entries(tasks).sort((a, b) => b[1] - a[1]).slice(0, seenLimit);
  return { since: seen.since, tasks: Object.fromEntries(kept) };
}

export function readSeen(value: string | null, now = Date.now()): SeenTasks {
  try {
    const parsed = record(JSON.parse(value ?? 'null')), tasks = record(parsed.tasks);
    if (typeof parsed.since === 'number') return { since: parsed.since, tasks: Object.fromEntries(Object.entries(tasks).filter((entry): entry is [string, number] => typeof entry[1] === 'number')) };
  } catch { /* Unreadable view state starts a new baseline. */ }
  // Tasks that finished before this browser tracked them are not reported as unread.
  return { since: now, tasks: {} };
}

/** The working state a native lifecycle notification implies, or undefined when it says nothing about it. */
export function liveWorking(method: string, params: unknown): boolean | undefined {
  if (method === 'turn/started') return true;
  if (method === 'turn/completed') return false;
  if (method === 'thread/status/changed') return text(record(record(params).status).type) === 'active';
  return undefined;
}
