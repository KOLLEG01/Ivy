import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { TaskBoard } from '../packages/contracts/src/generated.js';
import { TaskBoardEvidence } from '../services/task-board/src/runtime/evidence.js';
import { evidenceIdentity, intendedCall, retainNativeRequest, resolveNativeRequest } from '../services/task-board/src/runtime/native-intent.js';
import { nativeWorkflow } from './fixtures/task-board-native.js';

type Workflow = Awaited<ReturnType<typeof nativeWorkflow>>;
const reply = (w: Workflow) => ({ result: { thread: w.nativeThread(), cwd: '/fixture', model: 'fixture-model', modelProvider: 'openai',
  approvalPolicy: 'on-request', approvalsReviewer: 'user', sandbox: { type: 'readOnly' }, reasoningEffort: 'high' } });
async function confirmed(w: Workflow) {
  const initial = await w.prepare('thread'); return w.observe('thread', initial, reply(w));
}
async function successor(w: Workflow, predecessor: TaskBoard.ObjectPin, preparedEpoch = 'replacement-epoch', changes: Partial<TaskBoard.NativeCall> = {}) {
  const current = await w.f.first.store.read('task-board/run', w.state().run), origin: TaskBoard.NativeCallOrigin = { kind: 'reattach', predecessor, preparedEpoch };
  const intent = await intendedCall(w.f.first.store, current.value, 'thread', origin);
  const operationId = randomUUID(), request = await retainNativeRequest(w.f.first.store, intent.request, operationId);
  const value: TaskBoard.NativeCall = { ...intent, request, origin, operationId, state: 'prepared', preparedAt: new Date().toISOString(), observedAt: null, ownerPhase: null, epoch: null, evidence: null, code: null, ...changes };
  return w.f.first.store.write('task-board/native-call', value, randomUUID(), { create: { parentId: null, name: 'Reattach fixture ' + randomUUID() } });
}
const code = (expected: string) => (error: unknown) => error instanceof IvyError && error.code === expected;

test('reattachment retains original thread and prepared turn identities through a second epoch change', async t => {
  const w = await nativeWorkflow(t), previous = await confirmed(w), turn = await w.prepare('turn'), before = w.state();
  const call = await successor(w, previous), prepared = await w.f.first.store.read('task-board/native-call', call);
  const original = await w.f.first.store.read('task-board/native-call', previous);
  assert.equal(original.value.request.method, 'thread/start'); assert.equal(prepared.value.request.method, 'thread/resume');
  assert.notEqual(original.value.operationId, prepared.value.operationId);
  assert.deepEqual((await resolveNativeRequest(w.f.first.store, prepared.value.request)).params, { threadId: 'native-primary', cwd: w.planned.workspace.canonicalCwd, excludeTurns: true });
  const update = w.request({ kind: 'reattachCall', call });
  await w.f.first.nativeUpdate(update); await w.refresh();
  const attached = await w.f.first.store.read('task-board/run', w.state().run);
  assert.deepEqual(attached.value.calls.thread, call); assert.deepEqual(attached.value.calls.turn, turn);
  assert.deepEqual((await w.f.first.store.read('task-board/run', before.run)).value.calls.thread, previous);
  assert.equal(attached.value.nativeEpoch, 'fixture-epoch', 'Preparation does not claim a new native execution epoch.');
  await w.observe('thread', call, reply(w), 'succeeded', 'third-epoch');
  const resumed = await w.f.first.store.read('task-board/run', w.state().run);
  assert.equal(resumed.value.primaryResourceRef!.nativeId, 'native-primary'); assert.equal(resumed.value.nativeEpoch, 'third-epoch');
  assert.deepEqual(resumed.value.calls.turn, turn); assert.deepEqual((await w.f.first.store.read('task-board/native-call', call)).value, prepared.value);
  await w.observe('turn', turn, { result: { turn: w.turn() } }, 'succeeded', 'third-epoch');
  const running = await w.f.first.store.read('task-board/run', w.state().run); assert.equal(running.value.phase, 'running');
  assert.equal(running.value.turnId, 'native-turn'); assert.equal(running.value.calls.turn!.objectId, turn.objectId);
  const tooLate = await successor(w, resumed.value.calls.thread!, 'fourth-epoch');
  await assert.rejects(w.f.first.nativeUpdate(w.request({ kind: 'reattachCall', call: tooLate })), code('task_board_native_order'));
});

test('prepared resume parameters remain replayable after new calls switch to metadata-only replies', async t => {
  const w = await nativeWorkflow(t), previous = await confirmed(w), run = await w.f.first.store.read('task-board/run', w.state().run);
  const intent = await intendedCall(w.f.first.store, run.value, 'thread', { kind: 'reattach', predecessor: previous, preparedEpoch: 'replacement-epoch' });
  const params = { ...intent.request.params }; delete params['excludeTurns'];
  const request = await retainNativeRequest(w.f.first.store, { ...intent.request, params }, randomUUID());
  const call = await successor(w, previous, 'replacement-epoch', { request });
  await w.update({ kind: 'reattachCall', call });
  await w.observe('thread', call, reply(w), 'succeeded', 'replacement-epoch');
  const restored = await w.f.first.store.read('task-board/run', w.state().run);
  assert.equal(restored.value.nativeEpoch, 'replacement-epoch');
  assert.equal(restored.value.primaryResourceRef!.nativeId, 'native-primary');
  const saved = await w.f.first.store.read('task-board/native-call', call.objectId);
  assert.deepEqual(saved.value.request, request);
  assert.deepEqual((await resolveNativeRequest(w.f.first.store, saved.value.request)).params, params);
});

test('reattachment rejects same-epoch, altered predecessor/identity, cancellation and an unresolved turn', async t => {
  const w = await nativeWorkflow(t), previous = await confirmed(w), original = await w.f.first.store.read('task-board/native-call', previous);
  const sameEpoch = await successor(w, previous, 'fixture-epoch');
  await assert.rejects(w.f.first.nativeUpdate(w.request({ kind: 'reattachCall', call: sameEpoch })), code('task_board_native_intent_mismatch'));
  const wrongPredecessor = await successor(w, { ...previous, revision: 1 });
  await assert.rejects(w.f.first.nativeUpdate(w.request({ kind: 'reattachCall', call: wrongPredecessor })), code('task_board_native_intent_mismatch'));
  const reused = await successor(w, previous, 'replacement-epoch', { operationId: original.value.operationId });
  await assert.rejects(w.f.first.nativeUpdate(w.request({ kind: 'reattachCall', call: reused })), code('task_board_native_order'));
  const call = await successor(w, previous);
  const turn = await w.prepare('turn'); await w.observe('turn', turn, null, 'outcome_unknown');
  await assert.rejects(w.f.first.nativeUpdate(w.request({ kind: 'reattachCall', call })), code('task_board_outcome_unresolved'));
  await w.cancel();
  await assert.rejects(w.f.first.nativeUpdate(w.request({ kind: 'reattachCall', call })), code('task_board_native_order'));
  const current = await w.f.first.store.read('task-board/run', w.state().run);
  assert.deepEqual(current.value.calls.thread, previous); assert.equal(current.value.calls.turn!.objectId, turn.objectId); assert.equal(current.value.turnId, null);
});

test('reattachment rejects a turn observed after its pinned Run revision', async t => {
  const w = await nativeWorkflow(t), previous = await confirmed(w), turn = await w.prepare('turn');
  const resume = await successor(w, previous);
  const store = w.f.first.store, run = await store.read('task-board/run', w.state().run);
  const prepared = await store.read('task-board/native-call', turn);
  const observed = w.ownerOperation('turn/start', (await resolveNativeRequest(store, prepared.value.request)).params,
    { result: { turn: w.turn() } }, prepared.value.operationId);
  w.f.retainOperation(observed);
  const proof = await new TaskBoardEvidence(store).save(turn, observed,
    await evidenceIdentity(store, run.value, prepared.value, w.f.settings.principalId));
  await store.write('task-board/native-call', { ...prepared.value, state: 'observed', ownerPhase: 'succeeded',
    observedAt: observed.updatedAt, epoch: observed.epoch, evidence: proof, code: null }, randomUUID(),
    { objectId: turn.objectId, expectedRevision: turn.revision });

  await assert.rejects(w.update({ kind: 'reattachCall', call: resume }), code('task_board_outcome_unresolved'));
  assert.deepEqual((await store.read('task-board/run', w.state().run)).value.calls.turn, turn);
});

test('cancellation waits for both prepared turn and reattachment prevention before publishing cancelled', async t => {
  const w = await nativeWorkflow(t), previous = await confirmed(w), turn = await w.prepare('turn'), resume = await successor(w, previous);
  await w.update({ kind: 'reattachCall', call: resume }); await w.cancel();
  const fenced = { code: 'request_prevented', epoch: null, requestId: null, reply: null };
  await w.observe('turn', turn, null, 'failed', 'unused', fenced);
  const pending = await w.f.first.store.read('task-board/run', w.state().run), task = await w.f.first.store.read('task-board/task', w.state().task);
  assert.equal(pending.value.phase, 'cancel_requested'); assert.equal(pending.value.result, null); assert.equal(task.value.workflowState, 'waiting');
  assert.equal(task.value.waiting!.reason, 'external_outcome');
  await w.observe('thread', resume, null, 'failed', 'unused', fenced);
  const cancelled = await w.f.first.store.read('task-board/run', w.state().run);
  assert.equal(cancelled.value.phase, 'cancelled'); assert.equal(cancelled.value.externalOutcome.code, 'cancelled_before_dispatch');
  assert.ok(cancelled.value.result); assert.equal((await w.f.first.store.read('task-board/task', w.state().task)).value.workflowState, 'cancelled');
});

test('a prevented original thread request closes cancellation without inventing a native context or turn', async t => {
  const w = await nativeWorkflow(t), thread = await w.prepare('thread'); await w.cancel();
  await w.observe('thread', thread, null, 'failed', 'unused', { code: 'request_prevented', epoch: null, requestId: null, reply: null });
  const run = await w.f.first.store.read('task-board/run', w.state().run);
  assert.equal(run.value.phase, 'cancelled'); assert.equal(run.value.primaryResourceRef, null); assert.equal(run.value.turnId, null);
});
