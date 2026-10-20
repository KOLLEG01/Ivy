import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import type { TaskBoard } from '../packages/contracts/src/generated.js';
import { validateTaskBoard } from '../packages/sdk/src/node.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { TaskBoardScheduler } from '../services/task-board/src/runtime/scheduler.js';
import { content, fields, taskBoardFixture } from './fixtures/task-board.js';

test('omitted initial state queues both root and delegated agent tasks, while manual work stays manual', async t => {
  const f = taskBoardFixture(t);
  for (const control of ['user', 'agent'] as const) {
    const saved = await f.invoke({ action: 'create', operationId: randomUUID(), fields: { ...fields, control } });
    const task = (await f.first.store.read('task-board/task', saved.task!)).value;
    assert.equal(task.workflowState, 'todo');
    assert.equal(task.claim, null);
    assert.equal(task.attemptCount, 0);
    assert.equal(f.first.condition(task).state, control === 'agent' ? 'queued' : 'needs_user');
  }
  const parent = await f.create({ control: 'agent' });
  const child = await f.invoke({ action: 'create', operationId: randomUUID(), parentTaskId: parent.task!.objectId,
    fields: { ...fields, control: 'agent' } });
  assert.equal((await f.first.store.read('task-board/task', child.task!)).value.workflowState, 'todo');
});

test('every valid manual initial state is published directly without native work or invented acceptance', async t => {
  const f = taskBoardFixture(t), scheduler = new TaskBoardScheduler(f.first);
  for (const workflowState of ['backlog', 'todo', 'in_progress', 'waiting', 'review', 'done', 'cancelled'] as const) {
    const request: TaskBoard.CreateRequest = { action: 'create', operationId: randomUUID(), fields, workflowState,
      ...(workflowState === 'waiting' ? { nextSteps: [{ id: 'manual-appointment', scope: 'Continue the manual ticket later.', requires: [{ kind: 'time', at: new Date(Date.now() + 60000).toISOString() }] }] } : {}),
      ...(['review', 'done'].includes(workflowState) ? { initialResult: content } : {}) };
    const saved = await f.invoke(request);
    const task = (await f.first.store.read('task-board/task', saved.task!)).value;
    validateTaskBoard('Task', task);
    assert.equal(task.workflowState, workflowState);
    assert.equal(task.publication, null);
    assert.equal(task.claim, null);
    assert.equal(task.lastRun, null);
    assert.equal(task.acceptedReview, null);
    assert.equal(await scheduler.task(saved.task!.objectId), null);
    if (request.initialResult) {
      const result = (await f.first.store.read('task-board/result', task.latestResult!)).value;
      assert.equal(result.taskId, saved.task!.objectId);
      assert.equal(result.run, null);
      assert.equal(result.kind, 'final');
      assert.deepEqual(result.content, content);
    }
    assert.deepEqual(await f.invoke(request), saved);
    await assert.rejects(f.invoke({ ...request, workflowState: workflowState === 'todo' ? 'backlog' : 'todo' }), { code: 'mutation_conflict' });
  }
  for (const workflowState of ['done', 'cancelled'] as const) {
    const saved = await f.invoke({ action: 'create', operationId: randomUUID(), fields: { ...fields, control: 'agent' }, workflowState });
    const task = (await f.first.store.read('task-board/task', saved.task!)).value;
    assert.equal(task.workflowState, workflowState);
    assert.equal(task.latestResult, null);
    assert.equal(task.acceptedReview, null);
    assert.equal(await scheduler.task(saved.task!.objectId), null);
  }
  assert.equal((await f.first.store.page('task-board/run')).items.length, 0);
  assert.equal((await f.first.store.page('task-board/native-call')).items.length, 0);
  assert.equal((await f.first.store.page('task-board/delivery')).items.length, 0);
});

test('invalid initial states and wait material leave no partial task or execution', async t => {
  const f = taskBoardFixture(t);
  const cases: { change: Partial<TaskBoard.CreateRequest>; code: string }[] = [
    { change: { workflowState: 'in_progress', fields: { ...fields, control: 'agent' } }, code: 'task_board_user_controlled' },
    { change: { workflowState: 'review', fields: { ...fields, control: 'agent' }, initialResult: content }, code: 'task_board_native_evidence_required' },
    { change: { workflowState: 'review' }, code: 'task_board_result_required' },
    { change: { workflowState: 'todo', initialResult: content }, code: 'invalid_arguments' },
    { change: { workflowState: 'done', fields: { ...fields, control: 'agent' }, initialResult: content }, code: 'task_board_native_evidence_required' },
    { change: { workflowState: 'waiting' }, code: 'invalid_arguments' },
    { change: { workflowState: 'waiting', nextSteps: [{ id: 'missing-question', scope: 'Wait for an own question.', requires: [{ kind: 'user', questionId: 'missing' }] }] }, code: 'task_board_comment_mismatch' },
    { change: { workflowState: 'waiting', nextSteps: [{ id: 'missing-task', scope: 'Wait for a task.', requires: [{ kind: 'task', taskId: 'missing', until: 'done' }] }] }, code: 'not_found' },
    { change: { workflowState: 'waiting', nextSteps: [{ id: 'invalid-date', scope: 'Wait for a real date.', requires: [{ kind: 'time', at: '2026-02-30T00:00:00.000Z' }] }] }, code: 'task_board_invalid_date' },
  ];
  for (const { change, code } of cases)
    await assert.rejects(f.invoke({ action: 'create', operationId: randomUUID(), fields, ...change }), { code });
  for (const workflowState of [null, 'blocked', 'ready'])
    assert.throws(() => validateTaskBoard('CreateRequest', { action: 'create', operationId: randomUUID(), fields, workflowState }));
  assert.throws(() => validateTaskBoard('CreateRequest', { action: 'create', operationId: randomUUID(), fields, workflowState: 'waiting', waiting: { reason: 'external_outcome', detail: 'Not a real outcome.' } }));
  assert.equal((await f.first.store.page('task-board/task')).items.length, 0);
  assert.equal((await f.first.store.page('task-board/run')).items.length, 0);
});

test('initial waiting distinguishes idle review from accepted Done and keeps its original time on retry', async t => {
  const f = taskBoardFixture(t), scheduler = new TaskBoardScheduler(f.first);
  const peer = await f.invoke({ action: 'create', operationId: randomUUID(), fields, workflowState: 'review', initialResult: content });
  for (const until of ['done', 'idle'] as const) {
    const saved = await f.invoke({ action: 'create', operationId: randomUUID(), fields: { ...fields, control: 'agent' },
      workflowState: 'waiting', nextSteps: [{ id: 'peer', scope: 'Wait for peer.', requires: [{ kind: 'task', taskId: peer.task!.objectId, until }] }] });
    await scheduler.task(saved.task!.objectId);
    const task = (await f.first.store.read('task-board/task', saved.task!.objectId)).value;
    assert.equal(task.workflowState, until === 'idle' ? 'in_progress' : 'waiting');
    assert.equal(!!task.lastRun, until === 'idle');
  }
  const request: TaskBoard.CreateRequest = { action: 'create', operationId: randomUUID(), workflowState: 'waiting',
    fields, nextSteps: [{ id: 'later', scope: 'Review later.', requires: [{ kind: 'time', at: new Date(Date.now() + 60000).toISOString() }] }] };
  const saved = await f.invoke(request);
  const original = (await f.first.store.read('task-board/task', saved.task!)).value;
  assert.deepEqual(await f.invoke(request), saved);
  assert.deepEqual((await f.first.store.read('task-board/task', saved.task!)).value.nextSteps, original.nextSteps);
});

test('a registered offline host can retain an explicit initial wait without bypassing later live validation', async t => {
  const f = taskBoardFixture(t), scheduler = new TaskBoardScheduler(f.first);
  await f.clients.get('native-agent')!.client.request('service.heartbeat', { ready: false, diagnostics: [] });
  const request: TaskBoard.CreateRequest = { action: 'create', operationId: randomUUID(), workflowState: 'waiting',
    fields: { ...fields, control: 'agent', nativeOptions: { model: 'not-yet-verifiable', reasoningEffort: null, serviceTier: null } },
    nextSteps: [{ id: 'registered-host', scope: 'Continue on the selected host when it is available.', requires: [] }] };
  const saved = await f.invoke(request);
  await scheduler.task(saved.task!.objectId);
  assert.equal((await f.first.store.read('task-board/task', saved.task!.objectId)).value.lastRun, null);
  await f.clients.get('native-agent')!.client.request('service.heartbeat', { ready: true, diagnostics: [] });
  await scheduler.task(saved.task!.objectId);
  assert.equal((await f.first.store.read('task-board/task', saved.task!.objectId)).value.lastRun, null);
  await assert.rejects(f.invoke({ ...request, operationId: randomUUID(), fields: { ...request.fields,
    executionRequirement: { kind: 'host', hostId: 'unknown-host' } } }), { code: 'task_board_target_unavailable' });
});

test('a new Todo agent task cannot allocate until its original Create publication recovers', async t => {
  const f = taskBoardFixture(t), scheduler = new TaskBoardScheduler(f.first);
  const request: TaskBoard.CreateRequest = { action: 'create', operationId: randomUUID(), fields: { ...fields, control: 'agent' } };
  let lost = false;
  f.intercept(params => {
    if (!lost && 'create' in params && params.create.contractKey === 'task-board/task') {
      lost = true;
      throw new IvyError('synthetic_create_response_loss', 'Lost the original Create response.', 'unknown');
    }
  });
  await assert.rejects(f.invoke(request), { code: 'synthetic_create_response_loss' });
  const id = (await f.first.store.page('task-board/task')).items[0]!.objectId;
  const partial = (await f.first.store.read('task-board/task', id)).value;
  assert.equal(partial.workflowState, 'todo');
  assert.ok(partial.publication);
  assert.equal(await scheduler.task(id), null);
  assert.equal((await f.first.store.page('task-board/run')).items.length, 0);
  f.intercept(null);
  const operation = await f.first.operations.find('user', request.operationId);
  const saved = await f.first.recoverOperation(operation!.pin.objectId);
  assert.deepEqual(await f.invoke(request), saved);
  await scheduler.task(id);
  assert.equal((await f.first.store.read('task-board/task', id)).value.workflowState, 'in_progress');
  assert.equal((await f.first.store.page('task-board/run')).items.length, 1);
});

for (const interrupted of ['task', 'result', 'history']) test(`initial Review recovers a lost ${interrupted} write without changing state or duplicating material`, async t => {
  const f = taskBoardFixture(t);
  const request: TaskBoard.CreateRequest = { action: 'create', operationId: randomUUID(), fields,
    workflowState: 'review', initialResult: content };
  let lost = false;
  f.intercept(params => {
    if (!lost && 'create' in params && params.create.contractKey === 'task-board/' + interrupted) {
      lost = true;
      throw new IvyError('synthetic_create_response_loss', 'Lost the original Create response.', 'unknown');
    }
  });
  await assert.rejects(f.invoke(request), { code: 'synthetic_create_response_loss' });
  const tasks = (await f.first.store.page('task-board/task')).items;
  assert.equal(tasks.length, 1);
  const partial = (await f.first.store.read('task-board/task', tasks[0]!.objectId)).value;
  assert.equal(partial.workflowState, 'review');
  assert.ok(partial.publication);
  assert.equal(f.first.condition(partial).state, 'publishing');
  assert.equal(await new TaskBoardScheduler(f.first).task(tasks[0]!.objectId), null);
  f.intercept(null);
  const operation = await f.first.operations.find('user', request.operationId);
  const saved = await f.first.recoverOperation(operation!.pin.objectId);
  const task = (await f.first.store.read('task-board/task', saved.task!)).value;
  assert.equal(task.workflowState, 'review');
  assert.equal(task.publication, null);
  assert.ok(task.latestResult);
  assert.equal((await f.first.store.page('task-board/result')).items.length, 1);
  assert.deepEqual(await f.invoke(request), saved);
});
