import { openQuestions, queuedUserInput, runHandoff, unansweredQuestion } from './conversation.js';
import { applyHandoff, sameAssignment } from './handoff.js';
import { claimedWorkflowState } from './condition.js';
import { hashJson } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import { resultCommentFits, same, sharedProjectKey } from './checks.js';
import { TaskBoardEvidence } from './evidence.js';
import type { Publication, TaskBoardEngine } from './engine.js';
import { evidenceIdentity, intendedCall, nativeArray, nativeRecord, nativeString, resolveNativeRequest } from './native-intent.js';
import type { Document } from './store.js';
import { boundedOutputSummary, missingOutputSummary, TaskBoardTurnEvidence } from './turn-evidence.js';
import type { verifyNativeTranscript } from './full-turn-evidence.js';
import { initialDelivery } from './deliveries.js';
import { BoundToolClient } from '../../../../packages/sdk/src/client.js';
import { nativeErrorRetryAt, nativeRateLimitRetryAt, rateLimitWaitingDetail } from './native-rate-limit.js';
import { triggeredByComments } from './execution-mode.js';
import { publishAdoptedTurn } from './native-adoption.js';

export const finishedRun = (run: TaskBoard.Run): boolean => ['completed', 'failed', 'cancelled'].includes(run.phase);
type Completion = { state: 'succeeded' | 'failed' | 'cancelled'; summary: string; evidence: TaskBoard.Artifact | null; code: string | null; retryAt?: string | null };
const artifactFor = (document: Document<'task-board/run'>): TaskBoard.Artifact => ({ object: document.pin, label: 'Saved native attempt before closure', mediaType: 'application/json', contentHash: document.revision.contentHash });
const preventedRequest = (operation: Agent.Operation) => operation.phase === 'failed' && operation.code === 'request_prevented' && operation.requestId === null && operation.epoch === null && operation.reply === null;
function readOutcome(run: TaskBoard.Run, status: string): Pick<TaskBoard.Run['externalOutcome'], 'state' | 'code'> {
  const state = status === 'completed' ? 'succeeded' : status === 'failed' ? 'failed' : status === 'interrupted'
    ? run.externalOutcome.state === 'failed' ? 'failed' : run.cancellation ? 'cancelled' : 'failed' : 'active';
  const code = status === 'inProgress' || status === 'completed' ? null : 'native_' + status;
  // Codex can first expose interruption and then attach the actual terminal error.
  // This refines the same failed turn; it must never reopen or cancel a finished turn.
  const refinedFailure = !run.cancellation && run.externalOutcome.state === 'failed' &&
    run.externalOutcome.code === 'native_interrupted' && state === 'failed' && code === 'native_failed';
  requireThat(!['succeeded', 'failed', 'cancelled'].includes(run.externalOutcome.state) || state === run.externalOutcome.state && code === run.externalOutcome.code || refinedFailure,
    'task_board_native_state_changed', 'A native read cannot regress or reclassify an already known terminal outcome.');
  return { state, code };
}
export function projectNativeState(run: TaskBoard.Run, status: string, epoch: string | null, thread: Record<string, Wire.Json>, at: string, evidence: TaskBoard.Artifact) {
  let phase: TaskBoard.Run['phase'] = 'publishing_result', runtimeWait: TaskBoard.RuntimeWait | null = null;
  let externalOutcome: TaskBoard.Run['externalOutcome'] = { ...readOutcome(run, status), observedAt: at, evidence };
  if (status === 'inProgress') {
    const nativeStatus = nativeRecord(thread['status']), flags = nativeArray(nativeStatus['activeFlags']);
    if (!epoch || nativeStatus['type'] !== 'active') {
      phase = run.cancellation ? 'cancel_requested' : 'outcome_unknown';
      externalOutcome = { state: 'unknown', observedAt: at, evidence, code: 'native_active_owner_unconfirmed' };
      runtimeWait = { reason: 'external_outcome', detail: 'The stored unfinished turn is visible, but its original native owner has not confirmed active execution.', since: at };
    } else {
      phase = run.cancellation ? 'cancel_requested' : flags.some(value => ['waitingOnApproval', 'waitingOnUserInput'].includes(String(value))) ? 'waiting_input' : 'running';
      if (phase === 'waiting_input') runtimeWait = null;
    }
  } else runtimeWait = { reason: 'publication', detail: 'Native execution ended; the ticket handoff is being published.', since: at };
  return { phase, runtimeWait, externalOutcome };
}
// The ticket keeps a bounded summary; Codex retains the conversation history.
function outputSummary(turn: Record<string, Wire.Json>, status: string): string {
  const error = nativeString(nativeRecord(turn['error'])['message']);
  if (status === 'failed' && error) return boundedOutputSummary(error);
  const messages = nativeArray(turn['items']).map(nativeRecord).filter(item => item['type'] === 'agentMessage');
  const finals = messages.filter(item => item['phase'] === 'final_answer');
  const parts = (finals.length ? finals : messages.slice(-1)).map(item => nativeString(item['text']));
  const text = parts.join('\n\n') || missingOutputSummary(status);
  return boundedOutputSummary(text);
}

/** Publishes already saved owner observations. No native tool is called while holding the gate. */
export async function publishNative(engine: TaskBoardEngine, operation: Document<'task-board/operation'>, steps: Publication,
  verifiedTranscript?: Awaited<ReturnType<typeof verifyNativeTranscript>>): Promise<TaskBoard.ActionOutcome> {
  const request = operation.value.request;
  requireThat(request.action === 'nativeUpdate' && operation.value.actor.source === 'native' && operation.value.callerPrincipalId === engine.settings.principalId,
    'task_board_forbidden', 'Only the authenticated logical owner can publish native observations.');
  if (request.change.kind === 'adoptTurn') return publishAdoptedTurn(engine, operation, steps);
  const store = engine.store, at = operation.value.createdAt, first = operation.value.writes.length === 0;
  const reservation = operation.value.writes[0]?.destination;
  const taskPin = reservation && 'expectedRevision' in reservation
    ? { objectId: request.taskId, revision: reservation.expectedRevision }
    : first ? (await store.read('task-board/task', request.taskId)).pin
    : operation.value.publicationTask ?? { objectId: request.taskId, revision: request.expectedRevision };
  requireThat(taskPin.objectId === request.taskId, 'task_board_identity_conflict', 'The completion publication snapshot must belong to its original Task.');
  const task = await store.read('task-board/task', taskPin), originalRun = await store.read('task-board/run', request.run);
  const run = originalRun.value, active = task.value.claim?.run?.objectId === request.run.objectId, evidence = new TaskBoardEvidence(store);
  if (first) {
    if (request.change.kind === 'completion' || request.change.kind === 'transcript' || request.change.kind === 'snapshot') requireThat(
      !(await engine.operations.pendingHandoffs(request.run.objectId)).length,
      'task_board_handoff_pending', 'Reconcile the original accepted handoff before publishing this Run completion.');
    requireThat(task.metadata.currentRevision === task.pin.revision && !task.value.publication && originalRun.metadata.currentRevision === request.run.revision,
      'revision_conflict', 'Task or Run changed before native publication.');
    await engine.checks.aggregate(task.value, request.taskId);
    requireThat(run.taskId === request.taskId && run.originalRequest.taskId === request.taskId && run.operationId === run.originalRequest.operationId && !finishedRun(run),
      'task_board_run_mismatch', 'The native observation must identify this unfinished original execution attempt.');
    if (active) requireThat(same(task.value.claim!.run, request.run), 'revision_conflict', 'Native publication requires the exact current claimed Run revision.');
  }
  let next: TaskBoard.Run = { ...run, calls: { ...run.calls }, updatedAt: at }, completion: Completion | null = null;
  let runtimeWait: TaskBoard.RuntimeWait | null = null, detail = '', kind: TaskBoard.History['kind'] = 'recovered';
  let steeredComments: string[] = [];
  let acknowledgedWake: Extract<TaskBoard.NativeCallOrigin, { kind: 'steer' }> | null = null;
  const retryAt = (turn: Record<string, Wire.Json>, epoch: string | null) => {
    if (run.cancellation) return null;
    // A replacement owner can conclusively report the old turn interrupted.
    // Continue the saved conversation; never infer this from silence or replay
    // an uncertain turn. Interruptions in the same connection remain stopped.
    if (turn['status'] === 'interrupted' && epoch && run.nativeEpoch && epoch !== run.nativeEpoch)
      return new Date(Date.parse(at) + 30_000).toISOString();
    return nativeRateLimitRetryAt(turn, at);
  };
  const validateCall = (pin: TaskBoard.ObjectPin, _slot: keyof TaskBoard.Run['calls']) => store.read('task-board/native-call', pin);
  const observedCall = async (pin: TaskBoard.ObjectPin, slot: keyof TaskBoard.Run['calls']) => {
    const call = await validateCall(pin, slot);
    requireThat(call.value.evidence && call.value.observedAt && call.value.ownerPhase && call.value.state !== 'prepared', 'task_board_native_evidence_required', 'The NativeCall has no saved owner observation.');
    const observation = await evidence.read(pin, call.value.evidence, await evidenceIdentity(store, run, call.value, engine.settings.principalId));
    requireThat(call.value.ownerPhase === observation.phase && call.value.epoch === observation.epoch && call.value.code === observation.code, 'task_board_evidence_mismatch', 'NativeCall observation fields must agree with its verified owner evidence.');
    return { call, observation, artifact: call.value.evidence };
  };
  const finishTurn = (turn: Record<string, Wire.Json>, proof: TaskBoard.Artifact, epoch: string | null): Completion | null => {
    requireThat(nativeString(turn['id']) === next.turnId, 'task_board_turn_mismatch', 'A native snapshot cannot substitute another turn for the saved execution identity.');
    const status = nativeString(turn['status']);
    if (!['completed', 'failed', 'interrupted'].includes(status)) { requireThat(status === 'inProgress', 'task_board_native_state_unsupported', 'The native turn has an unsupported state.'); return null; }
    return { state: status === 'completed' ? 'succeeded' : status === 'interrupted' && !!next.cancellation ? 'cancelled' : 'failed', summary: outputSummary(turn, status), evidence: proof, code: status === 'completed' ? null : 'native_' + status, retryAt: retryAt(turn, epoch) };
  };
  switch (request.change.kind) {
    case 'readSnapshot': {
      const change = request.change, snapshot = await new TaskBoardTurnEvidence(store, engine.settings.principalId).snapshot(request.run, change.snapshot);
      requireThat(change.notificationCursor >= run.notificationCursor, 'task_board_cursor_regression', 'Native notification progress cannot move backwards.');
      next.notificationCursor = change.notificationCursor;
      const projected = projectNativeState(run, snapshot.document.value.status, snapshot.document.value.epoch, snapshot.thread, at, change.snapshot);
      next.phase = projected.phase; next.externalOutcome = projected.externalOutcome; runtimeWait = projected.runtimeWait;
      if (snapshot.document.value.status === 'inProgress' && snapshot.thread['id'] === run.primaryResourceRef?.nativeId &&
          nativeRecord(snapshot.thread['status'])['type'] === 'active') next.nativeEpoch = snapshot.document.value.epoch;
      detail = 'Saved the exact native turn state with separate read evidence.'; break;
    }
    case 'completion': {
      const change = request.change, verified = await new TaskBoardTurnEvidence(store, engine.settings.principalId).completion(request.run, change.snapshot, change.output);
      requireThat(change.notificationCursor >= run.notificationCursor, 'task_board_cursor_regression', 'Native notification progress cannot move backwards.');
      next.notificationCursor = change.notificationCursor;
      const observedOutcome = readOutcome(run, verified.status);
      requireThat(['succeeded', 'failed', 'cancelled'].includes(observedOutcome.state), 'task_board_native_state_changed', 'Completion must identify a terminal native outcome.');
      const handoff = runHandoff(task.value, request.run.objectId);
      const turn = verified.snapshot.turn;
      completion = { state: observedOutcome.state as Completion['state'], summary: verified.status === 'failed' && nativeString(nativeRecord(turn['error'])['message'])
        ? outputSummary(turn, verified.status) : handoff?.body ?? verified.summary, evidence: change.snapshot, code: observedOutcome.code, retryAt: retryAt(turn, verified.snapshot.document.value.epoch) };
      break;
    }
    case 'transcript': {
      const change = request.change, transcript = verifiedTranscript;
      requireThat(transcript && same(transcript.document.pin, change.evidence.object) && transcript.document.revision.contentHash === change.evidence.contentHash &&
        same(transcript.snapshot.source.run.pin, request.run), 'task_board_native_evidence_required', 'Transcript publication requires its exact immutable evidence graph verified before gate acquisition.');
      requireThat(change.notificationCursor >= run.notificationCursor, 'task_board_cursor_regression', 'Native notification progress cannot move backwards.');
      next.notificationCursor = change.notificationCursor;
      const observedOutcome = readOutcome(run, transcript.status);
      requireThat(['succeeded', 'failed', 'cancelled'].includes(observedOutcome.state), 'task_board_native_state_changed', 'A transcript must identify a terminal native outcome.');
      const turn = transcript.snapshot.turn;
      completion = { state: observedOutcome.state as Completion['state'], summary: transcript.status === 'failed' && nativeString(nativeRecord(turn['error'])['message'])
        ? outputSummary(turn, transcript.status) : transcript.summary, evidence: change.evidence, code: observedOutcome.code, retryAt: retryAt(turn, transcript.snapshot.document.value.epoch) };
      break;
    }
    case 'reattachCall': {
      const change = request.change;
      requireThat(active && !run.cancellation && run.primaryResourceRef && !run.turnId && run.calls.thread,
        'task_board_native_order', 'Pre-turn reattachment requires the active uncancelled claim and confirmed primary.');
      const call = await validateCall(change.call, 'thread'), origin = call.value.origin;
      requireThat(call.value.state === 'prepared' && origin?.kind === 'reattach' && same(origin.predecessor, run.calls.thread) &&
        change.call.objectId !== run.calls.thread.objectId,
        'task_board_native_intent_mismatch', 'Reattachment must be a new prepared call with the exact retained predecessor.');
      const previous = await observedCall(run.calls.thread, 'thread');
      const priorEpoch = previous.observation.epoch;
      requireThat(!!priorEpoch && (origin.preparedEpoch !== priorEpoch || previous.observation.phase === 'succeeded'),
        'task_board_native_intent_mismatch', 'An uncertain attachment requires a replacement native owner epoch.');
      requireThat(
        (previous.observation.phase === 'succeeded' && priorEpoch === run.nativeEpoch ||
          previous.observation.phase === 'outcome_unknown' &&
          previous.call.value.request.method === 'thread/resume' &&
          (!run.nativeEpoch || previous.call.value.origin?.kind === 'reattach')) &&
        call.value.operationId !== previous.call.value.operationId,
        'task_board_native_order', 'Reattachment needs the previously confirmed thread operation and a distinct successor identity.');
      if (run.calls.turn) {
        const turnCall = await store.read('task-board/native-call', run.calls.turn.objectId);
        requireThat(same(turnCall.pin, run.calls.turn) && turnCall.value.state === 'prepared' &&
          turnCall.value.ownerPhase === null && turnCall.value.evidence === null,
          'task_board_outcome_unresolved', 'An observed turn operation cannot be replaced or treated as unstarted by reattachment.');
      }
      next.calls.thread = change.call; next.phase = 'starting';
      detail = 'Attached a retained resume intent for the saved primary.'; break;
    }
    case 'attachCall': {
      const change = request.change, call = await validateCall(change.call, change.slot);
      if (change.slot === 'steer') {
        const origin = call.value.origin;
        requireThat(active && !run.cancellation && !!run.turnId && call.value.state === 'prepared' && origin?.kind === 'steer' && same(origin.predecessor, run.calls.steer ?? null),
          'task_board_native_order', 'Steering requires the active turn and exact predecessor.');
        if (run.calls.steer) requireThat(['succeeded', 'failed'].includes((await observedCall(run.calls.steer, 'steer')).observation.phase), 'task_board_outcome_unresolved', 'Resolve the original steering receipt before preparing another.');
        // Later edits do not invalidate this ticket-update prompt. Only the captured
        // input is acknowledged; newer work remains pending for the next pass.
        requireThat(origin.workRevision <= task.value.workRevision && origin.commentIds.every(id => task.value.commentDeliveries.some(delivery => delivery.commentId === id && delivery.state === 'queued')),
          'task_board_native_intent_mismatch', 'Steering must identify current queued input.');
        const intended = await intendedCall(store, run, 'steer', origin);
        requireThat(same(await resolveNativeRequest(store, call.value.request), intended.request) && call.value.expectedDefinitionHash === intended.expectedDefinitionHash,
          'task_board_native_intent_mismatch', 'Steering must contain only the ticket reference and target the exact turn.');
        next.calls.steer = change.call; runtimeWait = task.value.runtimeWait; detail = 'Prepared a ticket update for the active turn.';
        break;
      }
      requireThat(!call.value.origin || call.value.origin.kind === 'initial', 'task_board_native_order', 'A successor thread resume requires the explicit reattachment transition.');
      requireThat(!run.calls[change.slot] && call.value.state === 'prepared' && (active || change.slot === 'interrupt' && !!run.cancellation), 'task_board_claim_inactive', 'Only this current claim may attach a new execution call; retained cancellation may still interrupt its prior turn.');
      if (change.slot === 'turn') {
        requireThat(!run.cancellation && run.calls.thread, 'task_board_native_order', 'A turn requires thread confirmation and no pending cancellation.');
        requireThat((await observedCall(run.calls.thread, 'thread')).observation.phase === 'succeeded', 'task_board_native_order', 'The primary thread operation is not conclusively successful.');
      }
      if (change.slot === 'thread') requireThat(!run.cancellation, 'task_board_cancellation_pending', 'Cancellation prevents preparation of a new native context.');
      next.calls[change.slot] = change.call; detail = 'Attached exact native ' + change.slot + ' intent before dispatch.'; break;
    }
    case 'observeCall': {
      const change = request.change, prior = run.calls[change.slot];
      requireThat(prior && prior.objectId === change.call.objectId && change.call.revision > prior.revision, 'task_board_native_intent_mismatch', 'Observe a newer revision of the original attached native call.');
      const { call, observation, artifact } = await observedCall(change.call, change.slot);
      const original = await store.read('task-board/native-call', prior);
      requireThat(!['succeeded', 'failed'].includes(original.value.ownerPhase ?? ''), 'task_board_native_state_conflict', 'A conclusive native call observation cannot be rewritten or regressed.');
      requireThat(call.value.operationId === original.value.operationId && call.value.preparedAt === original.value.preparedAt && same(call.value.request, original.value.request) &&
        same(call.value.origin ?? { kind: 'initial' }, original.value.origin ?? { kind: 'initial' }), 'task_board_native_intent_mismatch', 'Call observation cannot rewrite its original identity, origin or preparation.');
      next.calls[change.slot] = change.call; detail = 'Native ' + change.slot + ' operation observed as ' + observation.phase + '.';
      if (change.slot === 'steer') {
        runtimeWait = task.value.runtimeWait;
        if (observation.phase === 'succeeded') {
          requireThat(observation.reply && 'result' in observation.reply && nativeRecord(observation.reply.result)['turnId'] === run.turnId && observation.epoch === run.nativeEpoch && call.value.origin?.kind === 'steer',
            'task_board_turn_mismatch', 'Steering acknowledgment must identify the original native turn and epoch.');
          steeredComments = call.value.origin.commentIds;
          acknowledgedWake = call.value.origin;
        }
        // Failure/unknown describes delivery, never the execution outcome. Queued input is
        // retried only after a conclusive failure; unresolved receipts fence turn completion.
        break;
      }
      if (['accepted', 'dispatched', 'outcome_unknown'].includes(observation.phase)) {
        next.phase = 'outcome_unknown'; next.externalOutcome = { state: 'unknown', observedAt: at, evidence: artifact, code: observation.code ?? 'native_operation_pending' };
        runtimeWait = { reason: 'external_outcome', detail, since: at }; kind = 'outcome_unknown';
      } else if (observation.phase === 'failed') {
        if (change.slot === 'interrupt') {
          next.phase = 'cancel_requested'; next.externalOutcome = { ...run.externalOutcome, state: 'unknown', evidence: artifact, observedAt: at, code: observation.code ?? 'native_interrupt_failed' };
          runtimeWait = { reason: 'external_outcome', detail: 'Interruption failed; the native turn still requires terminal observation.', since: at };
        } else completion = { state: 'failed', summary: 'Native ' + change.slot + ' operation failed.\n\n' + (observation.reply && 'error' in observation.reply ? observation.reply.error.message.slice(0, 16384) : observation.code ?? 'No native success was recorded.'), evidence: artifact, code: observation.code ?? 'native_operation_failed',
          retryAt: !run.cancellation && observation.reply && 'error' in observation.reply
            ? nativeErrorRetryAt(nativeRecord(observation.reply.error), at) : null };
      } else {
        requireThat(observation.reply && 'result' in observation.reply && observation.epoch, 'task_board_native_evidence_required', 'A successful native call needs its original reply and epoch.');
        const result = nativeRecord(observation.reply.result);
        if (change.slot === 'thread') {
          const thread = nativeRecord(result['thread']), id = nativeString(thread['id']);
          requireThat(id && thread['ephemeral'] !== true, 'task_board_primary_mismatch', 'Native context creation must return a durable primary identity.');
          if (call.value.request.method === 'thread/resume') requireThat(id === nativeString((await resolveNativeRequest(store, call.value.request)).params['threadId']), 'task_board_primary_mismatch', 'Resume returned another primary context.');
          next.primaryResourceRef = { serviceNodeId: run.target.serviceNodeId, namespace: 'codex', kind: 'thread', nativeId: id }; next.nativeEpoch = observation.epoch;
          next.phase = run.cancellation ? 'cancel_requested' : 'starting'; next.externalOutcome = { state: 'not_started', observedAt: at, evidence: artifact, code: null };
        } else if (change.slot === 'turn') {
          const turn = nativeRecord(result['turn']), id = nativeString(turn['id']); requireThat(id, 'task_board_turn_mismatch', 'Turn start must return its exact native identity.');
          next.turnId = id; next.nativeEpoch = observation.epoch; next.phase = run.cancellation ? 'cancel_requested' : 'running';
          // Start acknowledges identity; a separate observation confirms the execution outcome.
          next.externalOutcome = { state: 'active', observedAt: at, evidence: artifact, code: null };
        } else next.phase = 'cancel_requested'; // An interrupt receipt never proves terminal state.
      } break;
    }
    case 'snapshot': {
      const change = request.change; requireThat(run.primaryResourceRef && run.turnId && run.nativeEpoch, 'task_board_turn_mismatch', 'A snapshot cannot infer a lost turn/start identity.');
      const plan = await store.read('task-board/native-plan', run.originalRequest.intent);
      const observation = await evidence.read(request.run, change.evidence, { callerPrincipalId: engine.settings.principalId, serviceNodeId: run.target.serviceNodeId, nativeVersion: plan.value.nativeVersion,
        operationId: change.nativeOperationId, method: 'thread/read', params: { threadId: run.primaryResourceRef.nativeId, includeTurns: true } });
      requireThat(observation.phase === 'succeeded' && observation.reply && 'result' in observation.reply, 'task_board_native_evidence_required', 'Native state publication requires a successful exact thread read.');
      const thread = nativeRecord(nativeRecord(observation.reply.result)['thread']);
      requireThat(thread['id'] === run.primaryResourceRef.nativeId && thread['ephemeral'] !== true, 'task_board_primary_mismatch', 'The observed primary differs from the saved native context.');
      const turn = nativeArray(thread['turns']).map(nativeRecord).find(value => value['id'] === run.turnId);
      requireThat(turn, 'task_board_turn_unavailable', 'The owner snapshot does not contain the saved turn; do not infer completion.');
      requireThat(change.notificationCursor >= run.notificationCursor, 'task_board_cursor_regression', 'Native notification progress cannot move backwards.');
      next.notificationCursor = change.notificationCursor; completion = finishTurn(turn, change.evidence, observation.epoch);
      const state = readOutcome(run, nativeString(turn['status']));
      if (completion) completion = { ...completion, state: state.state as Completion['state'], code: state.code };
      if (!completion) {
        const projected = projectNativeState(run, nativeString(turn['status']), observation.epoch, thread, at, change.evidence);
        next.phase = projected.phase; next.externalOutcome = projected.externalOutcome; runtimeWait = projected.runtimeWait;
      }
      detail = 'Observed the exact saved native turn.'; break;
    }
    case 'unavailable': {
      const change = request.change; detail = change.detail; kind = 'outcome_unknown';
      if (change.code === 'native_process_lost') {
        requireThat(change.reason === 'external_outcome' && !!run.turnId && !!run.nativeEpoch &&
          !['succeeded', 'failed', 'cancelled'].includes(run.externalOutcome.state),
          'task_board_outcome_unresolved', 'Only a previously unfinished dispatched turn can close after proven owner process loss.');
        completion = { state: 'failed', summary: detail, evidence: run.externalOutcome.evidence ?? artifactFor(originalRun), code: change.code };
        kind = 'result_saved';
        break;
      }
      runtimeWait = { reason: change.reason, detail, since: at };
      if (['succeeded', 'failed', 'cancelled'].includes(run.externalOutcome.state)) next.phase = 'publishing_result';
      else if (change.reason === 'host' && !run.calls.thread && !run.calls.turn) next.phase = run.cancellation ? 'cancel_requested' : 'starting';
      else { next.phase = 'outcome_unknown'; next.externalOutcome = { ...run.externalOutcome, state: 'unknown', code: change.code, observedAt: at }; }
      break;
    }
    case 'cancelUnstarted': case 'retireUnstarted': {
      const cancelling = request.change.kind === 'cancelUnstarted';
      requireThat(!run.calls.turn && !run.turnId && !run.calls.interrupt && (cancelling ? !!run.cancellation : !active && !run.cancellation), 'task_board_outcome_unresolved', 'An execution cannot close as unstarted after turn preparation or without the required intent.');
      let proof: TaskBoard.Artifact = artifactFor(originalRun);
      if (run.calls.thread) {
        const observed = await observedCall(run.calls.thread, 'thread');
        requireThat(['succeeded', 'failed'].includes(observed.observation.phase), 'task_board_outcome_unresolved', 'Resolve the existing thread operation before closing an unstarted attempt.'); proof = observed.artifact;
      }
      completion = { state: cancelling ? 'cancelled' : 'failed', summary: cancelling ? 'Cancelled before any native turn was prepared.' : 'This superseded attempt closed before a native turn was prepared.', evidence: proof,
        code: cancelling ? 'cancelled_before_turn' : 'superseded_before_turn' }; detail = completion.summary; break;
    }
  }
  if (request.change.kind === 'observeCall' && request.change.slot === 'thread' && completion && next.calls.turn && !next.turnId) {
    const turn = await store.read('task-board/native-call', next.calls.turn);
    if (turn.value.ownerPhase !== 'failed') {
      const failedThread = await store.read('task-board/native-call', request.change.call);
      completion = null; next.phase = run.cancellation ? 'cancel_requested' : 'outcome_unknown';
      next.externalOutcome = { state: 'unknown', observedAt: at, evidence: failedThread.value.evidence, code: 'native_turn_resolution_required' };
      runtimeWait = { reason: 'external_outcome', detail: 'The primary operation failed; the original prepared turn still requires owner reconciliation or prevention.', since: at };
    }
  }
  if (request.change.kind === 'observeCall' && run.cancellation && !next.turnId) {
    let prevented: TaskBoard.Artifact | null = null, settled = true;
    for (const slot of ['thread', 'turn'] as const) {
      const pin = next.calls[slot]; if (!pin) continue;
      const call = await store.read('task-board/native-call', pin);
      if (call.value.state === 'prepared') { settled = false; continue; }
      const observed = await observedCall(pin, slot), fenced = preventedRequest(observed.observation);
      if (fenced) prevented = observed.artifact;
      if (slot === 'turn' ? !fenced : !['succeeded', 'failed'].includes(observed.observation.phase)) settled = false;
    }
    if (prevented) {
      if (settled) completion = { state: 'cancelled', summary: 'Cancelled with the original native request durably prevented before dispatch.', evidence: prevented, code: 'cancelled_before_dispatch' };
      else {
        completion = null; next.phase = 'cancel_requested';
        next.externalOutcome = { state: 'unknown', observedAt: at, evidence: prevented, code: 'native_prevention_pending' };
        runtimeWait = { reason: 'external_outcome', detail: 'A request was prevented; remaining original calls still require owner reconciliation.', since: at };
      }
    }
  }
  if (completion && run.calls.steer) requireThat(['succeeded', 'failed'].includes((await observedCall(run.calls.steer, 'steer')).observation.phase),
    'task_board_outcome_unresolved', 'Resolve the original steering receipt before closing its turn.');
  let finalTask: TaskBoard.Task = { ...task.value, updatedAt: at, publication: steps.pin,
    commentDeliveries: task.value.commentDeliveries.map(value => steeredComments.includes(value.commentId) ? { ...value, state: 'delivered', updatedAt: at, code: null } : value) };
  if (acknowledgedWake?.observedDependencies) finalTask.observedDependencies = acknowledgedWake.observedDependencies;
  if (acknowledgedWake?.wakeAt && finalTask.wakeAt === acknowledgedWake.wakeAt) finalTask.wakeAt = null;
  const hasComments = triggeredByComments(run.originalRequest);
  const initial = run.executionContext ?? {
    workflowState: 'todo' as const, latestResult: task.value.latestResult, acceptedReview: task.value.acceptedReview };
  const handoffComment = runHandoff(finalTask, request.run.objectId);
  // A directly started turn has no scheduler-owned follow-up to repeat. Without
  // an explicit handoff its completed result awaits review, never another start.
  const declaredHandoff = handoffComment?.handoff ?? (run.owner.source === 'native' &&
    !['done', 'cancelled'].includes(initial.workflowState) ? { workflowState: 'review' as const } : undefined);
  // All semantic/evidence checks precede reservation; deterministic replay below uses original pins.
  const reserved = await steps.write('task-board/task', finalTask, { objectId: request.taskId, expectedRevision: task.pin.revision });
  const outcome: TaskBoard.ActionOutcome = { operationId: request.operationId, task: null, plan: null, run: null, history: null, result: null, review: null, attachment: null };
  if (completion) {
    try {
      // Repository provenance is useful for final review, not every intermediate
      // reply or waiting turn. The task reservation has already released its gates.
      if (declaredHandoff?.workflowState === 'review') {
        const observed = await new BoundToolClient(engine.store.client, next.target.serviceNodeId, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]).call('agent.resolveWorkspace', { taskKey: task.value.taskKey, requirement: task.value.fields.workspaceRequirement, prepare: false, verifyOrigin: true }) as Agent.WorkspaceResolution;
        const { nativeProjectId: _nativeProjectId, ...workspace } = observed;
        if (workspace.hostId === next.target.hostId && workspace.serviceNodeId === next.target.serviceNodeId && workspace.intendedPath === next.workspace.intendedPath) next.workspace = workspace;
      }
    } catch { /* The retained allocation still identifies the material host and limitation. */ }
    const evidence = completion.evidence && !store.localKey(completion.evidence.object.objectId) ? [completion.evidence] : [];
    const content: TaskBoard.ResultContent = { summary: completion.summary, artifacts: evidence,
      codeReferences: [], repositoryResult: next.workspace.project && next.workspace.repository ? { hostId: next.target.hostId, serviceNodeId: next.target.serviceNodeId,
        project: next.workspace.project, repositoryName: next.workspace.repository.name, branch: next.workspace.repository.branch, commit: next.workspace.repository.commit,
        originName: next.workspace.repository.originName, originUrl: next.workspace.repository.originUrl,
        originState: next.workspace.repository.originState, remoteRef: next.workspace.repository.remoteRef, observedAt: next.workspace.repository.observedAt,
        limitation: next.workspace.repository.limitation } : null,
      limitations: ['Native completion does not automatically accept the task. Native conversation history remains available from its AgentManager owner while retained there.'] };
    const handoff = declaredHandoff;
    outcome.result = await steps.create('task-board/result', { schemaVersion: 1, taskId: request.taskId, run: request.run,
      kind: completion.state === 'succeeded' && handoff?.workflowState === 'review' ? 'final' : 'intermediate',
      content, createdAt: at, actor: operation.value.actor, operationId: request.operationId }, 'Native result');
    next = { ...next, result: outcome.result, phase: completion.state === 'succeeded' ? 'completed' : completion.state === 'cancelled' ? 'cancelled' : 'failed', finishedAt: at,
      externalOutcome: { state: completion.state, observedAt: at, evidence: completion.evidence, code: completion.code } };
    kind = completion.state === 'cancelled' ? 'cancelled' : 'result_saved'; detail = 'Saved native ' + completion.state + ' outcome' + (active ? '.' : ' for the retained reassigned attempt.');
  }
  const retiredCalls: TaskBoard.ObjectPin[] = finishedRun(next)
    ? Object.values(next.calls).filter((pin): pin is TaskBoard.ObjectPin => !!pin) : [];
  const seenCalls = new Set(retiredCalls.map(pin => pin.objectId));
  for (let index = 0; index < retiredCalls.length; index++) {
    requireThat(index < 2048, 'task_board_evidence_mismatch', 'The native call succession chain is too long.');
    const call = await store.read('task-board/native-call', retiredCalls[index]!);
    const predecessor = call.value.origin?.kind === 'reattach' || call.value.origin?.kind === 'steer' ? call.value.origin.predecessor : null;
    if (predecessor && !seenCalls.has(predecessor.objectId)) {
      seenCalls.add(predecessor.objectId);
      retiredCalls.push(predecessor);
    }
  }
  if (finishedRun(next)) next = { ...next, calls: { thread: null, turn: null, interrupt: null },
    externalOutcome: { ...next.externalOutcome, evidence: next.externalOutcome.evidence && !store.localKey(next.externalOutcome.evidence.object.objectId)
      ? next.externalOutcome.evidence : null } };
  outcome.run = await steps.write('task-board/run', next, { objectId: request.run.objectId, expectedRevision: request.run.revision });
  if (active) {
    if (completion) {
      const handoffIntent = declaredHandoff;
      const ready = completion.state === 'succeeded' && handoffIntent?.workflowState === 'review';
      if (ready) finalTask.workRevision++;
      if (completion.state === 'succeeded' && handoffIntent) {
        finalTask = applyHandoff(finalTask, handoffIntent, at);
        delete finalTask.completionIssue;
      }
      finalTask = { ...finalTask, claim: null, runtimeWait: null, primaryResourceRef: next.primaryResourceRef, lastRun: outcome.run };
      const publicationError = store.failedHandoff(request.run.objectId);
      if (completion.state === 'cancelled') finalTask.workflowState = 'cancelled';
      else if (completion.state === 'succeeded') {
        delete finalTask.completionIssue;
        finalTask.workflowState = handoffIntent?.workflowState ?? (initial.workflowState === 'in_progress' ? 'todo' : initial.workflowState);
        if (handoffIntent || initial.workflowState === 'todo') {
          finalTask.latestResult = outcome.result;
          finalTask.acceptedReview = null;
        } else {
          finalTask.latestResult = initial.latestResult;
          finalTask.acceptedReview = initial.acceptedReview;
        }
      } else {
        const retryAt = completion.retryAt ?? (!next.turnId ? new Date(Date.parse(at) + 30_000).toISOString() : null);
        finalTask.workflowState = initial.workflowState === 'in_progress' ? 'todo' : initial.workflowState;
        finalTask.latestResult = task.value.latestResult ?? outcome.result;
        finalTask.completionIssue = { code: completion.code ?? 'native_failed', detail: completion.summary.slice(0, 2048),
          run: request.run, operationId: request.operationId, retryAt };
        if (retryAt) {
          finalTask.commentDeliveries = finalTask.commentDeliveries.map(value => run.originalRequest.commentIds.includes(value.commentId)
            ? { ...value, state: 'queued', updatedAt: at, code: completion.code } : value);
          finalTask.runtimeWait = { reason: 'host', detail: rateLimitWaitingDetail(retryAt), since: at, retryAt };
        }
      }
      if (publicationError) {
        finalTask.completionIssue = { ...publicationError, run: request.run, retryAt: null };
        finalTask.runtimeWait = null;
      }
      if (completion.state !== 'cancelled') {
        const handoff = runHandoff(finalTask, request.run.objectId);
        const answered = hasComments && finalTask.comments.some(comment => comment.authorKind === 'agent' &&
          comment.run?.objectId === request.run.objectId && !!comment.replyTo && run.originalRequest.commentIds.includes(comment.replyTo));
        let comment: TaskBoard.Comment = handoff ?? { commentId: request.operationId, sequence: finalTask.comments.length + 1,
          author: operation.value.actor, authorKind: 'agent', purpose: 'update',
          body: completion.summary.slice(0, 65536), requests: [], responses: [], delivery: null, attachments: [],
          run: request.run, turnId: next.turnId, replyTo: hasComments ? run.originalRequest.commentIds.at(-1) ?? null : null,
          createdAt: at, operationId: request.operationId };
        const fits = !!handoff || resultCommentFits(finalTask, comment, steps.pin, request.taskId);
        if (fits && ready && !finalTask.parentTaskId && finalTask.fields.userContact !== 'ticket' && !comment.delivery) {
          const delivery = await steps.create('task-board/delivery', initialDelivery(engine.settings, steps.pin, request.taskId, comment.commentId, finalTask.fields.userContact, at), 'Handoff notification');
          comment = { ...comment, delivery };
        }
        if (handoff) finalTask.comments = finalTask.comments.map(value => value.commentId === handoff.commentId ? comment : value);
        else if (!answered && fits) finalTask = { ...finalTask, comments: [...finalTask.comments, comment], agentCommentCount: finalTask.agentCommentCount + 1 };
      }
    } else {
      const workflowState = next.phase === 'waiting_input' ? 'waiting' : claimedWorkflowState(runtimeWait);
      const phase: TaskBoard.Claim['phase'] = runtimeWait?.reason === 'host' || runtimeWait?.reason === 'external_outcome' || next.phase === 'outcome_unknown' ? 'outcome_unknown' : next.phase === 'cancel_requested' ? 'cancel_requested' : next.phase === 'waiting_input' ? 'waiting_input' : next.phase === 'publishing_result' ? 'publishing_result' : next.phase === 'running' ? 'running' : 'starting';
      finalTask = { ...finalTask, claim: { ...task.value.claim!, phase, run: outcome.run }, primaryResourceRef: next.primaryResourceRef, lastRun: outcome.run, workflowState, runtimeWait: runtimeWait,
        commentDeliveries: next.phase === 'running'
          ? finalTask.commentDeliveries.map(value => next.originalRequest.commentIds.includes(value.commentId) &&
              (value.state !== 'delivered' || value.code !== null) ? { ...value, state: 'delivered' as const, updatedAt: at, code: null } : value)
          : finalTask.commentDeliveries };
    }
  } else if (task.value.lastRun?.objectId === request.run.objectId) finalTask.lastRun = outcome.run;
  finalTask.openQuestionCount = openQuestions(finalTask).filter(question => question.recipient?.kind !== 'task').length;
  if (completion || runtimeWait?.reason !== task.value.runtimeWait?.reason) outcome.history = await steps.create('task-board/history', { schemaVersion: 1, taskId: request.taskId, operationId: request.operationId, actor: operation.value.actor, createdAt: at, kind, detail,
    previousTask: task.pin, previousHistory: task.value.lastHistory, priorRun: request.run, fromTarget: null, toTarget: null, result: outcome.result, review: null }, 'Native history');
  outcome.task = await steps.write('task-board/task', { ...finalTask, publication: null, lastHistory: outcome.history ?? task.value.lastHistory }, { objectId: request.taskId, expectedRevision: reserved.revision });
  if (active && completion && (completion.state === 'cancelled' || sharedProjectKey(task.value.fields.workspaceRequirement, next.workspace)))
    store.releaseWorkspace(request.taskId);
  if (finishedRun(next) && !finalTask.completionIssue) {
    // The result must be fully published before starting retention. Keep replay
    // dependencies if the operation's final receipt is interrupted afterwards.
    const publishedAt = new Date().toISOString();
    for (const root of [request.run, ...retiredCalls])
      store.completeLocalWorkflow(root.objectId, publishedAt, operation.pin.objectId);
    if (next.owner.source === 'scheduler' && store.localKey(next.originalRequest.intent.objectId) === 'task-board/native-plan')
      store.completeLocalWorkflow(next.originalRequest.intent.objectId, publishedAt, operation.pin.objectId);
  }

  return outcome;
}
