import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard } from '../../../../packages/sdk/src/node.js';
import { same } from './checks.js';
import { unansweredQuestion } from './conversation.js';
import { TaskBoardEvidence } from './evidence.js';
import type { TaskBoardEngine } from './engine.js';
import { evidenceIdentity, resolveNativeRequest } from './native-intent.js';
import type { Document, TaskBoardStore } from './store.js';

export const missingNativeThread = (reply: Agent.Reply | null, threadId: string): boolean => {
  if (!reply || !('error' in reply)) return false;
  const message = reply.error.message;
  return message.includes(threadId) && /no rollout found|(?:thread|rollout).*?(?:not found|does not exist)|(?:not found|does not exist).*?(?:thread|rollout)/i.test(message);
};

export async function contextRecoveryHistory(store: TaskBoardStore, task: TaskBoard.Task, taskId: string): Promise<TaskBoard.History | null> {
  let pin = task.lastHistory;
  const visited = new Set<string>();
  while (pin && task.lastRun) {
    requireThat(!visited.has(pin.objectId), 'task_board_evidence_mismatch', 'The recovery History lineage contains a cycle.');
    visited.add(pin.objectId);
    const history: Document<'task-board/history'> = await store.read('task-board/history', pin);
    requireThat(history.value.taskId === taskId, 'task_board_evidence_mismatch', 'Recovery History must belong to this ticket.');
    if (history.value.priorRun?.objectId !== task.lastRun.objectId || history.value.kind === 'reassigned') break;
    if (history.value.kind === 'recovered' && history.value.actor.source === 'scheduler') return history.value;
    pin = history.value.previousHistory;
  }
  return null;
}

/** An empty context is retired before its successor Run starts, retaining the original allocation. */
export async function freshContextRecovery(store: TaskBoardStore, task: TaskBoard.Task): Promise<TaskBoard.Run | null> {
  if (task.primaryResourceRef || !task.lastRun) return null;
  const run = await store.read('task-board/run', task.lastRun);
  if (run.value.phase !== 'failed' || run.value.turnId || run.value.cancellation) return null;
  return await contextRecoveryHistory(store, task, run.value.taskId) ? run.value : null;
}

async function unstartedAttempt(engine: TaskBoardEngine, taskId: string, primary: NonNullable<TaskBoard.Task['primaryResourceRef']>, run: Document<'task-board/run'>) {
  if (run.value.taskId !== taskId || !same(run.value.primaryResourceRef, primary) || run.value.phase !== 'failed' || run.value.externalOutcome.state !== 'failed' ||
      run.value.turnId || run.value.cancellation || !run.value.result) return null;
  const store = engine.store, evidence = new TaskBoardEvidence(store), result = await store.read('task-board/result', run.value.result);
  requireThat(result.value.taskId === taskId && result.value.kind === 'final' && result.value.run?.objectId === run.pin.objectId && result.value.run.revision < run.pin.revision,
    'task_board_evidence_mismatch', 'Recovery requires the exact saved attempt before its terminal publication.');
  const saved = await store.read('task-board/run', result.value.run);
  if (saved.value.turnId || saved.value.calls.steer || saved.value.calls.interrupt) return null;
  const observed = async (callPin: TaskBoard.ObjectPin) => {
    // Closure can consume an observation newer than the pre-closure Run's prepared call pin.
    const call = await store.read('task-board/native-call', callPin.objectId);
    if (!call.value.evidence || call.value.ownerPhase !== 'failed' || call.value.state !== 'observed') return null;
    const observation = await evidence.read(call.pin, call.value.evidence, await evidenceIdentity(store, saved.value, call.value, engine.settings.principalId));
    requireThat(observation.phase === call.value.ownerPhase && observation.code === call.value.code && observation.epoch === call.value.epoch,
      'task_board_evidence_mismatch', 'Recovery must use the verified original native receipt.');
    const request = await resolveNativeRequest(store, call.value.request);
    requireThat(request.params['threadId'] === primary.nativeId, 'task_board_primary_mismatch', 'Recovery proof must identify the saved native context.');
    return observation;
  };
  if (saved.value.calls.turn) {
    const turn = await observed(saved.value.calls.turn);
    if (!turn || turn.method !== 'turn/start' || turn.phase !== 'failed' || turn.code !== 'request_prevented' ||
        turn.requestId !== null || turn.epoch !== null || turn.reply !== null) return null;
  }
  return { saved, observed };
}

/** A confirmed pre-turn archive failure permits restoration of this same, previously used context. */
export async function unstartedArchivedContext(engine: TaskBoardEngine, task: Document<'task-board/task'>): Promise<boolean> {
  const primary = task.value.primaryResourceRef;
  if (!primary || primary.namespace !== 'codex' || primary.kind !== 'thread' || !task.value.lastRun || unansweredQuestion(task.value, task.value.lastRun.objectId)) return false;
  try {
    const run = await engine.store.read('task-board/run', task.value.lastRun), proof = await unstartedAttempt(engine, task.pin.objectId, primary, run);
    if (!proof?.saved.value.calls.thread) return false;
    const observed = await proof.observed(proof.saved.value.calls.thread);
    return !!observed && observed.method === 'thread/resume' && !!observed.reply && 'error' in observed.reply &&
      observed.reply.error.message.includes(primary.nativeId) && /\bis archived\b/i.test(observed.reply.error.message);
  } catch (error) {
    if (error instanceof IvyError && error.code === 'not_found') return false;
    throw error;
  }
}

/** Follow only this context's pinned Run lineage. Missing or uncertain proof forbids replacement. */
export async function unstartedMissingContext(engine: TaskBoardEngine, task: Document<'task-board/task'>): Promise<boolean> {
  const primary = task.value.primaryResourceRef;
  if (!primary || primary.namespace !== 'codex' || primary.kind !== 'thread' || !task.value.lastRun) return false;
  if (unansweredQuestion(task.value, task.value.lastRun.objectId)) return false;
  const store = engine.store, visited = new Set<string>();
  let pin: TaskBoard.ObjectPin | null = task.value.lastRun, first = true;
  try {
    while (pin) {
      requireThat(!visited.has(pin.objectId), 'task_board_evidence_mismatch', 'The native context Run lineage contains a cycle.');
      visited.add(pin.objectId);
      const run: Document<'task-board/run'> = await store.read('task-board/run', pin);
      if (!same(run.value.primaryResourceRef, primary)) return !first;
      // The failed Run stays immutable after restoration, including when later comments arrive.
      if (first && await contextRecoveryHistory(store, task.value, task.pin.objectId)) return false;
      const proof = await unstartedAttempt(engine, task.pin.objectId, primary, run);
      if (!proof) return false;
      if (first) {
        if (!proof.saved.value.calls.thread) return false;
        const thread = await proof.observed(proof.saved.value.calls.thread);
        if (!thread || thread.method !== 'thread/resume' || !missingNativeThread(thread.reply, primary.nativeId)) return false;
        first = false;
      }
      const initial: Document<'task-board/task'> = await store.read('task-board/task', { objectId: task.pin.objectId, revision: run.value.originalRequest.expectedRevision });
      if (!same(initial.value.primaryResourceRef, primary)) return true;
      pin = initial.value.lastRun;
    }
    return !first;
  } catch (error) {
    // Retention or missing original parameters cannot authorize a new execution context.
    if (error instanceof IvyError && error.code === 'not_found') return false;
    throw error;
  }
}
