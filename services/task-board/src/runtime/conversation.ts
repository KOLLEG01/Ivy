import type { TaskBoard } from '../../../../packages/sdk/src/node.js';

/** Directed comments from a different ticket; a ticket's own worker stays visible. */
export const crossAgentComment = (comment: TaskBoard.Comment, taskId: string): boolean =>
  comment.authorKind === 'agent' && !!comment.sourceTaskId && comment.sourceTaskId !== taskId;

export function unreadComments(task: Pick<TaskBoard.Task, 'comments'>, taskId: string, state?: TaskBoard.CommentReadState) {
  const agents = task.comments.filter(comment => comment.authorKind === 'agent');
  const prefix = Math.min(state?.readAgentCommentCount ?? 0, agents.length);
  const direct = agents.filter(comment => !crossAgentComment(comment, taskId));
  const readDirect = Math.max(state?.readDirectAgentCommentCount ?? 0,
    agents.slice(0, prefix).filter(comment => !crossAgentComment(comment, taskId)).length);
  const unreadDirectAgentComments = Math.max(0, direct.length - readDirect);
  return { unreadAgentComments: unreadDirectAgentComments + agents.slice(prefix).filter(comment => crossAgentComment(comment, taskId)).length,
    unreadDirectAgentComments };
}

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
