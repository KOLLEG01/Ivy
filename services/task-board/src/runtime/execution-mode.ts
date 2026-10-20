import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import { commentsPending } from './task-coordination.js';

export type ExecutionMode = 'execution' | 'recovery';
export const missingHandoff = (task: TaskBoard.Task): boolean => task.completionIssue?.code === 'task_board_handoff_missing';

export function executionMode(request: TaskBoard.ExecutionRequest): ExecutionMode {
  return request.action === 'continue' && request.recoveryFor ? 'recovery' : 'execution';
}

/** Comment delivery needs no ready scheduled step and may end with just a reply. */
export const triggeredByComments = (request: TaskBoard.ExecutionRequest): boolean =>
  executionMode(request) !== 'recovery' && !request.nextStepId && request.commentIds.length > 0;

export function nextExecutionMode(task: TaskBoard.Task, at: string): ExecutionMode | null {
  if (task.claim || task.publication) return null;
  if (commentsPending(task)) return 'execution';
  if (task.fields.control !== 'agent' || ['backlog', 'done', 'cancelled'].includes(task.workflowState)) return null;
  if (missingHandoff(task)) {
    if (!task.primaryResourceRef || (task.completionIssue?.attempts ?? 0) >= 2 ||
      task.completionIssue?.retryAt && task.completionIssue.retryAt > at) return null;
    return 'recovery';
  }
  return task.completionIssue ? null : 'execution';
}
