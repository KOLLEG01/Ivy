import type { TaskBoard } from './generated.js';
export declare function openTaskQuestions(task: Pick<TaskBoard.Task, 'comments'>, runId?: string): TaskBoard.Comment[];
