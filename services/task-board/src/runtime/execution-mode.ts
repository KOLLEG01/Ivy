import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import { coordinationPending } from './task-coordination.js';

export type ExecutionMode = 'execution' | 'conversation' | 'recovery';
export const missingHandoff = (task: TaskBoard.Task): boolean => task.completionIssue?.code === 'task_board_handoff_missing';

export function executionMode(request: TaskBoard.ExecutionRequest): ExecutionMode {
  return request.action === 'continue' && request.recoveryFor ? 'recovery' :
    request.coordinationOnly ? 'conversation' : 'execution';
}

export function nextExecutionMode(task: TaskBoard.Task, at: string): ExecutionMode | null {
  if (task.claim || task.publication) return null;
  if (coordinationPending(task)) return 'conversation';
  if (task.fields.control !== 'agent' || ['backlog', 'done', 'cancelled'].includes(task.workflowState)) return null;
  if (missingHandoff(task)) {
    if (!task.primaryResourceRef || (task.completionIssue?.attempts ?? 0) >= 2 ||
      task.completionIssue?.retryAt && task.completionIssue.retryAt > at) return null;
    return 'recovery';
  }
  return task.completionIssue ? null : 'execution';
}
