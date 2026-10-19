import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import { coordinationPending } from './task-coordination.js';
import { scheduledCheckDue } from './handoff.js';
import { queuedUserInput } from './conversation.js';

export type ExecutionMode = 'work' | 'coordination' | 'check' | 'recovery';
export const missingHandoff = (task: TaskBoard.Task): boolean => task.completionIssue ? task.completionIssue.code === 'task_board_handoff_missing' :
  task.waiting?.detail === 'Execution ended without an explicit completion intent; review the ticket before continuing.';

/** One mode, shared by admission, scheduling and publication; prompts are not authority. */
export function executionMode(request: TaskBoard.ExecutionRequest): ExecutionMode {
  const recovery = request.action === 'continue' && !!request.recoveryFor;
  const coordination = request.action === 'continue' && !!request.coordinationOnly;
  requireThat(Number(recovery) + Number(coordination) + Number(!!request.checkOnly) <= 1,
    'task_board_invalid_mode', 'Execution modes are mutually exclusive.');
  return recovery ? 'recovery' : coordination ? 'coordination' : request.checkOnly ? 'check' : 'work';
}

/** Missing bookkeeping has priority over peer messages and business waits, never over a live claim. */
export function nextExecutionMode(task: TaskBoard.Task, at: string): ExecutionMode | null {
  if (task.fields.control !== 'agent' || task.claim || task.publication ||
      ['backlog', 'done', 'cancelled'].includes(task.workflowState)) return null;
  const freshUserInput = queuedUserInput(task) && (!task.waiting || task.comments.some(comment =>
    comment.authorKind === 'user' && comment.createdAt > task.waiting!.since &&
    task.commentDeliveries.some(delivery => delivery.commentId === comment.commentId && delivery.state === 'queued')));
  if (missingHandoff(task)) {
    if ((task.completionIssue?.attempts ?? 0) >= 2 && freshUserInput) return 'work';
    if (!task.primaryResourceRef || (task.completionIssue?.attempts ?? 0) >= 2 ||
        task.completionIssue?.retryAt && task.completionIssue.retryAt > at) return null;
    return 'recovery';
  }
  if (task.completionIssue && !freshUserInput) return null;
  if (coordinationPending(task)) return 'coordination';
  if (scheduledCheckDue(task, at) && !(task.workflowState === 'todo' && queuedUserInput(task))) return 'check';
  return ['todo', 'waiting'].includes(task.workflowState) ? 'work' : null;
}
