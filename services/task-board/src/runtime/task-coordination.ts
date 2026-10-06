import { hashJson, requireThat } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import type { TaskBoardStore } from './store.js';

export const peerComment = (comment: TaskBoard.Comment): boolean => comment.authorKind === 'agent' && !!comment.sourceTaskId;
export function coordinationPending(task: TaskBoard.Task): boolean {
  return !!task.primaryResourceRef && ['review', 'waiting'].includes(task.workflowState) &&
    task.commentDeliveries.some(delivery => delivery.state === 'queued' && task.comments.some(comment => comment.commentId === delivery.commentId && peerComment(comment)));
}
export async function workspaceIdentity(store: TaskBoardStore, task: TaskBoard.Task): Promise<string | null> {
  const workspace = task.claim?.workspace ?? (task.lastRun ? (await store.read('task-board/run', task.lastRun)).value.workspace : null);
  if (!workspace) return null;
  const path = workspace.canonicalCwd.replaceAll('\\', '/').replace(/\/$/, '');
  return workspace.hostId + ':' + (/^[A-Za-z]:\//.test(path) || path.startsWith('//') ? path.toLowerCase() : path);
}
export async function peerState(store: TaskBoardStore, task: TaskBoard.Task, taskId: string): Promise<string> {
  const identity = await workspaceIdentity(store, task);
  if (!identity) return hashJson([]);
  const states: string[] = []; let cursor: string | undefined;
  do {
    const page = await store.page('task-board/task', { limit: 100, ...(cursor ? { cursor } : {}),
      where: { op: 'in', field: 'data:/workflowState', value: ['todo', 'in_progress', 'waiting', 'review'] } });
    for (const item of page.items) if (item.objectId !== taskId) {
      const peer = (await store.read('task-board/task', { objectId: item.objectId, revision: item.revision })).value;
      if (await workspaceIdentity(store, peer) === identity) states.push(item.objectId + ':' + peer.workflowState);
    }
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
  return hashJson(states.sort());
}
export async function checkBlocker(store: TaskBoardStore, blocker: TaskBoard.Blocker | undefined): Promise<void> {
  if (!blocker) return;
  const other = (await store.read('task-board/task', blocker.taskId)).value;
  const running = other.lastRun && !['completed', 'failed', 'cancelled'].includes((await store.read('task-board/run', other.lastRun)).value.phase);
  requireThat(blocker.until === 'done' ? other.workflowState === 'done' : !other.claim && !other.publication && !running,
    'task_board_dependency_waiting', `Waiting for Task ${other.taskKey} to be ${blocker.until}.`);
}
export async function validateBlocker(store: TaskBoardStore, blocker: TaskBoard.Blocker, self: string): Promise<void> {
  const pending = [blocker.taskId], seen = new Set<string>();
  while (pending.length) {
    const id = pending.pop()!;
    requireThat(id !== self, 'task_board_dependency_cycle', 'A task cannot wait on itself, directly or indirectly.');
    if (seen.has(id)) continue;
    requireThat(seen.size < 512, 'limit_exceeded', 'The blocking graph is too large.'); seen.add(id);
    const task = (await store.read('task-board/task', id)).value;
    pending.push(...task.fields.dependencies, ...(task.blocker ? [task.blocker.taskId] : []));
  }
}
