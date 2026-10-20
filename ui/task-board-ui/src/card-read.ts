import { nativeRead } from '../../../packages/ui-client/src/native';
import { client } from './runtime';
import type { TaskBoard } from './runtime';

// Global task hints refresh the query, but unchanged card revisions need no provider call.
const cards = new Map<string, { revision: number; expires: number; view: TaskBoard.TaskCardView }>();
export async function readCard(node: string, id: string, revision: number, signal: AbortSignal): Promise<TaskBoard.TaskCardView> {
  const key = node + ':' + id, cached = cards.get(key);
  if (cached?.revision === revision && cached.expires > Date.now()) return cached.view;
  const view = await nativeRead(client, node, 'task-board.read', { task: id, view: 'card' }, signal) as TaskBoard.TaskCardView;
  if (cards.size >= 512) cards.delete(cards.keys().next().value!);
  // Waiting conditions can change when another ticket publishes, or when a timer expires.
  const expires = ['todo', 'waiting', 'review'].includes(view.task.workflowState) ? Date.now() + 5_000 : Infinity;
  cards.set(key, { revision: view.object.revision, expires, view });
  return view;
}
