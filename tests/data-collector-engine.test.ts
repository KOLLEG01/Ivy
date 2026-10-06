import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { existsSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { CollectorEngine } from "../services/data-collector/src/engine.js";
import { CollectorStore } from "../services/data-collector/src/store.js";
import type { Run } from "../services/data-collector/src/store.js";
import {
  defaultRetention,
  outputSchema,
  registry,
  settingsSchema,
  taskSchema,
} from "../services/data-collector/src/schema.js";
import type { Output, Task } from "../services/data-collector/src/schema.js";
import type { TaskRunner } from "../services/data-collector/src/runner.js";
import { canonical, IvyError } from "../packages/sdk/src/node.js";
import { SchemaValidators } from "../packages/contracts/src/schema.js";
import type { RpcClient } from "../packages/sdk/src/node.js";

const rootId = "collector-root";
test("Collector accepts the namespaced events emitted by home collectors", () => {
  const events = [
    "heating.threshold.crossed",
    "laundry.finished",
    "laundry.status.changed",
    "tesla.delivery.changed",
  ].map((name) => ({ name, payload: { value: 1 } }));
  assert.deepEqual(outputSchema.parse({ data: {}, events }).events, events);
});
const task = (id = "fixture-task", patch: Partial<Task> = {}): Task =>
  taskSchema.parse({
    id,
    name: "Fixture collector",
    enabled: false,
    intervalSeconds: 0,
    timeoutSeconds: 10,
    memoryMb: 64,
    script: "export default async () => ({data:{}});",
    dependencies: {},
    config: {},
    secretNames: [],
    retention: defaultRetention,
    ...patch,
  });
const settings = () => settingsSchema.parse({ rootObjectId: rootId });

class HiveFixture {
  readonly calls: { method: string; params: Record<string, unknown> }[] = [];
  readonly objects = new Map<
    string,
    {
      object: {
        id: string;
        parentId: string | null;
        contractKey: string;
        effectivelyArchived: boolean;
        name: string;
      };
      revision: { revision: number };
      content: { encoding: string; value: unknown };
    }
  >();
  readonly events = new Map<string, unknown>();
  writes = 0;
  failAfterWrite = false;
  rejectWrite: IvyError | null = null;
  failEvent = -1;
  protected = 0;
  remainingBytes = 20;
  constructor() {
    this.objects.set(rootId, {
      object: {
        id: rootId,
        parentId: null,
        contractKey: "data-collector/root",
        effectivelyArchived: false,
        name: "Fixture root",
      },
      revision: { revision: 1 },
      content: { encoding: "json", value: { schemaVersion: 1 } },
    });
  }
  readonly client = {
    request: async (method: string, params: Record<string, unknown>) => {
      this.calls.push({ method, params: structuredClone(params) });
      if (method === "system.status")
        return { runtimeEpoch: "11111111-1111-4111-8111-111111111111" };
      if (method === "objects.read") {
        const object = this.objects.get(String(params.objectId));
        assert.ok(object, "Fixture object exists");
        return structuredClone(object);
      }
      if (method === "objects.query")
        return {
          items: [...this.objects.values()]
            .filter((item) => item.object.contractKey === params.contractKey)
            .map((item) => ({ objectId: item.object.id })),
        };
      if (method === "objects.write") {
        if (this.rejectWrite) throw this.rejectWrite;
        const create = params.create as { name: string } | undefined;
        const id = params.objectId
          ? String(params.objectId)
          : "result-" + create!.name;
        const previous = this.objects.get(id);
        if (previous)
          assert.equal(params.expectedRevision, previous.revision.revision);
        const object = {
          object: {
            id,
            parentId: rootId,
            contractKey: "data-collector/result",
            effectivelyArchived: false,
            name: create?.name ?? previous!.object.name,
          },
          revision: { revision: (previous?.revision.revision ?? 0) + 1 },
          content: params.content as { encoding: string; value: unknown },
        };
        this.objects.set(id, structuredClone(object));
        this.writes++;
        if (this.failAfterWrite) {
          this.failAfterWrite = false;
          throw new IvyError(
            "outcome_unknown",
            "Fixture lost the write reply.",
          );
        }
        return structuredClone(object);
      }
      if (method === "objects.stat")
        return {
          currentRevision: this.objects.get(String(params.objectId))!.revision
            .revision,
        };
      if (method === "objects.pruneRevisions")
        return {
          deleted: 0,
          protected: this.protected,
          remainingBytes: this.remainingBytes,
        };
      if (method === "events.publish") {
        canonical(params.payload, 4096);
        if (
          (params.payload as { eventIndex: number }).eventIndex ===
          this.failEvent
        ) {
          this.failEvent = -1;
          throw new IvyError(
            "service_unavailable",
            "Fixture event publication failed.",
          );
        }
        this.events.set(
          String(params.mutationId),
          structuredClone(params.payload),
        );
        return {};
      }
      throw new Error("Unexpected fixture request: " + method);
    },
  } as unknown as RpcClient;
}

async function fixture(
  t: TestContext,
  run: (
    task: Task,
    state: unknown,
    input: unknown,
    signal: AbortSignal,
  ) => Promise<Output> = async () => ({ data: {} }),
) {
  const store = new CollectorStore(":memory:");
  const hive = new HiveFixture();
  const engine = new CollectorEngine(
    store,
    {
      run,
      clearAuthentication() {},
      async stopped() {
        return false;
      },
    } as unknown as TaskRunner,
    settings(),
  );
  await engine.attach(hive.client);
  t.after(async () => {
    await engine.close();
    store.close();
  });
  return { store, hive, engine };
}

function save(
  engine: CollectorEngine,
  value = task(),
  expectedRevision = 0,
  operationId = randomUUID(),
) {
  return engine.update({
    action: "save",
    task: value,
    expectedRevision,
    operationId,
  });
}
async function execute(
  engine: CollectorEngine,
  id = "fixture-task",
  operationId: string = randomUUID(),
  input: unknown = null,
) {
  engine.update({ action: "run", id, operationId, input });
  await engine.tick();
  await Promise.all([...engine.active.values()].map((item) => item.work));
  return operationId;
}

test("Collector editing is revision checked and operation receipts do not apply stale edits", async (t) => {
  const { engine, store } = await fixture(t);
  const first = task();
  const operationId = randomUUID();
  const receipt = save(engine, first, 0, operationId);
  save(engine, { ...first, name: "Updated fixture" }, 1);
  assert.deepEqual(save(engine, first, 0, operationId), receipt);
  assert.equal(store.get(first.id)!.task.name, "Updated fixture");
  assert.throws(
    () => save(engine, first, 1),
    (error) => error instanceof IvyError && error.code === "revision_conflict",
  );
  assert.throws(
    () =>
      save(engine, { ...first, name: "Conflicting replay" }, 0, operationId),
    (error) => error instanceof IvyError && error.code === "operation_conflict",
  );
});

test("Manual collection preserves explicit zero data, event publication and an explicit null state", async (t) => {
  const { engine, store, hive } = await fixture(
    t,
    async (_task, state, input) => {
      assert.deepEqual(state, { cursor: "old" });
      assert.deepEqual(input, { sensor: 0 });
      return {
        data: { count: 0 },
        state: null,
        events: [{ name: "motion", payload: { active: false } }],
      };
    },
  );
  save(engine);
  const row = store.get("fixture-task")!;
  row.state = { cursor: "old" };
  store.save(row);
  const operationId = await execute(engine, "fixture-task", randomUUID(), {
    sensor: 0,
  });
  assert.equal(store.run(operationId)!.status, "publishing");
  await engine.tick();
  assert.equal(store.run(operationId)!.status, "succeeded");
  assert.equal(store.get("fixture-task")!.state, null);
  assert.equal(hive.writes, 1);
  assert.equal(hive.events.size, 1);
  const content = hive.objects.get("result-fixture-task")!.content.value as {
    data: unknown;
  };
  assert.deepEqual(content.data, { count: 0 });
  assert.equal(
    "state" in
      ((await engine.read({ view: "task", id: "fixture-task" })) as object),
    false,
  );
});

test("Oversized events fail before Hive effects and leave the task editable and its last result intact", async (t) => {
  let output: Output = { data: "previous", state: { cursor: 1 } };
  const { engine, store, hive } = await fixture(t, async () => output);
  save(engine);
  await execute(engine);
  await engine.tick();
  const previous = structuredClone(hive.objects.get("result-fixture-task"));
  for (const payload of ["x".repeat(5000), "ü".repeat(1950)]) {
    output = {
      data: "must not publish",
      state: { cursor: 2 },
      events: [
        { name: "small.first", payload: 1 },
        { name: "large.second", payload },
      ],
    };
    const id = randomUUID().padEnd(256, "x");
    await execute(engine, "fixture-task", id);
    await engine.tick();
    assert.equal(store.run(id)!.status, "failed");
    assert.equal(store.run(id)!.error, "content_too_large");
    assert.equal(store.busy("fixture-task"), false);
    assert.equal(hive.writes, 1);
    assert.equal(hive.events.size, 0);
    assert.deepEqual(hive.objects.get("result-fixture-task"), previous);
    assert.deepEqual(store.get("fixture-task")!.state, { cursor: 1 });
  }
  save(engine, task("fixture-task", { name: "Corrected" }), 1);
  output = { data: "corrected" };
  await execute(engine);
  await engine.tick();
  assert.equal(hive.writes, 2);
  engine.update({
    action: "delete",
    id: "fixture-task",
    expectedRevision: 2,
    operationId: randomUUID(),
  });
  assert.equal(store.get("fixture-task"), null);
});

test("Malformed pending events fail on recovery instead of retrying forever", async (t) => {
  const { engine, store, hive } = await fixture(t);
  save(engine);
  const id = await execute(engine);
  const pending = store.run(id)!;
  pending.output!.events = [{ name: "large", payload: "x".repeat(5000) }];
  store.saveRun(pending);
  await engine.tick();
  await engine.tick();
  assert.equal(store.run(id)!.status, "failed");
  assert.equal(store.run(id)!.error, "content_too_large");
  assert.equal(hive.writes, 0);
  assert.equal(hive.events.size, 0);
  save(engine, task(), 1);
});

test("Definitive publication rejection releases the task while an uncertain rejection stays pending", async (t) => {
  const { engine, store, hive } = await fixture(t);
  save(engine);
  const id = await execute(engine);
  hive.rejectWrite = new IvyError(
    "invalid_arguments",
    "Uncertain fixture reply",
    "unknown",
  );
  await engine.tick();
  assert.equal(store.run(id)!.status, "publishing");
  hive.rejectWrite = new IvyError(
    "invalid_arguments",
    "Definitive fixture rejection",
  );
  await engine.tick();
  assert.equal(store.run(id)!.status, "failed");
  assert.equal(store.run(id)!.error, "invalid_arguments");
  assert.equal(store.busy("fixture-task"), false);
  const attempts = hive.calls.length;
  await engine.tick();
  assert.equal(hive.calls.length, attempts);
  assert.equal(hive.writes, 0);
  assert.equal(store.get("fixture-task")!.state, null);
  save(engine, task(), 1);
});

test("A lost Hive write reply is reconciled without another script execution or result revision", async (t) => {
  let executions = 0;
  const { engine, hive, store } = await fixture(t, async () => {
    executions++;
    return { data: { count: executions } };
  });
  save(engine);
  const id = await execute(engine);
  hive.failAfterWrite = true;
  await engine.tick();
  assert.equal(store.run(id)!.status, "publishing");
  await engine.tick();
  assert.equal(store.run(id)!.status, "succeeded");
  assert.equal(executions, 1);
  assert.equal(hive.writes, 1);
});

test("Restart resumes pending events and commits state without rerunning the script", async (t) => {
  const path = join(
    process.env.IVY_TEST_TEMP ?? tmpdir(),
    "collector-restart-" + randomUUID() + ".sqlite",
  );
  const hive = new HiveFixture();
  let executions = 0;
  const runner = {
    run: async () => {
      executions++;
      return {
        data: {},
        state: { cursor: 2 },
        events: [
          { name: "first", payload: 1 },
          { name: "second", payload: 2 },
        ],
      };
    },
  } as unknown as TaskRunner;
  let store = new CollectorStore(path);
  let engine = new CollectorEngine(store, runner, settings());
  t.after(async () => {
    await engine.close();
    store.close();
    for (const name of [path, path + "-wal", path + "-shm"])
      if (existsSync(name)) unlinkSync(name);
  });
  await engine.attach(hive.client);
  save(engine);
  const id = await execute(engine);
  hive.failEvent = 1;
  await engine.tick();
  assert.equal(store.run(id)!.status, "publishing");
  assert.equal(store.run(id)!.eventIndex, 1);
  assert.equal(hive.writes, 1);
  assert.equal(store.get("fixture-task")!.state, null);
  await engine.close();
  store.close();
  store = new CollectorStore(path);
  engine = new CollectorEngine(store, runner, settings());
  await engine.attach(hive.client);
  assert.partialDeepStrictEqual(
    await engine.read({ view: "task", id: "fixture-task" }),
    { status: "publishing" },
  );
  await engine.tick();
  assert.equal(store.run(id)!.status, "succeeded");
  assert.equal(hive.writes, 1);
  assert.equal(hive.events.size, 2);
  assert.equal(store.run(id)!.eventIndex, 2);
  assert.equal(executions, 1);
  assert.deepEqual(store.get("fixture-task")!.state, { cursor: 2 });
  t.mock.timers.enable({ apis: ["Date"], now: Date.now() + 8 * 86400000 });
  store.cleanup();
  assert.partialDeepStrictEqual(
    await engine.read({ view: "task", id: "fixture-task" }),
    { status: "succeeded", error: null },
  );
  assert.equal(
    hive.calls.filter(
      (call) =>
        call.method === "events.publish" &&
        (call.params.payload as { eventIndex: number }).eventIndex === 0,
    ).length,
    1,
  );
});

test("Replaying an old disable receipt cannot cancel a newer run after re-enabling", async (t) => {
  let finish: (output: Output) => void = () => {
    throw new Error("Runner did not start");
  };
  let activeSignal: AbortSignal | undefined;
  const { engine, store } = await fixture(
    t,
    async (_task, _state, _input, signal) => {
      activeSignal = signal;
      return new Promise<Output>((resolve) => {
        finish = resolve;
      });
    },
  );
  save(engine);
  const disable = {
    action: "disable",
    id: "fixture-task",
    operationId: randomUUID(),
    expectedRevision: 1,
  };
  engine.update(disable);
  engine.update({
    action: "enable",
    id: "fixture-task",
    operationId: randomUUID(),
    expectedRevision: 2,
  });
  engine.update({
    action: "run",
    id: "fixture-task",
    operationId: randomUUID(),
  });
  await engine.tick();
  assert.ok(activeSignal);
  try {
    engine.update(disable);
    assert.equal(activeSignal.aborted, false);
    assert.equal(store.get("fixture-task")!.task.enabled, true);
  } finally {
    finish({ data: {} });
    await Promise.all([...engine.active.values()].map((item) => item.work));
  }
});

test("Disabling cancels queued work while an explicit later manual run remains available", async (t) => {
  let executions = 0;
  const { engine, store } = await fixture(t, async () => {
    executions++;
    return { data: {} };
  });
  save(engine, task("fixture-task", { enabled: true, intervalSeconds: 60 }));
  const queued = randomUUID();
  engine.update({ action: "run", id: "fixture-task", operationId: queued });
  engine.update({
    action: "disable",
    id: "fixture-task",
    operationId: randomUUID(),
    expectedRevision: 1,
  });
  await engine.tick();
  assert.equal(executions, 0);
  assert.equal(store.run(queued)!.status, "cancelled");
  await execute(engine);
  assert.equal(executions, 1);
});

test("Result retention takes the stricter task and service budgets and exposes protected revisions", async (t) => {
  const { engine, store, hive } = await fixture(t);
  save(
    engine,
    task("fixture-task", {
      retention: {
        maximumCount: 2000,
        maximumAgeDays: 2,
        maximumBytes: 200000000,
      },
    }),
  );
  hive.protected = 1;
  await execute(engine);
  await engine.tick();
  const prune = hive.calls.find(
    (call) => call.method === "objects.pruneRevisions",
  )!;
  assert.equal(prune.params.maximumCount, defaultRetention.maximumCount);
  assert.equal(prune.params.maximumAgeDays, 2);
  assert.equal(prune.params.maximumBytes, defaultRetention.maximumBytes);
  assert.match(store.get("fixture-task")!.retentionWarning!, /protected/);
});

test("Result retention describes pending history without blaming the current value", async (t) => {
  const { engine, store, hive } = await fixture(t);
  save(engine, task("fixture-task", { retention: { ...defaultRetention, maximumBytes: 10000 } }));
  hive.remainingBytes = 50000;
  await execute(engine);
  await engine.tick();
  assert.ok(hive.calls.some((call) => call.method === "objects.pruneRevisions"));
  assert.match(store.get("fixture-task")!.retentionWarning!, /Retained history.*cleanup may still be pending/);
  hive.remainingBytes = 1;
  await execute(engine);
  await engine.tick();
  assert.equal(store.get("fixture-task")!.retentionWarning, null);
});

test("Deleted and recreated tasks use the same result and retention lifecycle", async (t) => {
  const { engine, store, hive } = await fixture(t);
  save(
    engine,
    task("fixture-task", {
      retention: { ...defaultRetention, maximumCount: 1 },
    }),
  );
  await execute(engine);
  await engine.tick();
  const originalResultId = store.get("fixture-task")!.resultObjectId;
  assert.ok(originalResultId);
  engine.update({
    action: "delete",
    id: "fixture-task",
    expectedRevision: 1,
    operationId: randomUUID(),
  });
  assert.equal(store.get("fixture-task"), null);
  assert.deepEqual(store.list(), []);
  assert.equal(
    store.get("fixture-task", true)!.resultObjectId,
    originalResultId,
  );
  assert.equal(store.get("fixture-task", true)!.task.script, "");
  await assert.rejects(
    engine.read({ view: "task", id: "fixture-task" }),
    /not found/,
  );
  const deletedAt = Date.now();
  t.mock.timers.enable({ apis: ["Date"], now: deletedAt + 61000 });
  const beforeDeletedMaintenance = hive.calls.length;
  await engine.tick();
  const deletedRetention = hive.calls
    .slice(beforeDeletedMaintenance)
    .filter((call) => call.method === "objects.pruneRevisions");
  assert.equal(deletedRetention.length, 1);
  assert.equal(deletedRetention[0]!.params.maximumCount, 1);
  assert.equal(store.runs("queued").length, 0);
  save(
    engine,
    task("fixture-task", {
      retention: { ...defaultRetention, maximumCount: 5 },
    }),
  );
  assert.equal(store.get("fixture-task")!.resultObjectId, originalResultId);
  assert.equal(store.list(true).length, 1);
  assert.equal(store.get("fixture-task")!.revision, 3);
  assert.throws(() => save(engine, task(), 1), /changed/);
  const beforeMaintenance = hive.calls.length;
  t.mock.timers.setTime(Date.now() + 61000);
  await engine.tick();
  const retentionCalls = hive.calls
    .slice(beforeMaintenance)
    .filter((call) => call.method === "objects.pruneRevisions");
  assert.equal(retentionCalls.length, 1);
  assert.equal(retentionCalls[0]!.params.objectId, originalResultId);
  assert.equal(retentionCalls[0]!.params.maximumCount, 5);
  await execute(engine);
  await engine.tick();
  assert.equal(store.get("fixture-task")!.resultObjectId, originalResultId);
});

test("A manual run cannot overwrite an automatically scheduled run with the same ID", async (t) => {
  const { engine, store, hive } = await fixture(t);
  save(engine, task("scheduled", { enabled: true, intervalSeconds: 60 }));
  save(engine, task("manual"));
  const scheduled = store.get("scheduled")!;
  scheduled.nextAt = Date.now() - 1;
  store.save(scheduled);
  await engine.tick();
  await Promise.all([...engine.active.values()].map((item) => item.work));
  const automatic = store
    .runs("publishing")
    .find((run) => run.taskId === "scheduled")!;
  assert.ok(automatic);
  assert.equal(store.operation(automatic.id), null);
  const manualBefore = store.get("manual");
  assert.throws(
    () =>
      engine.update({
        action: "run",
        id: "manual",
        operationId: automatic.id,
        input: { sequence: 2 },
      }),
    (error) => error instanceof IvyError && error.code === "operation_conflict",
  );
  assert.deepEqual(store.run(automatic.id), automatic);
  assert.deepEqual(store.get("manual"), manualBefore);
  assert.equal(store.busy("scheduled"), true);
  assert.equal(store.busy("manual"), false);
  await engine.tick();
  assert.equal(store.run(automatic.id)!.status, "succeeded");
  assert.equal(hive.writes, 1);
  assert.throws(
    () =>
      engine.update({
        action: "run",
        id: "scheduled",
        operationId: automatic.id,
      }),
    (error) => error instanceof IvyError && error.code === "operation_conflict",
  );
});

test("Crash recovery keeps publication work but bounds interrupted execution receipts", (t) => {
  const path = join(
    process.env.IVY_TEST_TEMP ?? tmpdir(),
    "collector-engine-" + randomUUID() + ".sqlite",
  );
  let store = new CollectorStore(path);
  t.after(() => {
    store.close();
    for (const name of [path, path + "-wal", path + "-shm"])
      if (existsSync(name)) unlinkSync(name);
  });
  const first = task();
  store.save({
    task: first,
    revision: 1,
    resultObjectId: null,
    nextAt: 0,
    state: null,
    deleted: false,
    lastRunId: "running",
    retentionWarning: null,
  });
  const base: Run = {
    id: "running",
    taskId: first.id,
    task: first,
    status: "running",
    input: { sequence: 1 },
    output: null,
    startedAt: new Date().toISOString(),
    finishedAt: null,
    eventIndex: 0,
    error: null,
  };
  store.saveRun(base);
  store.saveRun({
    ...base,
    id: "publishing",
    status: "publishing",
    output: { data: {} },
    finishedAt: new Date().toISOString(),
  });
  const scheduled = task("named-worker", {
    enabled: true,
    intervalSeconds: 60,
  });
  store.save({
    task: scheduled,
    revision: 1,
    resultObjectId: null,
    nextAt: 0,
    state: { kept: true },
    deleted: false,
    lastRunId: "named-running",
    retentionWarning: null,
  });
  store.saveRun({
    ...base,
    id: "named-running",
    taskId: scheduled.id,
    task: scheduled,
    workerJobName: "Global\\Ivy.Collector.fixture",
  });
  store.close();
  store = new CollectorStore(path);
  const recovered = store.run("running")!;
  assert.equal(recovered.status, "interrupted");
  assert.equal(recovered.error, "outcome_unknown");
  assert.equal(store.get(first.id)!.task.enabled, false);
  assert.equal(
    store.get(scheduled.id)!.task.enabled,
    true,
    "An observable worker keeps its schedule after a crash",
  );
  assert.equal(store.get(scheduled.id)!.revision, 1);
  assert.equal(store.run("named-running")!.status, "interrupted");
  assert.equal(
    store.run("named-running")!.workerJobName,
    "Global\\Ivy.Collector.fixture",
  );
  const restored = new CollectorEngine(store, {} as TaskRunner, settings());
  assert.throws(
    () =>
      restored.update({
        action: "run",
        id: first.id,
        operationId: "after-crash",
      }),
    (error: unknown) =>
      error instanceof IvyError && error.code === "outcome_unknown",
  );
  assert.equal(typeof recovered.finishedAt, "string");
  assert.equal(store.run("publishing")!.status, "publishing");
  t.mock.timers.enable({ apis: ["Date"], now: Date.now() + 8 * 86400000 });
  store.cleanup();
  assert.equal(
    store.run("running")!.status,
    "interrupted",
    "Latest status survives receipt cleanup",
  );
  assert.equal(store.run("publishing")!.status, "publishing");
  const row = store.get(first.id)!;
  row.lastRunId = "publishing";
  store.save(row);
  store.cleanup();
  assert.equal(store.run("running"), null, "An older terminal run expires");
});

test("Named worker recovery waits for actual termination, preserves pauses and never replays interrupted input", async (t) => {
  const { engine, store } = await fixture(t);
  const startedAt = Date.now();
  t.mock.timers.enable({ apis: ["Date"], now: startedAt });
  for (const [id, enabled] of [
    ["scheduled", true],
    ["manual", false],
  ] as const) {
    save(engine, task(id, { enabled, intervalSeconds: enabled ? 60 : 0 }));
    const row = store.get(id)!;
    row.lastRunId = id + "-interrupted";
    row.state = { kept: true };
    store.save(row);
    store.saveRun({
      id: row.lastRunId,
      taskId: id,
      task: row.task,
      status: "interrupted",
      input: { neverReplay: true },
      output: null,
      startedAt: new Date().toISOString(),
      finishedAt: new Date().toISOString(),
      eventIndex: 0,
      error: "outcome_unknown",
      workerJobName: "Global\\Ivy.Collector." + id,
    });
  }
  let observation: boolean | Error = false;
  t.mock.method(engine.runner, "stopped", async () => {
    if (observation instanceof Error) throw observation;
    return observation;
  });
  await engine.tick();
  assert.equal(store.run("scheduled-interrupted")!.error, "outcome_unknown");
  assert.equal(store.get("scheduled")!.task.enabled, true);
  assert.throws(
    () =>
      engine.update({
        action: "run",
        id: "scheduled",
        operationId: "too-early",
      }),
    (error: unknown) =>
      error instanceof IvyError && error.code === "outcome_unknown",
  );
  observation = new Error("Unavailable process observation");
  t.mock.timers.setTime(startedAt + 2001);
  await engine.tick();
  assert.equal(store.run("scheduled-interrupted")!.error, "outcome_unknown");
  observation = true;
  t.mock.timers.setTime(startedAt + 4002);
  // A pause made while recovery was pending must remain authoritative.
  engine.update({
    action: "disable",
    id: "scheduled",
    expectedRevision: 1,
    operationId: "pause-pending-recovery",
  });
  await engine.tick();
  for (const id of ["scheduled", "manual"]) {
    assert.equal(store.get(id)!.task.enabled, false);
    assert.deepEqual(store.get(id)!.state, { kept: true });
    assert.equal(store.run(id + "-interrupted")!.status, "interrupted");
    assert.notEqual(store.run(id + "-interrupted")!.error, "outcome_unknown");
  }
  assert.equal(store.runs("queued").length, 0);
  engine.update({
    action: "enable",
    id: "scheduled",
    expectedRevision: 2,
    operationId: "resume-verified-worker",
  });
  t.mock.timers.setTime(startedAt + 64003);
  await engine.tick();
  await Promise.all([...engine.active.values()].map((entry) => entry.work));
  assert.equal(store.runs("publishing").length, 1);
  assert.notEqual(store.get("scheduled")!.lastRunId, "scheduled-interrupted");
  assert.equal(store.get("manual")!.lastRunId, "manual-interrupted");
});

test("An unconfirmed worker cannot block other tasks after a crash before its disable was saved", async (t) => {
  const calls: string[] = [];
  const { engine, store } = await fixture(t, async (value) => {
    calls.push(value.id);
    if (value.id === "unconfirmed")
      throw new IvyError("outcome_unknown", "Stop unconfirmed");
    return { data: "collected" };
  });
  save(engine, task("unconfirmed", { enabled: true, intervalSeconds: 60 }));
  const unknownRun = await execute(engine, "unconfirmed");
  const row = store.get("unconfirmed")!;
  // Reconstruct the durable state if the service crashed between saving the
  // failed run and disabling its task.
  row.task.enabled = true;
  row.nextAt = Date.now() - 1;
  store.save(row);
  save(engine, task("other"));
  engine.update({ action: "run", id: "other", operationId: "other-run" });
  await engine.tick();
  await Promise.all([...engine.active.values()].map((entry) => entry.work));
  await engine.tick();
  assert.deepEqual(calls, ["unconfirmed", "other"]);
  assert.equal(store.run("other-run")!.status, "succeeded");
  assert.equal(store.get("unconfirmed")!.lastRunId, unknownRun);
  t.mock.timers.enable({ apis: ["Date"], now: Date.now() + 8 * 86400000 });
  store.cleanup();
  assert.equal(store.run(unknownRun)!.error, "outcome_unknown");
  await engine.tick();
  assert.deepEqual(calls, ["unconfirmed", "other"]);
});

test("Collector registry presents two MCP tools and task names cannot escape runner paths", () => {
  const tools = registry().namespaces[0]!.tools;
  const validators = new SchemaValidators();
  for (const tool of tools) validators.compile(tool.inputSchema);
  assert.deepEqual(
    tools.map((tool) => tool.name),
    ["read", "update"],
  );
  assert.throws(() => task("../outside"));
  assert.throws(() => task("fixture", { dependencies: { package: "latest" } }));
});
