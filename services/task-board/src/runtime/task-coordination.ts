import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import type { TaskBoardStore } from './store.js';

export const peerComment = (comment: TaskBoard.Comment): boolean => comment.authorKind === 'agent' && !!comment.sourceTaskId;
export const queuedInput = (comment: TaskBoard.Comment): boolean => comment.authorKind === 'user' || peerComment(comment) ||
  comment.authorKind === 'system' && comment.body === 'Ticket edited' && !comment.nativeInput;
export function commentsPending(task: TaskBoard.Task): boolean {
  return task.commentDeliveries.some(delivery => delivery.state === 'queued' && task.comments.some(comment =>
    comment.commentId === delivery.commentId && queuedInput(comment)));
}
export async function workspaceIdentity(store: TaskBoardStore, task: TaskBoard.Task): Promise<string | null> {
  const workspace = task.claim?.workspace ?? (task.lastRun ? (await store.read('task-board/run', task.lastRun)).value.workspace : null);
  if (!workspace) return null;
  const path = workspace.canonicalCwd.replaceAll('\\', '/').replace(/\/$/, '');
  return workspace.hostId + ':' + (/^[A-Za-z]:\//.test(path) || path.startsWith('//') ? path.toLowerCase() : path);
}
