import test from 'node:test';
import assert from 'node:assert/strict';
import { hashJson } from '../packages/contracts/src/canonical.js';
import { content } from './fixtures/task-board.js';
import { randomUUID } from 'node:crypto';
import type { Agent, TaskBoard } from '../packages/contracts/src/generated.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { taskBoardTools } from '../services/task-board/src/runtime/registry.js';
import { admitSchema } from '../packages/contracts/src/schema.js';
import { validateTaskBoard } from '../packages/contracts/src/task-board-validation.js';
import { openQuestions } from '../services/task-board/src/runtime/conversation.js';
import { assessSteps, stepCondition } from '../services/task-board/src/runtime/next-steps.js';
import { TaskBoardScheduler } from '../services/task-board/src/runtime/scheduler.js';
import { nativeWorkflow } from './fixtures/task-board-native.js';
import { taskBoardFixture } from './fixtures/task-board.js';

type Workflow = Awaited<ReturnType<typeof nativeWorkflow>>;
const later = () => new Date(Date.now() + 6 * 60 * 60_000).toISOString();
const appointment = (at = later()): TaskBoard.Handoff => ({ workflowState: 'waiting', nextSteps: [
  { id: 'approval-observation', scope: 'Read external approval status; do not publish or bypass login.', requires: [{ kind: 'time', at }] },
] });

test('real work after Review can declare remaining work and return to Waiting', async context => {
  const workflow = await nativeWorkflow(context); await workflow.running();
  const first = await finish(workflow, { workflowState: 'review', nextSteps: [
    { id: 'scheduled-followup', scope: 'Collect a fresh observation before acceptance.', requires: [] },
  ] });
  const outcome = await new TaskBoardScheduler(workflow.f.second).task(workflow.state().task.objectId);
  assert.ok(outcome?.run); workflow.adopt(outcome);
  await workflow.running();
  const after = await finish(workflow, appointment());
  assert.equal(after.workflowState, 'waiting');
  assert.notDeepEqual(after.latestResult, first.latestResult);
  assert.equal(after.acceptedReview, null);
  assert.equal(stepCondition(await assessSteps(workflow.f.first.store, after, new Date().toISOString())).state, 'deferred');
});
async function comment(workflow: Workflow, values: Partial<TaskBoard.CommentActionRequest>) {
  await workflow.refresh();
  const { task } = workflow.state();
  const result = await workflow.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.objectId, expectedRevision: task.revision,
    commentId: randomUUID(), authorKind: 'agent', purpose: 'update', body: 'Progress.', requests: [], responses: [], attachments: [], replyTo: null, ...values });
  await workflow.refresh(); return result;
}
async function finish(workflow: Workflow, handoff?: TaskBoard.Handoff) {
  if (handoff) await comment(workflow, { purpose: 'handoff', body: 'The overall task follows the declared next steps.', handoff });
  const result = await workflow.f.first.nativeUpdate(await workflow.snapshot());
  workflow.adopt(result);
  return (await workflow.f.first.store.read('task-board/task', result.task!)).value;
}

test('time paths remain independent from own questions and publish only after native completion', async context => {
  const workflow = await nativeWorkflow(context); await workflow.running();
  await comment(workflow, { purpose: 'question', recipient: { kind: 'user' }, commentId: 'login-choice', body: 'Which login should be used for publishing?' });
  const handoff = appointment();
  await comment(workflow, { purpose: 'handoff', handoff });
  const active = (await workflow.f.first.store.read('task-board/task', workflow.state().task)).value;
  assert.equal(active.workflowState, 'in_progress'); assert.ok(active.claim);
  assert.notDeepEqual(active.nextSteps, handoff.nextSteps);
  const task = await finish(workflow);
  assert.deepEqual(task.nextSteps, handoff.nextSteps);
  assert.equal(openQuestions(task).length, 1);
  assert.equal(stepCondition(await assessSteps(workflow.f.first.store, task, new Date().toISOString())).state, 'deferred');
  const card = await workflow.f.first.task({ task: workflow.state().task.objectId }, 'user');
  assert.equal(card.task.openQuestionCount, 1);
  assert.equal(card.executionCondition.state, 'deferred');
  assert.equal(task.runtimeWait, null);
});

test('a due step restarts once without work/check modes and renews through its next time condition', async context => {
  const workflow = await nativeWorkflow(context); await workflow.running();
  await comment(workflow, { purpose: 'question', recipient: { kind: 'user' }, commentId: 'login-choice', body: 'Choose a login.' });
  const task = await finish(workflow, appointment());
  const at = task.nextSteps[0]!.requires[0]!;
  assert.equal(at.kind, 'time');
  context.mock.timers.enable({ apis: ['Date'], now: Date.parse(at.kind === 'time' ? at.at : '') + 1000 });
  const result = await new TaskBoardScheduler(workflow.f.second).task(workflow.state().task.objectId);
  assert.ok(result?.run); workflow.adopt(result);
  const run = (await workflow.f.first.store.read('task-board/run', result.run)).value;
  assert.equal(run.originalRequest.nextStepId, 'approval-observation');
  assert.equal('checkOnly' in run.originalRequest, false);
  assert.equal(await new TaskBoardScheduler(workflow.f.first).task(workflow.state().task.objectId), null);
  await workflow.running();
  const after = await finish(workflow, appointment());
  assert.equal(openQuestions(after).length, 1);
  assert.equal(stepCondition(await assessSteps(workflow.f.first.store, after, new Date().toISOString())).state, 'deferred');
});

test('new comments bypass work conditions, preserve schedules and finish without a work handoff', async context => {
  const workflow = await nativeWorkflow(context); await workflow.running();
  const original = await finish(workflow, appointment());
  await workflow.f.first.schedule({ action: 'defer', operationId: randomUUID(), taskId: workflow.state().task.objectId,
    expectedRevision: workflow.state().task.revision, nextSteps: original.nextSteps,
    runtimeWait: { reason: 'host', detail: 'Previous host observation failed.', since: new Date().toISOString(), retryAt: later() } });
  let unavailable = true, attempts = 0;
  workflow.f.nativeTools(async call => {
    assert.notEqual(call.definition.name, 'resolveWorkspace', 'Conversation must not reallocate its existing workspace.');
    assert.notEqual(call.definition.name, 'projects', 'Conversation must not depend on a complete project snapshot.');
    if (call.definition.name === 'capabilities') {
      attempts++;
      if (unavailable) throw new IvyError('native_capacity', 'Synthetic transient observation capacity.');
    }
    return workflow.f.nativeManagement(call);
  });
  await comment(workflow, { authorKind: 'user', body: 'Please explain the current status.', commentId: 'status-request' });
  assert.equal((await workflow.f.first.store.read('task-board/task', workflow.state().task.objectId)).value.runtimeWait?.retryAt, null);
  const deferred = await new TaskBoardScheduler(workflow.f.second).task(workflow.state().task.objectId);
  assert.equal(deferred?.run, null);
  const waiting = (await workflow.f.first.store.read('task-board/task', workflow.state().task.objectId)).value;
  assert.equal(waiting.commentDeliveries.find(value => value.commentId === 'status-request')?.state, 'queued');
  assert.deepEqual(waiting.nextSteps, original.nextSteps);
  assert.ok(waiting.runtimeWait?.retryAt);
  const before = attempts;
  assert.equal(await new TaskBoardScheduler(workflow.f.first).task(workflow.state().task.objectId), null);
  assert.equal(attempts, before, 'A failed delivery retains technical retry backoff.');
  unavailable = false;
  context.mock.timers.enable({ apis: ['Date'], now: Date.parse(waiting.runtimeWait!.retryAt!) + 1 });
  const result = await new TaskBoardScheduler(workflow.f.second).task(workflow.state().task.objectId);
  assert.ok(result?.run); workflow.adopt(result);
  const run = (await workflow.f.first.store.read('task-board/run', result.run)).value;
  assert.equal(run.originalRequest.coordinationOnly, true);
  assert.deepEqual(run.originalRequest.commentIds, ['status-request']);
  assert.deepEqual(run.workspace, workflow.planned.workspace);
  assert.equal(run.primaryResourceRef?.nativeId, 'native-primary');
  await workflow.running();
  const after = await finish(workflow);
  assert.deepEqual(after.nextSteps, original.nextSteps);
  assert.deepEqual(after.latestResult, original.latestResult);
  assert.equal(after.workflowState, original.workflowState);
  assert.equal(after.completionIssue, undefined);
  assert.equal(after.runtimeWait, null);
  assert.notEqual(after.commentDeliveries.find(value => value.commentId === 'status-request')?.state, 'queued');
  assert.equal(await new TaskBoardScheduler(workflow.f.first).task(workflow.state().task.objectId), null);
});

test('conversation workspace reuse rejects a changed native working directory without replacing the context', async context => {
  const workflow = await nativeWorkflow(context); await workflow.running();
  await finish(workflow, appointment());
  workflow.f.nativeTools(async call => {
    const value = await workflow.f.nativeManagement(call);
    if (call.definition.name !== 'read') return value;
    const observation = structuredClone(value) as Agent.ReadObservation;
    observation.reply = { result: { thread: { ...workflow.nativeThread(), cwd: '/another-workspace' } } };
    return observation;
  });
  await comment(workflow, { authorKind: 'user', body: 'Process this message in the existing context.', commentId: 'retained-context-message' });
  const result = await new TaskBoardScheduler(workflow.f.second).task(workflow.state().task.objectId);
  assert.equal(result?.run, null);
  const task = (await workflow.f.first.store.read('task-board/task', workflow.state().task.objectId)).value;
  assert.equal(task.primaryResourceRef?.nativeId, 'native-primary');
  assert.equal(task.claim, null);
  assert.equal(task.commentDeliveries.find(value => value.commentId === 'retained-context-message')?.state, 'queued');
  assert.match(task.runtimeWait?.detail ?? '', /retained working directory/);
  assert.equal((await workflow.f.first.store.page('task-board/run')).items.length, 1);
});


test('conditions are AND within a step and independent automatic paths outrank User', async context => {
  const fixture = taskBoardFixture(context), created = await fixture.planned();
  let task = await fixture.first.store.read('task-board/task', created.task);
  const question = { commentId: 'own-question', sequence: 1, author: { principalId: 'worker', serviceNodeId: fixture.first.owner.serviceNodeId, generation: 1, source: 'worker' as const },
    authorKind: 'agent' as const, purpose: 'question' as const, body: 'Choose the account.', requests: [], responses: [], delivery: null, attachments: [],
    run: null, turnId: null, replyTo: null, createdAt: new Date().toISOString(), operationId: randomUUID() };
  const steps: TaskBoard.NextStep[] = [
    { id: 'publish', scope: 'Publish after the answer.', requires: [{ kind: 'time', at: later() }, { kind: 'user', questionId: question.commentId }] },
    { id: 'observe', scope: 'Read status independently.', requires: [{ kind: 'time', at: later() }] },
  ];
  task = { ...task, value: { ...task.value, comments: [question], nextSteps: steps } };
  assert.equal(stepCondition(await assessSteps(fixture.first.store, task.value, new Date().toISOString())).state, 'deferred');
  assert.equal(stepCondition(await assessSteps(fixture.first.store, { ...task.value, nextSteps: [steps[0]!] }, new Date().toISOString())).state, 'needs_user');
  const other = await fixture.planned();
  await assert.rejects(fixture.invoke({ action: 'defer', operationId: randomUUID(), taskId: other.task.objectId, expectedRevision: other.task.revision,
    nextSteps: [{ id: 'foreign', scope: 'Wrong question owner.', requires: [{ kind: 'user', questionId: 'own-question' }] }] }), { code: 'task_board_comment_mismatch' });
});

test('the 513th comment is retained and replaying its original operation does not duplicate it', async context => {
  const fixture = taskBoardFixture(context), created = await fixture.create();
  const document = await fixture.first.store.read('task-board/task', created.task!);
  const comments: TaskBoard.Comment[] = Array.from({ length: 512 }, (_, index) => ({ commentId: 'retained-' + index, sequence: index + 1,
    author: { principalId: 'worker', serviceNodeId: fixture.first.owner.serviceNodeId, generation: 1, source: 'worker' },
    authorKind: 'agent', purpose: 'update', body: 'Retained conversation entry.', requests: [], responses: [], delivery: null, attachments: [],
    run: null, turnId: null, replyTo: null, createdAt: new Date().toISOString(), operationId: 'retained-op-' + index }));
  const pin = await fixture.first.store.write('task-board/task', { ...document.value, comments, agentCommentCount: 512 }, randomUUID(),
    { objectId: document.pin.objectId, expectedRevision: document.pin.revision });
  const request: TaskBoard.CommentActionRequest = { action: 'comment', operationId: randomUUID(), taskId: pin.objectId, expectedRevision: pin.revision,
    commentId: randomUUID(), body: 'Why is this ticket waiting?', requests: [], responses: [], attachments: [], replyTo: null };
  const first = await fixture.invoke(request), replay = await fixture.invoke(request);
  assert.deepEqual(replay, first);
  const saved = await fixture.first.store.read('task-board/task', pin.objectId);
  assert.equal(saved.value.comments.length, 513);
  assert.equal(saved.value.comments.filter(value => value.commentId === request.commentId).length, 1);
  assert.equal(saved.value.commentDeliveries.at(-1)?.state, 'queued');
});

test('comments on Done are processed without reopening implementation or replacing its final result', async context => {
  const workflow = await nativeWorkflow(context); await workflow.running();
  await finish(workflow, { workflowState: 'review', nextSteps: [] });
  const current = workflow.state().task;
  await workflow.f.invoke({ action: 'transition', operationId: randomUUID(), taskId: current.objectId, expectedRevision: current.revision, workflowState: 'done', detail: null });
  await comment(workflow, { authorKind: 'user', body: 'Explain the saved result.', commentId: 'done-comment' });
  const before = await workflow.f.first.store.read('task-board/task', workflow.state().task);
  const outcome = await new TaskBoardScheduler(workflow.f.second).task(before.pin.objectId);
  assert.ok(outcome?.run); workflow.adopt(outcome);
  await workflow.running();
  const after = await finish(workflow);
  assert.equal(after.workflowState, 'done'); assert.deepEqual(after.latestResult, before.value.latestResult);
  assert.equal(after.runtimeWait, null);
});

test('all public TaskBoard tool schemas stay below the existing admission bound', () => {
  for (const tool of taskBoardTools) for (const field of ['inputSchema', 'outputSchema'] as const)
    assert.doesNotThrow(() => admitSchema(tool[field]), tool.name + ' ' + field);
});

test('the 65th named delivery preserves all earlier exact receipts and result references', async context => {
  const workflow = await nativeWorkflow(context); await workflow.running();
  const store = workflow.f.first.store, document = await store.read('task-board/task', workflow.state().task);
  const result = await store.write('task-board/result', { schemaVersion: 1, taskId: document.pin.objectId, run: workflow.state().run,
    kind: 'intermediate', content, actor: document.value.claim!.owner, createdAt: new Date().toISOString(), operationId: randomUUID() },
    randomUUID(), { create: { parentId: store.rootObjectId, name: 'Retained delivery proof' } });
  const deliveries = Array.from({ length: 64 }, (_, index) => {
    const delivery = { key: 'retained-' + index, artifacts: [], codeReferences: [{ kind: 'commit' as const, url: 'https://example.test/commit/existing', label: 'Existing qualified output', commit: 'existing' }] };
    return { delivery, identity: hashJson({ taskId: document.pin.objectId, delivery }), result };
  });
  await store.write('task-board/task', { ...document.value, deliveries }, randomUUID(), { objectId: document.pin.objectId, expectedRevision: document.pin.revision });
  await workflow.refresh();
  const after = await finish(workflow, { workflowState: 'review', nextSteps: [],
    deliveries: [{ key: 'delivery-65', artifacts: [], codeReferences: [{ kind: 'commit', url: 'https://example.test/commit/new', label: 'New qualified output', commit: 'new' }] }] });
  assert.equal(after.deliveries?.length, 65);
  assert.deepEqual(after.deliveries?.slice(0, 64), deliveries);
});

test('the actual native owner can update conditions during conversation processing without a work handoff', async context => {
  const workflow = await nativeWorkflow(context); await workflow.running();
  await finish(workflow, appointment());
  await comment(workflow, { authorKind: 'user', commentId: 'scope-change', body: 'Keep the appointment but use the corrected authorized scope.' });
  const outcome = await new TaskBoardScheduler(workflow.f.first).task(workflow.state().task.objectId);
  assert.ok(outcome?.run); workflow.adopt(outcome); await workflow.running();
  const task = await workflow.f.first.store.read('task-board/task', workflow.state().task);
  const nextSteps = task.value.nextSteps.map(step => ({ ...step, scope: 'Use the corrected authorized scope at the unchanged appointment.' }));
  await workflow.f.invoke({ action: 'defer', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision, nextSteps },
    workflow.f.settings.principalId);
  await workflow.refresh();
  const after = await finish(workflow);
  assert.deepEqual(after.nextSteps, nextSteps);
  assert.ok(after.workRevision > task.value.workRevision);
  assert.equal(after.runtimeWait, null);
  assert.equal(after.completionIssue, undefined);
});
