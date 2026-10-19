import { hashJson, requireThat } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import type { TaskBoardStore } from './store.js';
import { openQuestions } from './conversation.js';

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
export async function checkBlocker(store: TaskBoardStore, blocker: TaskBoard.Blocker | undefined,
  consumed: TaskBoard.BlockerObservation[] = [], allowConsumed = false): Promise<TaskBoard.BlockerObservation | undefined> {
  if (!blocker) return;
  const other = (await store.read('task-board/task', blocker.taskId, true, true)).value;
  const running = other.lastRun && !['completed', 'failed', 'cancelled'].includes((await store.read('task-board/run', other.lastRun, true, true)).value.phase);
  const delivered = blocker.until === 'delivery' ? other.deliveries?.find(value => value.delivery.key === blocker.key) : undefined;
  requireThat(blocker.until === 'delivery' ? !!delivered : blocker.until === 'done' ? other.workflowState === 'done' : !other.claim && !other.publication && !running,
    'task_board_dependency_waiting', blocker.until === 'delivery' ? `Waiting for delivery ${blocker.key} from Task ${other.taskKey}.` : `Waiting for Task ${other.taskKey} to be ${blocker.until}.`);
  const fingerprint = delivered?.identity ?? hashJson({ state: other.workflowState, result: other.latestResult, review: other.acceptedReview });
  requireThat(allowConsumed || !consumed.some(value => hashJson(value.blocker) === hashJson(blocker) && value.fingerprint === fingerprint),
    'task_board_dependency_consumed', `Waiting for new work from Task ${other.taskKey}; this ${blocker.until} condition was already consumed.`);
  return { blocker, fingerprint };
}
export function consumeBlocker(consumed: TaskBoard.BlockerObservation[] = [], observation: TaskBoard.BlockerObservation): TaskBoard.BlockerObservation[] {
  return [...consumed.filter(value => hashJson(value.blocker) !== hashJson(observation.blocker)), observation].slice(-32);
}
export async function questionWaitOpen(store: TaskBoardStore, reference?: TaskBoard.QuestionWait | null): Promise<boolean> {
  if (!reference) return false;
  const source = await store.read('task-board/task', reference.taskId, true, true);
  requireThat(source.value.comments.some(value => value.commentId === reference.commentId && value.purpose === 'question' && !value.sourceTaskId),
    'task_board_comment_mismatch', 'The linked user question must identify an actual ticket question.');
  return openQuestions(source.value).some(value => value.commentId === reference.commentId);
}
export async function validateBlocker(store: TaskBoardStore, blocker: TaskBoard.Blocker, self: string | null): Promise<void> {
  const pending = [blocker.taskId], seen = new Set<string>();
  while (pending.length) {
    const id = pending.pop()!;
    requireThat(id !== self, 'task_board_dependency_cycle', 'A task cannot wait on itself, directly or indirectly.');
    if (seen.has(id)) continue;
    requireThat(seen.size < 512, 'limit_exceeded', 'The blocking graph is too large.'); seen.add(id);
    const task = (await store.read('task-board/task', id, true, true)).value;
    pending.push(...task.fields.dependencies, ...(task.blocker ? [task.blocker.taskId] : []));
  }
}
