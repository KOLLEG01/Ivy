import type { TaskBoard } from '../../../../packages/sdk/src/node.js';

/** The date remains in TaskFields; a scope explicitly authorizes an independent check. */
export function scheduledCheckDue(task: TaskBoard.Task, at: string): boolean {
  return !!task.scheduledCheck && !!task.fields.nextReviewAt && task.fields.nextReviewAt <= at &&
    ['todo', 'waiting', 'review'].includes(task.workflowState);
}

export function applyHandoff(task: TaskBoard.Task, handoff: TaskBoard.Handoff, at: string): TaskBoard.Task {
  const { scheduledCheck: _check, blocker: _blocker, questionWait, ...rest } = task;
  const linkedQuestion = handoff.questionWait === null ? undefined : handoff.questionWait ?? questionWait;
  return { ...rest, workflowState: handoff.workflowState,
    fields: { ...task.fields, nextReviewAt: handoff.nextReviewAt },
    waiting: handoff.workflowState === 'waiting' ? { reason: handoff.waiting!.reason, detail: handoff.waiting!.detail, since: at } : null,
    ...(handoff.waiting?.blocker ? { blocker: handoff.waiting.blocker } : handoff.workflowState !== 'todo' && task.blocker ? { blocker: task.blocker } : {}),
    ...(linkedQuestion ? { questionWait: linkedQuestion } : {}),
    ...(handoff.checkScope ? { scheduledCheck: { scope: handoff.checkScope,
      ...(handoff.checkFailure ? { failure: { ...handoff.checkFailure, at } } : {}) } } : {}) };
}

/** Retrying a resolved failed check cannot lose the pending question or its approved scope. */
export function retryScheduledCheck(task: TaskBoard.Task, check: TaskBoard.ScheduledCheck, at: string, code: string, detail: string, retryAt?: string, initialAt?: string): TaskBoard.Task {
  // A confirmed decision made during this check takes precedence over recovery defaults.
  if (initialAt && task.fields.nextReviewAt === null) {
    const { scheduledCheck: _check, ...stopped } = task;
    return stopped;
  }
  if (initialAt && task.scheduledCheck && task.scheduledCheck.scope !== check.scope) return task;
  const retry = retryAt ?? new Date(Date.parse(at) + 15 * 60_000).toISOString();
  const nextReviewAt = task.fields.nextReviewAt && task.fields.nextReviewAt > retry ? task.fields.nextReviewAt : retry;
  return { ...task, fields: { ...task.fields, nextReviewAt },
    scheduledCheck: { scope: check.scope, failure: { code, detail: detail.slice(0, 2048) || code, at } } };
}
