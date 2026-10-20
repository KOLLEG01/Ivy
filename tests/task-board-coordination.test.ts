import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { hashJson } from '../packages/contracts/src/canonical.js';
import { TaskBoardScheduler } from '../services/task-board/src/runtime/scheduler.js';
import { intendedCall, resolveNativeRequest } from '../services/task-board/src/runtime/native-intent.js';
import { workspaceIdentity } from '../services/task-board/src/runtime/task-coordination.js';
import { assessSteps } from '../services/task-board/src/runtime/next-steps.js';
import { nativeWorkflow } from './fixtures/task-board-native.js';
import { forwardDelegatedReview } from '../services/task-board/src/runtime/task-delegation.js';

async function comment(w: Awaited<ReturnType<typeof nativeWorkflow>>, body = 'Secret ticket content must stay in the ticket.') {
  await w.refresh(); const { task } = w.state(), commentId = randomUUID();
  await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.objectId, expectedRevision: task.revision,
    commentId, body, requests: [], responses: [], attachments: [], replyTo: null });
  await w.refresh(); return commentId;
}
async function steer(w: Awaited<ReturnType<typeof nativeWorkflow>>, commentIds: string[]) {
  const task = await w.f.first.store.read('task-board/task', w.state().task);
  const run = await w.f.first.store.read('task-board/run', w.state().run);
  return w.prepare('steer', { kind: 'steer', commentIds, workRevision: task.value.workRevision,
    peersHash: hashJson([]), predecessor: run.value.calls.steer ?? null });
}

test('steering contains only a ticket reference and acknowledges only its original comment batch', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const first = await comment(w), pin = await steer(w, [first]);
  const call = await w.f.first.store.read('task-board/native-call', pin);
  const request = await resolveNativeRequest(w.f.first.store, call.value.request);
  assert.equal(request.method, 'turn/steer'); assert.equal(request.params.expectedTurnId, 'native-turn');
  assert.match(JSON.stringify(request.params), /TaskBoard TASK-/);
  assert.doesNotMatch(JSON.stringify(request.params), /Secret ticket content/);
  assert.match(JSON.stringify(request.params), /Decide from the current ticket whether to reply, continue work or wait/);
  assert.doesNotMatch(JSON.stringify(request.params), /Coordination only|Preserve this ticket's result/);
  assert.ok(JSON.stringify(request.params).length < 500);
  const second = await comment(w, 'Another private correction.');
  await w.observe('steer', pin, { result: { turnId: 'native-turn' } });
  const task = (await w.f.first.store.read('task-board/task', w.state().task)).value;
  assert.equal(task.commentDeliveries.find(x => x.commentId === first)?.state, 'delivered');
  assert.equal(task.commentDeliveries.find(x => x.commentId === second)?.state, 'queued');
  assert.equal(task.claim?.phase, 'running');
  const next = await steer(w, [second]); assert.notEqual(next.objectId, pin.objectId);
});

test('failed steering preserves the run and queues feedback for the same primary after completion', async t => {
  const w = await nativeWorkflow(t); await w.running(); const id = await comment(w), pin = await steer(w, [id]);
  await w.observe('steer', pin, { error: { code: -32600, message: 'No active turn.' } }, 'failed');
  const run = (await w.f.first.store.read('task-board/run', w.state().run)).value;
  assert.equal(run.phase, 'running'); assert.equal(run.result, null);
  const finished = await w.f.first.nativeUpdate(await w.snapshot('completed'));
  const continued = await new TaskBoardScheduler(w.f.first).task(finished.task!.objectId);
  const next = (await w.f.first.store.read('task-board/run', continued!.run!)).value;
  assert.deepEqual(next.primaryResourceRef, run.primaryResourceRef);
  assert.deepEqual(next.originalRequest.commentIds, [id]);
  assert.equal(next.originalRequest.coordinationOnly, undefined);
  assert.equal((await w.f.first.store.read('task-board/task', finished.task!.objectId)).value.commentDeliveries.find(value => value.commentId === id)?.state, 'queued');
});

test('unknown steering receipts cannot be replaced and acknowledge only after exact recovery', async t => {
  const w = await nativeWorkflow(t); await w.running(); const id = await comment(w), pin = await steer(w, [id]);
  const observed = await w.observe('steer', pin, null, 'outcome_unknown');
  await assert.rejects(steer(w, [id]), /original steering receipt/);
  await w.observe('steer', observed, { result: { turnId: 'native-turn' } });
  assert.equal((await w.f.first.store.read('task-board/task', w.state().task)).value.commentDeliveries[0]?.state, 'delivered');
});

test('completion dependencies resume after acceptance and reject closed cycles', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const other = await w.f.planned('agent');
  const block = await w.f.invoke({ action: 'defer', operationId: randomUUID(), taskId: other.task.objectId, expectedRevision: other.task.revision, nextSteps: [{ id: 'next', scope: 'Waiting for peer acceptance.', requires: [{ kind: 'task', taskId: w.state().task.objectId, until: 'done' }] }] });
  assert.equal((await w.f.first.store.read('task-board/task', block.task!)).value.workflowState, 'waiting');
  const scheduler = new TaskBoardScheduler(w.f.first);
  await scheduler.task(other.task.objectId);
  assert.equal((await w.f.first.store.read('task-board/task', other.task.objectId)).value.claim, null);
  await w.refresh();
  await assert.rejects(w.f.invoke({ action: 'defer', operationId: randomUUID(), taskId: w.state().task.objectId, expectedRevision: w.state().task.revision, nextSteps: [{ id: 'next', scope: 'Cycle.', requires: [{ kind: 'task', taskId: other.task.objectId, until: 'done' }] }] }, w.f.settings.principalId), /indirectly/);
  await w.handoff();
  const finished = await w.f.first.nativeUpdate(await w.snapshot('completed'));
  assert.equal(await scheduler.task(other.task.objectId), null);
  await w.f.invoke({ action: 'transition', operationId: randomUUID(), taskId: finished.task!.objectId,
    expectedRevision: finished.task!.revision, workflowState: 'done', detail: 'The result is accepted.' });
  assert.ok((await scheduler.task(other.task.objectId))?.run);
  assert.equal((await w.f.first.store.read('task-board/task', other.task.objectId)).value.nextSteps[0]?.requires[0]?.kind, 'task');
  const blocked = await w.f.first.store.read('task-board/task', other.task.objectId);
  assert.equal((await assessSteps(w.f.first.store, { ...blocked.value, nextSteps: [{ id: 'accepted', scope: 'Wait for acceptance.', requires: [{ kind: 'task', taskId: w.state().task.objectId, until: 'done' }] }] }, new Date().toISOString()))[0]!.ready, true);
});

for (const blocked of [false, true]) test(`directed messages wake ${blocked ? 'a blocked task' : 'review'} for a normal turn, while ordinary agent comments stay quiet`, async t => {
  const w = await nativeWorkflow(t); await w.running();
  await w.handoff();
  const finished = await w.f.first.nativeUpdate(await w.snapshot('completed'));
  const source = await w.f.planned('agent'), scheduler = new TaskBoardScheduler(w.f.first);
  await scheduler.task(source.task.objectId);
  let task = await w.f.first.store.read('task-board/task', finished.task!);
  if (blocked) {
    const queued = await w.f.invoke({ action: 'transition', operationId: randomUUID(), taskId: task.pin.objectId,
      expectedRevision: task.pin.revision, workflowState: 'todo', detail: 'Queue the completed task before retaining its dependency wait.' });
    task = await w.f.first.store.read('task-board/task', queued.task!);
    const deferred = await w.f.invoke({ action: 'defer', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision, nextSteps: [{ id: 'next', scope: 'Wait for the active child.', requires: [{ kind: 'task', taskId: source.task.objectId, until: 'idle' }] }] });
    task = await w.f.first.store.read('task-board/task', deferred.task!);
    assert.equal((await assessSteps(w.f.first.store, task.value, new Date().toISOString()))[0]!.ready, false);
  }
  const send = async (directed: boolean) => {
    const outcome = await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision,
      commentId: randomUUID(), body: 'Please coordinate the next change.', authorKind: 'agent', agentDelivery: 'queue',
      ...(directed ? { sourceTaskId: source.task.objectId } : {}), requests: [], responses: [], attachments: [], replyTo: null });
    task = await w.f.first.store.read('task-board/task', outcome.task!);
  };
  await send(false); assert.ok(!(await scheduler.task(task.pin.objectId))?.run);
  task = await w.f.first.store.read('task-board/task', task.pin.objectId);
  await send(true); assert.equal(task.value.workflowState, blocked ? 'waiting' : 'review');
  const next = await scheduler.task(task.pin.objectId); assert.ok(next?.run);
  assert.equal((await w.f.first.store.read('task-board/task', task.pin.objectId)).value.workflowState, 'in_progress');
  const run = (await w.f.first.store.read('task-board/run', next.run)).value;
  assert.equal(run.originalRequest.action, 'continue');
  assert.equal(run.originalRequest.coordinationOnly, undefined);
  const prompt = await intendedCall(w.f.first.store, { ...run, nativeEpoch: 'fixture-epoch' }, 'turn');
  assert.match(JSON.stringify(prompt.request.params), /Read the ticket and new comments/);
  assert.match(JSON.stringify(prompt.request.params), /Decide from the current ticket whether to reply, continue work or wait/);
  assert.doesNotMatch(JSON.stringify(prompt.request.params), /Please coordinate the next change/);
  w.adopt(next);
  await w.running();
  assert.equal((await w.f.first.store.read('task-board/task', task.pin.objectId)).value.workflowState, 'in_progress');
  const update = randomUUID();
  await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: w.state().task.objectId,
    expectedRevision: w.state().task.revision, commentId: update, body: 'Another directed coordination update.',
    authorKind: 'agent', sourceTaskId: source.task.objectId, agentDelivery: 'queue',
    requests: [], responses: [], attachments: [], replyTo: null });
  await w.refresh();
  const steeringPin = await steer(w, [update]);
  const steering = await w.f.first.store.read('task-board/native-call', steeringPin);
  const steeringRequest = await resolveNativeRequest(w.f.first.store, steering.value.request);
  assert.deepEqual(steeringRequest.params.input, prompt.request.params.input);
  await w.observe('steer', steeringPin, { result: { turnId: 'native-turn' } });
  const ended = await w.f.first.nativeUpdate(await w.snapshot('completed', 'Coordination answer.'));
  const after = (await w.f.first.store.read('task-board/task', ended.task!)).value;
  assert.equal(after.workflowState, blocked ? 'waiting' : 'review'); assert.deepEqual(after.latestResult, task.value.latestResult);
  assert.deepEqual(after.nextSteps, task.value.nextSteps);
  assert.equal(after.comments.filter(c => c.purpose === 'handoff').length, task.value.comments.filter(c => c.purpose === 'handoff').length);
  assert.equal(await scheduler.task(task.pin.objectId), null);
});

test('delegation retains parent identity, lists children across workspaces and rejects cycles', async t => {
  const w = await nativeWorkflow(t);
  const parent = await w.f.planned('agent');
  const parentTask = await w.f.first.store.read('task-board/task', parent.task);
  const { userContact: _contact, ...fields } = parentTask.value.fields;
  const created = await w.f.invoke({ action: 'create', operationId: randomUUID(), parentTaskId: parent.task.objectId,
    fields: { ...fields, title: 'Delegated work', workspaceRequirement: { kind: 'directory_path', path: '/separate-work' } } });
  const child = await w.f.first.store.read('task-board/task', created.task!);
  assert.equal(child.value.parentTaskId, parent.task.objectId);
  assert.equal(child.value.fields.userContact, 'ticket');
  assert.deepEqual((await w.f.first.list({ parentTaskId: parent.task.objectId })).items.map(task => task.taskId), [child.pin.objectId]);
  assert.deepEqual((await w.f.first.list({ parentTaskId: w.state().task.objectId })).items, []);
  await assert.rejects(w.f.invoke({ action: 'edit', operationId: randomUUID(), taskId: parent.task.objectId,
    expectedRevision: parent.task.revision, fields: parentTask.value.fields, parentTaskId: child.pin.objectId }), /indirectly/);
  const unchangedParent = await w.f.first.store.read('task-board/task', parent.task.objectId);
  assert.equal(unchangedParent.value.parentTaskId, undefined);
  assert.equal(unchangedParent.value.publication, null);
  const edited = await w.f.invoke({ action: 'edit', operationId: randomUUID(), taskId: child.pin.objectId,
    expectedRevision: child.pin.revision, fields: { ...child.value.fields, title: 'Updated delegated work' } });
  assert.equal((await w.f.first.store.read('task-board/task', edited.task!)).value.parentTaskId, parent.task.objectId);
});

test('a delegated final result queues parent review once and closes only after acceptance', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const parent = await w.f.planned('agent');
  const child = await w.f.first.store.read('task-board/task', w.state().task);
  await w.f.invoke({ action: 'edit', operationId: randomUUID(), taskId: child.pin.objectId, expectedRevision: child.pin.revision,
    fields: { ...child.value.fields, userContact: 'chat' }, parentTaskId: parent.task.objectId });
  await w.refresh();
  await w.handoff();
  const finished = await w.f.first.nativeUpdate(await w.snapshot('completed'));
  const delegated = await w.f.first.store.read('task-board/task', finished.task!);
  await forwardDelegatedReview(w.f.first, delegated);
  await forwardDelegatedReview(w.f.second, delegated);
  const reviewed = await w.f.first.store.read('task-board/task', parent.task.objectId);
  assert.equal(reviewed.value.comments.length, 1);
  assert.equal(reviewed.value.comments[0]?.sourceTaskId, delegated.pin.objectId);
  assert.equal(reviewed.value.comments[0]?.authorKind, 'agent');
  assert.equal(reviewed.value.commentDeliveries[0]?.state, 'queued');
  assert.equal(delegated.value.comments.find(comment => comment.purpose === 'handoff')?.delivery, null);
  assert.deepEqual(w.f.kernel.store.all("SELECT payload FROM events WHERE topic='task-board.browser-notification'"), []);
  assert.equal((await w.f.first.store.read('task-board/task', delegated.pin.objectId)).value.workflowState, 'review');
  const accepted = await w.f.invoke({ action: 'transition', operationId: randomUUID(), taskId: delegated.pin.objectId,
    expectedRevision: delegated.pin.revision, workflowState: 'done', detail: 'Parent verified and integrated the delivered scope.' });
  assert.equal((await w.f.first.store.read('task-board/task', accepted.task!)).value.workflowState, 'done');
});

test('automatic peers share the canonical directory and host; separate worktrees and hosts are independent', async t => {
  const w = await nativeWorkflow(t); const task = (await w.f.first.store.read('task-board/task', w.state().task)).value;
  const identity = await workspaceIdentity(w.f.first.store, task);
  const differentPath = { ...task, claim: { ...task.claim!, workspace: { ...task.claim!.workspace, canonicalCwd: '/isolated-worktree' } } };
  const differentHost = { ...task, claim: { ...task.claim!, workspace: { ...task.claim!.workspace, hostId: 'another-host' } } };
  assert.notEqual(await workspaceIdentity(w.f.first.store, differentPath), identity);
  assert.notEqual(await workspaceIdentity(w.f.first.store, differentHost), identity);
  const win = (canonicalCwd: string) => ({ ...task, claim: { ...task.claim!, workspace: { ...task.claim!.workspace, canonicalCwd } } });
  assert.equal(await workspaceIdentity(w.f.first.store, win('C:\\Project\\')), await workspaceIdentity(w.f.first.store, win('c:/project')));
  const other = await w.f.planned('agent'); await new TaskBoardScheduler(w.f.first).task(other.task.objectId);
  const related = await w.f.first.list({ relatedTo: w.state().task.objectId });
  assert.deepEqual(related.items.map(x => x.taskId), [other.task.objectId]);
});

test('a running task can leave an explicit blocker and ends in Blocked instead of Review', async t => {
  const w = await nativeWorkflow(t); await w.running(); const other = await w.f.planned('user');
  await w.f.invoke({ action: 'defer', operationId: randomUUID(), taskId: w.state().task.objectId, expectedRevision: w.state().task.revision, nextSteps: [{ id: 'next', scope: 'Waiting for peer completion.', requires: [{ kind: 'task', taskId: other.task.objectId, until: 'done' }] }] }, w.f.settings.principalId);
  await w.refresh(); const finished = await w.f.first.nativeUpdate(await w.snapshot('completed'));
  const blocked = (await w.f.first.store.read('task-board/task', finished.task!)).value;
  assert.equal(blocked.workflowState, 'waiting'); assert.equal(blocked.runtimeWait, null);
  assert.equal(blocked.claim, null); assert.equal(blocked.nextSteps[0]?.requires[0]?.kind, 'task');
});
