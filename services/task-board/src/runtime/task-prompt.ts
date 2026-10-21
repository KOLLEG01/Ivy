import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import { taskBoardAttachmentGuidance } from '../../../../instructions/task-board.js';
import { missingWakeTrigger } from './wake.js';

export function taskPrompt(task: TaskBoard.Task, serviceNodeId: string, executionChangeRequested = false): string {
  if (executionChangeRequested) return 'TaskBoard ' + task.taskKey + ', Ivy MCP service ' + serviceNodeId +
    '. Your execution settings have changed. Read the ticket and new comments. At the next safe stopping point, save your work, briefly describe what remains, and end this turn. ' +
    'TaskBoard will continue this same conversation with the latest settings. Do not mark the ticket ready for review merely for this settings change.';
  return 'TaskBoard ' + task.taskKey + ', Ivy MCP service ' + serviceNodeId +
    '. Read the ticket and new comments. Decide from the current ticket whether to reply, continue work or wait, and act in this turn. ' +
    (missingWakeTrigger(task) ? 'This ticket was waiting without a wake-up trigger. Continue work, submit the completed result for review, or record a question, watched ticket or next appointment. ' : '') +
    taskBoardAttachmentGuidance;
}
