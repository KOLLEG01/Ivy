import { hashJson, requireThat } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import type { TaskBoardStore } from './store.js';
import { openQuestions } from './conversation.js';

export const peerComment = (comment: TaskBoard.Comment): boolean => comment.authorKind === 'agent' && !!comment.sourceTaskId;
export function coordinationPending(task: TaskBoard.Task): boolean {
  return task.commentDeliveries.some(delivery => delivery.state === 'queued' && task.comments.some(comment =>
    comment.commentId === delivery.commentId && (comment.authorKind === 'user' || peerComment(comment))));
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
