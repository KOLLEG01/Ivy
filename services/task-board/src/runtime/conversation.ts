import type { TaskBoard } from '../../../../packages/sdk/src/node.js';

export function runHandoff(task: TaskBoard.Task, runId: string): TaskBoard.Comment | undefined {
  return [...task.comments].reverse().find(comment => comment.purpose === 'handoff' && comment.run?.objectId === runId);
}

/** Questions remain open until a user response or an explicit worker handoff. */
export function unansweredQuestion(task: TaskBoard.Task, runId: string): boolean {
  return task.comments.some((comment, index) => {
    if (comment.purpose !== 'question' || comment.sourceTaskId) return false;
    if (comment.nativeInput) {
      if (comment.run?.objectId !== runId) return false;
      return !task.comments.slice(index + 1).some(reply => reply.replyTo === comment.commentId);
    }
    return !task.comments.slice(index + 1).some(reply => reply.authorKind === 'user' || reply.purpose === 'handoff');
  });
}
