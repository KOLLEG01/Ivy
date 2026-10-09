import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import type { TaskBoard, Wire } from '../packages/contracts/src/generated.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { TaskBoardNativeOrchestrator } from '../services/task-board/src/runtime/native-orchestrator.js';
import { executionCondition } from '../services/task-board/src/runtime/condition.js';
import { TaskBoardNativeSignals } from '../services/task-board/src/runtime/native-signals.js';
import { nativeRecord } from '../services/task-board/src/runtime/native-intent.js';
import { nativeDriverFixture } from './fixtures/native-driver.js';
import { nativeWorkflow } from './fixtures/task-board-native.js';
import { syncNativeComments } from '../services/task-board/src/runtime/native-comments.js';
import { TaskBoardScheduler } from '../services/task-board/src/runtime/scheduler.js';
import { unstartedArchivedContext, unstartedMissingContext } from '../services/task-board/src/runtime/native-recovery.js';
import { TaskBoardReconciler } from '../services/task-board/src/runtime/reconciler.js';
import { nativeRateLimitRetryAt } from '../services/task-board/src/runtime/native-rate-limit.js';

async function fixture(t: TestContext) {
  const f = await nativeDriverFixture(t); let status = 'inProgress', started = false, readFailed = false, archived = false, largeResume = false, outputError: number | null = null;
  let outputItems: Wire.Json[] = [{ id: 'two', type: 'agentMessage', phase: 'final_answer', text: 'Second' }];
  let turnError: Wire.Json = null, completedAt: number | null = null;
  f.reply(frame => {
    if (readFailed && frame['method'] === 'thread/turns/list') return { error: { code: -32000, message: 'Temporary native read failure.' } };
    if (frame['method'] === 'turn/start') { started = true; status = 'inProgress'; }
    if (frame['method'] === 'turn/steer') return { result: { turnId: 'native-turn' } };
    if (frame['method'] === 'turn/interrupt') { status = 'interrupted'; return { result: {} }; }
    if (frame['method'] === 'thread/read') return { result: { thread: { ...f.w.nativeThread([], archived ? { type: 'notLoaded' } : started && status === 'inProgress' ? { type: 'active', activeFlags: [] } : { type: 'idle' }),
      path: archived ? 'C:\\codex\\archived_sessions\\native-primary.jsonl' : '/fixture/sessions/native-primary.jsonl' } } };
    if (frame['method'] === 'thread/resume' && archived) return { error: { code: -32600, message: 'session native-primary is archived. Run codex unarchive native-primary to unarchive it first.' } };
    if (frame['method'] === 'thread/resume' && largeResume) {
      const turns = nativeRecord(frame['params'])['excludeTurns'] === true ? [] : [{ ...nativeRecord(f.w.turn('completed')),
        items: [{ id: 'large-history', type: 'agentMessage', phase: 'final_answer', text: 'x'.repeat(25 * 1024 * 1024) }] }];
      return { result: { thread: f.w.nativeThread(turns), cwd: '/fixture', model: 'fixture-model', modelProvider: 'openai',
        approvalPolicy: 'on-request', approvalsReviewer: 'user', sandbox: { type: 'readOnly' }, reasoningEffort: 'high' } };
    }
    if (frame['method'] === 'thread/unarchive') { archived = false; return { result: { thread: f.w.nativeThread() } }; }
    if (frame['method'] === 'thread/turns/list') return { result: { data: [{ ...nativeRecord(f.w.turn(status)), ...(turnError ? { error: turnError } : {}),
      ...(completedAt === null ? {} : { completedAt }), items: [], itemsView: 'notLoaded' }], nextCursor: null } };
    if (frame['method'] === 'thread/items/list') {
      const params = nativeRecord(frame['params']);
      assert.equal(params['limit'], 1); assert.equal(params['sortDirection'], 'desc');
      if (outputError !== null) return { error: { code: outputError, message: 'Output unavailable.' } };
      const index = params['cursor'] === null ? 0 : Number(params['cursor']);
      return { result: { data: outputItems[index] ? [{ turnId: 'native-turn', item: outputItems[index]! }] : [], nextCursor: index + 1 < outputItems.length ? String(index + 1) : null } };
    }
    return;
  });
  const first = new TaskBoardNativeOrchestrator(f.w.f.first), second = new TaskBoardNativeOrchestrator(f.w.f.second), runId = f.w.state().run.objectId;
  return { f, first, second, runId, status: (value: string) => { status = value; }, failRead: (value: boolean) => { readFailed = value; },
    archive: () => { archived = true; },
    largeResume: () => { largeResume = true; },
    outputs: (items: Wire.Json[]) => { outputItems = items; }, outputError: (code: number) => { outputError = code; },
    error: (value: Wire.Json, at: number | null = null) => { turnError = value; completedAt = at; },
    current: () => f.w.f.first.store.read('task-board/run', runId) };
}

test('native quota retries use the reported reset and timezone, with delayed retries when reset data is absent', () => {
  const turn = (message: string, codexErrorInfo: Wire.Json = null) => ({ status: 'failed', error: { message, codexErrorInfo } });
  assert.equal(nativeRateLimitRetryAt(turn("You've hit your session limit · resets 8:10pm (Europe/Berlin)"), '2026-10-09T13:57:31.803Z'), '2026-10-09T18:11:00.000Z');
  assert.equal(nativeRateLimitRetryAt(turn("You've hit your session limit · resets 1:10am (Europe/Berlin)"), '2026-10-09T21:57:31.803Z'), '2026-10-09T23:11:00.000Z');
  assert.equal(nativeRateLimitRetryAt(turn("You've hit your weekly limit · resets 8:10am (Europe/Berlin)"), '2026-10-24T21:57:31.803Z'), '2026-10-25T07:11:00.000Z');
  assert.equal(nativeRateLimitRetryAt(turn("You've hit your session limit · resets 8:10pm (Europe/Berlin)"), '2026-10-09T18:11:31.803Z'), '2026-10-09T18:26:31.803Z');
  assert.equal(nativeRateLimitRetryAt(turn('Claude five hour limit reached. Resets at 2026-10-09T18:10:00.000Z.'), '2026-10-09T13:57:31.803Z'), '2026-10-09T18:11:00.000Z');
  for (const message of ['Usage quota exceeded.', "You've hit your session limit", "You've hit your session limit · resets 8:10pm (invalid)", 'Claude five hour limit reached. Resets at 2026-10-09T12:00:00.000Z.'])
    assert.equal(nativeRateLimitRetryAt(turn(message, 'usageLimitExceeded'), '2026-10-09T13:57:31.803Z'), '2026-10-09T14:12:31.803Z');
  assert.equal(nativeRateLimitRetryAt({ ...turn("You've hit your session limit · resets 8:10pm (Europe/Berlin)"), completedAt: 1791554225 }, '2026-10-10T08:00:00.000Z'), '2026-10-09T18:11:00.000Z');
  for (const message of ['Command failed.', 'Claude usage is nearing the five hour limit.', 'Please handle rate limit exceeded errors.'])
    assert.equal(nativeRateLimitRetryAt(turn(message), '2026-10-09T13:57:31.803Z'), null);
  assert.equal(nativeRateLimitRetryAt({ status: 'completed', items: [{ type: 'agentMessage', text: "You've hit your session limit" }] }, '2026-10-09T13:57:31.803Z'), null);
});

test('a quota failure waits until reset and automatically continues the same native context exactly once', async t => {
  const f = await fixture(t), engine = f.f.w.f.first, reset = new Date(Date.now() + 120000).toISOString();
  await f.first.drain(f.runId);
  f.error({ message: `Claude five hour limit reached. Resets at ${reset}.` }); f.status('failed'); f.outputs([]);
  await f.first.drain(f.runId);
  const run = await f.current(), task = await engine.store.read('task-board/task', run.value.taskId);
  assert.equal(run.value.phase, 'failed'); assert.equal(task.value.claim, null);
  assert.equal(task.value.workflowState, 'waiting'); assert.equal(task.value.waiting?.reason, 'time');
  assert.equal(task.value.fields.nextReviewAt, new Date(Date.parse(reset) + 60000).toISOString());
  assert.match(task.value.comments.at(-1)!.body, /Claude five hour limit reached/);
  const scheduler = new TaskBoardScheduler(engine);
  for (let pass = 0; pass < 2; pass++) assert.equal(await scheduler.task(task.pin.objectId), null);
  assert.deepEqual((await engine.store.read('task-board/task', task.pin.objectId)).pin, task.pin);
  t.mock.timers.enable({ apis: ['Date'], now: Date.parse(task.value.fields.nextReviewAt!) + 1 });
  const continued = await new TaskBoardScheduler(engine).task(task.pin.objectId);
  assert.ok(continued?.run);
  assert.equal(await new TaskBoardScheduler(f.f.w.f.second).task(task.pin.objectId), null);
  await f.second.drain(continued.run.objectId);
  const current = await engine.store.read('task-board/task', task.pin.objectId);
  assert.deepEqual(current.value.primaryResourceRef, task.value.primaryResourceRef);
  assert.equal(current.value.waiting, null); assert.equal(current.value.attemptCount, 2);
  for (const [method, count] of [['thread/start', 1], ['thread/resume', 1], ['turn/start', 2]] as const)
    assert.equal(f.f.frames.filter(frame => frame['method'] === method).length, count);
});

test('a previously blocked quota failure is reclassified from its original native turn and respects explicit user waits', async t => {
  const f = await fixture(t), engine = f.f.w.f.first, reset = new Date(Date.now() + 120000).toISOString();
  await f.first.drain(f.runId); f.error({ message: `Claude five hour limit reached. Resets at ${reset}.` }); f.status('failed'); f.outputs([]);
  await f.first.drain(f.runId);
  const task = await engine.store.read('task-board/task', (await f.current()).value.taskId);
  const waiting = { reason: 'user' as const, detail: 'Execution failed; see the ticket comment.', since: task.value.updatedAt };
  // Reproduce the stored projection produced before quota-aware completion publication.
  await engine.store.write('task-board/task', { ...task.value, waiting, fields: { ...task.value.fields, nextReviewAt: null } }, randomUUID(),
    { objectId: task.pin.objectId, expectedRevision: task.pin.revision });
  const deferred = await new TaskBoardScheduler(engine).task(task.pin.objectId);
  assert.ok(deferred?.task);
  const recovered = await engine.store.read('task-board/task', task.pin.objectId);
  assert.equal(recovered.value.waiting?.reason, 'time'); assert.equal(recovered.value.attemptCount, 1);
  assert.equal(recovered.value.fields.nextReviewAt, new Date(Date.parse(reset) + 60000).toISOString());
  assert.deepEqual(recovered.value.primaryResourceRef, task.value.primaryResourceRef);
  await engine.store.write('task-board/task', { ...recovered.value, waiting: { ...waiting, detail: 'Waiting for a deliberate user decision.' }, fields: { ...recovered.value.fields, nextReviewAt: null } }, randomUUID(),
    { objectId: recovered.pin.objectId, expectedRevision: recovered.pin.revision });
  assert.equal(await new TaskBoardScheduler(engine).task(task.pin.objectId), null);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
});

test('quota recovery preserves an unanswered ticket question', async t => {
  const f = await fixture(t), engine = f.f.w.f.first;
  await f.first.drain(f.runId);
  let task = await engine.store.read('task-board/task', (await f.current()).value.taskId);
  await f.f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision,
    commentId: 'quota-question', authorKind: 'agent', purpose: 'question', body: 'Which input should be used?', requests: [], responses: [], attachments: [], replyTo: null });
  f.error({ message: "You've hit your session limit" }); f.status('failed'); f.outputs([]); await f.first.drain(f.runId);
  task = await engine.store.read('task-board/task', task.pin.objectId);
  assert.equal(task.value.waiting?.reason, 'user'); assert.equal(task.value.fields.nextReviewAt, null);
  assert.equal(await new TaskBoardScheduler(engine).task(task.pin.objectId), null);
});

test('continuation restores an externally archived context before claiming and delivers its queued input once', async t => {
  const f = await fixture(t), engine = f.f.w.f.first;
  await f.first.drain(f.runId); f.status('completed'); await f.first.drain(f.runId);
  const task = await engine.store.read('task-board/task', (await f.current()).value.taskId);
  await f.f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision,
    commentId: 'archived-follow-up', body: 'Continue the original work.', requests: [], responses: [], attachments: [], replyTo: null });
  const before = await engine.store.read('task-board/task', task.pin.objectId);
  f.archive(); f.largeResume(); f.f.loseReply();
  const continued = await new TaskBoardScheduler(engine).task(task.pin.objectId);
  assert.ok(continued?.run);
  const admitted = await engine.store.read('task-board/task', task.pin.objectId);
  assert.deepEqual(admitted.value.primaryResourceRef, before.value.primaryResourceRef);
  assert.deepEqual(admitted.value.commentDeliveries, before.value.commentDeliveries);
  assert.deepEqual(f.f.frames.filter(frame => frame['method'] === 'thread/unarchive').map(frame => nativeRecord(frame['params'])['threadId']), ['native-primary']);
  await f.second.drain(continued.run.objectId);
  assert.equal((await engine.store.read('task-board/run', continued.run.objectId)).value.phase, 'running');
  for (const [method, count] of [['thread/start', 1], ['thread/unarchive', 1], ['thread/resume', 1], ['turn/start', 2]] as const)
    assert.equal(f.f.frames.filter(frame => frame['method'] === method).length, count);
  const final = await engine.store.read('task-board/task', task.pin.objectId);
  assert.equal(final.value.commentDeliveries.find(value => value.commentId === 'archived-follow-up')!.state, 'delivered');
});

test('a confirmed archive failure retries the same previously executed context after the recovery delay', async t => {
  const f = await fixture(t), engine = f.f.w.f.first;
  await f.first.drain(f.runId); f.status('completed'); await f.first.drain(f.runId);
  const task = await engine.store.read('task-board/task', (await f.current()).value.taskId);
  await f.f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision,
    commentId: 'archive-race-follow-up', body: 'Retain this input through restoration.', requests: [], responses: [], attachments: [], replyTo: null });
  const continued = await new TaskBoardScheduler(engine).task(task.pin.objectId);
  f.archive(); await f.second.drain(continued!.run!.objectId);
  const blocked = await engine.store.read('task-board/task', task.pin.objectId);
  assert.equal(blocked.value.workflowState, 'waiting');
  assert.equal(await unstartedMissingContext(engine, blocked), false, 'An earlier executed turn forbids replacing this context.');
  assert.equal(await unstartedArchivedContext(engine, blocked), true);
  assert.equal(await new TaskBoardScheduler(engine).task(task.pin.objectId), null);
  const restored = await new TaskBoardScheduler(engine, () => new Date(Date.now() + 31000)).task(task.pin.objectId);
  assert.ok(restored?.run);
  const admitted = await engine.store.read('task-board/task', task.pin.objectId);
  assert.deepEqual(admitted.value.primaryResourceRef, blocked.value.primaryResourceRef);
  assert.deepEqual(admitted.value.fields, blocked.value.fields);
  assert.deepEqual(admitted.value.commentDeliveries, blocked.value.commentDeliveries);
  await f.first.drain(restored.run.objectId);
  assert.equal((await engine.store.read('task-board/run', restored.run.objectId)).value.phase, 'running');
  for (const [method, count] of [['thread/start', 1], ['thread/unarchive', 1], ['thread/resume', 2], ['turn/start', 2]] as const)
    assert.equal(f.f.frames.filter(frame => frame['method'] === method).length, count);
  assert.equal((await engine.store.read('task-board/task', task.pin.objectId)).value.commentDeliveries.find(value => value.commentId === 'archive-race-follow-up')!.state, 'delivered');
});

test('Run orchestration starts once, leaves unchanged polls ephemeral and completes with one latest output item', async t => {
  const f = await fixture(t);
  await f.first.drain(f.runId);
  const running = await f.current(); assert.equal(running.value.phase, 'running'); assert.equal(running.value.turnId, 'native-turn');
  const objects = async () => (await f.f.w.f.first.store.page('task-board/native-read-evidence', { limit: 100 })).items.length;
  const count = await objects(); await f.second.step(f.runId); await f.first.step(f.runId);
  assert.deepEqual((await f.current()).pin, running.pin); assert.equal(await objects(), count);
  f.status('completed'); await f.first.drain(f.runId);
  const completed = await f.current(), result = await f.f.w.f.first.store.read('task-board/result', completed.value.result!);
  assert.equal(completed.value.phase, 'completed'); assert.equal(result.value.content.summary, 'Second');
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/items/list').length, 1);
  assert.equal((await f.f.w.f.first.store.page('task-board/native-transcript', { limit: 2 })).items.length, 0);
  assert.equal((await f.f.w.f.first.store.read('task-board/task', completed.value.taskId)).value.workflowState, 'review');
  for (const method of ['thread/start', 'turn/start']) assert.equal(f.f.frames.filter(frame => frame['method'] === method).length, 1);
  assert.equal(f.f.journal.status().retained, 2);
  const finalFrames = f.f.frames.length; await f.first.step(f.runId); assert.equal(f.f.frames.length, finalFrames);
});

test('a ticket handoff completes without requesting any output history', async t => {
  const f = await fixture(t); await f.first.drain(f.runId);
  const engine = f.f.w.f.first, task = await engine.store.read('task-board/task', (await f.current()).value.taskId);
  await f.f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision,
    commentId: 'worker-handoff', authorKind: 'agent', purpose: 'handoff', body: 'Delivered and verified. See the attached result.', requests: [], responses: [], attachments: [], replyTo: null });
  f.outputError(-32000); f.status('completed'); await f.second.drain(f.runId);
  const completed = await f.current(), result = await engine.store.read('task-board/result', completed.value.result!);
  assert.equal(result.value.content.summary, 'Delivered and verified. See the attached result.');
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/items/list').length, 0);
  const final = (await engine.store.read('task-board/task', task.pin.objectId)).value;
  assert.equal(final.workflowState, 'review'); assert.equal(final.comments.length, 1);
});

test('additional output is read one item at a time only until the final assistant answer is found', async t => {
  const f = await fixture(t); await f.first.drain(f.runId);
  f.outputs([
    { id: 'trailing', type: 'reasoning', summary: [], content: [] },
    { id: 'final', type: 'agentMessage', phase: 'final_answer', text: 'Verified final answer.' },
    { id: 'earlier', type: 'agentMessage', phase: 'commentary', text: 'Earlier progress.' },
  ]);
  f.status('completed'); await f.first.drain(f.runId);
  const run = await f.current(), result = await f.f.w.f.first.store.read('task-board/result', run.value.result!);
  assert.equal(result.value.content.summary, 'Verified final answer.');
  const reads = f.f.frames.filter(frame => frame['method'] === 'thread/items/list');
  assert.deepEqual(reads.map(frame => nativeRecord(frame['params'])['cursor']), [null, '1']);
});

test('missing final text uses confirmed status without walking the full history or requesting a full turn', async t => {
  const f = await fixture(t); await f.first.drain(f.runId);
  f.outputs(Array.from({ length: 8 }, (_, index) => ({ id: String(index), type: 'reasoning', summary: [], content: [] })));
  f.status('completed'); await f.first.drain(f.runId);
  const run = await f.current(), result = await f.f.w.f.first.store.read('task-board/result', run.value.result!);
  assert.equal(run.value.phase, 'completed'); assert.equal(result.value.content.summary, 'Native turn completed.');
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/items/list').length, 3);
  assert.ok(!f.f.frames.some(frame => nativeRecord(frame['params'])['includeTurns'] === true || nativeRecord(frame['params'])['itemsView'] === 'full'));
});

test('an unavailable item API does not trigger a full turn fallback', async t => {
  const f = await fixture(t); await f.first.drain(f.runId); f.outputError(-32601);
  f.status('completed'); await f.first.drain(f.runId);
  assert.equal((await f.current()).value.phase, 'completed');
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/items/list').length, 1);
  assert.ok(!f.f.frames.some(frame => nativeRecord(frame['params'])['includeTurns'] === true || nativeRecord(frame['params'])['itemsView'] === 'full'));
});

test('completion refuses a nonterminal snapshot and final output from another epoch', async t => {
  const f = await fixture(t); await f.first.drain(f.runId);
  const engine = f.f.w.f.first, running = await f.current(), inspection = await f.first.inspector.find(running.pin);
  assert.equal(inspection.kind, 'found'); if (inspection.kind !== 'found') throw new Error('Missing fixture turn.');
  const snapshot = await f.first.evidence.saveSnapshot(running.pin, inspection.metadata, inspection.turns);
  const publish = async (change: TaskBoard.NativeUpdateRequest['change']) => {
    const run = await f.current(), task = await engine.store.read('task-board/task', run.value.taskId);
    return engine.nativeUpdate({ action: 'nativeUpdate', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision, run: run.pin, change });
  };
  await assert.rejects(publish({ kind: 'completion', snapshot, output: null, notificationCursor: 0 }),
    (error: unknown) => error instanceof IvyError && error.code === 'task_board_native_state_changed');
  assert.deepEqual((await f.current()).pin, running.pin);
  f.status('completed'); await f.first.step(f.runId);
  const terminal = await f.current(), proof = terminal.value.externalOutcome.evidence!;
  const context = await f.first.inspector.context(terminal.pin);
  const observed = await f.first.inspector.read(context, 'thread/items/list', { threadId: context.threadId, turnId: context.turnId, cursor: null, limit: 1, sortDirection: 'desc' });
  const output = await f.first.evidence.saveOutput(terminal.pin, proof, null, observed);
  await assert.rejects(publish({ kind: 'completion', snapshot: proof, output: { ...output, evidence: { ...output.evidence, epoch: 'other-epoch' } }, notificationCursor: 0 }),
    (error: unknown) => error instanceof IvyError && error.code === 'task_board_native_epoch_changed');
  assert.deepEqual((await f.current()).pin, terminal.pin); assert.equal((await f.current()).value.result, null);
  await publish({ kind: 'completion', snapshot: proof, output, notificationCursor: 0 });
  assert.equal((await f.current()).value.phase, 'completed');
});

test('completion replays its saved final read after a lost publication receipt without reading output again', async t => {
  const f = await fixture(t); await f.first.drain(f.runId); f.status('completed'); await f.first.step(f.runId);
  const engine = f.f.w.f.first, finish = engine.operations.finish.bind(engine.operations);
  let operationId = '';
  const update = engine.nativeUpdate.bind(engine);
  engine.nativeUpdate = async request => { if (request.change.kind === 'completion') operationId = request.operationId; return update(request); };
  engine.operations.finish = async () => { throw new IvyError('synthetic_receipt_loss', 'Original publication receipt unavailable.', 'unknown'); };
  try { await assert.rejects(f.first.step(f.runId), (error: unknown) => error instanceof IvyError && error.code === 'synthetic_receipt_loss'); }
  finally { engine.operations.finish = finish; engine.nativeUpdate = update; }
  const operation = await engine.operations.find(engine.settings.principalId, operationId);
  assert.equal(operation!.value.phase, 'needs_attention');
  await engine.store.collectLocal(Date.now() + 8 * 86400000);
  const outcome = await engine.recoverOperation(operation!.pin.objectId);
  assert.equal((await engine.store.read('task-board/task', outcome.task!)).value.workflowState, 'review');
  assert.equal((await engine.store.read('task-board/result', outcome.result!)).value.content.summary, 'Second');
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/items/list').length, 1);
  assert.equal((await engine.store.page('task-board/result', { limit: 2 })).items.length, 1);
});

test('an unfinished turn closes only when the original owned process loss is confirmed', async t => {
  const f = await fixture(t);
  await f.first.drain(f.runId);
  const original = await f.current();
  assert.equal(original.value.phase, 'running');
  f.f.restart();
  await f.first.step(f.runId);
  const uncertain = await f.current();
  assert.equal(uncertain.value.phase, 'outcome_unknown');
  assert.equal(uncertain.value.result, null);
  const uncertainTask = await f.f.w.f.first.store.read('task-board/task', uncertain.value.taskId);
  assert.equal(uncertainTask.value.workflowState, 'in_progress');
  const engine = f.f.w.f.first;
  await engine.store.write('task-board/task', { ...uncertainTask.value, workflowState: 'waiting', waiting: { reason: 'external_outcome', detail: 'Original execution is being reconciled.', since: new Date().toISOString() } }, randomUUID(),
    { objectId: uncertainTask.pin.objectId, expectedRevision: uncertainTask.pin.revision });
  await f.first.step(f.runId);
  const corrected = await engine.store.read('task-board/task', uncertain.value.taskId);
  assert.equal(corrected.value.workflowState, 'in_progress', 'An unchanged recovery observation corrects the old blocked projection.');
  assert.equal(engine.condition(corrected.value).state, 'recovering');
  await f.first.step(f.runId);
  assert.deepEqual((await engine.store.read('task-board/task', uncertain.value.taskId)).pin, corrected.pin, 'Unchanged recovery does not keep writing revisions.');
  assert.equal(await unstartedMissingContext(f.f.w.f.first, uncertainTask), false);
  assert.equal(await new TaskBoardScheduler(f.f.w.f.first).task(uncertain.value.taskId), null);

  // The replacement owner can close the old attempt after it reports a retained stop proof.
  f.f.confirmStopped(original.value.nativeEpoch!);
  await f.first.step(f.runId);
  const closed = await f.current();
  assert.equal(closed.value.phase, 'failed');
  assert.equal(closed.value.externalOutcome.code, 'native_process_lost');
  assert.ok(closed.value.result);
  assert.equal((await f.f.w.f.first.store.read('task-board/task', closed.value.taskId)).value.workflowState, 'waiting');
});

test('continuation obtains a new retained resume receipt before its new turn on the original primary', async t => {
  const f = await fixture(t);
  for (let index = 0; index < 4; index++) await f.first.step(f.runId);
  f.status('completed'); await f.first.step(f.runId); await f.first.step(f.runId);
  const original = await f.current(); assert.equal(original.value.phase, 'completed');
  const task = await f.f.w.f.first.store.read('task-board/task', original.value.taskId), feedback = 'Continue the same original task with this feedback.';
  const commented = await f.f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision,
    commentId: randomUUID(), body: feedback, requests: [], responses: [], attachments: [], replyTo: null });
  const plan = await f.f.w.f.invoke({ action: 'savePlan', operationId: randomUUID(), plan: { ...f.f.w.f.plan, turnStart: { input: [{ type: 'text', text: feedback, text_elements: [] }] } } });
  const continued = await f.f.w.f.invoke({ action: 'continue', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: commented.task!.revision, intent: plan.plan!, feedback,
    target: f.f.w.planned.target, workspace: f.f.w.planned.workspace, commentIds: [(await f.f.w.f.first.store.read('task-board/task', commented.task!)).value.comments.at(-1)!.commentId] });
  assert.ok(continued.run);
  await f.second.step(continued.run.objectId);
  let run = await f.f.w.f.first.store.read('task-board/run', continued.run.objectId);
  assert.ok(run.value.calls.thread); assert.equal(run.value.calls.turn, null); assert.equal(run.value.nativeEpoch, null);
  assert.equal((await f.f.w.f.first.store.read('task-board/native-call', run.value.calls.thread)).value.request.method, 'thread/resume');
  await f.second.step(continued.run.objectId); await f.first.step(continued.run.objectId); await f.second.step(continued.run.objectId);
  run = await f.f.w.f.first.store.read('task-board/run', continued.run.objectId);
  assert.equal(run.value.phase, 'running'); assert.deepEqual(run.value.primaryResourceRef, original.value.primaryResourceRef);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/start').length, 1);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/resume').length, 1);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'turn/start').length, 2);
  const sentInput = nativeRecord(f.f.frames.filter(frame => frame['method'] === 'turn/start').at(-1)!['params'])['input'] as Array<{ text: string }>;
  assert.equal(sentInput.length, 1);
  assert.match(sentInput[0]!.text, /Read the current ticket/);
  assert.ok(!sentInput[0]!.text.includes(feedback));
  assert.deepEqual((await f.current()).pin, original.pin, 'The previous completed Run remains immutable during continuation.');
});

test('uncertain resume after provider loss reattaches the same context without replaying ticket input', async t => {
  const f = await fixture(t);
  for (let index = 0; index < 4; index++) await f.first.step(f.runId);
  f.status('completed'); await f.first.step(f.runId); await f.first.step(f.runId);
  const original = await f.current();
  const task = await f.f.w.f.first.store.read('task-board/task', original.value.taskId);
  const commentId = randomUUID();
  const commented = await f.f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, commentId, body: 'Continue after reconnect.', requests: [], responses: [], attachments: [], replyTo: null });
  const plan = await f.f.w.f.invoke({ action: 'savePlan', operationId: randomUUID(), plan: f.f.w.f.plan });
  const continued = await f.f.w.f.invoke({ action: 'continue', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: commented.task!.revision, intent: plan.plan!, feedback: 'Continue after reconnect.', target: f.f.w.planned.target,
    workspace: f.f.w.planned.workspace, commentIds: [commentId] });
  const runId = continued.run!.objectId;
  await f.first.step(runId);
  f.largeResume();
  f.f.hold('thread/resume');
  await f.first.step(runId);
  let run = await f.f.w.f.first.store.read('task-board/run', runId);
  const uncertainCall = await f.f.w.f.first.store.read('task-board/native-call', run.value.calls.thread!);
  assert.equal(uncertainCall.value.ownerPhase, 'dispatched');
  f.f.restart(); f.f.hold(null);
  await f.second.step(runId);
  run = await f.f.w.f.first.store.read('task-board/run', runId);
  assert.equal(run.value.calls.thread!.objectId, uncertainCall.pin.objectId);
  await f.second.step(runId);
  run = await f.f.w.f.first.store.read('task-board/run', runId);
  assert.notEqual(run.value.calls.thread!.objectId, uncertainCall.pin.objectId);
  assert.equal((await f.f.w.f.first.store.read('task-board/native-call', run.value.calls.thread!)).value.origin?.kind, 'reattach');
  await f.second.drain(runId);
  run = await f.f.w.f.first.store.read('task-board/run', runId);
  assert.equal(run.value.phase, 'running');
  assert.deepEqual(run.value.primaryResourceRef, original.value.primaryResourceRef);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/resume').length, 2);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'turn/start').length, 2);
  const currentTask = await f.f.w.f.first.store.read('task-board/task', task.pin.objectId);
  assert.equal(currentTask.value.commentDeliveries.find(value => value.commentId === commentId)?.state, 'delivered');
});

test('closed provider retains in progress with visible automatic recovery and bounded backoff', async t => {
  const f = await fixture(t);
  let now = Date.now();
  t.mock.method(Date, 'now', () => now);
  await f.first.step(f.runId);
  f.f.providerClosed(true);
  await f.first.drain(f.runId);
  let run = await f.current();
  let task = await f.f.w.f.first.store.read('task-board/task', run.value.taskId);
  assert.equal(run.value.phase, 'outcome_unknown');
  assert.equal(task.value.waiting, null);
  assert.equal(task.value.workflowState, 'in_progress');
  assert.equal(task.value.claim?.run?.objectId, f.runId);
  assert.equal(executionCondition({ workflowState: task.value.workflowState, control: task.value.fields.control,
    publication: !!task.value.publication, claim: { phase: task.value.claim!.phase, hostId: run.value.target.hostId },
    waiting: task.value.waiting, executionRequirement: task.value.fields.executionRequirement }).state, 'recovering');
  const engine = f.f.w.f.first;
  await engine.store.write('task-board/task', { ...task.value, workflowState: 'waiting', waiting: { reason: 'host', detail: 'Native reconciliation is waiting.', since: new Date().toISOString() } }, randomUUID(),
    { objectId: task.pin.objectId, expectedRevision: task.pin.revision });
  await f.second.step(f.runId);
  task = await engine.store.read('task-board/task', run.value.taskId);
  assert.equal(task.value.workflowState, 'in_progress');
  assert.equal(task.value.waiting, null);
  await f.second.step(f.runId);
  assert.deepEqual((await engine.store.read('task-board/task', run.value.taskId)).pin, task.pin);
  const firstCalls = f.f.ownerCalls;
  await f.first.drain(f.runId);
  assert.equal(f.f.ownerCalls, firstCalls);
  now += 30_001;
  await f.first.drain(f.runId);
  assert.ok(f.f.ownerCalls > firstCalls);
  f.f.providerClosed(false);
  now += 60_001;
  await f.first.drain(f.runId);
  run = await f.current();
  task = await f.f.w.f.first.store.read('task-board/task', run.value.taskId);
  assert.equal(run.value.phase, 'running');
  assert.equal(task.value.workflowState, 'in_progress');
  assert.equal(task.value.waiting, null);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/start').length, 1);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
});

test('periodic reconciliation recovers completed work after provider loss and a definition update without editing the ticket', async t => {
  const f = await fixture(t), engine = f.f.w.f.first, runtime = new TaskBoardReconciler(engine);
  let now = Date.now(); t.mock.method(Date, 'now', () => now);
  const tick = async () => { await runtime.tick(); await runtime.settled(); now += 300_001; };
  for (let pass = 0; pass < 12 && (await f.current()).value.phase !== 'running'; pass++) await tick();
  assert.equal((await f.current()).value.phase, 'running');
  const identity = (await f.current()).value.primaryResourceRef;
  f.f.providerClosed(true);
  for (let pass = 0; pass < 8 && (await f.current()).value.phase !== 'outcome_unknown'; pass++) await tick();
  assert.equal((await f.current()).value.phase, 'outcome_unknown');
  f.status('completed');
  const registry = f.f.w.f.kernel.registry.getRegistry('native-agent')!;
  for (const tool of registry.namespaces.find(ns => ns.namespace === 'agent')!.tools)
    if (tool.annotations?.readOnlyHint) tool.description += ' Updated read contract.';
  await f.f.w.f.clients.get('native-agent')!.client.request('registry.sync', registry);
  f.f.providerClosed(false);
  for (let pass = 0; pass < 16 && (await f.current()).value.phase !== 'completed'; pass++) await tick();
  const run = await f.current(), task = await engine.store.read('task-board/task', run.value.taskId);
  assert.equal(run.value.phase, 'completed'); assert.equal(task.value.workflowState, 'review');
  assert.equal(task.value.waiting, null); assert.deepEqual(task.value.primaryResourceRef, identity);
  assert.equal(task.value.attemptCount, 1);
  for (const method of ['thread/start', 'turn/start']) assert.equal(f.f.frames.filter(frame => frame['method'] === method).length, 1);
  assert.deepEqual(runtime.diagnostics, []);
});

test('Run orchestration reattaches the original primary before its original turn and confirms cancellation through final material', async t => {
  const f = await fixture(t); await f.first.step(f.runId); await f.first.step(f.runId); f.f.restart();
  await f.first.step(f.runId); const preparedTurn = (await f.current()).value.calls.turn!;
  await f.first.step(f.runId); const resume = await f.f.w.f.first.store.read('task-board/native-call', (await f.current()).value.calls.thread!);
  assert.equal(resume.value.request.method, 'thread/resume'); assert.equal(resume.value.origin?.kind, 'reattach');
  await f.second.step(f.runId); await f.second.step(f.runId);
  assert.equal((await f.current()).value.calls.turn!.objectId, preparedTurn.objectId); assert.equal((await f.current()).value.phase, 'running');
  await f.f.w.refresh(); await f.f.w.cancel();
  await f.first.step(f.runId); await f.second.step(f.runId); assert.equal((await f.current()).value.phase, 'cancel_requested');
  await f.first.step(f.runId); assert.equal((await f.current()).value.phase, 'publishing_result');
  const publishing = await f.f.w.f.first.store.read('task-board/task', (await f.current()).value.taskId);
  assert.equal(publishing.value.workflowState, 'in_progress');
  assert.equal(f.f.w.f.first.condition(publishing.value).state, 'publishing');
  await f.second.step(f.runId); assert.equal((await f.current()).value.phase, 'cancelled');
  for (const method of ['thread/start', 'thread/resume', 'turn/start', 'turn/interrupt']) assert.equal(f.f.frames.filter(frame => frame['method'] === method).length, 1);
  const engine = f.f.w.f.first, finished = await f.current(), task = await engine.store.read('task-board/task', finished.value.taskId);
  const parked = await f.f.w.f.invoke({ action: 'transition', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, workflowState: 'backlog', detail: 'Superseded work stays available without being scheduled.' });
  assert.equal((await engine.store.read('task-board/task', parked.task!)).value.workflowState, 'backlog');
  assert.equal(await new TaskBoardScheduler(engine).task(task.pin.objectId), null);
  assert.deepEqual((await f.current()).pin, finished.pin, 'Parking cancelled work retains its original execution evidence.');
});

test('a second provider replacement reattaches an uncertain resume without replaying the prepared turn', async t => {
  const f = await fixture(t);
  await f.first.step(f.runId); await f.first.step(f.runId);
  const confirmedEpoch = (await f.current()).value.nativeEpoch;
  f.f.restart();
  await f.first.step(f.runId);
  const preparedTurn = (await f.current()).value.calls.turn!;
  await f.first.step(f.runId);
  f.f.hold('thread/resume');
  await f.second.step(f.runId);
  f.f.restart(); f.f.hold(null);
  await f.first.step(f.runId);
  const uncertain = (await f.current()).value;
  assert.equal(uncertain.phase, 'outcome_unknown');
  assert.equal(uncertain.nativeEpoch, confirmedEpoch);
  assert.deepEqual(uncertain.calls.turn, preparedTurn);
  const uncertainResume = uncertain.calls.thread!;
  assert.equal((await f.f.w.f.first.store.read('task-board/native-call', uncertainResume)).value.ownerPhase, 'outcome_unknown');

  f.f.restart();
  await f.second.step(f.runId);
  const successor = (await f.current()).value;
  assert.notEqual(successor.calls.thread!.objectId, uncertainResume.objectId);
  assert.deepEqual(successor.calls.turn, preparedTurn);
  await f.second.drain(f.runId);
  const running = (await f.current()).value;
  assert.equal(running.phase, 'running');
  assert.equal(running.turnId, 'native-turn');
  assert.equal(running.calls.turn!.objectId, preparedTurn.objectId);
  for (const [method, count] of [['thread/start', 1], ['thread/resume', 2], ['turn/start', 1]] as const)
    assert.equal(f.f.frames.filter(frame => frame['method'] === method).length, count);
});

test('Run orchestration closes a cancelled undispatched attempt without native calls', async t => {
  const f = await fixture(t); await f.f.w.cancel(); await f.second.step(f.runId);
  assert.equal((await f.current()).value.phase, 'cancelled'); assert.equal(f.f.frames.length, 0);
});

test('failed reattachment fences the original prepared turn before Run closure', async t => {
  const f = await fixture(t); await f.first.step(f.runId); await f.first.step(f.runId); f.f.restart();
  await f.first.step(f.runId); const preparedTurn = (await f.current()).value.calls.turn!;
  await f.first.step(f.runId);
  f.f.reply(frame => frame['method'] === 'thread/resume' ? { error: { code: -32000, message: 'Original resume failed.' } } : undefined);
  await f.second.step(f.runId);
  assert.equal((await f.current()).value.phase, 'outcome_unknown'); assert.equal((await f.current()).value.result, null);
  await f.second.step(f.runId); assert.equal((await f.current()).value.phase, 'failed');
  const original = await f.f.w.f.first.store.read('task-board/native-call', preparedTurn);
  assert.equal(f.f.journal.get({ callerPrincipalId: f.f.w.f.settings.principalId, operationId: original.value.operationId })!.code, 'request_prevented');
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'turn/start').length, 0);
  const task = await f.f.w.f.first.store.read('task-board/task', (await f.current()).value.taskId);
  assert.equal(await unstartedMissingContext(f.f.w.f.first, task), false, 'A generic resume failure does not authorize replacement.');
  assert.equal(await unstartedArchivedContext(f.f.w.f.first, task), false, 'A generic resume failure does not authorize restoration.');
});

async function missingUnstarted(t: TestContext) {
  const f = await fixture(t);
  await f.first.step(f.runId); await f.first.step(f.runId); f.f.restart();
  await f.first.step(f.runId); await f.first.step(f.runId);
  let restored = false, loseStart = false, active = false;
  const reply = (id: string) => ({ result: { thread: { ...f.f.w.nativeThread([], { type: 'idle' }), id, sessionId: id },
    cwd: '/fixture', model: 'fixture-model', modelProvider: 'openai', approvalPolicy: 'on-request', approvalsReviewer: 'user',
    sandbox: { type: 'readOnly' }, reasoningEffort: 'high' } });
  f.f.reply(frame => {
    const params = nativeRecord(frame['params']);
    if (frame['method'] === 'thread/resume') {
      const id = String(params['threadId']);
      return id === 'native-primary' && !restored || id === 'recovered-primary' && !active
        ? { error: { code: -32600, message: 'no rollout found for thread id ' + id } } : reply(id);
    }
    if (frame['method'] === 'thread/start') {
      if (loseStart) { loseStart = false; f.f.loseReply(); }
      return reply('recovered-primary');
    }
    if (frame['method'] === 'thread/read') return { result: { thread: { ...f.f.w.nativeThread([], active ? { type: 'active', activeFlags: [] } : { type: 'idle' }), id: String(params['threadId']) } } };
    if (frame['method'] === 'turn/start') { active = true; return { result: { turn: f.f.w.turn() } }; }
    return;
  });
  await f.second.step(f.runId); await f.second.step(f.runId);
  const closed = await f.current(), task = await f.f.w.f.first.store.read('task-board/task', closed.value.taskId);
  assert.equal(closed.value.phase, 'failed'); assert.equal(closed.value.turnId, null);
  const clock = () => new Date(Date.parse(closed.value.finishedAt!) + 30001);
  return { ...f, task, clock, restore: () => { restored = true; }, loseStart: () => { loseStart = true; } };
}

test('missing unstarted context recovers once across lost native and publication replies and delivers queued input once', async t => {
  const f = await missingUnstarted(t), engine = f.f.w.f.first;
  assert.equal(await unstartedMissingContext(engine, f.task), true);
  assert.equal(await new TaskBoardScheduler(engine).task(f.task.pin.objectId), null, 'Recovery waits before retrying a failed attempt.');
  const commentId = 'retained-recovery-input';
  await f.f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: f.task.pin.objectId, expectedRevision: f.task.pin.revision,
    commentId, body: 'AgentManager is repaired; continue the original work.', requests: [], responses: [], attachments: [], replyTo: null });
  // An already blocked ticket can have an additional failed continuation from before this fix.
  const queued = await engine.store.read('task-board/task', f.task.pin.objectId), allocation = await new TaskBoardScheduler(engine).allocate(queued, randomUUID());
  const continuation = await engine.schedule({ action: 'continue', operationId: randomUUID(), taskId: queued.pin.objectId,
    expectedRevision: queued.pin.revision, ...allocation, feedback: 'Continue the current ticket.', commentIds: [commentId] });
  await f.second.drain(continuation.run!.objectId);
  const before = await engine.store.read('task-board/task', f.task.pin.objectId), failed = await engine.store.read('task-board/run', continuation.run!.objectId);
  const clock = () => new Date(Date.now() + 31000);
  assert.equal(await unstartedMissingContext(engine, before), true);
  let lostPublication = false;
  f.f.w.f.intercept(async (params, result) => {
    if (!lostPublication && params.objectId === before.pin.objectId && result.object.currentRevision > before.pin.revision) {
      lostPublication = true; throw new IvyError('deadline_exceeded', 'The recovery publication reply was lost.', 'unknown');
    }
  });
  f.loseStart();
  await assert.rejects(new TaskBoardScheduler(engine, clock).task(before.pin.objectId), (error: unknown) => error instanceof IvyError && error.outcome === 'unknown');
  const attempt = await engine.store.named('task-board/scheduler-attempt', 'Scheduled task ' + before.pin.objectId + ' revision ' + before.pin.revision, before.pin.objectId);
  assert.equal(attempt!.value.request.action, 'recoverThread');
  await engine.store.collectLocal(Date.now() + 8 * 86400000);
  const recovered = await f.f.w.f.second.schedule(attempt!.value.request), task = await engine.store.read('task-board/task', recovered.task!);
  assert.deepEqual(await engine.schedule(attempt!.value.request), recovered);
  assert.equal(task.value.workflowState, 'todo'); assert.equal(task.value.primaryResourceRef, null);
  assert.deepEqual(task.value.comments, before.value.comments); assert.deepEqual(task.value.commentDeliveries, before.value.commentDeliveries);
  assert.deepEqual(task.value.fields.workspaceRequirement, before.value.fields.workspaceRequirement);
  assert.deepEqual((await engine.store.read('task-board/run', failed.pin.objectId)).pin, failed.pin, 'The failed original Run remains immutable.');
  const started = await new TaskBoardScheduler(f.f.w.f.second, clock).task(task.pin.objectId);
  await f.second.drain(started!.run!.objectId);
  const running = await engine.store.read('task-board/run', started!.run!.objectId), delivered = await engine.store.read('task-board/task', task.pin.objectId);
  assert.equal(running.value.phase, 'running'); assert.deepEqual(running.value.workspace, failed.value.workspace);
  assert.equal(running.value.originalRequest.action, 'start'); assert.deepEqual(running.value.target, failed.value.target);
  assert.equal(running.value.primaryResourceRef!.nativeId, 'recovered-primary');
  assert.deepEqual(running.value.originalRequest.commentIds, [commentId]);
  assert.equal(delivered.value.commentDeliveries.find(value => value.commentId === commentId)!.state, 'delivered');
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/start').length, 2, 'Only one replacement is dispatched.');
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'turn/steer').length, 0);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/resume' && nativeRecord(frame['params'])['threadId'] === 'recovered-primary').length, 0,
    'The empty replacement starts its first turn before any resume attempt.');
});

test('recovery reuses a restored original context and respects an unanswered ticket question', async t => {
  const f = await missingUnstarted(t), engine = f.f.w.f.first;
  await f.f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: f.task.pin.objectId, expectedRevision: f.task.pin.revision,
    commentId: 'open-recovery-question', authorKind: 'agent', purpose: 'question', body: 'Which input should be used?', requests: [], responses: [], attachments: [], replyTo: null });
  assert.equal(await new TaskBoardScheduler(engine, f.clock).task(f.task.pin.objectId), null);
  const waiting = await engine.store.read('task-board/task', f.task.pin.objectId);
  await f.f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: waiting.pin.objectId, expectedRevision: waiting.pin.revision,
    commentId: 'recovery-answer', body: 'Use the existing ticket input.', requests: [], responses: [], attachments: [], replyTo: 'open-recovery-question' });
  f.restore();
  const recovered = await new TaskBoardScheduler(engine, f.clock).task(f.task.pin.objectId), task = await engine.store.read('task-board/task', recovered!.task!);
  assert.equal(task.value.workflowState, 'todo'); assert.equal(task.value.primaryResourceRef!.nativeId, 'native-primary');
  await f.f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision,
    commentId: 'after-restoration', body: 'Also retain this follow-up.', requests: [], responses: [], attachments: [], replyTo: null });
  const next = await new TaskBoardScheduler(f.f.w.f.second, f.clock).task(task.pin.objectId);
  await f.second.drain(next!.run!.objectId);
  assert.equal((await engine.store.read('task-board/run', next!.run!.objectId)).value.phase, 'running');
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/start').length, 1);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
});

test('a queued fresh recovery cannot silently move its retained workspace', async t => {
  const f = await missingUnstarted(t), engine = f.f.w.f.first;
  const recovered = await new TaskBoardScheduler(engine, f.clock).task(f.task.pin.objectId);
  const task = await engine.store.read('task-board/task', recovered!.task!);
  assert.equal(task.value.primaryResourceRef, null);
  f.f.w.planned.workspace = { ...f.f.w.planned.workspace, canonicalCwd: '/another-workspace', intendedPath: '/another-workspace' };
  const deferred = await new TaskBoardScheduler(f.f.w.f.second, f.clock).task(task.pin.objectId);
  const blocked = await engine.store.read('task-board/task', deferred!.task!);
  assert.equal(blocked.value.waiting!.reason, 'workspace');
  assert.match(blocked.value.waiting!.detail, /retain the original workspace/);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/start').length, 1);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'turn/start').length, 0);
});

test('missing context recovery refuses a context with an earlier executed turn', async t => {
  const f = await fixture(t), engine = f.f.w.f.first;
  await f.first.drain(f.runId); f.status('completed'); await f.first.drain(f.runId);
  const original = await f.current(), task = await engine.store.read('task-board/task', original.value.taskId);
  await f.f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision,
    commentId: 'next-work', body: 'Continue the existing work.', requests: [], responses: [], attachments: [], replyTo: null });
  const continued = await new TaskBoardScheduler(engine).task(task.pin.objectId);
  f.f.reply(frame => frame['method'] === 'thread/resume' ? { error: { code: -32600, message: 'no rollout found for thread id native-primary' } } : undefined);
  await f.second.drain(continued!.run!.objectId);
  const failed = await engine.store.read('task-board/run', continued!.run!.objectId), blocked = await engine.store.read('task-board/task', task.pin.objectId);
  assert.equal(failed.value.phase, 'failed'); assert.equal(failed.value.turnId, null);
  assert.equal(await unstartedMissingContext(engine, blocked), false);
  assert.equal(await new TaskBoardScheduler(engine, () => new Date(Date.now() + 31000)).task(task.pin.objectId), null);
  await assert.rejects(engine.schedule({ action: 'recoverThread', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: blocked.pin.revision, run: failed.pin }), (error: unknown) => error instanceof IvyError && error.code === 'task_board_execution_unresolved');
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'thread/start').length, 1);
  assert.equal(f.f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
});

test('detached Run reconciliation avoids repeated failure writes and preserves terminal evidence', async t => {
  const f = await fixture(t); for (let index = 0; index < 4; index++) await f.first.step(f.runId);
  const original = await f.current(), task = await f.f.w.f.first.store.read('task-board/task', original.value.taskId);
  await f.f.w.f.invoke({ action: 'reassign', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision,
    executionRequirement: task.value.fields.executionRequirement, workspaceRequirement: task.value.fields.workspaceRequirement, previousRun: original.pin, acknowledgeUnresolved: true, reason: 'Retain this earlier attempt separately.' });
  f.failRead(true); await f.first.step(f.runId); const unknown = await f.current();
  assert.equal(unknown.value.externalOutcome.state, 'unknown'); await f.second.step(f.runId); assert.deepEqual((await f.current()).pin, unknown.pin);
  f.failRead(false); f.status('completed'); await f.second.step(f.runId); const terminal = await f.current();
  assert.equal(terminal.value.externalOutcome.state, 'succeeded'); assert.equal(terminal.value.phase, 'publishing_result');
  f.failRead(true); await assert.rejects(f.first.step(f.runId), (error: unknown) => error instanceof IvyError && error.code === 'task_board_native_read_failed');
  assert.deepEqual((await f.current()).pin, terminal.pin); f.failRead(false); await f.second.step(f.runId);
  assert.equal((await f.current()).value.phase, 'completed');
  assert.equal((await f.f.w.f.first.store.read('task-board/task', task.pin.objectId)).value.latestResult, null);
});

test('native completion wakes only its owning run and publishes a small result in one drain', async t => {
  const f = await fixture(t); await f.first.drain(f.runId);
  const event = { jsonrpc: '2.0' as const, method: 'notifications.provider' as const, params: { namespace: 'agent', name: 'notification', version: '1.0.0', serviceNodeId: 'native-agent', generation: 1, payload: { method: 'turn/completed', params: { threadId: 'native-thread' } } } };
  const run = await f.current(); event.params.serviceNodeId = run.value.target.serviceNodeId; event.params.payload.params.threadId = run.value.primaryResourceRef!.nativeId;
  assert.equal(f.first.notification(event), true);
  assert.equal(f.first.notification({ ...event, params: { ...event.params, serviceNodeId: 'unrelated-agent' } }), false);
  f.status('completed'); await f.first.drain(f.runId);
  assert.equal((await f.current()).value.phase, 'completed');
  assert.equal(f.first.notification(event), false);
});

test('foreign native notifications advance an ephemeral cursor without revising run signals', async t => {
  const w = await nativeWorkflow(t);
  await w.running();
  const run = await w.f.first.store.read('task-board/run', w.state().run);
  const signals = new TaskBoardNativeSignals(w.f.first);
  const current = { run: run.pin, nativeVersion: run.value.plan.nativeVersion,
    catalogSourceHash: run.value.plan.catalogSourceHash, serviceNodeId: run.value.target.serviceNodeId,
    epoch: 'fixture-epoch', threadId: run.value.primaryResourceRef!.nativeId, turnId: run.value.turnId! };
  signals.inspector.context = async () => current;
  let sequence = 0;
  const cursors: number[] = [];
  Object.defineProperty(signals, 'read', { value: async (_node: string, name: string, args: { afterSequence?: number }) => {
    if (name === 'inputs') return { epoch: current.epoch, items: [], truncated: false };
    cursors.push(args.afterSequence ?? 0);
    sequence++;
    return { epoch: current.epoch, firstAvailableSequence: 1, throughSequence: sequence,
      gap: false, hasMore: false, items: [{ sequence, serviceNodeId: current.serviceNodeId,
        epoch: current.epoch, nativeVersion: current.nativeVersion, method: 'item/updated',
        params: { threadId: 'unrelated-thread' }, observedAt: new Date().toISOString() }] };
  } });
  const first = await signals.step(run.pin.objectId);
  assert.ok(first);
  assert.deepEqual(await signals.step(run.pin.objectId), first);
  assert.deepEqual(await signals.step(run.pin.objectId), first);
  assert.deepEqual(cursors, [0, 1, 2]);
  const saved = await w.f.first.store.read('task-board/native-signals', first!, false);
  assert.equal(saved.metadata.currentRevision, 1);
});

test('terminal publication rereads a native answer received after the earlier input scan', async t => {
  const f = await fixture(t); await f.first.drain(f.runId);
  const engine = f.f.w.f.first, run = await f.current();
  const identity = { serviceNodeId: run.value.target.serviceNodeId, epoch: f.f.epoch(), requestId: 'handoff-question' };
  f.f.journal.observeInput(identity, 'item/tool/requestUserInput', { threadId: run.value.primaryResourceRef!.nativeId,
    turnId: run.value.turnId, questions: [{ id: 'option', question: 'Which option?', isSecret: false, options: [] }] });
  await syncNativeComments(engine, f.runId);
  const operation = { callerPrincipalId: 'user', operationId: 'answer-before-completion' };
  const reply = { result: { answers: { option: { answers: ['First option'] } } } };
  f.f.journal.accept(operation, 'agent.answer', { identity, reply });
  f.f.journal.dispatchAnswer(operation, identity, reply);
  f.f.journal.resolveInput(identity.epoch, identity.requestId, run.value.primaryResourceRef!.nativeId);
  f.status('completed'); await f.first.drain(f.runId);
  const task = (await engine.store.read('task-board/task', run.value.taskId)).value;
  assert.equal(task.workflowState, 'review');
  assert.equal(task.comments.filter(comment => comment.nativeInput?.state === 'answered').length, 1);
});

test('running updates steer once across a lost reply and reconciliation by another owner', async t => {
  const f = await fixture(t); await f.first.drain(f.runId);
  const task = await f.f.w.f.first.store.read('task-board/task', (await f.current()).value.taskId);
  const commentId = randomUUID();
  await f.f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: task.pin.objectId, expectedRevision: task.pin.revision,
    commentId, body: 'Private changed requirements.', requests: [], responses: [], attachments: [], replyTo: null });
  await f.first.step(f.runId);
  f.f.loseReply();
  await f.second.step(f.runId);
  await f.first.step(f.runId);
  const calls = f.f.frames.filter(frame => frame['method'] === 'turn/steer');
  assert.equal(calls.length, 1);
  assert.doesNotMatch(JSON.stringify(calls), /Private changed requirements/);
  assert.equal((await f.f.w.f.first.store.read('task-board/task', task.pin.objectId)).value.commentDeliveries.find(c => c.commentId === commentId)?.state, 'delivered');
});
