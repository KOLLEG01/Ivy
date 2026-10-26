import type { TaskBoard } from '../../sdk/src/client.js';
export { openTaskQuestions as openQuestions } from '../../sdk/src/client.js';

/** Directed comments from a different ticket; a ticket's own worker stays visible. */
export const crossAgentComment = (comment: TaskBoard.Comment, taskId: string): boolean =>
  comment.recipient?.kind === 'task' || !!comment.questionSource ||
  comment.authorKind === 'agent' && !!comment.sourceTaskId && comment.sourceTaskId !== taskId;
