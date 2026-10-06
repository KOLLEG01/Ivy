import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { digest, hashJson } from "../packages/contracts/src/canonical.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import type { TaskBoard } from "../packages/contracts/src/generated.js";
import { context } from "./fixtures/task-board.js";
import { nativeWorkflow } from "./fixtures/task-board-native.js";
import { TaskBoardScheduler } from "../services/task-board/src/runtime/scheduler.js";
import { syncNativeComments } from '../services/task-board/src/runtime/native-comments.js';
import type { Agent } from '../packages/contracts/src/generated.js';
import { catalogFor } from '../services/task-board/src/runtime/evidence.js';
import { TaskBoardReconciler } from '../services/task-board/src/runtime/reconciler.js';

const code = (expected: string) => (error: unknown) =>
  error instanceof IvyError && error.code === expected;

for (const failureAt of ["task", "receipt"] as const) {
  test(`unfinished native publication retains recovery evidence beyond seven days (${failureAt})`, async t => {
    const w = await nativeWorkflow(t); await w.running();
    const request = await w.snapshot("completed", "Keep this result in the ticket.");
    const store = w.f.first.store, client = store.client, original = client.request.bind(client);
    const finish = w.f.first.operations.finish.bind(w.f.first.operations);
    if (failureAt === "task") client.request = (async (method, params, options) => {
      if (method === "objects.write" && "objectId" in params && params.objectId === request.taskId &&
          "content" in params && params.content.encoding === "json" &&
          (params.content.value as TaskBoard.Task).publication === null)
        throw new IvyError("synthetic_write_failure", "Final Task write unavailable.", "unknown");
      return original(method, params, options);
    }) as typeof client.request;
    else w.f.first.operations.finish = async () => { throw new IvyError("synthetic_write_failure", "Receipt unavailable.", "unknown"); };
    try { await assert.rejects(w.f.first.nativeUpdate(request), code("synthetic_write_failure")); }
    finally { client.request = original; w.f.first.operations.finish = finish; }
    const operation = await w.f.first.operations.find(w.f.settings.principalId, request.operationId);
    assert.equal(operation!.value.phase, "needs_attention");
    w.f.kernel.retention.collect(); w.f.kernel.retention.collect();
    await store.collectLocal(Date.now() + 8 * 86400000);
    const outcome = await w.f.first.recoverOperation(operation!.pin.objectId);
    const task = await store.read("task-board/task", outcome.task!);
    assert.equal(task.value.publication, null);
    assert.equal(task.value.workflowState, "review");
    assert.equal((await store.read("task-board/result", outcome.result!)).value.content.summary, "Keep this result in the ticket.");
    await store.collectLocal(Date.now() + 16 * 86400000);
    if (request.change.kind === "snapshot")
      assert.equal(store.localKey(request.change.evidence.object.objectId), null);
    assert.equal((await store.read("task-board/task", outcome.task!)).value.latestResult?.objectId, outcome.result!.objectId);
  });
}

for (const bound of ["count", "bytes"] as const) {
  test(`native completion preserves its Result when the extra comment exceeds ${bound}`, async t => {
    const w = await nativeWorkflow(t); await w.running();
    const store = w.f.first.store, task = await store.read("task-board/task", w.state().task);
    const comments: TaskBoard.Comment[] = Array.from({ length: bound === "count" ? 512 : 15 }, (_, index) => ({
      commentId: randomUUID(), sequence: index + 1,
      author: { principalId: "user", source: "user", ...w.f.first.owner }, authorKind: "user",
      body: bound === "count" ? "Retained comment" : "x".repeat(65536), requests: [], responses: [], delivery: null,
      attachments: [], run: null, turnId: null, replyTo: null, createdAt: new Date().toISOString(), operationId: randomUUID(),
    }));
    await store.write("task-board/task", { ...task.value, comments }, randomUUID(), { objectId: task.pin.objectId, expectedRevision: task.pin.revision });
    await w.refresh();
    const summary = bound === "count" ? "Complete final result" : "r".repeat(65536);
    const outcome = await w.f.first.nativeUpdate(await w.snapshot("completed", summary));
    const saved = await store.read("task-board/task", outcome.task!);
    assert.equal(saved.value.publication, null);
    assert.equal(saved.value.workflowState, "review");
    assert.deepEqual(saved.value.comments, comments);
    assert.equal((await store.read("task-board/result", outcome.result!)).value.content.summary, summary);
    assert.ok(outcome.history);
  });
}

test("full native snapshots retain uncertain execution in progress without reporting it as running", async (t) => {
  const w = await nativeWorkflow(t);
  await w.running();
  await w.f.first.nativeUpdate(await w.snapshot("inProgress"));
  await w.refresh();
  const run = await w.f.first.store.read("task-board/run", w.state().run);
  const task = await w.f.first.store.read("task-board/task", w.state().task);
  assert.equal(run.value.phase, "outcome_unknown");
  assert.equal(task.value.workflowState, "in_progress");
  assert.equal(task.value.waiting, null);
  assert.equal(w.f.first.condition(task.value).state, "recovering");
  assert.equal(task.value.claim?.run?.objectId, run.pin.objectId);
  assert.equal(task.value.attemptCount, 1);
  assert.equal(task.value.latestResult, null);
  assert.equal(
    run.value.externalOutcome.code,
    "native_active_owner_unconfirmed",
  );
});

test("native publication saves a bounded result for review and retains the original start receipt", async (t) => {
  const w = await nativeWorkflow(t);
  await w.running();
  for (const flag of ["waitingOnUserInput", "waitingOnApproval"]) {
    await w.f.first.nativeUpdate(await w.snapshot("inProgress", undefined, { type: "active", activeFlags: [flag] }));
    await w.refresh();
    const waiting = await w.f.first.store.read("task-board/task", w.state().task);
    assert.equal(waiting.value.workflowState, "waiting");
    assert.equal(waiting.value.waiting?.reason, "user");
    assert.equal(w.f.first.condition(waiting.value).state, "needs_user");
  }
  const activeRun = await w.f.first.store.read("task-board/run", w.state().run);
  const nativeCall = activeRun.value.calls.turn!;
  const request = await w.snapshot(
    "completed",
    "Exact native material retained for user acceptance.",
  );
  const recovered = await w.f.first.nativeUpdate(request);
  assert.ok(recovered.result);
  const task = await w.f.first.store.read("task-board/task", recovered.task!),
    result = await w.f.first.store.read("task-board/result", recovered.result!);
  assert.equal(task.value.workflowState, "review");
  assert.equal(task.value.claim, null);
  assert.equal(task.value.publication, null);
  assert.equal(task.value.comments.at(-1)?.authorKind, "agent");
  assert.equal(task.value.comments.at(-1)?.body, "Exact native material retained for user acceptance.");
  assert.equal(task.value.comments.at(-1)?.delivery, null);
  assert.equal(
    result.value.content.summary,
    "Exact native material retained for user acceptance.",
  );
  assert.equal(result.value.actor.source, "native");
  const finalRun = await w.f.first.store.read("task-board/run", recovered.run!);
  assert.deepEqual(finalRun.value.calls, { thread: null, turn: null, interrupt: null });
  assert.equal(finalRun.value.externalOutcome.evidence, null);
  assert.deepEqual(
    await w.f.invoke(w.startRequest, "user", w.f.first),
    w.started,
  );
  assert.deepEqual(await w.f.first.nativeUpdate(request), recovered);
  assert.equal((await w.f.first.store.page("task-board/result")).items.length, 1);
  const accepted = await w.f.invoke({
    action: "review",
    operationId: randomUUID(),
    taskId: recovered.task!.objectId,
    expectedRevision: recovered.task!.revision,
    result: recovered.result,
    acceptance: "Explicit user acceptance of the saved native result.",
  });
  assert.equal(
    (await w.f.first.store.read("task-board/task", accepted.task!)).value
      .workflowState,
    "done",
  );
  await w.f.first.store.collectLocal(Date.parse(finalRun.value.finishedAt!) + 6 * 86_400_000);
  await w.f.first.store.read("task-board/native-call", nativeCall, false);
  await w.f.first.store.collectLocal(Date.parse(finalRun.value.finishedAt!) + 8 * 86_400_000);
  await assert.rejects(w.f.first.store.read("task-board/native-call", nativeCall, false), code("task_board_scope_mismatch"));
  assert.equal((await w.f.first.store.read("task-board/result", recovered.result!)).value.content.summary,
    "Exact native material retained for user acceptance.");
});

test("native publication recovers an ambiguous first Task reservation without duplicating its result", async (t) => {
  const w = await nativeWorkflow(t);
  await w.running();
  const request = await w.snapshot(
    "completed",
    "Recover this exact native result once.",
  );
  let loseReply = true;
  w.f.intercept((params) => {
    if (
      loseReply &&
      !("create" in params) &&
      params.objectId === request.taskId
    ) {
      loseReply = false;
      throw new IvyError(
        "synthetic_native_task_response_loss",
        "Synthetic native Task response loss.",
        "unknown",
      );
    }
  });
  await assert.rejects(
    w.f.first.nativeUpdate(request),
    code("synthetic_native_task_response_loss"),
  );
  w.f.intercept(null);
  const recovered = await w.f.second.recoverPublication();
  assert.ok(recovered?.result);
  assert.equal(
    (await w.f.second.store.page("task-board/result")).items.length,
    1,
  );
  assert.equal(
    (await w.f.second.store.read("task-board/task", recovered!.task!)).value
      .publication,
    null,
  );
});

test("native publication retains owner-observed repository reachability provenance", async (t) => {
  const w = await nativeWorkflow(t);
  await w.running();
  const repository = {
    name: "ivy",
    branch: "main",
    commit: "0123456789abcdef",
    dirty: false,
    originName: "origin",
    originUrl: "https://example.test/ivy.git",
    originState: "confirmed" as const,
    remoteRef: "refs/heads/main",
    observedAt: new Date().toISOString(),
    limitation: null,
  };
  const workspace: TaskBoard.WorkspaceResolution = {
    ...w.planned.workspace,
    project: {
      namespace: "codex",
      kind: "project",
      serviceNodeId: "native-agent",
      nativeId: "ivy",
    },
    repository,
  };
  w.f.nativeTools(async (call) => {
    assert.equal(call.definition.name, "resolveWorkspace");
    assert.equal(
      (call.arguments as { verifyOrigin?: boolean }).verifyOrigin,
      true,
    );
    return { ...workspace, nativeProjectId: "fixture-codex-project" };
  });
  const outcome = await w.f.first.nativeUpdate(
    await w.snapshot("completed", "Published Git result."),
  );
  const result = await w.f.first.store.read("task-board/result", outcome.result!);
  const run = await w.f.first.store.read("task-board/run", outcome.run!);
  assert.equal("nativeProjectId" in run.value.workspace, false);
  assert.deepEqual(result.value.content.repositoryResult, {
    hostId: "fixture-host",
    serviceNodeId: "native-agent",
    project: workspace.project,
    repositoryName: "ivy",
    branch: "main",
    commit: "0123456789abcdef",
    originName: "origin",
    originUrl: "https://example.test/ivy.git",
    originState: "confirmed",
    remoteRef: "refs/heads/main",
    observedAt: repository.observedAt,
    limitation: null,
  });
});

test("interrupt receipts preserve cancellation intent; native success racing cancel still becomes review", async (t) => {
  const w = await nativeWorkflow(t);
  await w.running();
  await w.cancel();
  const interrupt = await w.prepare("interrupt");
  await w.observe("interrupt", interrupt, { result: {} });
  let run = await w.f.first.store.read("task-board/run", w.state().run);
  assert.equal(run.value.phase, "cancel_requested");
  assert.ok(run.value.cancellation);
  assert.equal(
    (await w.f.first.store.read("task-board/task", w.state().task)).value
      .workflowState,
    "in_progress",
  );
  const result = await w.f.first.nativeUpdate(await w.snapshot("completed"));
  run = await w.f.first.store.read("task-board/run", result.run!);
  assert.equal(run.value.phase, "completed");
  assert.ok(run.value.cancellation);
  assert.equal(run.value.externalOutcome.state, "succeeded");
  assert.equal(
    (await w.f.first.store.read("task-board/task", result.task!)).value
      .workflowState,
    "review",
  );
});

test("a user comment arriving during active work schedules one continuation on the same primary", async (t) => {
  const w = await nativeWorkflow(t);
  await w.running();
  await w.refresh();
  const before = await w.f.first.store.read("task-board/task", w.state().task),
    commentId = randomUUID();
  await w.f.invoke({
    action: "comment",
    operationId: randomUUID(),
    taskId: before.pin.objectId,
    expectedRevision: before.pin.revision,
    commentId,
    body: "Apply this queued correction exactly once.",
    requests: [],
    responses: [],
    attachments: [],
    replyTo: null,
  });
  await w.refresh();
  const completion = await w.f.first.nativeUpdate(
    await w.snapshot(
      "completed",
      "Initial turn completed before queued feedback.",
    ),
  );
  const queued = await w.f.first.store.read("task-board/task", completion.task!);
  assert.equal(queued.value.workflowState, "todo");
  assert.equal(
    queued.value.commentDeliveries.find(
      (value) => value.commentId === commentId,
    )?.state,
    "queued",
  );
  const continued = await new TaskBoardScheduler(w.f.first).task(
    queued.pin.objectId,
  );
  assert.ok(continued?.run);
  const run = await w.f.first.store.read("task-board/run", continued.run);
  assert.equal(run.value.originalRequest.action, "continue");
  if (run.value.originalRequest.action === "continue") {
    const feedback = run.value.originalRequest.feedback;
    assert.deepEqual(run.value.originalRequest.commentIds, [commentId]);
    assert.match(feedback, /TaskBoard TASK-/);
    assert.doesNotMatch(feedback, /Apply this queued correction/);
    assert.equal(
      (
        run.value.plan.turnStart.input as { type?: string; text?: string }[]
      ).filter((item) => item.type === "text" && item.text === feedback).length,
      1,
    );
  }
  assert.deepEqual(
    run.value.primaryResourceRef,
    queued.value.primaryResourceRef,
  );
});

test("a queued comment does not restart an attempt that failed before its first turn", async (t) => {
  const w = await nativeWorkflow(t);
  const thread = await w.prepare("thread");
  const before = await w.f.first.store.read("task-board/task", w.state().task);
  const commentId = randomUUID();
  await w.f.invoke({ action: "comment", operationId: randomUUID(), taskId: before.pin.objectId,
    expectedRevision: before.pin.revision, commentId, body: "A queued correction.", requests: [], responses: [],
    attachments: [], replyTo: null });
  await w.refresh();
  await w.observe("thread", thread, null, "failed");
  const task = await w.f.first.store.read("task-board/task", w.state().task);
  assert.equal(task.value.workflowState, "waiting");
  assert.equal(task.value.claim, null);
  assert.equal(task.value.commentDeliveries.find(value => value.commentId === commentId)?.state, "queued");
  assert.equal(await new TaskBoardScheduler(w.f.first).task(task.pin.objectId), null);
});

for (const state of ['done', 'cancelled', 'backlog'] as const) test(`transitioning a review task to ${state} releases its workspace reservation`, async (t) => {
  const w = await nativeWorkflow(t, "0.154.0", true);
  await w.running();
  const completed = await w.f.first.nativeUpdate(await w.snapshot("completed"));
  const task = await w.f.first.store.read("task-board/task", completed.task!);
  const path = w.planned.workspace.intendedPath;
  w.f.first.store.reserveWorkspace("fixture-host", path, task.pin.objectId, "legacy-reservation");
  assert.equal(w.f.first.store.reserveWorkspace("fixture-host", path, "other-task", "other-operation"), task.pin.objectId);
  const cancelled = await w.f.invoke({ action: "transition", operationId: randomUUID(), taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, workflowState: state, detail: "Finished using this workspace." });
  assert.equal((await w.f.first.store.read("task-board/task", cancelled.task!)).value.workflowState, state);
  assert.equal(w.f.first.store.reserveWorkspace("fixture-host", path, "other-task", "other-operation"), null);
});

for (const state of ['review', 'done', 'waiting'] as const) test(`periodic scheduling reclaims a stale shared reservation from ${state} without editing either ticket`, async t => {
  const w = await nativeWorkflow(t, '0.154.0', true); await w.running();
  const store = w.f.first.store, host = w.planned.target.hostId, path = w.planned.workspace.canonicalCwd;
  const key = 'directory:' + path, owner = w.state().task.objectId;
  const blocked = await w.f.planned('agent', { kind: 'directory_path', path });
  await new TaskBoardScheduler(w.f.first).task(blocked.task.objectId);
  assert.equal((await store.read('task-board/task', blocked.task.objectId)).value.waiting?.reason, 'workspace');
  await w.update({ kind: 'unavailable', reason: 'host', code: 'service_unavailable', detail: 'Owner temporarily unavailable.' });
  assert.equal(await store.workspaceOwner(host, key, blocked.task.objectId), owner, 'Unknown native work must keep its reservation.');
  const completed = await w.f.first.nativeUpdate(await w.snapshot(state === 'waiting' ? 'failed' : 'completed'));
  let task = await store.read('task-board/task', completed.task!);
  assert.equal(store.reserveWorkspace(host, key, 'probe', 'probe'), null, 'Completion must release shared admission immediately.');
  store.releaseWorkspace('probe');
  if (state === 'done') {
    const done = await w.f.invoke({ action: 'transition', operationId: randomUUID(), taskId: owner,
      expectedRevision: task.pin.revision, workflowState: 'done', detail: null });
    task = await store.read('task-board/task', done.task!);
  }
  assert.equal(task.value.workflowState, state);
  // Reproduce a retained reservation from an older service or an interrupted cleanup.
  store.reserveWorkspace(host, key, owner, 'retained-reservation');
  const request = store.client.request.bind(store.client);
  store.client.request = (async (method, params, options) => {
    if (method === 'objects.read' && 'objectId' in params && params.objectId === completed.run!.objectId)
      throw new IvyError('not_found', 'Run evidence is temporarily unavailable.');
    return request(method, params, options);
  }) as typeof store.client.request;
  try {
    await assert.rejects(store.workspaceOwner(host, key, blocked.task.objectId), code('not_found'));
    assert.equal(store.reserveWorkspace(host, key, 'probe', 'probe'), owner, 'Missing Run evidence cannot release admission.');
  } finally { store.client.request = request; }
  const pending = await w.f.first.operations.begin({ action: 'edit', operationId: randomUUID(), taskId: owner,
    expectedRevision: task.pin.revision, fields: task.value.fields }, { ...w.f.first.owner, principalId: w.f.settings.principalId, source: 'user' });
  await w.f.first.coordination.acquire(pending);
  assert.equal(await store.workspaceOwner(host, key, blocked.task.objectId), owner, 'An unpublished admission must remain protected.');
  await w.f.first.operations.failure(pending.pin.objectId, new IvyError('fixture_refused', 'No publication was performed.'), false);
  await w.f.first.coordination.release(pending.pin.objectId);
  const runtime = new TaskBoardReconciler(w.f.second);
  for (let pass = 0; pass < 12 && !(await store.read('task-board/task', blocked.task.objectId)).value.claim; pass++) {
    await runtime.tick(); await runtime.settled();
  }
  const resumed = await store.read('task-board/task', blocked.task.objectId);
  assert.ok(resumed.value.claim); assert.equal(resumed.value.attemptCount, 1);
  assert.equal(resumed.value.waiting, null); assert.equal(resumed.value.blocker, undefined);
  assert.deepEqual((await store.read('task-board/task', owner)).pin, task.pin, 'Reclaiming local admission must not revise the completed ticket.');
  await runtime.stop();
});

test("archiving a review task releases its workspace reservation", async (t) => {
  const w = await nativeWorkflow(t, "0.154.0", true);
  await w.running();
  const completed = await w.f.first.nativeUpdate(await w.snapshot("completed"));
  const task = await w.f.first.store.read("task-board/task", completed.task!);
  const path = w.planned.workspace.intendedPath;
  w.f.first.store.reserveWorkspace("fixture-host", path, task.pin.objectId, "legacy-reservation");
  assert.equal(w.f.first.store.reserveWorkspace("fixture-host", path, "other-task", "other-operation"), task.pin.objectId);
  w.f.nativeTools(async call => {
    const args = call.arguments as { operationId: string; method: string; params: { threadId: string } };
    if (call.definition.name === 'operation') throw new IvyError('not_found', 'No observed operation.', 'not_executed', {
      kind: 'agent_operation_absent', operationId: args.operationId, serviceNodeId: 'native-agent', epoch: 'fixture-epoch' });
    if (call.definition.name === 'read') {
      const native = catalogFor('0.154.0').catalog;
      return { schemaVersion: 1, observationId: randomUUID(), callerPrincipalId: w.f.settings.principalId,
        serviceNodeId: 'native-agent', nativeVersion: '0.154.0', nativeExecutableHash: native.nativeExecutableHash,
        catalogHash: hashJson(native), epoch: 'fixture-epoch', requestId: randomUUID(), observedAt: new Date().toISOString(),
        method: args.method, params: args.params, requestHash: hashJson({ method: args.method, params: args.params }),
        reply: { result: { thread: w.nativeThread() } } };
    }
    if (call.definition.name === 'invoke') {
      assert.equal(args.method, 'thread/archive'); assert.equal(args.params.threadId, 'native-primary');
      const observed = w.ownerOperation(args.method, args.params, { result: {} }, args.operationId);
      w.f.retainOperation(observed); return observed;
    }
    return w.f.nativeManagement(call);
  });
  const archivedId = randomUUID();
  await w.f.first.archive({ action: "archive", operationId: archivedId, taskId: task.pin.objectId,
    expectedRevision: task.pin.revision, archived: true }, context(archivedId));
  assert.equal(w.f.first.store.reserveWorkspace("fixture-host", path, "other-task", "other-operation"), null);
});

test("a deleted task cannot retain a workspace reservation", async (t) => {
  const w = await nativeWorkflow(t, "0.154.0", true);
  await w.running();
  const completed = await w.f.first.nativeUpdate(await w.snapshot("completed"));
  const task = await w.f.first.store.read("task-board/task", completed.task!);
  const path = w.planned.workspace.intendedPath;
  w.f.first.store.reserveWorkspace("fixture-host", path, task.pin.objectId, "legacy-reservation");
  assert.equal(w.f.first.store.reserveWorkspace("fixture-host", path, "other-task", "other-operation"), task.pin.objectId);
  await w.f.first.store.client.request("objects.delete", {
    objectId: task.pin.objectId,
    expectedRevision: task.pin.revision,
    mutationId: await w.f.first.store.preparedMutationId("delete-completed-task"),
  });
  assert.equal(await w.f.first.store.reserveAvailableWorkspace("fixture-host", path, "other-task", "other-operation"), null);
});

test("a task archived directly in Hive cannot retain a workspace reservation", async (t) => {
  const w = await nativeWorkflow(t, "0.154.0", true);
  await w.running();
  const completed = await w.f.first.nativeUpdate(await w.snapshot("completed"));
  const task = await w.f.first.store.read("task-board/task", completed.task!);
  const path = w.planned.workspace.intendedPath;
  w.f.first.store.reserveWorkspace("fixture-host", path, task.pin.objectId, "legacy-reservation");
  await w.f.first.store.client.request("objects.archive", {
    objectId: task.pin.objectId,
    archived: true,
    mutationId: await w.f.first.store.preparedMutationId("archive-completed-task"),
  });
  assert.equal(await w.f.first.store.reserveAvailableWorkspace("fixture-host", path, "other-task", "other-operation"), null);
});

test("unknown interruption remains in progress until the owner confirms the exact turn was interrupted", async (t) => {
  const w = await nativeWorkflow(t);
  await w.running();
  await w.cancel();
  const interrupt = await w.prepare("interrupt");
  await w.observe("interrupt", interrupt, null, "outcome_unknown");
  const unknown = await w.f.first.store.read("task-board/task", w.state().task);
  assert.equal(unknown.value.workflowState, "in_progress");
  assert.equal(unknown.value.waiting, null);
  assert.equal(w.f.first.condition(unknown.value).state, "recovering");
  const result = await w.f.first.nativeUpdate(await w.snapshot("interrupted"));
  const run = await w.f.first.store.read("task-board/run", result.run!);
  assert.equal(run.value.phase, "cancelled");
  assert.equal(run.value.externalOutcome.state, "cancelled");
  assert.equal(
    (await w.f.first.store.read("task-board/task", result.task!)).value
      .workflowState,
    "cancelled",
  );
  assert.ok(result.result);
});

test("native final material from a reassigned attempt cannot replace a newer claim or primary", async (t) => {
  const w = await nativeWorkflow(t);
  await w.running();
  const old = w.state();
  const task = await w.f.first.store.read("task-board/task", old.task),
    reassigned = await w.f.invoke({
      action: "reassign",
      operationId: randomUUID(),
      taskId: old.task.objectId,
      expectedRevision: old.task.revision,
      executionRequirement: task.value.fields.executionRequirement,
      workspaceRequirement: task.value.fields.workspaceRequirement,
      reason:
        "Acknowledge the unresolved prior attempt and start a separate one.",
      previousRun: old.run,
      acknowledgeUnresolved: true,
    });
  const next = await w.f.invoke({
    action: "start",
    operationId: randomUUID(),
    taskId: old.task.objectId,
    expectedRevision: reassigned.task!.revision,
    intent: w.planned.plan,
    target: w.planned.target,
    workspace: w.planned.workspace,
    commentIds: [],
  });
  await w.refresh();
  const result = await w.f.first.nativeUpdate(
    await w.snapshot("completed", "Earlier attempt completed later."),
  );
  const current = await w.f.first.store.read("task-board/task", result.task!),
    oldRun = await w.f.first.store.read("task-board/run", old.run.objectId);
  assert.equal(oldRun.value.phase, "completed");
  assert.ok(oldRun.value.result);
  assert.equal(current.value.claim?.run?.objectId, next.run!.objectId);
  assert.equal(current.value.attemptCount, 2);
  assert.equal(current.value.primaryResourceRef, null);
  assert.equal(current.value.latestResult, null);
  assert.equal(current.value.workflowState, "in_progress");
  assert.equal(current.value.lastRun?.objectId, next.run!.objectId);
});

test("owner-only observations refuse fabricated turn identity and close a cancelled unstarted Run without dispatch", async (t) => {
  const w = await nativeWorkflow(t),
    fake: TaskBoard.Artifact = {
      object: w.state().task,
      label: "Not native evidence",
      mediaType: "application/json",
      contentHash: digest("wrong"),
    };
  const request = w.request({
    kind: "snapshot",
    evidence: fake,
    nativeOperationId: "invented",
    notificationCursor: 0,
  });
  await assert.rejects(
    w.f.first.invoke(
      request as unknown as TaskBoard.ActionInput,
      context(request.operationId),
    ),
  );
  await assert.rejects(
    w.f.first.nativeUpdate(request),
    code("task_board_turn_mismatch"),
  );
  await w.cancel();
  const cancelled = await w.update({ kind: "cancelUnstarted" });
  assert.equal(
    (await w.f.first.store.read("task-board/run", cancelled.run!)).value.phase,
    "cancelled",
  );
  assert.equal(
    (await w.f.first.store.page("task-board/native-call")).items.length,
    0,
  );
});

test("pending native operations cannot be treated as thread success or replaced by a new call", async (t) => {
  const w = await nativeWorkflow(t),
    thread = await w.prepare("thread");
  await w.observe("thread", thread, null, "outcome_unknown");
  const task = await w.f.first.store.read("task-board/task", w.state().task);
  assert.equal(task.value.workflowState, "in_progress");
  assert.equal(task.value.waiting, null);
  assert.equal(w.f.first.condition(task.value).state, "recovering");
  assert.equal(task.value.primaryResourceRef, null);
  await assert.rejects(w.prepare("turn"), code("task_board_primary_required"));
  await assert.rejects(
    w.update({ kind: "attachCall", slot: "thread", call: thread }),
    code("task_board_claim_inactive"),
  );
  assert.equal(
    (await w.f.first.store.read("task-board/task", w.state().task.objectId)).value
      .attemptCount,
    1,
  );
});

for (const purpose of ['question', 'handoff'] as const) {
  test(`a worker ${purpose} does not enqueue itself and controls the terminal handoff`, async t => {
    const w = await nativeWorkflow(t); await w.running();
    const state = w.state();
    await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: state.task.objectId,
      expectedRevision: state.task.revision, commentId: 'worker-output', authorKind: 'agent', purpose,
      body: purpose === 'question' ? 'Which option should I use?' : 'Delivered the specification.',
      requests: [], responses: [], attachments: [], replyTo: null });
    await w.refresh();
    const outcome = await w.f.first.nativeUpdate(await w.snapshot('completed', 'Native final message.'));
    const task = (await w.f.first.store.read('task-board/task', outcome.task!)).value;
    assert.equal(task.workflowState, purpose === 'question' ? 'waiting' : 'review');
    assert.equal(task.comments.length, 1);
    assert.equal(task.comments[0]?.authorKind, 'agent');
    assert.equal(task.commentDeliveries.length, 0);
    if (purpose === 'handoff') {
      const done = await w.f.invoke({ action: 'transition', operationId: randomUUID(), taskId: outcome.task!.objectId,
        expectedRevision: outcome.task!.revision, workflowState: 'done', detail: null });
      assert.equal((await w.f.first.store.read('task-board/task', done.task!)).value.workflowState, 'done');
      const note = await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: done.task!.objectId,
        expectedRevision: done.task!.revision, commentId: 'done-note', body: 'For the record.',
        requests: [], responses: [], attachments: [], replyTo: null });
      const after = (await w.f.first.store.read('task-board/task', note.task!)).value;
      assert.equal(after.workflowState, 'done'); assert.equal(after.commentDeliveries.length, 0);
      const reopened = await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: note.task!.objectId,
        expectedRevision: note.task!.revision, commentId: 'reopen-note', body: 'Please revise.', moveToTodo: true,
        requests: [], responses: [], attachments: [], replyTo: null });
      assert.equal((await w.f.first.store.read('task-board/task', reopened.task!)).value.workflowState, 'todo');
    }
  });
}
test('failed execution waits without a successful completion notification', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const outcome = await w.f.first.nativeUpdate(await w.snapshot('failed', 'Execution failed.'));
  const task = (await w.f.first.store.read('task-board/task', outcome.task!)).value;
  assert.equal(task.workflowState, 'waiting'); assert.equal(task.comments.at(-1)?.delivery, null);
});

test('native requests and answers are mirrored once with their exact identities', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const run = (await w.f.first.store.read('task-board/run', w.state().run)).value;
  const now = new Date().toISOString();
  const input: Agent.PendingInput = { identity: { serviceNodeId: run.target.serviceNodeId, epoch: run.nativeEpoch!, requestId: 42 },
    method: 'item/tool/requestUserInput', params: { questions: [{ id: 'choice', question: 'Which option?', isSecret: false, options: [] }] },
    observedAt: now, updatedAt: now, threadId: run.primaryResourceRef!.nativeId, turnId: run.turnId,
    state: 'pending', answerOperationId: null, answerCallerPrincipalId: null, reply: null, code: null };
  w.f.nativeTools(async call => {
    assert.equal(call.definition.name, 'inputs');
    return { epoch: input.identity.epoch, items: [input], truncated: false };
  });
  await syncNativeComments(w.f.first, w.state().run.objectId);
  await syncNativeComments(w.f.first, w.state().run.objectId);
  let task = (await w.f.first.store.read('task-board/task', w.state().task.objectId)).value;
  assert.equal(task.comments.length, 1); assert.deepEqual(task.comments[0]?.nativeInput?.identity, input.identity);
  input.state = 'answered'; input.reply = { result: { answers: { choice: { answers: ['First option'] } } } };
  input.answerOperationId = 'native-answer'; input.answerCallerPrincipalId = 'user';
  await syncNativeComments(w.f.first, w.state().run.objectId);
  await syncNativeComments(w.f.first, w.state().run.objectId);
  task = (await w.f.first.store.read('task-board/task', w.state().task.objectId)).value;
  assert.equal(task.comments.length, 2); assert.equal(task.comments[1]?.replyTo, task.comments[0]?.commentId);
  assert.equal(task.commentDeliveries.length, 0);
  await w.refresh();
  const outcome = await w.f.first.nativeUpdate(await w.snapshot('completed', 'Finished after the response.'));
  assert.equal((await w.f.first.store.read('task-board/task', outcome.task!)).value.workflowState, 'review');
});

test('ordinary worker updates stay in the ticket and handoff notification is published once', async t => {
  const w = await nativeWorkflow(t); await w.running();
  const browserNotices = () => w.f.kernel.store.all("SELECT payload FROM events WHERE topic='task-board.browser-notification'");
  const store = w.f.first.store, before = await store.read('task-board/task', w.state().task);
  await store.write('task-board/task', { ...before.value, fields: { ...before.value.fields, userContact: 'chat' } }, randomUUID(),
    { objectId: before.pin.objectId, expectedRevision: before.pin.revision });
  await w.refresh();
  for (const purpose of ['update', 'handoff'] as const) {
    await w.f.invoke({ action: 'comment', operationId: randomUUID(), taskId: w.state().task.objectId,
      expectedRevision: w.state().task.revision, commentId: purpose, authorKind: 'agent', purpose,
      body: purpose === 'handoff' ? 'Delivered.' : 'Useful progress.', requests: [], responses: [], attachments: [], replyTo: null });
    await w.refresh();
    assert.equal(browserNotices().length, purpose === 'handoff' ? 1 : 0);
  }
  assert.equal((await store.page('task-board/delivery')).items.length, 0);
  const request = await w.snapshot('completed', 'Delivered.');
  const outcome = await w.f.first.nativeUpdate(request);
  assert.deepEqual(await w.f.first.nativeUpdate(request), outcome);
  const task = (await store.read('task-board/task', outcome.task!)).value;
  assert.equal(task.workflowState, 'review'); assert.equal(task.comments.length, 2);
  assert.ok(task.comments.at(-1)?.delivery);
  assert.equal((await store.page('task-board/delivery')).items.length, 1);
  assert.equal(browserNotices().length, 1);
  assert.equal(JSON.parse(String(browserNotices()[0]!['payload'])).body, 'Delivered.');
});
