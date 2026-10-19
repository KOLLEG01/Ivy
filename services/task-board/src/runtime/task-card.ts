import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import { crossAgentComment, openQuestions } from './conversation.js';

/** Board reads carry counts and execution facts, never conversation bodies or attachments. */
export function taskCard(view: TaskBoard.TaskView): TaskBoard.TaskCardView {
  const task = view.task;
  const fields = Object.fromEntries(['title', 'category', 'control', 'priority', 'executionRequirement', 'nextReviewAt', 'dueAt', 'dependencies']
    .map(key => [key, task.fields[key as keyof TaskBoard.TaskFields]])) as TaskBoard.TaskCard['fields'];
  const comments = task.comments;
  return { object: view.object, executionCondition: view.executionCondition, unreadAgentComments: view.unreadAgentComments,
    unreadDirectAgentComments: view.unreadDirectAgentComments,
    commentCount: comments.length, directCommentCount: comments.filter(comment => !crossAgentComment(comment, view.object.objectId)).length,
    task: { taskKey: task.taskKey, fields, workflowState: task.workflowState, waiting: task.waiting, claim: task.claim,
      publication: task.publication, latestResult: task.latestResult, openQuestionCount: openQuestions(task).length,
      questionWait: task.questionWait ?? null, blocker: task.blocker ?? null, scheduledCheckFailure: task.scheduledCheck?.failure?.detail ?? '' } };
}
