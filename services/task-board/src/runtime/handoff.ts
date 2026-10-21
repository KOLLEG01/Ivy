import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import { same } from './checks.js';

/** Routing, priority, contact and dates do not change the authorized assignment. */
export function sameAssignment(left: Omit<TaskBoard.TaskFields, 'userContact'>, right: Omit<TaskBoard.TaskFields, 'userContact'>): boolean {
  return (['title', 'description', 'acceptanceCriteria', 'control', 'requiredCapabilities', 'executionRequirement',
    'workspaceRequirement', 'nativeOptions', 'allowParallel'] as const).every(key => same(left[key], right[key]));
}

export function applyHandoff(task: TaskBoard.Task, handoff: TaskBoard.Handoff, at: string): TaskBoard.Task {
  return { ...task, workflowState: handoff.workflowState, runtimeWait: null, updatedAt: at };
}
