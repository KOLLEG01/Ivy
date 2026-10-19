import type { TaskBoard } from '../../../../packages/sdk/src/node.js';

export function runHandoff(task: TaskBoard.Task, runId: string): TaskBoard.Comment | undefined {
  return [...task.comments].reverse().find(comment => comment.purpose === 'handoff' && comment.run?.objectId === runId);
}

/** Only an addressed answer or an explicit resolution closes a ticket question. */
export function openQuestions(task: Pick<TaskBoard.Task, 'comments'>, runId?: string): TaskBoard.Comment[] {
  return task.comments.filter((comment, index) => {
    if (comment.purpose !== 'question' || comment.sourceTaskId || comment.replyTo) return false;
    if (comment.nativeInput) {
      if (runId && comment.run?.objectId !== runId) return false;
      return !task.comments.slice(index + 1).some(reply => reply.replyTo === comment.commentId && !!reply.nativeInput && ['answered', 'expired'].includes(reply.nativeInput.state));
    }
    return !task.comments.slice(index + 1).some(reply =>
      reply.resolvesQuestions?.includes(comment.commentId) ||
      reply.authorKind === 'user' && reply.replyTo === comment.commentId ||
      comment.requests.length > 0 && comment.requests.every(request =>
        task.comments.slice(index + 1).some(answer => answer.responses.some(response => response.requestId === request.requestId))));
  });
}

export function unansweredQuestion(task: TaskBoard.Task, runId: string): boolean {
  return openQuestions(task, runId).length > 0;
}

export function queuedUserInput(task: Pick<TaskBoard.Task, 'comments' | 'commentDeliveries'>): boolean {
  return task.commentDeliveries.some(delivery => delivery.state === 'queued' &&
    task.comments.some(comment => comment.commentId === delivery.commentId && comment.authorKind === 'user'));
}

/** These queued markers refer to fields already included in the pinned check snapshot. */
export function queuedTaskEdits(task: Pick<TaskBoard.Task, 'comments' | 'commentDeliveries'>): string[] {
  return task.commentDeliveries.filter(delivery => delivery.state === 'queued' && task.comments.some(comment =>
    comment.commentId === delivery.commentId && comment.authorKind === 'system' && comment.body === 'Ticket edited' && !comment.nativeInput))
    .map(delivery => delivery.commentId);
}
