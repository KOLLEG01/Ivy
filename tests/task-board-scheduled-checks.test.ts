import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import type { TaskBoard } from '../packages/contracts/src/generated.js';
import { openQuestions } from '../services/task-board/src/runtime/conversation.js';
import { TaskBoardScheduler } from '../services/task-board/src/runtime/scheduler.js';
import { nativeWorkflow } from './fixtures/task-board-native.js';
import { resolveNativeRequest } from '../services/task-board/src/runtime/native-intent.js';
import { IvyError } from '../packages/contracts/src/errors.js';

type Workflow = Awaited<ReturnType<typeof nativeWorkflow>>;
const later = () => new Date(Date.now() + 6 * 60 * 60_000).toISOString();
const waiting = (): TaskBoard.Handoff => ({ workflowState: 'waiting', waiting: { reason: 'time', detail: 'External approval is still pending.' }, nextReviewAt: later(), checkScope: 'Read the approval status only; do not perform any action needing user approval.' });
async function comment(w: Workflow, fields: Partial<TaskBoard.CommentActionRequest>) {
  await w.refresh();
  const { task } = w.state();
  const result = await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.objectId, expectedRevision: task.revision,
    commentId: randomUUID(), authorKind: 'agent', purpose: 'update', body: 'Progress.', requests: [], responses: [], attachments: [], replyTo: null, ...fields });
  await w.refresh(); return result;
}
async function finish(w: Workflow, handoff?: TaskBoard.Handoff, status = 'completed') {
  if (handoff) await comment(w, { purpose: 'handoff', body: 'Check finished; the overall work follows the declared intent.', handoff });
  const result = await w.f.first.nativeUpdate(await w.snapshot(status));
  w.adopt(result);
  return (await w.f.first.store.read('task-board/task', result.task!)).value;
}

test('a successful native turn without a declared completion intent is not ready for Review', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const task = await finish(w);
  assert.equal(task.workflowState, 'waiting');
  assert.match(task.waiting!.detail, /without an explicit completion intent/);
  assert.equal(task.claim, null);
});

test('a handoff retains the active claim and publishes waiting and its next check only at confirmed completion', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const handoff = waiting();
  await comment(w, { purpose: 'handoff', handoff });
  const active = (await w.f.first.store.read('task-board/task', w.state().task)).value;
  assert.equal(active.workflowState, 'in_progress'); assert.ok(active.claim);
  assert.equal(active.fields.nextReviewAt, null); assert.equal(active.scheduledCheck, undefined);
  assert.equal(await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId), null);
  const task = await finish(w);
  assert.equal(task.workflowState, 'waiting'); assert.equal(task.waiting?.reason, 'time');
  assert.equal(task.fields.nextReviewAt, handoff.nextReviewAt);
  assert.equal(task.scheduledCheck?.scope, handoff.checkScope);
  assert.equal((await w.f.first.store.read('task-board/result', task.latestResult!)).value.kind, 'intermediate');
});

test('handoffs and unrelated user comments cannot answer an outstanding question', async t => {
  const w = await nativeWorkflow(t); await w.running();
  await comment(w, { purpose: 'question', commentId: 'approve-action', body: 'May I perform the write?' });
  await comment(w, { authorKind: 'user', body: 'Please also keep the log.', agentDelivery: 'none' });
  const task = await finish(w, waiting());
  assert.equal(task.openQuestionCount, 1); assert.equal(openQuestions(task)[0]?.commentId, 'approve-action');
  assert.equal(w.f.first.condition(task).state, 'needs_user');
  await comment(w, { authorKind: 'user', body: 'Yes, approved.', replyTo: 'approve-action', agentDelivery: 'none' });
  assert.equal(openQuestions((await w.f.first.store.read('task-board/task', w.state().task)).value).length, 0);
});

test('a due independent check starts once after scheduler recreation while its question remains open', async t => {
  const w = await nativeWorkflow(t); await w.running();
  await comment(w, { purpose: 'question', commentId: 'needs-choice', body: 'Choose the account before publishing.' });
  const task = await finish(w, waiting());
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(task.fields.nextReviewAt!) + 1000 });
  const result = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId);
  assert.ok(result?.run); w.adopt(result);
  const run = (await w.f.first.store.read('task-board/run', result.run)).value;
  assert.equal(run.originalRequest.checkOnly, true); assert.deepEqual(run.originalRequest.commentIds, []);
  assert.match(JSON.stringify(run.plan.turnStart), /only the due independent check/);
  assert.equal(await new TaskBoardScheduler(w.f.first).task(w.state().task.objectId), null);
  await w.running();
  const running = (await w.f.first.store.read('task-board/run', w.state().run)).value;
  const call = (await w.f.first.store.read('task-board/native-call', running.calls.turn!)).value;
  assert.match(JSON.stringify((await resolveNativeRequest(w.f.first.store, call.request)).params), /only the due independent check/);
  const after = await finish(w, waiting());
  assert.equal(after.workflowState, 'waiting'); assert.equal(after.openQuestionCount, 1);
  assert.equal(openQuestions(after)[0]?.commentId, 'needs-choice');
  assert.ok(after.fields.nextReviewAt! > new Date().toISOString());
});

test('an independent check preserves a result awaiting Review and does not emit another completion', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const handoff = waiting(); handoff.workflowState = 'review'; delete handoff.waiting;
  const task = await finish(w, handoff);
  assert.equal(task.workflowState, 'review');
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(task.fields.nextReviewAt!) + 1000 });
  const result = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId);
  assert.ok(result?.run); w.adopt(result); await w.running();
  const after = await finish(w, waiting());
  assert.equal(after.workflowState, 'review'); assert.deepEqual(after.latestResult, task.latestResult);
  assert.deepEqual(after.acceptedReview, task.acceptedReview);
  assert.equal(after.comments.at(-1)?.delivery, null);
});

test('failed checks keep their scope, show their failure and wait before retrying', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const task = await finish(w, waiting());
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(task.fields.nextReviewAt!) + 1000 });
  const result = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId);
  assert.ok(result?.run); w.adopt(result); await w.running();
  const after = await finish(w, undefined, 'failed');
  assert.equal(after.workflowState, 'waiting'); assert.ok(after.scheduledCheck?.failure);
  assert.equal(after.scheduledCheck.scope, task.scheduledCheck?.scope);
  assert.ok(Date.parse(after.fields.nextReviewAt!) >= Date.now() + 15 * 60_000);
  assert.equal(w.f.first.condition(after).state, 'blocked_environment');
  assert.equal(await new TaskBoardScheduler(w.f.first).task(w.state().task.objectId), null);
});

test('an unscoped date cannot bypass an unanswered question', async t => {
  const w = await nativeWorkflow(t); await w.running();
  await comment(w, { purpose: 'question', body: 'What should be done?', commentId: 'unresolved' });
  const handoff = waiting(); delete handoff.checkScope;
  const task = await finish(w, handoff);
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(task.fields.nextReviewAt!) + 1000 });
  assert.equal(await new TaskBoardScheduler(w.f.first).task(w.state().task.objectId), null);
});

test('missing check intent preserves a confirmed later retry instead of shortening it', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const task = await finish(w, waiting());
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(task.fields.nextReviewAt!) + 1000 });
  const started = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId);
  assert.ok(started?.run); w.adopt(started); await w.running();
  const nextReviewAt = new Date(Date.now() + 30 * 60_000).toISOString();
  const peer = await w.f.planned('agent');
  await w.f.invoke({ action: 'defer', operationId: randomUUID(), taskId: w.state().task.objectId,
    expectedRevision: w.state().task.revision, reason: 'dependency', detail: 'The same independent check will retry later.',
    nextReviewAt, checkScope: task.scheduledCheck!.scope, blocker: { taskId: peer.task.objectId, until: 'idle' } });
  await w.refresh();
  const after = await finish(w);
  assert.equal(after.fields.nextReviewAt, nextReviewAt);
  assert.equal(after.scheduledCheck?.failure?.code, 'task_board_check_handoff_missing');
  assert.equal(await new TaskBoardScheduler(w.f.first).task(w.state().task.objectId), null);
});

for (const review of [false, true]) test(`missing check intent cannot restore a check whose timer was explicitly cleared (${review ? 'Review' : 'waiting'})`, async t => {
  const w = await nativeWorkflow(t); await w.running();
  const intent = waiting();
  if (review) { intent.workflowState = 'review'; delete intent.waiting; }
  const task = await finish(w, intent);
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(task.fields.nextReviewAt!) + 1000 });
  const started = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId);
  assert.ok(started?.run); w.adopt(started); await w.running();
  const current = await w.f.first.store.read('task-board/task', w.state().task);
  await w.f.invoke({ action: 'edit', operationId: randomUUID(), taskId: current.pin.objectId,
    expectedRevision: current.pin.revision, fields: { ...current.value.fields, nextReviewAt: null } });
  await w.refresh();
  assert.equal((await w.f.first.store.read('task-board/task', w.state().task)).value.scheduledCheck, undefined);
  const after = await finish(w);
  assert.equal(after.fields.nextReviewAt, null);
  assert.equal(after.scheduledCheck, undefined);
  const recovery = await new TaskBoardScheduler(w.f.first).task(w.state().task.objectId);
  assert.ok(recovery?.run);
  const recoveryRun = await w.f.first.store.read('task-board/run', recovery.run);
  assert.equal(recoveryRun.value.originalRequest.checkOnly, undefined);
  assert.ok(recoveryRun.value.originalRequest.action === 'continue' && recoveryRun.value.originalRequest.recoveryFor);
  assert.equal((await w.f.first.store.read('task-board/task', w.state().task.objectId)).value.fields.nextReviewAt, null);
  if (review) {
    w.adopt(recovery); await w.running();
    const restored = await finish(w, { workflowState: 'review', nextReviewAt: null });
    assert.equal(restored.workflowState, 'review');
    assert.deepEqual(restored.latestResult, task.latestResult);
    assert.equal(restored.completionIssue, undefined);
    assert.equal(restored.fields.nextReviewAt, null);
    assert.equal(restored.scheduledCheck, undefined);
    assert.equal(await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId), null);
  }
});

test('an explicit deferral stop removes the check scope and dependency reconciliation cannot rearm it', async t => {
  const w = await nativeWorkflow(t); await w.running();
  await finish(w, waiting());
  const current = await w.f.first.store.read('task-board/task', w.state().task);
  const peer = await w.f.planned('agent');
  await w.f.invoke({ action: 'defer', operationId: randomUUID(), taskId: current.pin.objectId, expectedRevision: current.pin.revision,
    reason: 'dependency', detail: 'The periodic check is stopped; the real delivery remains pending.', nextReviewAt: null,
    blocker: { taskId: peer.task.objectId, until: 'done' } });
  const stopped = await w.f.first.store.read('task-board/task', current.pin.objectId);
  assert.equal(stopped.value.scheduledCheck, undefined); assert.equal(stopped.value.fields.nextReviewAt, null);
  await new TaskBoardScheduler(w.f.second).task(stopped.pin.objectId);
  const after = await w.f.first.store.read('task-board/task', current.pin.objectId);
  assert.equal(after.value.scheduledCheck, undefined); assert.equal(after.value.fields.nextReviewAt, null);
});

test('an explicit resolution closes only its addressed question', async t => {
  const w = await nativeWorkflow(t); await w.running();
  for (const id of ['first', 'second']) await comment(w, { purpose: 'question', commentId: id, body: 'Question ' + id });
  await comment(w, { resolvesQuestions: ['first'], body: 'The first choice became unnecessary because the user cancelled that part.' });
  assert.deepEqual(openQuestions((await w.f.first.store.read('task-board/task', w.state().task)).value).map(q => q.commentId), ['second']);
});

test('an unavailable host retains Review, its result and a visible bounded retry', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const handoff = waiting(); handoff.workflowState = 'review'; delete handoff.waiting;
  const task = await finish(w, handoff);
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(task.fields.nextReviewAt!) + 1000 });
  w.f.nativeTools(async () => { throw new IvyError('service_unavailable', 'The execution host is unavailable.'); });
  const result = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId);
  assert.ok(result?.task); assert.equal(result.run, null); w.adopt({ ...result, run: w.state().run });
  const after = (await w.f.first.store.read('task-board/task', result.task)).value;
  assert.equal(after.workflowState, 'review'); assert.deepEqual(after.latestResult, task.latestResult);
  assert.ok(after.scheduledCheck?.failure); assert.ok(Date.parse(after.fields.nextReviewAt!) >= Date.now() + 15 * 60_000);
  assert.equal(await new TaskBoardScheduler(w.f.first).task(result.task.objectId), null);
});

test('a scheduled check can finish the overall work and stop its timer explicitly', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const task = await finish(w, waiting());
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(task.fields.nextReviewAt!) + 1000 });
  const result = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId);
  assert.ok(result?.run); w.adopt(result); await w.running();
  const after = await finish(w, { workflowState: 'review', nextReviewAt: null });
  assert.equal(after.workflowState, 'review'); assert.equal(after.scheduledCheck, undefined);
  assert.equal(after.fields.nextReviewAt, null);
  assert.equal((await w.f.first.store.read('task-board/result', after.latestResult!)).value.kind, 'final');
  assert.equal(await new TaskBoardScheduler(w.f.first).task(w.state().task.objectId), null);
});

test('an addressed user answer resumes ordinary work before the next independent check', async t => {
  const w = await nativeWorkflow(t); await w.running();
  await comment(w, { purpose: 'question', commentId: 'needs-account', body: 'Which account should I use?' });
  await finish(w, waiting());
  await comment(w, { authorKind: 'user', replyTo: 'needs-account', body: 'Use the existing account.' });
  const result = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId);
  assert.ok(result?.run);
  const run = (await w.f.first.store.read('task-board/run', result.run)).value;
  assert.equal(run.originalRequest.checkOnly, undefined); assert.equal(run.originalRequest.commentIds.length, 1);
});

test('user feedback during a Review check stays queued for a normal continuation', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const handoff = waiting(); handoff.workflowState = 'review'; delete handoff.waiting;
  const task = await finish(w, handoff);
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(task.fields.nextReviewAt!) + 1000 });
  const result = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId);
  assert.ok(result?.run); w.adopt(result); await w.running();
  await comment(w, { authorKind: 'user', body: 'Please revise the result.' });
  const after = await finish(w, waiting());
  assert.equal(after.workflowState, 'todo'); assert.deepEqual(after.latestResult, task.latestResult);
  const resumed = await new TaskBoardScheduler(w.f.first).task(w.state().task.objectId);
  assert.ok(resumed?.run);
  assert.equal((await w.f.first.store.read('task-board/run', resumed.run)).value.originalRequest.checkOnly, undefined);
});

test('a failed pre-turn check cannot turn queued feedback into an early unrestricted retry', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const task = await finish(w, waiting());
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(task.fields.nextReviewAt!) + 1000 });
  const started = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId);
  assert.ok(started?.run); w.adopt(started);
  await comment(w, { authorKind: 'user', body: 'Please revise the result.' });
  const thread = await w.prepare('thread');
  await w.observe('thread', thread, { error: { code: -32001, message: 'Synthetic pre-turn failure.' } }, 'failed');
  const failed = (await w.f.first.store.read('task-board/task', w.state().task)).value;
  assert.equal(failed.workflowState, 'waiting'); assert.ok(failed.scheduledCheck?.failure);
  assert.equal(failed.commentDeliveries.filter(delivery => delivery.state === 'queued').length, 1);
  assert.equal(await new TaskBoardScheduler(w.f.first).task(w.state().task.objectId), null);
  t.mock.timers.setTime(Date.parse(failed.fields.nextReviewAt!) + 1000);
  const retry = await new TaskBoardScheduler(w.f.first).task(w.state().task.objectId);
  assert.ok(retry?.run);
  assert.equal((await w.f.first.store.read('task-board/run', retry.run)).value.originalRequest.checkOnly, true);
});

test('field edits are acknowledged by their check instead of reopening the waiting task', async t => {
  const w = await nativeWorkflow(t); await w.running();
  await finish(w, waiting());
  const old = await w.f.first.store.read('task-board/task', w.state().task);
  await w.f.invoke({ action: 'edit', operationId: randomUUID(), taskId: old.pin.objectId, expectedRevision: old.pin.revision,
    fields: { ...old.value.fields, description: 'The updated scope is read before the independent check.' } });
  await w.refresh();
  const task = (await w.f.first.store.read('task-board/task', w.state().task)).value;
  assert.ok(task.commentDeliveries.some(delivery => delivery.state === 'queued'));
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(task.fields.nextReviewAt!) + 1000 });
  const result = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId);
  assert.ok(result?.run); w.adopt(result); await w.running();
  const after = await finish(w, waiting());
  assert.equal(after.workflowState, 'waiting');
  assert.equal(after.commentDeliveries.filter(delivery => delivery.state === 'queued').length, 0);
});
