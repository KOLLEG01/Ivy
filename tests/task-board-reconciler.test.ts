import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { IvyError } from "../packages/contracts/src/errors.js";
import { TaskBoardReconciler } from "../services/task-board/src/runtime/reconciler.js";
import { fields, taskBoardFixture } from "./fixtures/task-board.js";
import { nativeWorkflow } from './fixtures/task-board-native.js';

test('an unavailable input owner does not prevent Run recovery', async t => {
  const w = await nativeWorkflow(t); await w.running();
  w.f.nativeTools(async () => { throw new IvyError('service_unavailable', 'The owner is offline.'); });
  const runtime = new TaskBoardReconciler(w.f.first);
  let reconciled = false;
  runtime.native.drain = async () => { reconciled = true; return true; };
  runtime.signals.step = async () => null;
  await (runtime as unknown as { process: (lane: 'runs', id: string) => Promise<void> }).process('runs', w.state().run.objectId);
  assert.equal(reconciled, true);
  assert.ok(runtime.diagnostics.some(value => JSON.stringify(value.resource) === JSON.stringify({ objectId: 'inputs:' + w.state().run.objectId })));
});

test('periodic scans give recovery a turn even when the same waiting Task is always schedulable', async t => {
  const w = await nativeWorkflow(t); await w.running();
  await w.update({ kind: 'unavailable', reason: 'host', code: 'service_unavailable', detail: 'Provider connection is closed.' });
  const runtime = new TaskBoardReconciler(w.f.first);
  let recovered = 0, scheduled = 0;
  runtime.native.drain = async () => { recovered++; return true; };
  runtime.signals.step = async () => null;
  runtime.scheduler.task = async () => { scheduled++; return null; };
  while (!runtime.recovered) await step(runtime);
  recovered = 0;
  for (let pass = 0; pass < 12; pass++) await step(runtime);
  assert.ok(scheduled > 0);
  assert.ok(recovered >= 3, 'Scheduling the waiting ticket must not starve its original Run recovery.');
});

const step = async (runtime: TaskBoardReconciler) => { await runtime.tick(); await runtime.settled(); };

test('recovery errors stay visible through backoff and clear only after a successful pass', async t => {
  const w = await nativeWorkflow(t), runtime = new TaskBoardReconciler(w.f.first);
  let now = Date.now(), attempts = 0;
  t.mock.method(Date, 'now', () => now);
  runtime.native.step = async () => { if (++attempts === 1) throw new IvyError('fixture_recovery_error', 'Read failed.'); };
  runtime.signals.step = async () => null;
  const process = () => (runtime as unknown as { process: (lane: 'runs', id: string) => Promise<void> }).process('runs', w.state().run.objectId);
  await process(); await process();
  assert.equal(attempts, 1);
  assert.ok(runtime.diagnostics.some(value => value.code === 'fixture_recovery_error'));
  now += 30_001; await process();
  assert.equal(attempts, 2);
  assert.deepEqual(runtime.diagnostics, []);
});

const todo = async (
  f: ReturnType<typeof taskBoardFixture>,
  pin: { objectId: string; revision: number },
) =>
  f.invoke({
    action: "transition",
    operationId: randomUUID(),
    taskId: pin.objectId,
    expectedRevision: pin.revision,
    workflowState: "todo",
    detail: null,
  });

test("reconciliation excludes Backlog and discovers Todo after startup recovery", async (t) => {
  const f = taskBoardFixture(t),
    backlog = await f.create({ control: "agent" }),
    moved = await f.create({ control: "agent" });
  await todo(f, moved.task!);
  const runtime = new TaskBoardReconciler(f.first),
    seen: string[] = [];
  runtime.scheduler.task = async (id) => {
    seen.push(id);
    return null;
  };
  for (let index = 0; index < 6; index++) await step(runtime);
  assert.equal(runtime.recovered, true);
  assert.ok(seen.includes(moved.task!.objectId));
  assert.ok(!seen.includes(backlog.task!.objectId));
});

test("idle reconciliation does not initialize or poll the category projection", async (t) => {
  const f = taskBoardFixture(t),
    client = f.first.store.client,
    request = client.request.bind(client);
  let subscriptions = 0;
  client.request = ((method: string, params: unknown, options?: unknown) => {
    if (method === "events.subscribe") subscriptions++;
    return request(method as never, params as never, options as never);
  }) as typeof client.request;
  await new TaskBoardReconciler(f.first).tick();
  assert.equal(subscriptions, 0);
});

test("Todo scans preserve priority order across pages", async (t) => {
  const f = taskBoardFixture(t);
  f.settings.scheduler.pageSize = 1;
  const low = await f.create({ control: "agent", priority: 1 }),
    high = await f.create({ control: "agent", priority: 4 });
  await todo(f, low.task!);
  await todo(f, high.task!);
  const runtime = new TaskBoardReconciler(f.first),
    seen: string[] = [];
  runtime.scheduler.task = async (id) => {
    seen.push(id);
    return null;
  };
  for (let index = 0; index < 12; index++) await step(runtime);
  assert.equal(seen[0], high.task!.objectId);
  assert.ok(seen.includes(low.task!.objectId));
});

test("operation reconciliation progresses an independent Task past a blocked marked publication", async (t) => {
  const f = taskBoardFixture(t),
    blocked = await f.create(),
    independent = await f.create();
  const blockedOperationId = randomUUID();
  let blockedOperationObjectId: string | null = null;
  f.intercept((params) => {
    if (
      "create" in params &&
      params.create.contractKey === "task-board/history" &&
      params.create.name.endsWith(String(blockedOperationObjectId))
    ) {
      throw new IvyError(
        "synthetic_history_unavailable",
        "Keep the marked publication blocked.",
        "unknown",
      );
    }
  });
  const pending = f.invoke({
    action: "edit",
    operationId: blockedOperationId,
    taskId: blocked.task!.objectId,
    expectedRevision: blocked.task!.revision,
    fields: { ...fields, title: "Blocked marked Task" },
  });
  while (!blockedOperationObjectId)
    blockedOperationObjectId =
      (await f.first.operations.find("user", blockedOperationId))?.pin
        .objectId ?? null;
  await assert.rejects(
    pending,
    (error: unknown) =>
      error instanceof IvyError &&
      error.code === "synthetic_history_unavailable",
  );
  const independentOperationId = randomUUID();
  const accepted = await f.first.operations.begin(
    {
      action: "edit",
      operationId: independentOperationId,
      taskId: independent.task!.objectId,
      expectedRevision: independent.task!.revision,
      fields: { ...fields, title: "Reconciled independently" },
    },
    {
      principalId: "user",
      serviceNodeId: f.first.owner.serviceNodeId,
      generation: f.first.owner.generation,
      source: "user",
    },
  );
  const runtime = new TaskBoardReconciler(f.first);
  await step(runtime);
  const completed = await f.first.store.read(
    "task-board/operation",
    accepted.pin.objectId,
  );
  assert.equal(completed.value.phase, "succeeded");
  assert.equal(
    (await f.first.store.read("task-board/task", independent.task!.objectId))
      .value.fields.title,
    "Reconciled independently",
  );
});

test("an independent Task keeps progressing while another owner remains blocked", async t => {
  const f = taskBoardFixture(t), a = await f.create({ control: "agent" }), b = await f.create({ control: "agent" });
  await todo(f, a.task!); await todo(f, b.task!);
  const runtime = new TaskBoardReconciler(f.first);
  await step(runtime); await step(runtime);
  assert.equal(runtime.recovered, true);
  let release!: () => void;
  const blocked = new Promise<void>(resolve => { release = resolve; });
  let aCalls = 0, bCalls = 0;
  runtime.scheduler.task = async id => {
    if (id === a.task!.objectId) { aCalls++; await blocked; } else bCalls++;
    return null;
  };
  try {
    for (let pass = 0; pass < 3; pass++) await runtime.tick();
    await new Promise<void>(resolve => setImmediate(resolve));
    assert.equal(aCalls, 1); assert.equal(bCalls, 1);
    for (let pass = 0; pass < 4; pass++) await runtime.tick();
    await new Promise<void>(resolve => setImmediate(resolve));
    assert.equal(aCalls, 1, "The same Task must not be admitted twice");
    assert.equal(bCalls, 2, "Free slots must not wait for the previous whole pass");
  } finally { release(); await runtime.stop(); }
});

test("reconciliation admits at most ten Tasks, resumes its pending page and drains on stop", async t => {
  const f = taskBoardFixture(t);
  for (let index = 0; index < 12; index++) { const created = await f.create({ control: "agent" }); await todo(f, created.task!); }
  const runtime = new TaskBoardReconciler(f.first);
  await step(runtime); await step(runtime);
  const releases: (() => void)[] = [];
  let active = 0, maximum = 0;
  const seen = new Set<string>();
  runtime.scheduler.task = async id => {
    assert.ok(!seen.has(id), "Pending rows must progress before repeating the page"); seen.add(id);
    active++; maximum = Math.max(maximum, active);
    await new Promise<void>(resolve => releases.push(resolve)); active--; return null;
  };
  try {
    for (let pass = 0; pass < 3; pass++) await runtime.tick(); assert.equal(active, 10);
    releases[0]!(); await new Promise<void>(resolve => setImmediate(resolve));
    for (let pass = 0; pass < 4; pass++) await runtime.tick(); assert.equal(seen.size, 11); assert.equal(active, 10);
    let stopped = false;
    const stop = runtime.stop().then(() => { stopped = true; });
    await runtime.tick(); assert.equal(seen.size, 11); assert.equal(stopped, false);
    releases.forEach(release => release()); await stop;
    assert.equal(maximum, 10); assert.equal(active, 0);
  } finally { releases.forEach(release => release()); await runtime.stop(); }
});
