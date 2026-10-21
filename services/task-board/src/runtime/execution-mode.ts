import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import { commentsPending } from './task-coordination.js';

export const triggeredByComments = (request: Pick<TaskBoard.ExecutionRequest, 'commentIds'>): boolean => request.commentIds.length > 0;

export function nextExecutionMode(task: TaskBoard.Task, at: string): 'execution' | null {
  if (task.claim || task.publication) return null;
  if (commentsPending(task)) return 'execution';
  if (task.fields.control !== 'agent' || ['backlog', 'done', 'cancelled'].includes(task.workflowState)) return null;
  return task.completionIssue && (!task.completionIssue.retryAt || task.completionIssue.retryAt > at) ? null : 'execution';
}
