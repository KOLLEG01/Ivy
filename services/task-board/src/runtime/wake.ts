import { hashJson, IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import type { TaskBoardStore } from './store.js';
import { commentsPending } from './task-coordination.js';

/** Only material changes count. Claim, publication and acknowledgment churn never wakes peers. */
export async function dependencySnapshot(store: TaskBoardStore, task: Pick<TaskBoard.Task, 'dependencies'>): Promise<Record<string, string>> {
  const entries: [string, string][] = [];
  for (let offset = 0; offset < task.dependencies.length; offset += 8) {
    entries.push(...await Promise.all(task.dependencies.slice(offset, offset + 8).map(async id => {
      try {
        const other = await store.read('task-board/task', id, true, true, true);
        return [id, hashJson({ work: other.value.workRevision,
          review: other.value.acceptedReview, archived: other.metadata.effectivelyArchived,
          terminal: ['done', 'cancelled'].includes(other.value.workflowState) ? other.value.workflowState : null })] as [string, string];
      } catch (error) {
        if (!(error instanceof IvyError && ['not_found', 'object_deleted'].includes(error.code))) throw error;
        return [id, hashJson('deleted')] as [string, string];
      }
    })));
  }
  return Object.fromEntries(entries);
}

export const dependenciesChanged = (task: TaskBoard.Task, current: Record<string, string>): boolean =>
  task.dependencies.some(id => current[id] !== task.observedDependencies?.[id]);

export async function shouldWake(store: TaskBoardStore, task: TaskBoard.Task, at: string): Promise<boolean> {
  if (task.claim || task.publication || task.runtimeWait?.retryAt && task.runtimeWait.retryAt > at) return false;
  if (commentsPending(task)) return true;
  if (task.fields.control !== 'agent' || ['backlog', 'done', 'cancelled'].includes(task.workflowState)) return false;
  if (task.completionIssue) return !!task.completionIssue.retryAt && task.completionIssue.retryAt <= at;
  return task.workflowState === 'todo' || !!task.runtimeWait?.retryAt && task.runtimeWait.retryAt <= at || !!task.wakeAt && task.wakeAt <= at ||
    dependenciesChanged(task, await dependencySnapshot(store, task));
}

export async function validateDependencies(store: TaskBoardStore, self: string | null, ids: string[]): Promise<void> {
  requireThat(!self || !ids.includes(self), 'invalid_arguments', 'A task cannot watch itself.');
  for (const id of ids) await store.read('task-board/task', id, true, true, true);
}

/** Board cards need no foreign reads to explain a wait. */
export function wakeCondition(task: TaskBoard.Task): TaskBoard.ExecutionCondition {
  if (task.workflowState === 'todo') return { state: 'queued', detail: 'Ready for the agent to continue.' };
  if (task.openQuestionCount) return { state: 'needs_user', detail: 'The agent has a question for you. A normal comment wakes it.' };
  if (task.wakeAt) return { state: 'deferred', detail: 'Next wake-up: ' + task.wakeAt + '.' };
  if (task.dependencies.length) return { state: 'blocked_dependency', detail: 'Watching related tickets for changes.' };
  return { state: 'idle', detail: 'Waiting for a comment or ticket change.' };
}
