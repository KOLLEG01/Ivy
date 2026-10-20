import type { TaskBoard } from './generated.js';

export function openTaskQuestions(task: Pick<TaskBoard.Task, 'comments'>, runId?: string): TaskBoard.Comment[] {
  return task.comments.filter((comment, index) => {
    if (comment.purpose !== 'question' || comment.sourceTaskId) return false;
    if (comment.nativeInput) {
      if (runId && comment.run?.objectId !== runId) return false;
      return !task.comments.slice(index + 1).some(reply => reply.replyTo === comment.commentId && !!reply.nativeInput && ['answered', 'expired'].includes(reply.nativeInput.state));
    }
    return !task.comments.slice(index + 1).some(reply => reply.resolvesQuestions?.includes(comment.commentId) ||
      reply.authorKind === 'user' && reply.replyTo === comment.commentId ||
      comment.requests.length > 0 && comment.requests.every(request => task.comments.slice(index + 1).some(answer => answer.responses.some(response => response.requestId === request.requestId))));
  });
}
