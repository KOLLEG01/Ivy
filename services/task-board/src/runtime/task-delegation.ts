import { hashJson, requireThat } from '../../../../packages/sdk/src/node.js';
import { scopedOperationId } from '../../../../packages/sdk/src/client.js';
import type { TaskBoardEngine } from './engine.js';
import type { Document, TaskBoardStore } from './store.js';

export async function validateParent(store: TaskBoardStore, parentTaskId: string, self?: string): Promise<void> {
  const seen = new Set(self ? [self] : []);
  let id: string | undefined = parentTaskId;
  while (id) {
    requireThat(!seen.has(id), 'task_board_dependency_cycle', 'A task cannot delegate to itself, directly or indirectly.');
    requireThat(seen.size < 64, 'limit_exceeded', 'The delegation chain is too large.');
    seen.add(id);
    const parent: Document<'task-board/task'> = await store.read('task-board/task', id);
    requireThat(!parent.metadata.effectivelyArchived && parent.value.fields.control === 'agent' &&
      !['done', 'cancelled'].includes(parent.value.workflowState), 'task_board_invalid_parent', 'The delegating task must be an unfinished, unarchived agent task.');
    id = parent.value.parentTaskId;
  }
}

/** Deliver each saved final result once; acceptance stays with the delegating task. */
export async function forwardDelegatedReview(engine: TaskBoardEngine, child: Document<'task-board/task'>): Promise<void> {
  const { parentTaskId, workflowState, latestResult } = child.value;
  if (!parentTaskId || workflowState !== 'review' || !latestResult || child.value.claim || child.value.publication) return;
  const parent = await engine.store.read('task-board/task', parentTaskId, true, true);
  if (parent.metadata.effectivelyArchived || parent.value.publication || ['done', 'cancelled'].includes(parent.value.workflowState)) return;
  const commentId = 'delegated-review-' + hashJson([child.pin.objectId, latestResult]).slice('sha256:'.length);
  if (parent.value.comments.some(comment => comment.commentId === commentId)) return;
  await engine.checks.aggregate(child.value, child.pin.objectId);
  const operationId = await scopedOperationId(engine.store.client, ['delegated-review', child.pin.objectId, latestResult, parent.pin]);
  await engine.nativeComment({ action: 'comment', operationId, taskId: parentTaskId, expectedRevision: parent.pin.revision,
    commentId, authorKind: 'agent', sourceTaskId: child.pin.objectId, agentDelivery: 'queue', purpose: 'update',
    body: `Delegated task ${child.value.taskKey} is ready for your review. Read its saved final result, verify the delivered scope and integration, then move the accepted task to Done and archive it. Request corrections when needed; its Review is your responsibility.`,
    requests: [], responses: [], attachments: [], replyTo: null,
  });
}
