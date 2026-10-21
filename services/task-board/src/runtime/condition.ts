import type { TaskBoard } from '../../../../packages/sdk/src/node.js';

export interface ExecutionFacts {
  workflowState: TaskBoard.Task['workflowState'];
  control: TaskBoard.TaskFields['control'];
  publication: boolean;
  claim: { phase: TaskBoard.Claim['phase']; hostId: string; mode?: string } | null;
  runtimeWait: TaskBoard.RuntimeWait | null;
  executionRequirement: TaskBoard.ExecutionRequirement | null;
  requiredCapabilities?: string[] | undefined;
  completionIssue?: string | undefined;
  wake?: TaskBoard.ExecutionCondition | undefined;
}

export function claimedWorkflowState(runtimeWait: Pick<TaskBoard.RuntimeWait, 'reason'> | null): 'waiting' | 'in_progress' {
  return 'in_progress';
}

export function executionCondition(task: ExecutionFacts): TaskBoard.ExecutionCondition {
  if (task.publication) return { state: 'publishing', detail: 'Publishing the current task update.' };
  if (task.claim) {
    if (task.claim.phase === 'waiting_input') return { state: 'needs_user', detail: 'The native execution needs an owner-bound answer.' };
    if (['outcome_unknown', 'cancel_requested'].includes(task.claim.phase)) return { state: 'recovering', detail: 'Reconciling the original native execution.' };
    return { state: task.claim.phase === 'publishing_result' ? 'publishing' : ['claimed', 'run_created', 'starting'].includes(task.claim.phase) ? 'starting' : 'active',
      detail: 'Working on ' + task.claim.hostId + '.' };
  }
  if (['backlog', 'done', 'cancelled'].includes(task.workflowState)) return { state: 'idle', detail: 'No task work is scheduled.' };
  if (task.completionIssue) return { state: 'failed', detail: task.completionIssue };
  if (task.runtimeWait) return { state: task.runtimeWait.reason === 'external_outcome' ? 'recovering' : 'blocked_environment',
    detail: task.runtimeWait.detail };
  if (task.workflowState === 'review') return { state: 'needs_user', detail: 'The final result is ready for review.' };
  if (task.wake) return task.wake;
  return { state: task.control === 'user' ? 'needs_user' : 'queued', detail: task.control === 'user' ? 'Ready for manual work.' : 'Waiting for the agent to continue.' };
}
