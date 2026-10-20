import { hashJson, requireThat, validateTaskBoard } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard } from '../../../../packages/sdk/src/node.js';
import type { TaskBoardStore } from './store.js';
import { openQuestions } from './conversation.js';
import type { QuestionIndex } from '../../../../packages/sdk/src/task-comments.js';

export interface StepAssessment {
  step: TaskBoard.NextStep;
  fingerprint: string;
  ready: boolean;
  consumed: boolean;
  pending: TaskBoard.Condition[];
  needsUser?: boolean;
}

export async function assessSteps(store: TaskBoardStore, task: TaskBoard.Task, at: string, overrides = new Map<string, TaskBoard.Task>(), index?: QuestionIndex[]): Promise<StepAssessment[]> {
  const questions = new Set(index ? index.filter(question => question.open).map(question => question.commentId) :
    openQuestions(task).map(question => question.commentId));
  const ownQuestions = index ?? task.comments.filter(comment => comment.purpose === 'question' && !comment.sourceTaskId);
  const sources = new Map<string, Promise<TaskBoard.Task>>();
  const source = (taskId: string) => {
    if (overrides.has(taskId)) return Promise.resolve(overrides.get(taskId)!);
    // A foreign dependency only needs state and named deliveries from its header.
    // Its conversation, including its own questions, is not part of this gate.
    if (!sources.has(taskId)) sources.set(taskId, store.read('task-board/task', taskId, true, true, true).then(value => value.value));
    return sources.get(taskId)!;
  };
  return await Promise.all(task.nextSteps.map(async step => {
    const pending: TaskBoard.Condition[] = [], observations: unknown[] = [];
    for (const condition of step.requires) {
      let satisfied = false, observation: unknown = condition;
      if (condition.kind === 'time') satisfied = condition.at <= at;
      else if (condition.kind === 'user' || condition.kind === 'question') {
        requireThat(ownQuestions.some(comment => comment.commentId === condition.questionId),
          'task_board_comment_mismatch', 'A user condition must identify a question owned by this ticket.');
        satisfied = !questions.has(condition.questionId);
        observation = { condition, resolved: satisfied };
      } else {
        const other = await source(condition.taskId);
        if (condition.kind === 'delivery') {
          const delivery = other.deliveries?.find(receipt => receipt.delivery.key === condition.key);
          satisfied = !!delivery && delivery.identity !== condition.after;
          observation = { condition, identity: delivery?.identity ?? null };
        } else {
          const running = other.lastRun && !['completed', 'failed', 'cancelled'].includes((await store.read('task-board/run', other.lastRun, true, true)).value.phase);
          satisfied = condition.until === 'done' ? other.workflowState === 'done' && !other.publication : !other.claim && !other.publication && !running;
          observation = { condition, state: other.workflowState, result: other.latestResult, review: other.acceptedReview };
        }
      }
      observations.push(observation);
      if (!satisfied) pending.push(condition);
    }
    const fingerprint = hashJson({ step, observations });
    const consumed = task.consumedSteps?.some(value => value.stepId === step.id && value.fingerprint === fingerprint) ?? false;
    const needsUser = pending.some(condition => (condition.kind === 'user' || condition.kind === 'question') &&
      ownQuestions.find(comment => comment.commentId === condition.questionId)?.recipient?.kind !== 'task');
    return { step, fingerprint, ready: pending.length === 0 && !consumed, consumed, pending, needsUser };
  }));
}

export function stepCondition(assessments: StepAssessment[]): TaskBoard.ExecutionCondition {
  if (assessments.some(value => value.ready)) return { state: 'queued', detail: 'An authorized next step is ready.' };
  const pending = assessments.filter(value => !value.consumed);
  const user = (value: StepAssessment) => value.needsUser ?? value.pending.some(condition => condition.kind === 'user' || condition.kind === 'question');
  const automatic = pending.filter(value => !user(value));
  const timed = automatic.filter(value => value.pending.every(condition => condition.kind === 'time'));
  if (timed.length) {
    const at = timed.map(value => value.pending.filter(condition => condition.kind === 'time').map(condition => condition.at).sort().at(-1)!).sort()[0];
    return { state: 'deferred', detail: 'Next authorized step is scheduled for ' + at + '.' };
  }
  if (automatic.some(value => value.pending.some(condition => condition.kind !== 'time')))
    return { state: 'blocked_dependency', detail: 'Waiting for another ticket or its named delivery.' };
  if (pending.some(user))
    return { state: 'needs_user', detail: 'The remaining next steps require an answer to this ticket\'s own question.' };
  return { state: 'workflow_attention', detail: assessments.length ? 'This unchanged next step has already been processed. Publish a new next step or delivery condition.' : 'No next step is recorded.' };
}

export function nextStepTime(task: Pick<TaskBoard.Task, 'nextSteps'>): string | null {
  return task.nextSteps.flatMap(step => step.requires.filter(condition => condition.kind === 'time').map(condition => condition.at)).sort()[0] ?? null;
}

export function consumeStep(task: TaskBoard.Task, selected: StepAssessment): TaskBoard.StepObservation[] {
  return [...(task.consumedSteps ?? []).filter(value => value.stepId !== selected.step.id),
    { stepId: selected.step.id, fingerprint: selected.fingerprint }].slice(-64);
}

export async function validateNextSteps(store: TaskBoardStore, taskId: string | null, steps: TaskBoard.NextStep[], task?: TaskBoard.Task, at = new Date().toISOString()): Promise<void> {
  requireThat(new Set(steps.map(step => step.id)).size === steps.length, 'invalid_arguments', 'Next-step identities must be unique.');
  if (taskId && task) {
    const sources = new Map<string, TaskBoard.Task>([[taskId, { ...task, nextSteps: steps }]]);
    const assessments = new Map<string, StepAssessment[]>();
    const pendingSteps = async (id: string, value: TaskBoard.Task) => {
      if (!assessments.has(id)) assessments.set(id, (await assessSteps(store, value, at, sources)).filter(step => !step.consumed));
      return assessments.get(id)!;
    };
    const closesCycle = async (sourceId: string, path: Set<string>): Promise<boolean> => {
      if (sourceId === taskId) return true;
      if (path.has(sourceId)) return false;
      requireThat(sources.size < 1024, 'task_board_dependency_limit', 'The next-step dependency graph exceeds its validation bound.');
      let other = sources.get(sourceId);
      if (!other) { other = (await store.read('task-board/task', sourceId, true, true)).value; sources.set(sourceId, other); }
      if (other.claim || ['done', 'cancelled', 'review'].includes(other.workflowState) || !other.nextSteps.length) return false;
      const route = new Set([...path, sourceId]);
      const remaining = await pendingSteps(sourceId, other);
      if (!remaining.length) return false;
      for (const step of remaining) {
        let cyclic = false;
        for (const condition of step.pending) if (condition.kind === 'task' || condition.kind === 'delivery') {
          if (await closesCycle(condition.taskId, route)) cyclic = true;
        }
        if (!cyclic) return false;
      }
      return true;
    };
    const remaining = await pendingSteps(taskId, sources.get(taskId)!);
    let allCyclic = remaining.length > 0;
    for (const step of remaining) {
      let cyclic = false;
      for (const condition of step.pending) if ((condition.kind === 'task' || condition.kind === 'delivery') && await closesCycle(condition.taskId, new Set())) cyclic = true;
      if (!cyclic) allCyclic = false;
    }
    requireThat(!allCyclic, 'task_board_dependency_cycle', 'Every next step would wait indirectly on this ticket. Keep an independent progress path.');
  }
  for (const step of steps) {
    validateTaskBoard('NextStep', step);
    for (const condition of step.requires) {
      if (condition.kind === 'time') requireThat(Number.isFinite(Date.parse(condition.at)) && new Date(condition.at).toISOString() === (condition.at.includes('.') ? condition.at : condition.at.replace('Z', '.000Z')), 'task_board_invalid_date', 'A time condition needs a valid UTC timestamp.');
      if (condition.kind === 'user' || condition.kind === 'question') requireThat(task?.comments.some(comment => comment.commentId === condition.questionId && comment.purpose === 'question' && !comment.sourceTaskId),
        'task_board_comment_mismatch', 'A user condition must reference a question owned by this ticket.');
      if (condition.kind === 'task' || condition.kind === 'delivery') {
        requireThat(condition.taskId !== taskId, 'task_board_dependency_cycle', 'A task cannot wait on itself.');
        await store.read('task-board/task', condition.taskId, true, true);
      }
    }
  }
}
