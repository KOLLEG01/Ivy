import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import { crossAgentComment, openQuestions } from './conversation.js';

export function taskCard(view: TaskBoard.TaskView): TaskBoard.TaskCardView {
  const task = view.task;
  const fields = Object.fromEntries(['title', 'category', 'control', 'priority', 'executionRequirement', 'dueAt']
    .map(key => [key, task.fields[key as keyof TaskBoard.TaskFields]])) as TaskBoard.TaskCardFields;
  const comments = task.comments;
  return { object: view.object, executionCondition: view.executionCondition,
    unreadAgentComments: view.unreadAgentComments, unreadDirectAgentComments: view.unreadDirectAgentComments,
    commentCount: comments.length, directCommentCount: comments.filter(comment => !crossAgentComment(comment, view.object.objectId)).length,
    task: { taskKey: task.taskKey, fields, workflowState: task.workflowState, runtimeWait: task.runtimeWait,
      nextSteps: task.nextSteps, claim: task.claim, publication: task.publication,
      latestResult: task.latestResult, openQuestionCount: openQuestions(task).length } };
}
