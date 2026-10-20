import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { TaskBoard, Wire } from '../packages/contracts/src/generated.js';
import { TaskBoardEvidence } from '../services/task-board/src/runtime/evidence.js';
import { openQuestions } from '../services/task-board/src/runtime/conversation.js';
import { assessSteps } from '../services/task-board/src/runtime/next-steps.js';
import { NativeProgress } from '../services/task-board/src/runtime/native-progress.js';
import { TaskBoardScheduler } from '../services/task-board/src/runtime/scheduler.js';
import { nativeWorkflow } from './fixtures/task-board-native.js';
import { taskBoardFixture } from './fixtures/task-board.js';

type Workflow = Awaited<ReturnType<typeof nativeWorkflow>>;
const future = () => new Date(Date.now() + 3600_000).toISOString();
const waiting = (): TaskBoard.Handoff => ({ workflowState: 'waiting',
  nextSteps: [{ id: 'later', scope: 'Check the authorized result later.', requires: [{ kind: 'time', at: future() }] }] });
async function comment(w: Workflow, values: Partial<TaskBoard.CommentActionRequest> = {}) {
  await w.refresh();
  const { task } = w.state();
  await w.f.invoke({ action: 'comment', taskId: task.objectId, expectedRevision: task.revision,
    operationId: randomUUID(), commentId: randomUUID(), authorKind: 'agent', purpose: 'update',
    body: 'Workflow evidence.', requests: [], responses: [], attachments: [], replyTo: null, ...values });
  await w.refresh();
}
async function finish(w: Workflow, handoff?: TaskBoard.Handoff) {
  if (handoff) await comment(w, { purpose: 'handoff', handoff });
  await w.refresh();
  w.adopt(await w.f.first.nativeUpdate(await w.snapshot()));
  return w.f.first.store.read('task-board/task', w.state().task);
}
async function snapshot(w: Workflow, threadId: string, turnId: string, status = 'completed', error?: Wire.Json) {
  await w.refresh();
  const observed = w.ownerOperation('thread/read', { threadId, includeTurns: true }, { result: {
    thread: { ...w.nativeThread([{ ...w.turn(status) as object, id: turnId, ...(error ? { error } : {}) }]), id: threadId, sessionId: threadId },
  } });
  w.f.retainOperation(observed);
  const evidence = await new TaskBoardEvidence(w.f.first.store).save(w.state().run, observed, {
    operationId: observed.operationId, callerPrincipalId: observed.callerPrincipalId, serviceNodeId: observed.serviceNodeId,
    nativeVersion: observed.nativeVersion, method: observed.method, params: observed.params,
  });
  await w.update({ kind: 'snapshot', evidence, nativeOperationId: observed.operationId, notificationCursor: 1 });
  return w.f.first.store.read('task-board/task', w.state().task);
}
async function running(w: Workflow, threadId: string, turnId: string) {
  const thread = await w.prepare('thread');
  await w.observe('thread', thread, { result: { thread: { ...w.nativeThread(), id: threadId, sessionId: threadId },
    cwd: '/fixture', model: 'fixture-model', modelProvider: 'openai', approvalPolicy: 'on-request',
    approvalsReviewer: 'user', sandbox: { type: 'readOnly' }, reasoningEffort: 'high' } });
  const turn = await w.prepare('turn');
  await w.observe('turn', turn, { result: { turn: { ...w.turn() as object, id: turnId } } });
}

test('a question replying to a comment remains a prerequisite until that question is answered', async t => {
  const w = await nativeWorkflow(t); await w.running();
  await comment(w, { commentId: 'request', authorKind: 'user', agentDelivery: 'none' });
  await comment(w, { commentId: 'question', purpose: 'question', recipient: { kind: 'user' }, replyTo: 'request' });
  const task = await finish(w, { workflowState: 'waiting', nextSteps: [
    { id: 'answer', scope: 'Use the answer.', requires: [{ kind: 'user', questionId: 'question' }] },
  ] });
  assert.deepEqual(openQuestions(task.value).map(value => value.commentId), ['question']);
  assert.equal((await assessSteps(w.f.first.store, task.value, new Date().toISOString()))[0]!.ready, false);
  assert.equal(await new TaskBoardScheduler(w.f.first).task(task.pin.objectId), null);
  await comment(w, { authorKind: 'user', replyTo: 'question', agentDelivery: 'none' });
  const answered = await w.f.first.store.read('task-board/task', w.state().task);
  assert.equal(openQuestions(answered.value).length, 0);
  assert.equal((await assessSteps(w.f.first.store, answered.value, new Date().toISOString()))[0]!.ready, true);
});

test('manually completing an intermediate result keeps the ticket editable', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const task = await finish(w, waiting());
  assert.equal((await w.f.first.store.read('task-board/result', task.value.latestResult!)).value.kind, 'intermediate');
  await w.f.invoke({ action: 'transition', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, workflowState: 'done', detail: null });
  await comment(w, { authorKind: 'user', agentDelivery: 'none' });
  const current = await w.f.first.store.read('task-board/task', w.state().task);
  assert.equal(current.value.workflowState, 'done');
  await w.f.first.checks.aggregate(current.value, current.pin.objectId);
});

test('metadata preserves Review and Done; a changed assignment reopens with an executable step', async t => {
  const w = await nativeWorkflow(t); await w.running();
  let task = await finish(w, { workflowState: 'review', nextSteps: [] });
  for (const state of ['review', 'done'] as const) {
    if (state === 'done') {
      const result = await w.f.invoke({ action: 'transition', operationId: randomUUID(), taskId: task.pin.objectId,
        expectedRevision: task.pin.revision, workflowState: state, detail: null });
      task = await w.f.first.store.read('task-board/task', result.task!);
    }
    const edited = await w.f.invoke({ action: 'edit', operationId: randomUUID(), taskId: task.pin.objectId,
      expectedRevision: task.pin.revision, fields: { ...task.value.fields, priority: state === 'review' ? 3 : 4 } });
    task = await w.f.first.store.read('task-board/task', edited.task!);
    assert.equal(task.value.workflowState, state);
    assert.equal(task.value.commentDeliveries.filter(value => value.state === 'queued').length, 0);
    assert.equal(await new TaskBoardScheduler(w.f.first).task(task.pin.objectId), null);
  }
  const edited = await w.f.invoke({ action: 'edit', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, fields: { ...task.value.fields, description: 'Implement the changed requirement.' } });
  task = await w.f.first.store.read('task-board/task', edited.task!);
  assert.equal(task.value.workflowState, 'todo');
  assert.equal((await assessSteps(w.f.first.store, task.value, new Date().toISOString())).some(value => value.ready), true);
});

test('native completion preserves a newer next-step plan and exposes the obsolete handoff', async t => {
  const w = await nativeWorkflow(t); await w.running();
  await comment(w, { purpose: 'handoff', handoff: waiting() });
  const task = await w.f.first.store.read('task-board/task', w.state().task);
  const nextSteps: TaskBoard.NextStep[] = [{ id: 'replacement', scope: 'A newer authorized plan.', requires: [{ kind: 'time', at: future() }] }];
  await w.f.invoke({ action: 'edit', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, fields: task.value.fields, nextSteps });
  const done = await finish(w);
  assert.deepEqual(done.value.nextSteps, nextSteps);
  assert.equal(done.value.claim, null);
  assert.equal(done.value.publication, null);
  assert.equal(done.value.completionIssue?.code, 'task_board_handoff_changed');
  assert.ok((await w.f.first.store.read('task-board/run', done.value.lastRun!)).value.result);
});

test('an already published delivery is an independent path through an apparent cycle', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const b = await w.f.planned();
  const a = await finish(w, { workflowState: 'waiting', deliveries: [{ key: 'a', artifacts: [],
    codeReferences: [{ kind: 'commit', url: 'https://example.test/commit/a', label: 'A', commit: 'a' }] }],
    nextSteps: [{ id: 'b', scope: 'Use B.', requires: [{ kind: 'delivery', taskId: b.task.objectId, key: 'b' }] }] });
  const result = await w.f.invoke({ action: 'defer', operationId: randomUUID(), taskId: b.task.objectId,
    expectedRevision: b.task.revision,
    nextSteps: [{ id: 'a', scope: 'Use A.', requires: [{ kind: 'delivery', taskId: a.pin.objectId, key: 'a' }] }] });
  const task = await w.f.first.store.read('task-board/task', result.task!);
  assert.equal((await assessSteps(w.f.first.store, task.value, new Date().toISOString()))[0]!.ready, true);
});

test('two admitted handoffs cannot silently publish a mutual dependency deadlock', async t => {
  const w = await nativeWorkflow(t); await running(w, 'thread-a', 'turn-a');
  const a = w.state(), b = await w.f.planned();
  w.adopt(await w.f.invoke({ action: 'start', operationId: randomUUID(), taskId: b.task.objectId,
    expectedRevision: b.task.revision, intent: b.plan, target: b.target, workspace: b.workspace, commentIds: [], nextStepId: 'main' }));
  await running(w, 'thread-b', 'turn-b');
  await comment(w, { purpose: 'handoff', handoff: { workflowState: 'waiting', nextSteps: [
    { id: 'a', scope: 'Wait for A.', requires: [{ kind: 'delivery', taskId: a.task.objectId, key: 'a' }] },
  ] } });
  const bState = w.state();
  w.adopt({ ...w.started, task: a.task, run: a.run }); await w.refresh();
  await comment(w, { purpose: 'handoff', handoff: { workflowState: 'waiting', nextSteps: [
    { id: 'b', scope: 'Wait for B.', requires: [{ kind: 'delivery', taskId: b.task.objectId, key: 'b' }] },
  ] } });
  const first = await snapshot(w, 'thread-a', 'turn-a');
  assert.equal(first.value.completionIssue, undefined);
  w.adopt({ ...w.started, task: bState.task, run: bState.run }); await w.refresh();
  const second = await snapshot(w, 'thread-b', 'turn-b');
  assert.equal(second.value.completionIssue?.code, 'task_board_dependency_cycle');
  assert.equal(second.value.claim, null);
  assert.equal(second.value.publication, null);
});

test('capacity failures retry the original conversation after backoff without reopening work', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const reviewed = await finish(w, { workflowState: 'review', nextSteps: [] });
  await comment(w, { authorKind: 'user', commentId: 'retry-message', agentDelivery: 'queue' });
  const next = await new TaskBoardScheduler(w.f.first).task(w.state().task.objectId);
  assert.ok(next?.run); w.adopt(next); await w.running();
  const failed = await snapshot(w, 'native-primary', 'native-turn', 'failed',
    { message: 'Selected model is at capacity.', codexErrorInfo: 'serverOverloaded' });
  assert.equal(failed.value.workflowState, 'review');
  assert.deepEqual(failed.value.latestResult, reviewed.value.latestResult);
  assert.equal(failed.value.commentDeliveries.find(value => value.commentId === 'retry-message')?.state, 'queued');
  assert.ok(failed.value.runtimeWait?.retryAt);
  assert.equal(await new TaskBoardScheduler(w.f.first).task(failed.pin.objectId), null);
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(failed.value.runtimeWait!.retryAt!) + 1000 });
  const retry = await new TaskBoardScheduler(w.f.first).task(failed.pin.objectId);
  assert.ok(retry?.run);
  const run = (await w.f.first.store.read('task-board/run', retry.run)).value;
  assert.equal(run.originalRequest.coordinationOnly, undefined);
  assert.deepEqual(run.originalRequest.commentIds, ['retry-message']);
});

test('execution slots cover existing Runs and are checked again at admission', async t => {
  const f = taskBoardFixture(t); f.settings.scheduler.maxConcurrentRuns = 2;
  for (let index = 0; index < 3; index++) {
    const p = await f.planned();
    const request: TaskBoard.StartRequest = { action: 'start', operationId: randomUUID(), taskId: p.task.objectId,
      expectedRevision: p.task.revision, intent: p.plan, target: p.target, workspace: p.workspace, commentIds: [], nextStepId: 'main' };
    if (index < 2) assert.ok((await f.invoke(request)).run);
    else {
      await assert.rejects(f.invoke(request), error => error instanceof IvyError && error.code === 'task_board_execution_capacity');
      const scheduled = await new TaskBoardScheduler(f.first).task(p.task.objectId);
      assert.ok(scheduled?.task);
      const deferred = (await f.first.store.read('task-board/task', scheduled.task)).value;
      assert.equal(deferred.claim, null);
      assert.match(deferred.runtimeWait!.detail, /2 TaskBoard execution slots/);
    }
  }
});

test('silence warnings clear on real activity, user waits and completion without changing execution', () => {
  let now = 0;
  const progress = new NativeProgress('board', 60_000, () => now);
  progress.observe('run', 'task', true, 'unchanged');
  now = 60_001; progress.observe('run', 'task', true, 'unchanged');
  assert.equal(progress.diagnostics[0]?.code, 'task_board_native_silent');
  progress.activity('run'); assert.equal(progress.diagnostics.length, 0);
  now += 60_001; assert.equal(progress.diagnostics.length, 1);
  progress.observe('run', 'task', false, 'waiting_input'); assert.equal(progress.diagnostics.length, 0);
  progress.observe('run', 'task', true, 'resumed'); now += 60_001;
  progress.forget('run'); assert.equal(progress.diagnostics.length, 0);
});

test('an unfinished claim reservation keeps the capacity gate until its Run is published', async t => {
  const f = taskBoardFixture(t); f.settings.scheduler.maxConcurrentRuns = 1;
  const a = await f.planned(), b = await f.planned();
  let release!: () => void, entered!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  const reserved = new Promise<void>(resolve => { entered = resolve; });
  let hold = true;
  f.intercept(async params => {
    if (hold && !('create' in params) && params.objectId === a.task.objectId) {
      hold = false; entered(); await held;
    }
  });
  const start = (p: typeof a): TaskBoard.StartRequest => ({ action: 'start', operationId: randomUUID(),
    taskId: p.task.objectId, expectedRevision: p.task.revision, intent: p.plan, target: p.target,
    workspace: p.workspace, commentIds: [], nextStepId: 'main' });
  const pending = f.invoke(start(a));
  await reserved;
  const other = start(b);
  try { await assert.rejects(f.invoke(other), error => error instanceof IvyError && error.code === 'task_board_publication_busy'); }
  finally { release(); }
  await pending; f.intercept(null);
  const operation = await f.first.operations.find(f.settings.principalId, other.operationId);
  assert.ok(operation);
  await assert.rejects(f.first.recoverOperation(operation.pin.objectId),
    error => error instanceof IvyError && error.code === 'task_board_execution_capacity');
});

test('journal pressure retains the admitted handoff baseline until native completion', async t => {
  const w = await nativeWorkflow(t); await w.running();
  await comment(w, { purpose: 'handoff', handoff: { workflowState: 'review', nextSteps: [] } });
  const store = w.f.first.store;
  await store.collectLocal(Date.now(), 0, store.historyLimits.count);
  const done = await finish(w);
  assert.equal(done.value.workflowState, 'review');
  assert.equal(done.value.completionIssue, undefined);
});
