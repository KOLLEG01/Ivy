import { randomUUID } from 'node:crypto';
import { hashJson } from '../../packages/contracts/src/canonical.js';
import type { Agent, TaskBoard, Wire } from '../../packages/contracts/src/generated.js';
import { TaskBoardEvidence, catalogFor } from '../../services/task-board/src/runtime/evidence.js';
import { evidenceIdentity, intendedCall, retainNativeRequest, resolveNativeRequest } from '../../services/task-board/src/runtime/native-intent.js';
import type { NativeSlot } from '../../services/task-board/src/runtime/native-intent.js';
import { taskBoardFixture } from './task-board.js';

export async function nativeWorkflow(t: { after: (fn: () => void) => void }, nativeVersion: '0.154.0' = '0.154.0', projectWorkspace = false) {
  const f = taskBoardFixture(t, nativeVersion), planned = await f.planned('agent', projectWorkspace
    ? { kind: 'directory_path', path: String(f.plan.threadStart.cwd) } : { kind: 'task_workspace' });
  const startRequest: TaskBoard.StartRequest = { action: 'start', operationId: randomUUID(), taskId: planned.task.objectId, expectedRevision: planned.task.revision, intent: planned.plan, target: planned.target, workspace: planned.workspace, commentIds: [] };
  const started = await f.invoke(startRequest);
  let task = started.task!, run = started.run!;
  const refresh = async () => { task = (await f.first.store.read('task-board/task', task.objectId)).pin; run = (await f.first.store.read('task-board/run', run.objectId)).pin; };
  const request = (change: TaskBoard.NativeUpdateRequest['change'], id = randomUUID()): TaskBoard.NativeUpdateRequest => ({ action: 'nativeUpdate', operationId: id, taskId: task.objectId, expectedRevision: task.revision, run, change });
  const update = async (change: TaskBoard.NativeUpdateRequest['change']) => {
    const result = await f.first.nativeUpdate(request(change)); task = result.task!; run = result.run!; return result;
  };
  const ownerOperation = (method: string, params: Wire.Json, reply: Agent.Reply | null, operationId: string = randomUUID(), phase: Agent.Operation['phase'] = 'succeeded'): Agent.Operation => {
    const now = new Date().toISOString();
    return { schemaVersion: 1, operationId, callerPrincipalId: f.settings.principalId, serviceNodeId: 'native-agent', nativeVersion, nativeExecutableHash: catalogFor(nativeVersion).catalog.nativeExecutableHash, method, params,
      requestHash: hashJson({ method, params }), phase, createdAt: now, updatedAt: now, epoch: 'fixture-epoch', requestId: 'fixture-request-' + operationId, reply, code: phase === 'outcome_unknown' ? 'native_epoch_lost' : phase === 'failed' ? 'native_error' : null };
  };
  const evidence = new TaskBoardEvidence(f.first.store);
  const prepare = async (slot: NativeSlot, origin: TaskBoard.NativeCallOrigin = { kind: "initial" }) => {
    const current = await f.first.store.read('task-board/run', run), intention = await intendedCall(f.first.store, current.value, slot, origin);
    const operationId = randomUUID(), retained = await retainNativeRequest(f.first.store, intention.request, operationId);
    const value: TaskBoard.NativeCall = { ...intention, request: retained, origin, operationId, state: 'prepared', preparedAt: new Date().toISOString(), observedAt: null, ownerPhase: null, epoch: null, evidence: null, code: null };
    const pin = await f.first.store.write('task-board/native-call', value, randomUUID(), { create: { parentId: null, name: 'Native call fixture ' + value.operationId } });
    await update({ kind: 'attachCall', slot, call: pin }); return pin;
  };
  const observe = async (slot: NativeSlot, pin: TaskBoard.ObjectPin, reply: Agent.Reply | null, phase: Agent.Operation['phase'] = 'succeeded', epoch = 'fixture-epoch', overrides: Partial<Agent.Operation> = {}) => {
    const current = await f.first.store.read('task-board/native-call', pin), currentRun = await f.first.store.read('task-board/run', run);
    const observed = ownerOperation(current.value.request.method, (await resolveNativeRequest(f.first.store, current.value.request)).params, reply, current.value.operationId, phase);
    observed.epoch = epoch;
    Object.assign(observed, overrides);
    f.retainOperation(observed);
    const proof = await evidence.save(pin, observed, await evidenceIdentity(f.first.store, currentRun.value, current.value, f.settings.principalId));
    const saved = await f.first.store.write('task-board/native-call', { ...current.value, state: phase === 'outcome_unknown' ? 'outcome_unknown' : 'observed', ownerPhase: phase, observedAt: observed.updatedAt, epoch: observed.epoch, evidence: proof, code: observed.code },
      randomUUID(), { objectId: pin.objectId, expectedRevision: pin.revision });
    await update({ kind: 'observeCall', slot, call: saved }); return saved;
  };
  const nativeThread = (turns: Wire.Json[] = [], status: Wire.Json = { type: 'idle' }) => ({ cliVersion: nativeVersion, createdAt: 1788690000, cwd: '/fixture', ephemeral: false, id: 'native-primary', modelProvider: 'openai', preview: 'Native publication fixture', projectId: null, sessionId: 'native-primary', source: 'appServer', status, turns, updatedAt: 1788690000, canAcceptDirectInput: true, name: 'Fixture' });
  const turn = (status = 'inProgress', text = 'Owner-observed final material.'): Wire.Json => ({ id: 'native-turn', status, items: status === 'inProgress' ? [] : [{ id: 'native-final-message', type: 'agentMessage', text }] });
  const running = async () => {
    const thread = await prepare('thread');
    await observe('thread', thread, { result: { thread: nativeThread(), cwd: '/fixture', model: 'fixture-model', modelProvider: 'openai', approvalPolicy: 'on-request', approvalsReviewer: 'user', sandbox: { type: 'readOnly' }, reasoningEffort: 'high' } });
    const start = await prepare('turn'); await observe('turn', start, { result: { turn: turn() } });
  };
  const snapshot = async (status = 'completed', text?: string, threadStatus: Wire.Json = { type: 'idle' }) => {
    const observed = ownerOperation('thread/read', { threadId: 'native-primary', includeTurns: true }, { result: { thread: nativeThread([turn(status, text)], threadStatus) } });
    f.retainOperation(observed);
    const proof = await evidence.save(run, observed, { operationId: observed.operationId, callerPrincipalId: observed.callerPrincipalId, serviceNodeId: observed.serviceNodeId, nativeVersion: observed.nativeVersion, method: observed.method, params: observed.params });
    return request({ kind: 'snapshot', evidence: proof, nativeOperationId: observed.operationId, notificationCursor: 1 });
  };
  const cancel = async () => { await f.invoke({ action: 'cancel', operationId: randomUUID(), taskId: task.objectId, expectedRevision: task.revision, reason: 'Stop this isolated attempt.' }); await refresh(); };
  return { f, startRequest, started, planned, request, update, prepare, observe, snapshot, running, cancel, refresh, nativeThread, turn,
    ownerOperation, adopt: (outcome: TaskBoard.ActionOutcome) => { task = outcome.task!; run = outcome.run!; }, state: () => ({ task, run }) };
}
