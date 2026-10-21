import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { taskBoardFixture, fields } from './fixtures/task-board.js';
import { nativeWorkflow } from './fixtures/task-board-native.js';
import { TaskBoardScheduler } from '../services/task-board/src/runtime/scheduler.js';
import { dependencySnapshot, shouldWake, wakeCondition } from '../services/task-board/src/runtime/wake.js';
import { upgradeTaskRecord } from '../packages/contracts/src/task-board-records.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { TaskBoard } from '../packages/contracts/src/generated.js';

test('append-only comments and uploads rebase without losing concurrent input or duplicating retries', async t => {
  const f = taskBoardFixture(t), created = await f.create(), pin = created.task!;
  const comment = (text: string) => ({ action: 'comment' as const, operationId: randomUUID(), taskId: pin.objectId,
    expectedRevision: pin.revision, commentId: randomUUID(), body: text, requests: [], responses: [], attachments: [], replyTo: null });
  const first = comment('First'), second = comment('Second');
  await f.invoke(first); const result = await f.invoke(second);
  const upload = { action: 'uploadAttachment' as const, operationId: randomUUID(), taskId: pin.objectId,
    expectedRevision: pin.revision, attachmentId: randomUUID(), filename: 'note.txt', mediaType: 'text/plain', bytesBase64: Buffer.from('proof').toString('base64') };
  await f.invoke(upload);
  assert.deepEqual(await f.invoke(second), result);
  const task = (await f.first.store.read('task-board/task', pin.objectId)).value;
  assert.deepEqual(task.comments.map(value => value.body), ['First', 'Second']);
  assert.equal(task.attachments.length, 1);
  assert.equal(task.commentDeliveries.length, 2);
});

test('a normal successful turn needs no step plan or handoff and active work can continue', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const ended = await w.f.first.nativeUpdate(await w.snapshot());
  const task = (await w.f.first.store.read('task-board/task', ended.task!)).value;
  assert.equal(task.workflowState, 'todo'); assert.equal(task.completionIssue, undefined);
  assert.equal('nextSteps' in task, false);
  assert.ok((await new TaskBoardScheduler(w.f.first).task(ended.task!.objectId))?.run);
});

test('an agent wait without a trigger is an error and retries until it records a real wait', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const finishWaiting = async (wakeAt: string | null) => {
    await w.refresh();
    await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: w.state().task.objectId,
      expectedRevision: w.state().task.revision, commentId: randomUUID(), authorKind: 'agent', purpose: 'handoff',
      body: 'Current outcome.', handoff: { workflowState: 'waiting', wakeAt, dependencies: [] },
      requests: [], responses: [], attachments: [], replyTo: null });
    await w.refresh(); w.adopt(await w.f.first.nativeUpdate(await w.snapshot()));
    return (await w.f.first.store.read('task-board/task', w.state().task)).value;
  };
  for (let attempt = 0; attempt < 2; attempt++) {
    const task = await finishWaiting(null), now = Date.parse(task.updatedAt);
    const card = await w.f.first.read({ task: w.state().task.objectId, view: 'card' });
    assert.equal(card.executionCondition.state, 'failed');
    assert.equal(await shouldWake(w.f.first.store, task, new Date(now + 29_999).toISOString()), false);
    assert.equal(await shouldWake(w.f.first.store, task, new Date(now + 30_000).toISOString()), true);
    // Admission checks the real clock again; age the persisted wait for that independent check.
    const pin = w.state().task;
    await w.f.first.store.write('task-board/task', { ...task, updatedAt: new Date(Date.now() - 31_000).toISOString() }, randomUUID(),
      { objectId: pin.objectId, expectedRevision: pin.revision });
    const scheduler = new TaskBoardScheduler(w.f.first);
    const started = await scheduler.task(w.state().task.objectId);
    assert.ok(started?.run); w.adopt(started);
    const run = (await w.f.first.store.read('task-board/run', started.run)).value;
    assert.match(JSON.stringify(run.plan.turnStart.input), /waiting without a wake-up trigger/);
    assert.equal(await scheduler.task(w.state().task.objectId), null);
    await w.running();
  }
  const waiting = await finishWaiting('2099-01-01T00:00:00.000Z');
  assert.equal(wakeCondition(waiting).state, 'deferred');
  assert.equal(await shouldWake(w.f.first.store, waiting, new Date(Date.now() + 60_000).toISOString()), false);
});

test('wake recovery preserves explicit pauses, manual work and both question recipients', async t => {
  const f = taskBoardFixture(t), created = await f.create({ control: 'agent' });
  const original = (await f.first.store.read('task-board/task', created.task!)).value;
  const task: TaskBoard.Task = { ...original, workflowState: 'waiting' };
  const at = new Date(Date.parse(task.updatedAt) + 60_000).toISOString();
  for (const workflowState of ['backlog', 'review', 'done', 'cancelled'] as const)
    assert.equal(await shouldWake(f.first.store, { ...task, workflowState }, at), false);
  const manual = { ...task, fields: { ...task.fields, control: 'user' as const } };
  assert.equal(await shouldWake(f.first.store, manual, at), false);
  assert.equal(wakeCondition(manual).state, 'needs_user');
  for (const recipient of [{ kind: 'user' as const }, { kind: 'task' as const, taskId: 'peer' }]) {
    const question: TaskBoard.Comment = { commentId: 'question', sequence: 1, author: { principalId: 'worker', serviceNodeId: 'board', generation: 1, source: 'worker' },
      authorKind: 'agent', purpose: 'question', recipient, body: 'Please answer.', requests: [], responses: [],
      delivery: null, attachments: [], run: null, turnId: null, replyTo: null, createdAt: task.updatedAt, operationId: 'question' };
    const waiting = { ...task, comments: [question], openQuestionCount: recipient.kind === 'user' ? 1 : 0 };
    assert.equal(await shouldWake(f.first.store, waiting, at), false);
    assert.equal(wakeCondition(waiting).state, recipient.kind === 'user' ? 'needs_user' : 'blocked_dependency');
  }
});

test('watched tickets wake on material changes, archive and deletion, not claim or receipt churn', async t => {
  const f = taskBoardFixture(t), peer = await f.create();
  const waiting = await f.invoke({ action: 'create', operationId: randomUUID(), fields: { ...fields, control: 'agent' },
    workflowState: 'waiting', dependencies: [peer.task!.objectId] });
  const task = (await f.first.store.read('task-board/task', waiting.task!)).value, at = new Date().toISOString();
  assert.equal(await shouldWake(f.first.store, task, at), false);
  const other = await f.first.store.read('task-board/task', peer.task!);
  await f.first.store.write('task-board/task', { ...other.value, updatedAt: at }, randomUUID(),
    { objectId: other.pin.objectId, expectedRevision: other.pin.revision });
  assert.equal(await shouldWake(f.first.store, task, at), false);
  const fresh = await f.first.store.read('task-board/task', other.pin.objectId);
  await f.invoke({ action: 'edit', operationId: randomUUID(), taskId: fresh.pin.objectId, expectedRevision: fresh.pin.revision,
    fields: { ...fresh.value.fields, description: 'A materially changed requirement.' } });
  assert.equal(await shouldWake(f.first.store, task, at), true);
  task.observedDependencies = await dependencySnapshot(f.first.store, task);
  assert.equal(await shouldWake(f.first.store, task, at), false);
  const read = f.first.store.read.bind(f.first.store);
  f.first.store.read = (async (...args: Parameters<typeof read>) => {
    const value = await read(...args);
    return args[1] === peer.task!.objectId ? { ...value, metadata: { ...value.metadata, effectivelyArchived: true } } : value;
  }) as typeof read;
  assert.equal(await shouldWake(f.first.store, task, at), true);
  task.observedDependencies = await dependencySnapshot(f.first.store, task);
  f.first.store.read = (async (...args: Parameters<typeof read>) => {
    if (args[1] === peer.task!.objectId) throw new IvyError('not_found', 'The watched ticket was deleted.');
    return read(...args);
  }) as typeof read;
  assert.equal(await shouldWake(f.first.store, task, at), true);
});

test('due appointments wake without closing questions and future appointments survive a comment turn', async t => {
  const w = await nativeWorkflow(t); await w.running(); await w.refresh();
  const wakeAt = new Date(Date.now() + 3600_000).toISOString();
  await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: w.state().task.objectId, expectedRevision: w.state().task.revision,
    commentId: 'wait', authorKind: 'agent', purpose: 'handoff', handoff: { workflowState: 'waiting', wakeAt },
    body: 'Wait for the appointment.', requests: [], responses: [], attachments: [], replyTo: null });
  await w.refresh(); w.adopt(await w.f.first.nativeUpdate(await w.snapshot()));
  const saved = await w.f.first.store.read('task-board/task', w.state().task);
  assert.equal(await shouldWake(w.f.first.store, saved.value, new Date().toISOString()), false);
  assert.equal(await shouldWake(w.f.first.store, saved.value, wakeAt), true);
  await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: saved.pin.objectId, expectedRevision: saved.pin.revision,
    commentId: 'new-input', body: 'A normal comment.', requests: [], responses: [], attachments: [], replyTo: null });
  const next = await new TaskBoardScheduler(w.f.first).task(saved.pin.objectId); assert.ok(next?.run); w.adopt(next);
  await w.running(); w.adopt(await w.f.first.nativeUpdate(await w.snapshot()));
  const after = (await w.f.first.store.read('task-board/task', w.state().task)).value;
  assert.equal(after.wakeAt, wakeAt); assert.equal(after.workflowState, 'waiting'); assert.equal(after.completionIssue, undefined);
});

test('retained steps become context and preserve all appointment notes with only the next wake-up scheduled', async t => {
  const f = taskBoardFixture(t), created = await f.create(), task = (await f.first.store.read('task-board/task', created.task!)).value;
  const { wakeAt: _wake, dependencies: _deps, ...old } = task;
  const migrated = upgradeTaskRecord({ ...old, nextSteps: [
    { id: 'first', scope: 'First provider check', requires: [{ kind: 'time', at: '2099-01-01T00:00:00.000Z' }] },
    { id: 'second', scope: 'Second provider check', requires: [{ kind: 'time', at: '2099-01-02T00:00:00.000Z' }, { kind: 'delivery', taskId: 'other', key: 'proof' }] },
  ] } as unknown as TaskBoard.Task);
  assert.equal(migrated.wakeAt, '2099-01-01T00:00:00.000Z');
  assert.deepEqual(migrated.dependencies, ['other']); assert.equal('nextSteps' in migrated, false);
  assert.match(migrated.comments.at(-1)!.body, /First provider check[\s\S]*Second provider check[\s\S]*2099-01-02/);
  assert.deepEqual(upgradeTaskRecord(migrated), migrated);
});
