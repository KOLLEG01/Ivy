import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { hashJson } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Agent, TaskBoard, Wire } from '../packages/contracts/src/generated.js';
import { context } from './fixtures/task-board.js';
import { nativeWorkflow } from './fixtures/task-board-native.js';
import { catalogFor } from '../services/task-board/src/runtime/evidence.js';
import { TaskBoardScheduler } from '../services/task-board/src/runtime/scheduler.js';
import { readPlan } from '../services/task-board/src/runtime/native-plan.js';
import { checkBlocker, validateBlocker } from '../services/task-board/src/runtime/task-coordination.js';

async function lifecycle(t: { after: (fn: () => void) => void }) {
  const w = await nativeWorkflow(t);
  await w.running();
  await w.f.first.nativeUpdate(await w.snapshot());
  await w.refresh();
  const result = (await w.f.first.store.read('task-board/task', w.state().task)).value.latestResult!;
  await w.f.invoke({ action: 'review', operationId: randomUUID(), taskId: w.state().task.objectId,
    expectedRevision: w.state().task.revision, result, acceptance: 'Verified the original result.' });
  await w.refresh();
  const native = catalogFor('0.154.0').catalog, calls: Agent.Operation[] = [];
  const threads = new Map<string, { archived: boolean; active?: boolean }>([['native-primary', { archived: false }]]);
  let unarchive: 'success' | 'failed' | 'pending' = 'success', lostReply = false;
  let readFailure: string | null = null, pathless = false;
  const archiveFailures = new Map<string, string>();
  const receipt = (operationId: string, method: string, params: Wire.Json, phase: Agent.Operation['phase'], reply: Agent.Reply | null): Agent.Operation => {
    const now = new Date().toISOString();
    return { schemaVersion: 1, callerPrincipalId: w.f.settings.principalId, serviceNodeId: 'native-agent', nativeVersion: '0.154.0',
      nativeExecutableHash: native.nativeExecutableHash, operationId, method, params, requestHash: hashJson({ method, params }),
      phase, createdAt: now, updatedAt: now, epoch: 'fixture-epoch', requestId: 'native-' + operationId, reply,
      code: phase === 'failed' ? 'native_error' : phase === 'outcome_unknown' ? 'native_epoch_lost' : null };
  };
  w.f.nativeTools(async call => {
    const args = call.arguments as Record<string, Wire.Json>;
    if (call.definition.name === 'operation') throw new IvyError('not_found', 'No observed operation.', 'not_executed', {
      kind: 'agent_operation_absent', operationId: args['operationId'], serviceNodeId: 'native-agent', epoch: 'fixture-epoch' });
    if (call.definition.name === 'read') {
      const params = args['params'] as { threadId: string }, state = threads.get(params.threadId);
      const reply: Agent.Reply = readFailure ? { error: { code: -32000, message: readFailure } } : state ? { result: { thread: { ...w.nativeThread(), id: params.threadId,
        status: { type: state.active ? 'active' : 'idle' }, path: pathless ? null : '/fixture/' + (state.archived ? 'archived_sessions' : 'sessions') + '/' + params.threadId + '.jsonl' } } }
        : { error: { code: -32000, message: 'Thread not found: ' + params.threadId } };
      return { schemaVersion: 1, observationId: randomUUID(), callerPrincipalId: w.f.settings.principalId,
        serviceNodeId: 'native-agent', nativeVersion: '0.154.0', nativeExecutableHash: native.nativeExecutableHash,
        catalogHash: hashJson(native), epoch: 'fixture-epoch', requestId: randomUUID(), observedAt: new Date().toISOString(),
        method: args['method'], params, requestHash: hashJson({ method: args['method'], params }), reply };
    }
    if (call.definition.name !== 'invoke') return w.f.nativeManagement(call);
    const method = String(args['method']), params = args['params'] as Record<string, Wire.Json>;
    let phase: Agent.Operation['phase'] = 'succeeded', reply: Agent.Reply = { result: {} };
    if (method === 'thread/start') {
      const id = 'fresh-' + randomUUID(); threads.set(id, { archived: false });
      reply = { result: { thread: { ...w.nativeThread(), id, cwd: String(params['cwd']) }, cwd: String(params['cwd']),
        model: 'fixture-model', modelProvider: 'openai', approvalPolicy: 'on-request', approvalsReviewer: 'user',
        sandbox: { type: 'readOnly' }, reasoningEffort: 'high' } };
    } else if (method === 'thread/archive') {
      const failure = archiveFailures.get(String(params['threadId']));
      if (failure) { phase = 'failed'; reply = { error: { code: -32000, message: failure } }; }
      else threads.get(String(params['threadId']))!.archived = true;
    }
    else if (method === 'thread/unarchive') {
      if (unarchive === 'failed') { phase = 'failed'; reply = { error: { code: -32000, message: 'Original session cannot be restored.' } }; }
      else if (unarchive === 'pending') phase = 'outcome_unknown';
      else {
        threads.get(String(params['threadId']))!.archived = false;
        reply = { result: { thread: { ...w.nativeThread(), id: String(params['threadId']) } } };
      }
    } else throw new Error('Unexpected native mutation: ' + method);
    const observed = receipt(String(args['operationId']), method, params, phase, phase === 'outcome_unknown' ? null : reply);
    calls.push(observed); w.f.retainOperation(observed);
    if (lostReply) { lostReply = false; throw new IvyError('deadline_exceeded', 'The original reply was lost.', 'unknown'); }
    return observed;
  });
  const current = () => w.f.first.store.read('task-board/task', w.state().task.objectId, true, true);
  const request = async (archived: boolean): Promise<TaskBoard.ArchiveRequest> => ({ action: 'archive', operationId: randomUUID(),
    taskId: w.state().task.objectId, expectedRevision: (await current()).pin.revision, archived });
  const archive = async (archived: boolean) => { const input = await request(archived); return w.f.first.archive(input, context(input.operationId)); };
  return { w, calls, threads, current, request, archive, setUnarchive: (value: typeof unarchive) => { unarchive = value; },
    loseReply: () => { lostReply = true; }, archiveFailures,
    setReadFailure: (value: string | null) => { readFailure = value; }, setPathless: () => { pathless = true; } };
}

async function continuation(f: Awaited<ReturnType<typeof lifecycle>>) {
  const original = await f.current();
  await f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: original.pin.objectId,
    expectedRevision: original.pin.revision, commentId: 'restore-follow-up', body: 'Continue the existing work.',
    requests: [], responses: [], attachments: [], replyTo: null, moveToTodo: true });
  const task = await f.current(), allocation = await new TaskBoardScheduler(f.w.f.first).allocate(task, randomUUID());
  const input: TaskBoard.ContinueRequest = { action: 'continue', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, target: allocation.target, workspace: allocation.workspace, intent: allocation.intent,
    feedback: 'Continue the existing work.', commentIds: ['restore-follow-up'] };
  return { task, input };
}

test('an uncertain automatic restore retains the original operation before claiming or admitting a Run', async t => {
  const f = await lifecycle(t), { task, input } = await continuation(f);
  f.threads.get('native-primary')!.archived = true; f.setUnarchive('pending');
  await assert.rejects(f.w.f.invoke(input), (error: unknown) =>
    error instanceof IvyError && error.code === 'task_board_native_outcome_unresolved' && error.outcome === 'unknown');
  assert.deepEqual((await f.current()).pin, task.pin);
  assert.deepEqual((await f.current()).value, task.value);
  assert.deepEqual(f.calls.map(call => call.method), ['thread/unarchive']);
  const pending = f.calls[0]!;
  f.threads.get('native-primary')!.archived = false;
  f.w.f.retainOperation({ ...pending, phase: 'succeeded', code: null,
    updatedAt: new Date(Date.parse(pending.updatedAt) + 1).toISOString(), reply: { result: { thread: f.w.nativeThread() } } });
  const resumed = await f.w.f.invoke(input, 'user', f.w.f.second), current = await f.current();
  assert.ok(resumed.run); assert.equal(current.value.workflowState, 'in_progress');
  assert.deepEqual(current.value.primaryResourceRef, task.value.primaryResourceRef);
  assert.deepEqual(current.value.commentDeliveries, task.value.commentDeliveries);
  assert.equal(current.value.attemptCount, task.value.attemptCount + 1);
  assert.deepEqual(await f.w.f.invoke(input), resumed);
  assert.deepEqual(f.calls.map(call => call.method), ['thread/unarchive']);
});

test('a refused automatic restore leaves the ticket unclaimed and preserves its original context and input', async t => {
  const f = await lifecycle(t), { task, input } = await continuation(f);
  f.threads.get('native-primary')!.archived = true; f.setUnarchive('failed');
  await assert.rejects(f.w.f.invoke(input), (error: unknown) =>
    error instanceof IvyError && error.code === 'task_board_native_restore_failed' && error.outcome === 'not_executed');
  assert.deepEqual((await f.current()).pin, task.pin);
  assert.deepEqual((await f.current()).value, task.value);
  assert.deepEqual(f.calls.map(call => call.method), ['thread/unarchive']);
});

test('ticket archive covers distinct historical contexts and replays a lost native reply exactly once', async t => {
  const f = await lifecycle(t), run = await f.w.f.first.store.read('task-board/run', f.w.state().run);
  f.threads.set('earlier-thread', { archived: false });
  await f.w.f.first.store.write('task-board/run', { ...run.value, primaryResourceRef: { ...run.value.primaryResourceRef!, nativeId: 'earlier-thread' } },
    randomUUID(), { create: { parentId: null, name: 'Earlier context fixture' } });
  const input = await f.request(true); f.loseReply();
  const archived = await f.w.f.first.archive(input, context(input.operationId));
  assert.equal(archived.effectivelyArchived, true);
  assert.ok([...f.threads.values()].every(thread => thread.archived));
  assert.deepEqual(f.calls.map(call => call.method), ['thread/archive', 'thread/archive']);
  assert.deepEqual(await f.w.f.second.archive(input, context(input.operationId)), archived);
  assert.equal(f.calls.length, 2);
});

test('ticket archive survives pruned Task snapshots and archives retained Run contexts', async t => {
  const f = await lifecycle(t), before = await f.current();
  const run = await f.w.f.first.store.read('task-board/run', f.w.state().run);
  f.threads.set('earlier-thread', { archived: false });
  await f.w.f.first.store.write('task-board/run', { ...run.value, primaryResourceRef: { ...run.value.primaryResourceRef!, nativeId: 'earlier-thread' } },
    randomUUID(), { create: { parentId: null, name: 'Earlier context with pruned Task fixture' } });
  let prunedReads = 0;
  f.w.f.interceptRead(request => {
    if (request.objectId === before.pin.objectId && request.revision !== undefined && request.revision < before.pin.revision) {
      prunedReads++;
      throw new IvyError('revision_pruned', 'Object revision was pruned by its retention policy.');
    }
  });
  assert.equal((await f.archive(true)).effectivelyArchived, true);
  assert.ok(prunedReads > 0);
  assert.ok([...f.threads.values()].every(thread => thread.archived));
  assert.deepEqual(f.calls.map(call => call.method), ['thread/archive', 'thread/archive']);
});

test('repeated archive preserves the ticket, result and history with a pathless native owner', async t => {
  const f = await lifecycle(t), before = await f.current(); f.setPathless();
  const input = await f.request(true), first = await f.w.f.first.archive(input, context(input.operationId));
  assert.deepEqual(await f.w.f.second.archive(input, context(input.operationId)), first);
  assert.equal(f.calls.length, 1);
  assert.equal((await f.archive(true)).effectivelyArchived, true);
  assert.deepEqual((await f.current()).value, before.value);
  assert.equal(f.calls.length, 2);
  await f.archive(false);
  assert.deepEqual((await f.current()).value, before.value);
  assert.equal(f.threads.get('native-primary')!.archived, false);
});

test('already archived and precisely missing native contexts do not block ticket archive', async t => {
  for (const state of ['archived', 'missing', 'unknown'] as const) {
    const f = await lifecycle(t), before = await f.current();
    if (state === 'archived') f.threads.get('native-primary')!.archived = true;
    else if (state === 'missing') f.threads.delete('native-primary');
    else f.setReadFailure('unknown thread: native-primary');
    assert.equal((await f.archive(true)).effectivelyArchived, true);
    assert.equal(f.calls.length, 0);
    assert.deepEqual((await f.current()).value, before.value);
  }
});

test('unrelated native not_found errors preserve the ticket and cannot authorize a replacement', async t => {
  for (const archived of [true, false]) {
    const f = await lifecycle(t); if (!archived) await f.archive(true);
    const before = await f.current(), callCount = f.calls.length;
    f.setReadFailure('Configuration file not found: native-primary');
    await assert.rejects(f.archive(archived), (error: unknown) => error instanceof IvyError && error.code === 'task_board_native_read_failed');
    assert.deepEqual(await f.current(), before);
    assert.equal(f.calls.length, callCount);
  }
});

test('native disappearance after the read is accepted only for the requested thread', async t => {
  for (const threadId of ['native-primary', 'another-thread']) {
    const f = await lifecycle(t), before = await f.current(), input = await f.request(true);
    f.archiveFailures.set('native-primary', 'Thread not found: ' + threadId);
    const archive = () => f.w.f.first.archive(input, context(input.operationId));
    if (threadId === 'native-primary') {
      assert.equal((await archive()).effectivelyArchived, true);
      assert.equal((await archive()).effectivelyArchived, true);
    } else {
      await assert.rejects(archive(), (error: unknown) => error instanceof IvyError && error.code === 'task_board_native_archive_failed');
      assert.equal((await f.current()).metadata.effectivelyArchived, false);
    }
    assert.equal(f.calls.length, 1);
    assert.deepEqual((await f.current()).value, before.value);
  }
});

test('partial native archive retains the failure and a fresh request completes only remaining work', async t => {
  const f = await lifecycle(t), before = await f.current();
  const run = await f.w.f.first.store.read('task-board/run', f.w.state().run);
  f.threads.set('earlier-thread', { archived: false });
  await f.w.f.first.store.write('task-board/run', { ...run.value, primaryResourceRef: { ...run.value.primaryResourceRef!, nativeId: 'earlier-thread' } },
    randomUUID(), { create: { parentId: null, name: 'Partially archived context fixture' } });
  f.archiveFailures.set('earlier-thread', 'Storage is unavailable.');
  const input = await f.request(true), reject = (error: unknown) => error instanceof IvyError && error.code === 'task_board_native_archive_failed';
  await assert.rejects(f.w.f.first.archive(input, context(input.operationId)), reject);
  assert.equal(f.threads.get('native-primary')!.archived, true);
  assert.deepEqual(await f.current(), before);
  await assert.rejects(f.w.f.second.archive(input, context(input.operationId)), reject);
  assert.equal(f.calls.length, 2);
  f.archiveFailures.clear();
  assert.equal((await f.archive(true)).effectivelyArchived, true);
  assert.deepEqual(f.calls.map(call => (call.params as { threadId: string }).threadId), ['native-primary', 'earlier-thread', 'earlier-thread']);
  assert.deepEqual((await f.current()).value, before.value);
});

test('archiving accepted delegated work preserves its parent and closes the child native context', async t => {
  const f = await lifecycle(t), parent = await f.w.f.planned('agent'), child = await f.current();
  await f.w.f.invoke({ action: 'edit', operationId: randomUUID(), taskId: child.pin.objectId, expectedRevision: child.pin.revision,
    fields: child.value.fields, parentTaskId: parent.task.objectId });
  assert.equal((await f.current()).value.workflowState, 'done');
  await f.archive(true);
  assert.equal((await f.current()).metadata.effectivelyArchived, true);
  assert.equal((await f.current()).value.parentTaskId, parent.task.objectId);
  assert.equal(f.threads.get('native-primary')?.archived, true);
  assert.equal((await f.w.f.first.store.read('task-board/task', parent.task.objectId)).metadata.effectivelyArchived, false);
  assert.deepEqual((await f.w.f.first.list({ parentTaskId: parent.task.objectId })).items, []);
  for (const until of ['idle', 'done'] as const) {
    const blocker = { taskId: child.pin.objectId, until };
    await checkBlocker(f.w.f.first.store, blocker);
    await validateBlocker(f.w.f.first.store, blocker, parent.task.objectId);
  }
  await f.w.f.first.checks.dependencies({ ...child.value.fields, dependencies: [child.pin.objectId] }, parent.task.objectId, true);
});

test('an archived cancelled child is idle but never satisfies an accepted-result dependency', async t => {
  const f = await lifecycle(t), child = await f.w.f.create({ control: 'user' });
  const cancelled = await f.w.f.invoke({ action: 'transition', operationId: randomUUID(), taskId: child.task!.objectId,
    expectedRevision: child.task!.revision, workflowState: 'cancelled', detail: 'Delegated work is no longer needed.' });
  const task = await f.w.f.first.store.read('task-board/task', cancelled.task!);
  const input = { action: 'archive' as const, operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, archived: true };
  await f.w.f.first.archive(input, context(input.operationId));
  await checkBlocker(f.w.f.first.store, { taskId: task.pin.objectId, until: 'idle' });
  const waiting = (error: unknown) => error instanceof IvyError && error.code === 'task_board_dependency_waiting';
  await assert.rejects(checkBlocker(f.w.f.first.store, { taskId: task.pin.objectId, until: 'done' }), waiting);
  await assert.rejects(f.w.f.first.checks.dependencies({ ...task.value.fields, dependencies: [task.pin.objectId] }, null, true), waiting);
});

test('restoring a ticket restores its original native context without scheduling Done work', async t => {
  const f = await lifecycle(t), before = await f.current();
  await f.archive(true); await f.archive(false);
  const restored = await f.current();
  assert.equal(restored.metadata.effectivelyArchived, false); assert.equal(restored.value.workflowState, 'done');
  assert.deepEqual(restored.value, before.value);
  assert.deepEqual(f.calls.map(call => call.method), ['thread/archive', 'thread/unarchive']);
  assert.equal(f.threads.get('native-primary')!.archived, false);
});

test('a conclusive native restore failure creates one replacement and preserves ticket material', async t => {
  const f = await lifecycle(t), before = await f.current();
  await f.archive(true); f.setUnarchive('failed'); f.loseReply();
  const input = await f.request(false), restored = await f.w.f.first.archive(input, context(input.operationId));
  const task = await f.current();
  assert.equal(restored.effectivelyArchived, false); assert.equal(task.value.workflowState, 'done');
  assert.notDeepEqual(task.value.primaryResourceRef, before.value.primaryResourceRef);
  for (const field of ['fields', 'lastRun', 'latestResult', 'acceptedReview', 'comments'] as const) assert.deepEqual(task.value[field], before.value[field]);
  assert.equal(task.value.publication, null);
  assert.deepEqual(f.calls.map(call => call.method), ['thread/archive', 'thread/unarchive', 'thread/start']);
  assert.deepEqual(await f.w.f.second.archive(input, context(input.operationId)), restored);
  assert.equal(f.calls.filter(call => call.method === 'thread/start').length, 1);
});

test('an unknown native restore outcome keeps the ticket archived until exact recovery can decide', async t => {
  const f = await lifecycle(t); await f.archive(true); f.setUnarchive('pending');
  const input = await f.request(false);
  await assert.rejects(f.w.f.first.archive(input, context(input.operationId)), (error: unknown) =>
    error instanceof IvyError && error.code === 'task_board_native_outcome_unresolved' && error.outcome === 'unknown');
  assert.equal((await f.current()).metadata.effectivelyArchived, true);
  assert.equal(f.calls.filter(call => call.method === 'thread/start').length, 0);
  const pending = f.calls.at(-1)!;
  f.w.f.retainOperation({ ...pending, phase: 'failed', code: 'native_error',
    updatedAt: new Date(Date.parse(pending.updatedAt) + 1).toISOString(), reply: { error: { code: -32000, message: 'Confirmed restore failure.' } } });
  const restored = await f.w.f.second.archive(input, context(input.operationId));
  assert.equal(restored.effectivelyArchived, false);
  assert.equal(f.calls.filter(call => call.method === 'thread/start').length, 1);
  assert.equal(f.calls.filter(call => call.method === 'thread/unarchive').length, 1);
});

test('explicit new native work queues Todo and ordinary continuation reuses the new identity', async t => {
  const f = await lifecycle(t), original = await f.current();
  const commented = await f.w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: original.pin.objectId,
    expectedRevision: original.pin.revision, commentId: 'fresh-context-input', body: 'Include this current ticket context in the new work.',
    requests: [], responses: [], attachments: [], replyTo: null });
  const before = await f.current(), input: TaskBoard.NewThreadRequest = { action: 'newThread', operationId: randomUUID(),
    taskId: original.pin.objectId, expectedRevision: commented.task!.revision, reason: 'User explicitly requests fresh context.' };
  let lost = false;
  f.w.f.intercept(async (params, result) => {
    if (!lost && params.objectId === original.pin.objectId && result.object.currentRevision > before.pin.revision) {
      lost = true; throw new IvyError('deadline_exceeded', 'The ticket publication reply was lost.', 'unknown');
    }
  });
  await assert.rejects(f.w.f.invoke(input), (error: unknown) => error instanceof IvyError && error.outcome === 'unknown');
  const outcome = await f.w.f.invoke(input, 'user', f.w.f.second), task = await f.current();
  assert.deepEqual(outcome.task, task.pin); assert.equal(task.value.workflowState, 'todo');
  assert.equal(task.value.fields.control, 'agent'); assert.equal(task.value.acceptedReview, null);
  assert.deepEqual(task.value.comments, before.value.comments); assert.deepEqual(task.value.lastRun, before.value.lastRun);
  assert.equal(f.threads.get('native-primary')!.archived, false);
  assert.deepEqual(f.calls.map(call => call.method), ['thread/start']);
  const allocation = await new TaskBoardScheduler(f.w.f.first).allocate(task, randomUUID());
  const plan = await readPlan(f.w.f.first.store, allocation.intent);
  assert.ok(JSON.stringify(plan.turnStart).includes(task.value.taskKey));
  assert.ok(JSON.stringify(plan.turnStart).includes('Read the current ticket and comments'));
  const continuation = await f.w.f.invoke({ action: 'continue', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, target: allocation.target, workspace: allocation.workspace, intent: allocation.intent, feedback: 'Continue the current ticket.', commentIds: [] });
  const run = await f.w.f.first.store.read('task-board/run', continuation.run!);
  assert.deepEqual(run.value.primaryResourceRef, task.value.primaryResourceRef);
  assert.equal(f.calls.filter(call => call.method === 'thread/start').length, 1, 'Ordinary continuation never requests another fresh context.');
});

test('active native turns reject archival and stale ticket revisions reject fresh work', async t => {
  const f = await lifecycle(t), task = await f.current();
  const input: TaskBoard.NewThreadRequest = { action: 'newThread', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, reason: 'Explicit fresh-context request.' };
  f.threads.get('native-primary')!.active = true;
  const archive = await f.request(true);
  await assert.rejects(f.w.f.first.archive(archive, context(archive.operationId)),
    (error: unknown) => error instanceof IvyError && error.code === 'task_board_native_not_idle');
  assert.equal(f.calls.length, 0);
  await assert.rejects(f.w.f.invoke({ ...input, operationId: randomUUID(), expectedRevision: 1 }),
    (error: unknown) => error instanceof IvyError && error.code === 'revision_conflict');
  assert.equal(f.calls.length, 0);
});

test('explicit fresh work does not need access to the old writer and archive includes contexts with no Run', async t => {
  const f = await lifecycle(t), task = await f.current();
  f.threads.get('native-primary')!.active = true;
  const first = await f.w.f.invoke({ action: 'newThread', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, reason: 'Use a fresh chat without touching the old writer.' });
  const second = await f.w.f.invoke({ action: 'newThread', operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: first.task!.revision, reason: 'Explicitly replace the unused fresh chat.' });
  assert.deepEqual(f.calls.map(call => call.method), ['thread/start', 'thread/start']);
  assert.equal(f.threads.size, 3);
  f.threads.get('native-primary')!.active = false;
  const archive = { action: 'archive' as const, operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: second.task!.revision, archived: true };
  await f.w.f.first.archive(archive, context(archive.operationId));
  assert.ok([...f.threads.values()].every(thread => thread.archived));
  assert.equal(f.calls.filter(call => call.method === 'thread/archive').length, 3);
});

test('an explicit new-context request also starts a previously manual ticket with no native task', async t => {
  const f = await lifecycle(t), created = await f.w.f.create({ control: 'user' });
  const started = await f.w.f.invoke({ action: 'newThread', operationId: randomUUID(), taskId: created.task!.objectId,
    expectedRevision: created.task!.revision, reason: 'Ask Codex to work on this manual ticket.' });
  const task = await f.w.f.first.store.read('task-board/task', started.task!);
  assert.equal(task.value.workflowState, 'todo'); assert.equal(task.value.fields.control, 'agent');
  assert.ok(task.value.primaryResourceRef!.nativeId.startsWith('fresh-'));
  assert.deepEqual(f.calls.map(call => call.method), ['thread/start']);
});
