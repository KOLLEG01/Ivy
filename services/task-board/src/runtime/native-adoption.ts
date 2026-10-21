import { hashJson, IvyError, requireThat, validateAgent } from '../../../../packages/sdk/src/node.js';
import type { Agent, TaskBoard } from '../../../../packages/sdk/src/node.js';
import { NativeOwner } from '../../../../packages/sdk/src/native-owner.js';
import { serviceTools } from '../../../../packages/sdk/src/client.js';
import { catalogFor } from './evidence.js';
import { same } from './checks.js';
import type { Publication, TaskBoardEngine } from './engine.js';
import { nativeArray, nativeRecord, nativeString } from './native-intent.js';
import { TaskBoardReadEvidence } from './read-evidence.js';
import { mutation } from './store.js';
import type { Document } from './store.js';

const terminal = (run: TaskBoard.Run) => ['completed', 'failed', 'cancelled'].includes(run.phase);
const turnsParams = (threadId: string) => ({ threadId, cursor: null, limit: 1, sortDirection: 'desc', itemsView: 'notLoaded' });

function latestTurn(observation: Agent.ReadObservation): ReturnType<typeof nativeRecord> {
  requireThat('result' in observation.reply, 'task_board_native_read_failed', 'Direct turn inspection requires a successful native read.');
  const page = nativeRecord(observation.reply.result), rows = nativeArray(page['data']).map(nativeRecord);
  requireThat(rows.length <= 100, 'task_board_native_page_limit', 'Direct turn discovery requires a bounded page of turn metadata.');
  if (rows.length <= 1) return rows[0] ?? {};
  // Some owners return a complete metadata page despite the requested limit.
  // Only a complete bounded page with owner timestamps can identify its newest turn.
  requireThat(page['nextCursor'] === null && rows.every(row => typeof row['startedAt'] === 'number'),
    'task_board_native_page_limit', 'A multi-turn discovery page must be complete and ordered by owner timestamps.');
  const newest = Math.max(...rows.map(row => row['startedAt'] as number));
  const selected = rows.filter(row => row['startedAt'] === newest);
  return selected.length === 1 ? selected[0]! : {};
}

function candidate(task: TaskBoard.Task, previous: TaskBoard.Run, metadata: Agent.ReadObservation, turns: Agent.ReadObservation) {
  requireThat('result' in metadata.reply && 'result' in turns.reply, 'task_board_native_read_failed', 'Direct turn inspection requires successful native reads.');
  const thread = nativeRecord(nativeRecord(metadata.reply.result)['thread']);
  requireThat(thread['id'] === task.primaryResourceRef?.nativeId && thread['ephemeral'] === false,
    'task_board_evidence_mismatch', 'Direct turn inspection must identify the saved durable thread.');
  if (/[\\/]archived_sessions[\\/]/.test(nativeString(thread['path']))) return null;
  const turn = latestTurn(turns), id = nativeString(turn['id']), status = nativeString(turn['status']);
  if (!id || id === previous.turnId) return null;
  if (status === 'inProgress') return nativeRecord(thread['status'])['type'] === 'active' ? turn : null;
  // A short turn may finish before a notification or reconnect scan reaches it.
  // Older history must never be imported as fresh work after a failed start.
  const completedAt = typeof turn['completedAt'] === 'number' ? turn['completedAt'] * 1000 : NaN;
  return ['completed', 'failed', 'interrupted'].includes(status) && completedAt >= Date.parse(previous.createdAt) ? turn : null;
}

/** Observes the existing primary; it never resumes, starts or steers native work. */
export async function adoptDirectTurn(engine: TaskBoardEngine, taskId: string): Promise<TaskBoard.ActionOutcome | null> {
  const task = await engine.store.read('task-board/task', taskId, true, true, true), primary = task.value.primaryResourceRef;
  if (task.metadata.effectivelyArchived || task.value.fields.control !== 'agent' || task.value.claim || task.value.publication ||
      !primary || !task.value.lastRun || primary.namespace !== 'codex' || primary.kind !== 'thread') return null;
  const previous = await engine.store.read('task-board/run', task.value.lastRun);
  if (!terminal(previous.value) || !same(primary, previous.value.primaryResourceRef)) return null;
  const node = await engine.store.client.request('serviceNodes.get', { serviceNodeId: primary.serviceNodeId });
  if (!node.connected || !node.synced || !node.ready) return null;
  const status = await serviceTools(engine.store.client, primary.serviceNodeId, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]).read('agent.status', {}) as Agent.Status;
  validateAgent('Status', status);
  if (status.state !== 'ready' || !status.epoch) return null;
  requireThat(node.serviceName === 'agent-manager' && status.serviceNodeId === primary.serviceNodeId && status.hostId === previous.value.target.hostId &&
    status.nativeVersion === node.nativeVersion, 'task_board_native_unavailable', 'Direct turns must belong to the saved native owner.');
  const owner = new NativeOwner(engine.store.client, engine.settings.principalId, {
    serviceNodeId: status.serviceNodeId, hostId: status.hostId, nativeVersion: status.nativeVersion,
    nativeExecutableHash: status.nativeExecutableHash, catalogHash: status.catalogHash,
  });
  const turns = await owner.read('thread/turns/list', turnsParams(primary.nativeId), status.epoch, true);
  if (!('result' in turns.reply)) {
    // Missing/archived sessions use the existing lifecycle recovery. A transport
    // or native read failure is not proof that starting another turn is safe.
    if (/^(?:thread|session) .+ (?:not found|is archived)|^unknown thread:/i.test(turns.reply.error.message)) return null;
    throw new IvyError('task_board_native_read_failed', turns.reply.error.message);
  }
  const latest = latestTurn(turns);
  if (!latest['id'] || latest['id'] === previous.value.turnId) return null;
  const metadata = await owner.read('thread/read', { threadId: primary.nativeId, includeTurns: false }, status.epoch);
  const turn = candidate(task.value, previous.value, metadata, turns);
  if (!turn) return null;
  return engine.nativeUpdate({ action: 'nativeUpdate', operationId: mutation(taskId,
    'adopt:' + hashJson({ primary, turnId: turn['id'], task: task.pin, observation: turns.observationId })), taskId, expectedRevision: task.pin.revision,
    run: previous.pin, change: { kind: 'adoptTurn', metadata, turns } });
}

/** The observation and all writes share the existing crash-recoverable publication journal. */
export async function publishAdoptedTurn(engine: TaskBoardEngine, operation: Document<'task-board/operation'>, steps: Publication): Promise<TaskBoard.ActionOutcome> {
  const request = operation.value.request;
  requireThat(request.action === 'nativeUpdate' && request.change.kind === 'adoptTurn', 'task_board_native_order', 'Expected a direct turn observation.');
  const store = engine.store, at = operation.value.createdAt, { metadata, turns } = request.change;
  const task = await store.read('task-board/task', { objectId: request.taskId, revision: request.expectedRevision });
  const previous = await store.read('task-board/run', request.run), primary = task.value.primaryResourceRef;
  requireThat(primary && same(primary, previous.value.primaryResourceRef) && previous.value.taskId === request.taskId && terminal(previous.value),
    'task_board_primary_mismatch', 'A direct turn must continue this ticket\'s saved primary and finished attempt.');
  const source = catalogFor(metadata.nativeVersion, { catalogHash: metadata.catalogHash });
  const reads: TaskBoardReadEvidence = new TaskBoardReadEvidence(store);
  for (const [observation, method, params] of [[metadata, 'thread/read', { threadId: primary.nativeId, includeTurns: false }],
    [turns, 'thread/turns/list', turnsParams(primary.nativeId)]] as const) {
    reads.verify(observation, { observationId: observation.observationId, callerPrincipalId: engine.settings.principalId,
      serviceNodeId: primary.serviceNodeId, nativeVersion: metadata.nativeVersion, epoch: metadata.epoch, method, params }, source.catalog.sourceHash);
  }
  const turn = candidate(task.value, previous.value, metadata, turns);
  requireThat(turn, 'task_board_turn_mismatch', 'The observation must identify a new native turn.');
  if (!operation.value.writes.length) {
    requireThat(task.metadata.currentRevision === task.pin.revision && !task.metadata.effectivelyArchived &&
      task.value.fields.control === 'agent' && !task.value.claim && !task.value.publication && same(task.value.lastRun, request.run),
      'revision_conflict', 'The ticket changed before its direct turn could be adopted.');
    await engine.checks.aggregate(task.value, request.taskId);
  }
  // The existing Run shape retains continuation settings. Native ownership and
  // the known turn distinguish observation from a scheduled start; calls stay null.
  const plan: TaskBoard.NativePlanDraft = { ...previous.value.plan, nativeVersion: metadata.nativeVersion,
    catalogSourceHash: source.catalog.sourceHash, definitions: {
      threadStart: source.contract.definitionHash('thread/start'), threadResume: source.contract.definitionHash('thread/resume'),
      turnStart: source.contract.definitionHash('turn/start'), turnInterrupt: source.contract.definitionHash('turn/interrupt'),
    } };
  const originalRequest: TaskBoard.ContinueRequest = { action: 'continue', operationId: request.operationId, taskId: request.taskId,
    expectedRevision: task.pin.revision, feedback: 'Track the turn started directly in the saved native thread.',
    target: previous.value.target, workspace: previous.value.workspace, intent: previous.value.originalRequest.intent, commentIds: [] };
  const requestHash = hashJson(originalRequest), actor = operation.value.actor, attempt = task.value.attemptCount + 1;
  const reserved = await steps.write('task-board/task', { ...task.value, publication: steps.pin, updatedAt: at },
    { objectId: request.taskId, expectedRevision: task.pin.revision });
  const run = await steps.create('task-board/run', {
    schemaVersion: 1, taskId: request.taskId, attempt, operationId: request.operationId, callerPrincipalId: engine.settings.principalId,
    originalRequest, requestHash, target: originalRequest.target, workspace: originalRequest.workspace, plan, owner: actor,
    executionContext: { workflowState: task.value.workflowState, latestResult: task.value.latestResult, acceptedReview: task.value.acceptedReview },
    phase: 'running', primaryResourceRef: primary, turnId: nativeString(turn['id']), nativeEpoch: metadata.epoch,
    calls: { thread: null, turn: null, interrupt: null }, notificationCursor: 0, result: null, cancellation: null,
    externalOutcome: { state: 'active', observedAt: turns.observedAt, evidence: null, code: null },
    createdAt: at, updatedAt: at, finishedAt: null,
  }, 'Observed native Run');
  const history = await steps.create('task-board/history', { schemaVersion: 1, taskId: request.taskId, operationId: request.operationId,
    actor, createdAt: at, kind: 'continued', detail: 'Tracking a turn started directly in the existing agent conversation.',
    previousTask: task.pin, previousHistory: task.value.lastHistory, priorRun: request.run, fromTarget: null, toTarget: originalRequest.target,
    result: null, review: null }, 'Native history');
  const next: TaskBoard.Task = { ...task.value, workflowState: 'in_progress', publication: null, runtimeWait: null,
    attemptCount: attempt, lastRun: run, lastHistory: history, updatedAt: at,
    claim: { operationId: request.operationId, callerPrincipalId: engine.settings.principalId, attempt, owner: actor, request: originalRequest,
      requestHash, target: originalRequest.target, workspace: originalRequest.workspace, primaryAtStart: primary, claimedAt: at, phase: 'running', run } };
  delete next.completionIssue;
  const published = await steps.write('task-board/task', next, { objectId: request.taskId, expectedRevision: reserved.revision });
  return { operationId: request.operationId, task: published, run, history, plan: null, result: null, review: null, attachment: null };
}
