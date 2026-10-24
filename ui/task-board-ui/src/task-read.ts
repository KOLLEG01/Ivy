import { nativeRead } from '../../../packages/ui-client/src/native';
import { client, type TaskBoard } from './runtime';

// Start the ticket read while the user points at its card. Consume each snapshot
// once, so subsequent refreshes always consult the service again.
const pending = new Map<string, { expires: number; read: Promise<TaskBoard.TaskView> }>();
export function prefetchTask(node: string, task: string): void {
  const key = node + ':' + task;
  if ((pending.get(key)?.expires ?? 0) > Date.now()) return;
  if (pending.size >= 24) pending.delete(pending.keys().next().value!);
  const read = nativeRead(client, node, 'task-board.read', { task }, AbortSignal.timeout(15000)) as Promise<TaskBoard.TaskView>;
  const entry = { expires: Date.now() + 5000, read };
  pending.set(key, entry);
  void read.catch(() => { if (pending.get(key) === entry) pending.delete(key); });
}
export function readTask(node: string, task: string, signal: AbortSignal): Promise<TaskBoard.TaskView> {
  const key = node + ':' + task, cached = pending.get(key);
  pending.delete(key);
  return cached && cached.expires > Date.now() ? cached.read :
    nativeRead(client, node, 'task-board.read', { task }, signal) as Promise<TaskBoard.TaskView>;
}
