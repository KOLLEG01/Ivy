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
import { assessSteps, consumeStep, stepCondition } from '../services/task-board/src/runtime/next-steps.js';
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

test('uploads and recovery handoffs at the reference limit remain admitted and preserve exact evidence', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const store = w.f.first.store, client = store.client, task = await store.read('task-board/task', w.state().task);
  const at = new Date().toISOString(), attachments: TaskBoard.Task['attachments'] = [];
  for (let index = 0; index < 254; index++) {
    const bytes = Buffer.from('Retained evidence ' + index), attachmentId = 'capacity-' + index;
    const object = await store.writeAttachment(task.pin.objectId, randomUUID(), attachmentId, bytes);
    attachments.push({ attachmentId, object, filename: attachmentId + '.txt', mediaType: 'text/plain',
      byteLength: bytes.length, contentHash: digest(bytes), uploader: { ...w.f.first.owner, principalId: 'user', source: 'user' }, createdAt: at });
  }
  const priorResult = await store.write('task-board/result', { schemaVersion: 1, taskId: task.pin.objectId,
    run: w.state().run, kind: 'intermediate', content, createdAt: at, actor: task.value.claim!.owner, operationId: randomUUID() },
  randomUUID(), { create: { parentId: store.rootObjectId, name: 'Prior retained result' } });
  await store.write('task-board/task', { ...task.value, latestResult: priorResult, attachments }, randomUUID(),
    { objectId: task.pin.objectId, expectedRevision: task.pin.revision });
  // Exercise public admission as well as storage: Run/Result references do not consume upload slots.
  for (let index = 254; index < 256; index++) {
    const current = await store.read('task-board/task', task.pin.objectId);
    const uploaded = await w.f.invoke({ action: 'uploadAttachment', operationId: randomUUID(),
      taskId: task.pin.objectId, expectedRevision: current.pin.revision, attachmentId: 'capacity-' + index,
      filename: 'capacity-' + index + '.txt', mediaType: 'text/plain', bytesBase64: Buffer.from('Retained evidence ' + index).toString('base64') });
    assert.ok(uploaded.attachment); attachments.push(uploaded.attachment);
  }
  await w.refresh();
  const closed = await finish(w), raw = await client.request('objects.read', closed.pin);
  assert.equal(closed.value.claim, null);
  assert.equal(closed.value.publication, null);
  assert.equal(closed.value.completionIssue?.code, 'task_board_handoff_missing');
  assert.deepEqual(closed.value.latestResult, priorResult);
  assert.notDeepEqual(closed.value.lastRun, closed.value.completionIssue!.run);
  assert.deepEqual(raw.revision.references['recovery-run'], closed.value.completionIssue!.run);
  const pagePin = raw.revision.references['reference-page-0']!;
  const page = await client.request('objects.read', pagePin);
  assert.equal(page.object.ownerObjectId, task.pin.objectId);
  assert.equal(Object.keys(page.revision.references).length, attachments.length);
  const named = { run: closed.value.lastRun!, result: priorResult, 'recovery-run': closed.value.completionIssue!.run };
  const deduplicated = await taskReferences(client, task.pin.objectId, named,
    [...attachments.slice(0, 253).map(attachment => attachment.object), priorResult], scope => store.preparedMutationId(scope));
  assert.equal(Object.keys(deduplicated).length, 256);
  assert.equal(Object.values(deduplicated).filter(pin => hashJson(pin) === hashJson(priorResult)).length, 1);
  assert.equal(deduplicated['reference-page-0'], undefined);

  const reordered = { ...closed.value, attachments: [...attachments].reverse() };
  let lost = false;
  w.f.intercept(params => {
    if (!lost && params.content.encoding === 'json' && params.content.value && typeof params.content.value === 'object' &&
        'pins' in params.content.value) {
      lost = true; throw new IvyError('synthetic_response_loss', 'The committed reference page reply was lost.', 'unknown');
    }
  });
  const destination = { objectId: task.pin.objectId, expectedRevision: closed.pin.revision };
  await assert.rejects(store.write('task-board/task', reordered, 'lost-reference-page', destination), code('synthetic_response_loss'));
  w.f.intercept(null);
  const next = await w.f.second.store.write('task-board/task', reordered, 'lost-reference-page', destination);
  const updated = await client.request('objects.read', next);
  assert.equal(updated.revision.references['reference-page-0']!.revision, pagePin.revision + 1);
  assert.deepEqual((await client.request('objects.query', { contractKey: referencePageKey })).items.map(item => item.objectId), [pagePin.objectId]);
  const now = Date.now() + 2 * 86_400_000;
  w.f.kernel.retention.collect(now); w.f.kernel.retention.collect(now);
  assert.deepEqual((await client.request('objects.read', pagePin)).revision.references, page.revision.references);
  assert.deepEqual((await store.read('task-board/task', closed.pin)).value.attachments, attachments);
  assert.deepEqual((await store.read('task-board/task', next)).value.attachments, reordered.attachments);
  const repair = await new TaskBoardScheduler(w.f.second).task(task.pin.objectId); assert.ok(repair?.run);
  w.adopt(repair); await w.running();
  const recovered = await finish(w, { workflowState: 'todo', deliveries: [{ key: 'retained-capacity-proof', artifacts: [], codeReferences: [
      { kind: 'commit', url: 'https://example.test/commit/abc', commit: 'abc', label: 'Existing implementation' },
    ] }], nextSteps: [{ id: 'next', scope: 'Continue the existing authorized work.', requires: [] }] });
  assert.equal(recovered.value.completionIssue, undefined);
  assert.equal(recovered.value.publication, null);
  assert.equal(recovered.value.attachments.length, 256);
  assert.equal(recovered.value.deliveries?.[0]?.delivery.key, 'retained-capacity-proof');
  const recoveredHeader = await client.request('objects.read', recovered.pin);
  assert.ok(recoveredHeader.revision.references['reference-page-1'], 'the added delivery result gets its own overflow page');
});

test('an own handoff rejects sourceTaskId precisely and keeps the active claim available for correction', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const task = w.state().task;
  const request: TaskBoard.CommentActionRequest = { action: 'comment', operationId: randomUUID(), taskId: task.objectId,
    expectedRevision: task.revision, sourceTaskId: task.objectId, commentId: randomUUID(), authorKind: 'agent', purpose: 'handoff',
    body: 'Ready for acceptance.', handoff: { workflowState: 'review', nextSteps: [] }, requests: [], responses: [], attachments: [], replyTo: null };
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
    handoff: { workflowState: 'review', nextSteps: [] }, requests: [], responses: [], attachments: [], replyTo: null };
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

test('an unchanged idle dependency wakes ordinary work once across scheduler recreation', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const peer = await w.f.planned('agent'), blocker: TaskBoard.Condition = { kind: 'task', taskId: peer.task.objectId, until: 'idle' };
  const waiting: TaskBoard.Handoff = { workflowState: 'waiting', nextSteps: [{ id: 'next', scope: 'Integrate the next child result.', requires: [blocker] }] };
  await finish(w, waiting);
  const started = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId);
  assert.ok(started?.run); w.adopt(started); await w.running();
  const after = await finish(w, waiting);
  assert.ok(after.value.consumedSteps?.some(value => value.stepId === 'next'));
  assert.equal(await new TaskBoardScheduler(w.f.first).task(after.pin.objectId), null);
  const source = await w.f.first.store.read('task-board/task', peer.task.objectId);
  await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: source.pin.objectId, expectedRevision: source.pin.revision,
    commentId: randomUUID(), authorKind: 'agent', body: 'An ordinary update is not another delivery.', requests: [], responses: [], attachments: [], replyTo: null });
  assert.equal(await new TaskBoardScheduler(w.f.second).task(after.pin.objectId), null);
});

test('named deliveries require concrete output, publish at completion and cannot be consumed twice', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const delivery: TaskBoard.WorkDelivery = { key: 'service-health', artifacts: [],
    codeReferences: [{ kind: 'commit', url: 'https://example.test/commit/abc', label: 'Health API implementation', commit: 'abc' }] };
  const taskId = w.state().task.objectId, consumer = await w.f.planned('agent');
  const base = (await w.f.first.store.read('task-board/task', consumer.task)).value;
  const nextSteps: TaskBoard.NextStep[] = [{ id: 'health', scope: 'Integrate the exact health delivery.', requires: [{ kind: 'delivery', taskId, key: delivery.key }] }];
  const assess = (consumedSteps: TaskBoard.StepObservation[] = []) => assessSteps(w.f.first.store, { ...base, nextSteps, consumedSteps }, new Date().toISOString());
  assert.throws(() => validateTaskBoard('WorkDelivery', { ...delivery, codeReferences: [] }));
  assert.equal((await assess())[0]!.ready, false);
  await handoff(w, { workflowState: 'review', nextSteps: [], deliveries: [delivery] });
  assert.equal((await assess())[0]!.ready, false);
  const done = await finish(w), observed = (await assess())[0]!;
  assert.equal(observed.ready, true);
  const consumed = consumeStep(base, observed);
  assert.equal((await assess(consumed))[0]!.ready, false);
  assert.equal((await assess(consumed))[0]!.consumed, true);
  assert.equal(done.value.deliveries![0]!.identity, hashJson({ taskId, delivery }));
});

test('a foreign question is not inherited and only its concrete delivery gates the dependent scope', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const parent = await w.f.planned('agent');
  await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: parent.task.objectId, expectedRevision: parent.task.revision,
    commentId: 'parent-approval', authorKind: 'agent', purpose: 'question', recipient: { kind: 'user' }, body: 'May the lab be provisioned?', requests: [], responses: [], attachments: [], replyTo: null });
  const done = await finish(w, { workflowState: 'waiting', nextSteps: [
    { id: 'lab', scope: 'Qualify the actual lab after it is supplied.', requires: [{ kind: 'delivery', taskId: parent.task.objectId, key: 'isolated-lab-ready' }] },
    { id: 'independent', scope: 'Read independently at the next appointment.', requires: [{ kind: 'time', at: new Date(Date.now() + 60000).toISOString() }] },
  ] });
  assert.equal(done.value.openQuestionCount, 0);
  assert.equal(stepCondition(await assessSteps(w.f.first.store, done.value, new Date().toISOString())).state, 'deferred');
  const source = await w.f.first.store.read('task-board/task', parent.task.objectId);
  await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: source.pin.objectId, expectedRevision: source.pin.revision,
    commentId: randomUUID(), authorKind: 'user', body: 'Approved, but not provisioned yet.', replyTo: 'parent-approval', agentDelivery: 'none', requests: [], responses: [], attachments: [] });
  const assessed = await assessSteps(w.f.first.store, done.value, new Date().toISOString());
  assert.equal(assessed.find(value => value.step.id === 'lab')!.ready, false);
  assert.equal(await new TaskBoardScheduler(w.f.second).task(done.pin.objectId), null);
});

test('missing handoff queues recovery and an old terminal claim is never unlocked by age', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const running = await w.f.first.store.read('task-board/task', w.state().task);
  const done = await finish(w);
  assert.equal(w.f.first.condition(done.value).state, 'recovering');
  assert.equal(done.value.openQuestionCount, 0);
  const stale = await w.f.first.store.write('task-board/task', { ...running.value, claim: { ...running.value.claim!, run: done.value.lastRun }, publication: null, lastRun: done.value.lastRun }, randomUUID(),
    { objectId: done.pin.objectId, expectedRevision: done.pin.revision });
  t.mock.timers.enable({ apis: ['Date'], now: Date.now() + 8 * 86400_000 });
  await assert.rejects(new TaskBoardScheduler(w.f.second).task(stale.objectId), code('task_board_claim_inconsistent'));
  assert.ok((await w.f.first.store.read('task-board/task', stale.objectId)).value.claim);
});

test('a missing handoff recovers in the same context after processing new comments without user input', async t => {
  const w = await nativeWorkflow(t); await w.running();
  await w.refresh();
  await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: w.state().task.objectId, expectedRevision: w.state().task.revision,
    commentId: 'queued-feedback', body: 'Keep the requested scope.', requests: [], responses: [], attachments: [], replyTo: null });
  await w.refresh();
  const closed = await finish(w), original = closed.value.completionIssue!.run;
  const scheduler = new TaskBoardScheduler(w.f.second);
  const conversation = await scheduler.task(closed.pin.objectId); assert.ok(conversation?.run);
  const conversationRun = await w.f.first.store.read('task-board/run', conversation.run);
  assert.equal(conversationRun.value.originalRequest.coordinationOnly, true);
  assert.deepEqual(conversationRun.value.originalRequest.commentIds, ['queued-feedback']);
  w.adopt(conversation); await w.running();
  const discussed = await finish(w);
  assert.deepEqual(discussed.value.completionIssue?.run, original);
  assert.deepEqual(discussed.value.latestResult, closed.value.latestResult);
  const repair = await scheduler.task(closed.pin.objectId); assert.ok(repair?.run);
  const run = await w.f.first.store.read('task-board/run', repair.run);
  assert.equal(run.value.originalRequest.action, 'continue');
  assert.deepEqual(run.value.originalRequest.action === 'continue' && run.value.originalRequest.recoveryFor, original);
  assert.deepEqual(run.value.originalRequest.commentIds, []);
  assert.match(JSON.stringify(run.value.plan.turnStart), /Recover the original missing completion intent/);
  assert.equal(await scheduler.task(closed.pin.objectId), null);
  w.adopt(repair); await w.running();
  const before = await w.f.first.store.read('task-board/task', repair.task!.objectId);
  await assert.rejects(w.f.worker({ action: 'edit', operationId: randomUUID(), taskId: before.pin.objectId,
    expectedRevision: before.pin.revision, fields: before.value.fields }), code('task_board_recovery_scope'));
  await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: before.pin.objectId, expectedRevision: before.pin.revision,
    commentId: 'during-recovery', body: 'Preserve this instruction for the next normal turn.', requests: [], responses: [], attachments: [], replyTo: null });
  await w.refresh();
  const after = await finish(w, { workflowState: 'todo', nextSteps: [{ id: 'next', scope: 'Continue the already requested implementation.', requires: [] }] });
  assert.equal(after.value.completionIssue, undefined); assert.equal(after.value.workflowState, 'todo');
  assert.deepEqual(after.value.latestResult, closed.value.latestResult);
  assert.equal(after.value.openQuestionCount, 0);
  const followup = await scheduler.task(after.pin.objectId); assert.ok(followup?.run);
  const followupRun = await w.f.first.store.read('task-board/run', followup.run);
  assert.equal(followupRun.value.originalRequest.coordinationOnly, true);
  assert.deepEqual(followupRun.value.primaryResourceRef, closed.value.primaryResourceRef);
  assert.deepEqual(followupRun.value.originalRequest.commentIds, ['during-recovery']);
  w.adopt(followup); await w.running(); await finish(w);
  const work = await scheduler.task(after.pin.objectId); assert.ok(work?.run);
  const workRun = await w.f.first.store.read('task-board/run', work.run);
  assert.equal(workRun.value.originalRequest.coordinationOnly, undefined);
  assert.equal(workRun.value.originalRequest.nextStepId, 'next');
  assert.deepEqual(workRun.value.primaryResourceRef, closed.value.primaryResourceRef);
});

test('recovery retains exact native evidence under collection pressure and respects a real user question', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const originalRun = await w.f.first.store.read('task-board/run', w.state().run);
  await w.refresh();
  await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: w.state().task.objectId, expectedRevision: w.state().task.revision,
    commentId: 'real-question', authorKind: 'agent', purpose: 'question', recipient: { kind: 'user' }, body: 'Which existing target should be used?', requests: [], responses: [], attachments: [], replyTo: null });
  await w.refresh(); const closed = await finish(w);
  w.f.first.store.historyLimits.count = 0; await w.f.first.store.collectLocal();
  assert.ok(await w.f.first.store.read('task-board/native-call', originalRun.value.calls.thread!));
  const repair = await new TaskBoardScheduler(w.f.second).task(closed.pin.objectId); assert.ok(repair?.run);
  w.adopt(repair); await w.running();
  await handoff(w, { workflowState: 'todo', nextSteps: [{ id: 'independent', scope: 'Continue only already authorized independent work.', requires: [] }] });
  const after = await finish(w, { workflowState: 'waiting', nextSteps: [{ id: 'next', scope: 'The target question remains unanswered.', requires: [{ kind: 'user', questionId: 'real-question' }] }] });
  assert.equal(after.value.completionIssue, undefined); assert.equal(after.value.openQuestionCount, 1);
  assert.equal(stepCondition(await assessSteps(w.f.first.store, after.value, new Date().toISOString())).state, 'needs_user');
});

test('a legacy missing handoff after coordination takes recovery mode even when an independent check is due', async t => {
  const w = await nativeWorkflow(t); await w.running(); const closed = await finish(w);
  const store = w.f.first.store, source = await store.read('task-board/run', closed.value.lastRun!);
  const request: TaskBoard.ContinueRequest = { ...source.value.originalRequest, action: 'continue', feedback: 'Legacy coordination.', coordinationOnly: true };
  const coordination = await store.write('task-board/run', { ...source.value,
    originalRequest: request, requestHash: hashJson(request),
  }, randomUUID(), { create: { parentId: store.rootObjectId, name: 'Legacy later coordination' } });
  const task = await store.write('task-board/task', { ...closed.value, lastRun: coordination,
    runtimeWait: null, nextSteps: [{ id: 'appointment', scope: 'Observe at the appointment.', requires: [{ kind: 'time', at: new Date(Date.now() - 1000).toISOString() }] }],
  }, randomUUID(), { objectId: closed.pin.objectId, expectedRevision: closed.pin.revision });
  const repair = await new TaskBoardScheduler(w.f.second).task(task.objectId); assert.ok(repair?.run);
  const run = await store.read('task-board/run', repair.run);
  assert.equal('checkOnly' in run.value.originalRequest, false);
  assert.deepEqual(run.value.originalRequest.action === 'continue' && run.value.originalRequest.recoveryFor, closed.value.completionIssue!.run);
  assert.equal(run.value.executionContext?.workflowState, 'waiting');
});

test('a pruned legacy pre-closure revision uses the same terminal Run without losing its recovery identity', async t => {
  const w = await nativeWorkflow(t); await w.running(); const closed = await finish(w);
  const store = w.f.first.store, client = store.client, original = client.request.bind(client), issue = closed.value.completionIssue!;
  const current = await store.read('task-board/run', issue.run.objectId);
  client.request = (async (method, params, options) => {
    if (method === 'objects.read' && 'objectId' in params && params.objectId === issue.run.objectId &&
        'revision' in params && params.revision === issue.run.revision)
      throw new IvyError('revision_pruned', 'The legacy pre-closure revision was already collected.');
    return original(method, params, options);
  }) as typeof client.request;
  t.after(() => { client.request = original; });
  await store.write('task-board/task', closed.value, randomUUID(), { objectId: closed.pin.objectId, expectedRevision: closed.pin.revision });
  const raw = await client.request('objects.read', { objectId: closed.pin.objectId });
  assert.deepEqual(raw.revision.references['recovery-run'], current.pin);
  store.historyLimits.count = 0; await store.collectLocal();
  const repair = await new TaskBoardScheduler(w.f.first).task(closed.pin.objectId); assert.ok(repair?.run);
  const run = await store.read('task-board/run', repair.run);
  assert.deepEqual(run.value.originalRequest.action === 'continue' && run.value.originalRequest.recoveryFor, issue.run);
  w.adopt(repair); await w.running();
  const recovered = await finish(w, { workflowState: 'todo', nextSteps: [{ id: 'next', scope: 'Continue the authorized work.', requires: [] }] });
  assert.equal(recovered.value.completionIssue, undefined);
});

test('repeating the same next-step fingerprint stops instead of making an empty execution loop', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const intent: TaskBoard.Handoff = { workflowState: 'todo', nextSteps: [{ id: 'same', scope: 'Continue the same already authorized scope.', requires: [] }] };
  await finish(w, intent);
  const next = await new TaskBoardScheduler(w.f.first).task(w.state().task.objectId);
  assert.ok(next?.run); w.adopt(next); await w.running();
  const after = await finish(w, intent);
  assert.equal(await new TaskBoardScheduler(w.f.second).task(after.pin.objectId), null);
  assert.equal(stepCondition(await assessSteps(w.f.first.store, after.value, new Date().toISOString())).state, 'workflow_attention');
});

test('recovery attempts survive scheduler replacement and stop after two unusable terminal handoffs', async t => {
  const w = await nativeWorkflow(t); await w.running(); await w.refresh();
  await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: w.state().task.objectId, expectedRevision: w.state().task.revision,
    commentId: 'prior-feedback', body: 'Keep this correction for ordinary work.', requests: [], responses: [], attachments: [], replyTo: null });
  await w.refresh(); const closed = await finish(w);
  const conversation = await new TaskBoardScheduler(w.f.second).task(closed.pin.objectId); assert.ok(conversation?.run);
  w.adopt(conversation); await w.running(); await finish(w);
  let now = Date.now(); t.mock.timers.enable({ apis: ['Date'], now });
  for (let attempt = 1; attempt <= 2; attempt++) {
    const repair = await new TaskBoardScheduler(w.f.second).task(w.state().task.objectId); assert.ok(repair?.run);
    w.adopt(repair); await w.running();
    const after = await finish(w);
    assert.equal(after.value.completionIssue?.attempts, attempt);
    if (attempt < 2) assert.equal(await new TaskBoardScheduler(w.f.second).task(after.pin.objectId), null);
    else assert.equal(await new TaskBoardScheduler(w.f.second).task(after.pin.objectId), null);
    now += 31_000; t.mock.timers.setTime(now);
  }
  const after = await w.f.first.store.read('task-board/task', w.state().task.objectId);
  assert.equal(w.f.first.condition(after.value).state, 'failed');
  assert.equal(after.value.openQuestionCount, 0);
  assert.equal(after.value.commentDeliveries.find(value => value.commentId === 'prior-feedback')?.state, 'delivered');
  assert.equal(await new TaskBoardScheduler(w.f.second).task(after.pin.objectId), null);
  await w.f.invoke({ action: 'transition', operationId: randomUUID(), taskId: after.pin.objectId,
    expectedRevision: after.pin.revision, workflowState: 'todo', detail: 'Retry the original handoff after repairing storage.' });
  const resumed = await w.f.first.store.read('task-board/task', after.pin.objectId);
  assert.equal(resumed.value.completionIssue?.attempts, 0);
  assert.equal(resumed.value.completionIssue?.retryAt, null);
  assert.deepEqual(resumed.value.nextSteps, after.value.nextSteps);
  const retry = await new TaskBoardScheduler(w.f.second).task(after.pin.objectId);
  assert.ok(retry?.run);
  const recovery = await w.f.first.store.read('task-board/run', retry.run);
  assert.deepEqual(recovery.value.originalRequest.action === 'continue' && recovery.value.originalRequest.recoveryFor,
    after.value.completionIssue!.run);
  assert.deepEqual(recovery.value.primaryResourceRef, after.value.primaryResourceRef);
});

test('native capacity backoff during recovery spends no attempt and cannot rearm a stopped business timer', async t => {
  const w = await nativeWorkflow(t); await w.running(); const closed = await finish(w);
  const repair = await new TaskBoardScheduler(w.f.first).task(closed.pin.objectId); assert.ok(repair?.run);
  w.adopt(repair); await w.running();
  const observed = w.ownerOperation('thread/read', { threadId: 'native-primary', includeTurns: true },
    { result: { thread: w.nativeThread([{ ...(w.turn('failed') as Record<string, Wire.Json>), error: {
      message: 'Selected model is at capacity.', codexErrorInfo: 'serverOverloaded' } }]) } });
  w.f.retainOperation(observed);
  const proof = await new TaskBoardEvidence(w.f.first.store).save(w.state().run, observed, {
    operationId: observed.operationId, callerPrincipalId: observed.callerPrincipalId, serviceNodeId: observed.serviceNodeId,
    nativeVersion: observed.nativeVersion, method: observed.method, params: observed.params });
  const outcome = await w.update({ kind: 'snapshot', evidence: proof, nativeOperationId: observed.operationId, notificationCursor: 1 });
  const task = await w.f.first.store.read('task-board/task', outcome.task!);
  assert.equal(task.value.completionIssue?.attempts, 0);
  assert.ok(task.value.completionIssue?.retryAt);
  assert.equal(task.value.runtimeWait?.retryAt ?? null, null);
  assert.equal('latestChecks' in task.value, false);
  assert.equal(await new TaskBoardScheduler(w.f.second).task(task.pin.objectId), null);
});

test('a lost original context before recovery dispatch becomes attention without an empty retry loop', async t => {
  const w = await nativeWorkflow(t); await w.running(); const closed = await finish(w);
  const repair = await new TaskBoardScheduler(w.f.second).task(closed.pin.objectId); assert.ok(repair?.run);
  w.adopt(repair);
  const call = await w.prepare('thread');
  await w.observe('thread', call, { error: { code: -32600, message: 'no rollout found for native-primary' } }, 'failed');
  const after = await w.f.first.store.read('task-board/task', w.state().task.objectId);
  assert.equal(after.value.completionIssue?.code, 'task_board_recovery_context_unavailable');
  assert.equal(w.f.first.condition(after.value).state, 'failed');
  assert.equal(await new TaskBoardScheduler(w.f.second).task(after.pin.objectId), null);
});

test('terminal receipts survive payload eviction, remain caller-bound and do not authorize replay', async t => {
  const w = await nativeWorkflow(t); await w.running(); await finish(w, { workflowState: 'review', nextSteps: [] });
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
