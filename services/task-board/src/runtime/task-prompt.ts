import type { TaskBoard } from '../../../../packages/sdk/src/node.js';

export function taskPrompt(task: TaskBoard.Task, serviceNodeId: string): string {
  return 'TaskBoard ' + task.taskKey + ', Ivy MCP service ' + serviceNodeId +
    '. Read the ticket and new comments. Decide from the current ticket whether to reply, continue work or wait, and act in this turn.';
}
