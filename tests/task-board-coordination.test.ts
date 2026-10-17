import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { TaskBoardScheduler } from '../services/task-board/src/runtime/scheduler.js';
import { intendedCall, resolveNativeRequest } from '../services/task-board/src/runtime/native-intent.js';
import { checkBlocker, peerState, workspaceIdentity } from '../services/task-board/src/runtime/task-coordination.js';
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
    peersHash: await peerState(w.f.first.store, task.value, task.pin.objectId), predecessor: run.value.calls.steer ?? null });
}

test('steering contains only a ticket reference and acknowledges only its original comment batch', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const first = await comment(w), pin = await steer(w, [first]);
  const call = await w.f.first.store.read('task-board/native-call', pin);
  const request = await resolveNativeRequest(w.f.first.store, call.value.request);
  assert.equal(request.method, 'turn/steer'); assert.equal(request.params.expectedTurnId, 'native-turn');
  assert.match(JSON.stringify(request.params), /TaskBoard TASK-/);
  assert.doesNotMatch(JSON.stringify(request.params), /Secret ticket content/);
  assert.match(JSON.stringify(request.params), /Continue the requested work/);
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
});

test('unknown steering receipts cannot be replaced and acknowledge only after exact recovery', async t => {
  const w = await nativeWorkflow(t); await w.running(); const id = await comment(w), pin = await steer(w, [id]);
  const observed = await w.observe('steer', pin, null, 'outcome_unknown');
  await assert.rejects(steer(w, [id]), /original steering receipt/);
  await w.observe('steer', observed, { result: { turnId: 'native-turn' } });
  assert.equal((await w.f.first.store.read('task-board/task', w.state().task)).value.commentDeliveries[0]?.state, 'delivered');
});

test('explicit task blockers resume automatically after the linked task finishes and reject cycles', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const other = await w.f.planned('agent');
  const block = await w.f.invoke({ action: 'defer', operationId: randomUUID(), taskId: other.task.objectId, expectedRevision: other.task.revision,
    reason: 'dependency', detail: 'Waiting for peer.', nextReviewAt: null, blocker: { taskId: w.state().task.objectId, until: 'idle' } });
  assert.equal((await w.f.first.store.read('task-board/task', block.task!)).value.workflowState, 'waiting');
  const scheduler = new TaskBoardScheduler(w.f.first);
  await scheduler.task(other.task.objectId);
  assert.equal((await w.f.first.store.read('task-board/task', other.task.objectId)).value.claim, null);
  await w.refresh();
  await assert.rejects(w.f.invoke({ action: 'defer', operationId: randomUUID(), taskId: w.state().task.objectId, expectedRevision: w.state().task.revision,
    reason: 'dependency', detail: 'Cycle.', nextReviewAt: null, blocker: { taskId: other.task.objectId, until: 'idle' } }), /indirectly/);
  await w.f.first.nativeUpdate(await w.snapshot('completed'));
  assert.ok((await scheduler.task(other.task.objectId))?.run);
  assert.equal((await w.f.first.store.read('task-board/task', other.task.objectId)).value.blocker, undefined);
  await assert.rejects(checkBlocker(w.f.first.store, { taskId: w.state().task.objectId, until: 'done' }), /to be done/);
});

for (const blocked of [false, true]) test(`directed messages wake ${blocked ? 'a blocked task' : 'review'} for coordination, while ordinary agent comments stay quiet`, async t => {
  const w = await nativeWorkflow(t); await w.running();
  const finished = await w.f.first.nativeUpdate(await w.snapshot('completed'));
  const source = await w.f.planned('agent'), scheduler = new TaskBoardScheduler(w.f.first);
  await scheduler.task(source.task.objectId);
  let task = await w.f.first.store.read('task-board/task', finished.task!);
  if (blocked) {
    const queued = await w.f.invoke({ action: 'transition', operationId: randomUUID(), taskId: task.pin.objectId,
      expectedRevision: task.pin.revision, workflowState: 'todo', detail: 'Queue the completed task before retaining its dependency wait.' });
    task = await w.f.first.store.read('task-board/task', queued.task!);
    const deferred = await w.f.invoke({ action: 'defer', operationId: randomUUID(), taskId: task.pin.objectId,
      expectedRevision: task.pin.revision, reason: 'dependency', detail: 'Wait for the active child.', nextReviewAt: null,
      blocker: { taskId: source.task.objectId, until: 'idle' } });
    task = await w.f.first.store.read('task-board/task', deferred.task!);
    await assert.rejects(checkBlocker(w.f.first.store, task.value.blocker), /to be idle/);
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
  const run = (await w.f.first.store.read('task-board/run', next.run)).value;
  assert.equal(run.originalRequest.action === 'continue' && run.originalRequest.coordinationOnly, true);
  const prompt = await intendedCall(w.f.first.store, { ...run, nativeEpoch: 'fixture-epoch' }, 'turn');
  assert.match(JSON.stringify(prompt.request.params), /coordination only/);
  assert.match(JSON.stringify(prompt.request.params), /review completed delegated children/);
  assert.doesNotMatch(JSON.stringify(prompt.request.params), /Please coordinate the next change/);
  w.adopt(next);
  await w.running();
  const update = randomUUID();
  await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: w.state().task.objectId,
    expectedRevision: w.state().task.revision, commentId: update, body: 'Another directed coordination update.',
    authorKind: 'agent', sourceTaskId: source.task.objectId, agentDelivery: 'queue',
    requests: [], responses: [], attachments: [], replyTo: null });
  await w.refresh();
  const steeringPin = await steer(w, [update]);
  const steering = await w.f.first.store.read('task-board/native-call', steeringPin);
  const steeringRequest = await resolveNativeRequest(w.f.first.store, steering.value.request);
  assert.match(JSON.stringify(steeringRequest.params), /Coordination only/);
  assert.match(JSON.stringify(steeringRequest.params), /Preserve this ticket's result and blocker/);
  assert.doesNotMatch(JSON.stringify(steeringRequest.params), /Continue the requested work/);
  await w.observe('steer', steeringPin, { result: { turnId: 'native-turn' } });
  const ended = await w.f.first.nativeUpdate(await w.snapshot('completed', 'Coordination answer.'));
  const after = (await w.f.first.store.read('task-board/task', ended.task!)).value;
  assert.equal(after.workflowState, blocked ? 'waiting' : 'review'); assert.deepEqual(after.latestResult, task.value.latestResult);
  assert.deepEqual(after.blocker, task.value.blocker);
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
  await w.f.invoke({ action: 'defer', operationId: randomUUID(), taskId: w.state().task.objectId, expectedRevision: w.state().task.revision,
    reason: 'dependency', detail: 'Waiting for peer completion.', nextReviewAt: null, blocker: { taskId: other.task.objectId, until: 'done' } });
  await w.refresh(); const finished = await w.f.first.nativeUpdate(await w.snapshot('completed'));
  const blocked = (await w.f.first.store.read('task-board/task', finished.task!)).value;
  assert.equal(blocked.workflowState, 'waiting'); assert.equal(blocked.waiting?.reason, 'dependency');
  assert.equal(blocked.claim, null); assert.equal(blocked.blocker?.taskId, other.task.objectId);
});
