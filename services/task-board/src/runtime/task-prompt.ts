import type { TaskBoard } from '../../../../packages/sdk/src/node.js';

export function taskPrompt(task: TaskBoard.Task, serviceNodeId: string, recoveryOnly = false): string {
  const reference = 'TaskBoard ' + task.taskKey + ', Ivy MCP service ' + serviceNodeId + '. Read the ticket and new comments.';
  if (recoveryOnly) return reference + ' Recover the original missing completion intent from retained evidence in this same context. Do not repeat implementation, external effects or uncertain original operations. Publish one agent handoff with nextSteps and actual readiness. Keep genuine own questions open.';
  return reference + ' Decide from the current ticket whether to reply, continue work or wait, and act in this turn.';
}
