import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { digest, hashJson } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { validateTaskBoard } from '../packages/contracts/src/task-board-validation.js';
import { admitSchema } from '../packages/contracts/src/schema.js';
import { taskBoardTools } from '../services/task-board/src/runtime/registry.js';
import type { TaskBoard, Wire } from '../packages/contracts/src/generated.js';
import { TaskBoardScheduler } from '../services/task-board/src/runtime/scheduler.js';
import { TaskBoardReconciler } from '../services/task-board/src/runtime/reconciler.js';
import { shouldWake, wakeCondition, dependencySnapshot, validateDependencies } from '../services/task-board/src/runtime/wake.js';
import { nativeWorkflow } from './fixtures/task-board-native.js';
import { content, context } from './fixtures/task-board.js';
import { TaskBoardEvidence } from '../services/task-board/src/runtime/evidence.js';
import { referencePageKey, taskReferences } from '../services/task-board/src/runtime/reference-pages.js';

type Workflow = Awaited<ReturnType<typeof nativeWorkflow>>;
const code = (expected: string) => (error: unknown) => error instanceof IvyError && error.code === expected;
async function handoff(w: Workflow, intent: TaskBoard.Handoff) {
  await w.refresh();
  const task = w.state().task;
  const request: TaskBoard.CommentActionRequest = { action: 'comment', operationId: randomUUID(), taskId: task.objectId,
    expectedRevision: task.revision, commentId: randomUUID(), authorKind: 'agent', purpose: 'handoff', body: 'The work follows this completion intent.',
    handoff: intent, requests: [], responses: [], attachments: [], replyTo: null };
  await w.f.invoke(request); await w.refresh(); return request;
}
async function finish(w: Workflow, intent?: TaskBoard.Handoff) {
  if (intent) await handoff(w, intent);
  const outcome = await w.f.first.nativeUpdate(await w.snapshot()); w.adopt(outcome);
  return w.f.first.store.read('task-board/task', outcome.task!);
}

test('operation receipts fit the normal service schema admission limits', () => {
  for (const name of ['operation', 'operationReceipt']) assert.doesNotThrow(() => admitSchema(taskBoardTools.find(tool => tool.name === name)!.outputSchema));
});

test('an own handoff rejects sourceTaskId precisely and keeps the active claim available for correction', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const task = w.state().task;
  const request: TaskBoard.CommentActionRequest = { action: 'comment', operationId: randomUUID(), taskId: task.objectId,
    expectedRevision: task.revision, sourceTaskId: task.objectId, commentId: randomUUID(), authorKind: 'agent', purpose: 'handoff',
    body: 'Ready for acceptance.', handoff: { workflowState: 'review', wakeAt: null, dependencies: [] }, requests: [], responses: [], attachments: [], replyTo: null };
  await assert.rejects(w.f.invoke(request), code('task_board_handoff_source'));
  assert.ok((await w.f.first.store.read('task-board/task', task.objectId)).value.claim);
  const done = await finish(w, request.handoff);
  assert.equal(done.value.workflowState, 'review'); assert.equal(done.value.claim, null);
});

test('native completion reconciles an accepted handoff after another original publication, without changing its identity', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const client = w.f.first.store.client, original = client.request.bind(client), task = await w.f.first.store.read('task-board/task', w.state().task);
  const edit: TaskBoard.EditRequest = { action: 'edit', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, fields: { ...task.value.fields, priority: 4 } };
  client.request = (async (method, params, options) => {
    if (method === 'objects.write' && 'create' in params && params.create?.contractKey === 'task-board/history')
      throw new IvyError('synthetic_write_failure', 'The history destination is temporarily unavailable.', 'unknown');
    return original(method, params, options);
  }) as typeof client.request;
  try { await assert.rejects(w.f.invoke(edit), code('synthetic_write_failure')); }
  finally { client.request = original; }
  await w.refresh();
  const current = w.state().task;
  const intent: TaskBoard.CommentActionRequest = { action: 'comment', operationId: randomUUID(), taskId: current.objectId,
    expectedRevision: current.revision, commentId: 'retained-handoff', authorKind: 'agent', purpose: 'handoff', body: 'Ready after the original publication settles.',
    handoff: { workflowState: 'review', wakeAt: null, dependencies: [] }, requests: [], responses: [], attachments: [], replyTo: null };
  await assert.rejects(w.f.invoke(intent), code('task_board_publication_busy'));
  const accepted = await w.f.first.operations.find('user', intent.operationId);
  assert.equal(accepted?.value.phase, 'accepted'); assert.equal(accepted?.value.requestHash, hashJson(intent));
  assert.equal(accepted?.value.handoffRun?.objectId, w.state().run.objectId);
  const originalEdit = await w.f.first.operations.find('user', edit.operationId);
  await w.f.second.recoverOperation(originalEdit!.pin.objectId); await w.refresh();
  w.f.interceptRead(params => {
    if (params.objectId === intent.taskId && params.revision === intent.expectedRevision)
      throw new IvyError('revision_pruned', 'The old ticket snapshot was pruned; the accepted intent keeps its original work binding.');
  });
  const native = await w.snapshot(), outcome = await w.f.second.nativeUpdate(native);
  w.f.interceptRead(null);
  const done = await w.f.first.store.read('task-board/task', outcome.task!);
  assert.equal(done.value.workflowState, 'review'); assert.equal(done.value.claim, null); assert.equal(done.value.fields.priority, 4);
  assert.equal(done.value.comments.filter(comment => comment.commentId === intent.commentId).length, 1);
  const recovered = await w.f.first.operations.find('user', intent.operationId);
  assert.equal(recovered?.value.phase, 'succeeded'); assert.deepEqual(recovered?.value.request, intent);
  assert.equal(recovered?.value.requestHash, accepted?.value.requestHash);
  assert.deepEqual(await w.f.first.nativeUpdate(native), outcome);
});

test('terminal receipts survive payload eviction, remain caller-bound and do not authorize replay', async t => {
  const w = await nativeWorkflow(t); await w.running(); await finish(w, { workflowState: 'review', wakeAt: null, dependencies: [] });
  const operationId = w.startRequest.operationId, caller = context(operationId, w.f.settings.principalId),
    before = await w.f.first.operation(operationId, caller, true);
  w.f.first.store.historyLimits.count = 0; await w.f.first.store.collectLocal();
  const after = await w.f.second.operation(operationId, caller, true);
  assert.deepEqual(after, before); assert.equal('receipt' in after && after.receipt, true);
  assert.equal('request' in after, false); assert.equal(after.requestHash, hashJson(w.startRequest));
  await assert.rejects(w.f.first.operation(operationId, caller), code('operation_expired'));
  await assert.rejects(w.f.first.operation(operationId, context(operationId, 'other-caller'), true), code('not_found'));
  await assert.rejects(w.f.invoke(w.startRequest), code('operation_expired'));
});

test('comment compaction failures back off independently while workflow recovery keeps progressing', async t => {
  const w = await nativeWorkflow(t); await w.running();
  let now = Date.now(), compactions = 0, recovered = 0, scheduled = 0;
  t.mock.method(Date, 'now', () => now);
  const runtime = new TaskBoardReconciler(w.f.first);
  t.after(() => runtime.stop());
  runtime.native.drain = async () => { recovered++; return true; };
  runtime.signals.step = async () => null;
  runtime.scheduler.task = async () => { scheduled++; return null; };
  w.f.first.store.compactComments = async () => { compactions++; throw new IvyError('synthetic_compaction_failure', 'Conversation storage is unavailable.'); };
  for (let pass = 0; pass < 20; pass++) { await runtime.tick(); await runtime.settled(); }
  assert.equal(compactions, 1); assert.ok(recovered > 0); assert.ok(scheduled > 0);
  assert.ok(runtime.diagnostics.some(value => value.code === 'synthetic_compaction_failure'));
  now += 60_001;
  w.f.first.store.compactComments = async () => { compactions++; return true; };
  await runtime.tick(); await runtime.settled();
  assert.equal(compactions, 2); assert.ok(!runtime.diagnostics.some(value => value.code === 'synthetic_compaction_failure'));
});
