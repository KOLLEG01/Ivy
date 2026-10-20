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
      latestResult: task.latestResult, openQuestionCount: openQuestions(task).filter(question => question.recipient?.kind !== 'task').length } };
}

/** Public reads are bounded; workflow code retains its exact full snapshot. */
export function taskViewPage(view: TaskBoard.TaskView, query: TaskBoard.TaskQuery): TaskBoard.TaskView {
  const all = query.comments?.questionsOnly ? openQuestions(view.task) : view.task.comments;
  const limit = query.comments?.limit ?? 50;
  const offset = query.comments?.offset ?? Math.max(0, all.length - limit);
  const comments = all.slice(offset, offset + limit);
  return { ...view, task: { ...view.task, comments,
    commentDeliveries: view.task.commentDeliveries.filter(receipt => comments.some(comment => comment.commentId === receipt.commentId)) },
    openQuestions: openQuestions(view.task).filter(question => question.recipient?.kind !== 'task'),
    commentPage: { total: all.length, offset, nextOffset: offset + comments.length < all.length ? offset + comments.length : null } };
}
