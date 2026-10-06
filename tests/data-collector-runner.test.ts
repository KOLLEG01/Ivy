import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { join, relative, resolve } from "node:path";
import { tmpdir } from "node:os";
import { pathToFileURL } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { TaskRunner } from "../services/data-collector/src/runner.js";
import { CollectorEngine } from "../services/data-collector/src/engine.js";
import { CollectorStore } from "../services/data-collector/src/store.js";
import type { Run } from "../services/data-collector/src/store.js";
import {
  settingsSchema,
  taskSchema,
} from "../services/data-collector/src/schema.js";
import type { Output } from "../services/data-collector/src/schema.js";
import { IvyError } from "../packages/sdk/src/node.js";
import type { RpcClient } from "../packages/sdk/src/node.js";

const settings = settingsSchema.parse({
  secrets: { SELECTED: "selected-fixture", UNSELECTED: "unselected-fixture" },
});
const task = (script = "export default async () => ({data:{value:1}})") =>
  taskSchema.parse({
    id: "camera",
    name: "Camera",
    enabled: true,
    intervalSeconds: 300,
    timeoutSeconds: 5,
    memoryMb: 64,
    dependencies: {},
    config: {},
    secretNames: [],
    retention: {
      maximumCount: 1000,
      maximumAgeDays: 7,
      maximumBytes: 104857600,
    },
    script,
  });
const processAvailable =
  process.platform !== "win32" ||
  existsSync(resolve("dist/native/ivy-job.exe"));
const processOptions = {
  skip: processAvailable ? false : "Windows supervision binary is not built",
  timeout: 20_000,
};

async function tempRoot(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), "ivy-collector-runner-"));
  t.after(async () => {
    assert.ok(relative(tmpdir(), root).startsWith("ivy-collector-runner-"));
    // Windows may release the terminated process's working-directory handle late.
    await rm(root, {
      recursive: true,
      force: true,
      maxRetries: 10,
      retryDelay: 100,
    });
  });
  return root;
}

test(
  "collector workers receive only selected secrets and use their task working directory",
  processOptions,
  async (t) => {
    const root = await tempRoot(t),
      runner = new TaskRunner(root, resolve("."), settings);
    const value = task(
      "export default async ({secrets,config,state,signal}) => ({data:{keys:Object.keys(secrets),cwd:process.cwd(),home:process.env.HOME,hasSignal:signal instanceof AbortSignal,config},state,events:[{name:'motion',payload:{cameraId:'camera'}}]})",
    );
    value.secretNames = ["SELECTED"];
    value.config = { cameraId: "camera" };
    const result = await runner.run(
      value,
      { seen: [1] },
      null,
      new AbortController().signal,
    );
    assert.deepEqual(result.data, {
      keys: ["SELECTED"],
      cwd: join(root, "camera"),
      home: join(root, "camera"),
      hasSignal: true,
      config: value.config,
    });
    assert.deepEqual(result.state, { seen: [1] });
    assert.deepEqual(result.events, [
      { name: "motion", payload: { cameraId: "camera" } },
    ]);
  },
);

test(
  "collector workers preserve UTF-8 across stdin chunks",
  processOptions,
  async (t) => {
    const root = await tempRoot(t),
      runner = new TaskRunner(root, resolve("."), settings);
    const text = "€😀ä".repeat(25000);
    const value = task(`import {createHash} from 'node:crypto';
export default async ({config,secrets,state,input}) => ({data:createHash('sha256').update(JSON.stringify({config,secrets,state,input})).digest('hex')});`);
    value.config = { text };
    value.secretNames = ["SELECTED"];
    const state = { text };
    const input = { text };
    const expected = createHash("sha256")
      .update(
        JSON.stringify({
          config: value.config,
          secrets: { SELECTED: settings.secrets["SELECTED"] },
          state,
          input,
        }),
      )
      .digest("hex");
    const result = await runner.run(
      value,
      state,
      input,
      new AbortController().signal,
    );
    assert.equal(result.data, expected);
  },
);

test(
  "collector workers cannot reuse a previous result after an early successful exit",
  processOptions,
  async (t) => {
    const root = await tempRoot(t),
      runner = new TaskRunner(root, resolve("."), settings),
      signal = new AbortController().signal;
    const first = await runner.run(task(), null, null, signal);
    assert.deepEqual(first.data, { value: 1 });
    await assert.rejects(
      runner.run(
        task("process.exit(0); export default async () => ({data:{value:2}})"),
        null,
        null,
        signal,
      ),
    );
  },
);

test(
  "collector workers expose only known authentication codes and redact provider messages",
  { ...processOptions, timeout: 60000 },
  async (t) => {
    const root = await tempRoot(t),
      runner = new TaskRunner(root, resolve("."), settings);
    for (const code of [
      "authentication_required",
      "interaction_required",
      "configuration_invalid",
      "provider_unavailable",
      "sensitive_unknown_code",
    ]) {
      await assert.rejects(
        runner.run(
          task(
            `export default async () => { throw Object.assign(new Error('sensitive-provider-message'), {code:${JSON.stringify(code)}}); }`,
          ),
          null,
          null,
          new AbortController().signal,
        ),
        (error: unknown) => {
          assert.ok(error instanceof IvyError);
          assert.equal(
            error.code,
            code === "sensitive_unknown_code" ? "task_failed" : code,
          );
          assert.doesNotMatch(error.message, /sensitive/);
          return true;
        },
      );
    }
  },
);

test(
  "Tesla session expiry retains its actionable code across the real worker boundary",
  processOptions,
  async (t) => {
    const root = await tempRoot(t),
      runner = new TaskRunner(root, resolve("."), settings);
    const source = pathToFileURL(
      resolve("docs/examples/data-collector/home/tesla-delivery.mjs"),
    ).href;
    const value = task(
      `import collect from ${JSON.stringify(source)};
globalThis.fetch = async () => new Response('private-provider-body', {status:401});
export default collect;`,
    );
    value.config = {
      orderUrl: "https://www.tesla.com/teslaaccount/order/RN123456789",
      cookieSecret: "SELECTED",
    };
    value.secretNames = ["SELECTED"];
    await assert.rejects(
      runner.run(value, null, null, new AbortController().signal),
      (error: unknown) => {
        assert.ok(error instanceof IvyError);
        assert.equal(error.code, "authentication_required");
        assert.doesNotMatch(
          error.message,
          /private-provider-body|selected-fixture/,
        );
        return true;
      },
    );
  },
);

test(
  "collector workers identify oversized returned JSON as an explicit size failure",
  processOptions,
  async (t) => {
    const root = await tempRoot(t),
      runner = new TaskRunner(root, resolve("."), settings);
    await assert.rejects(
      runner.run(
        task("export default async () => ({data:'x'.repeat(1100000)})"),
        null,
        null,
        new AbortController().signal,
      ),
      (error: unknown) =>
        error instanceof IvyError && error.code === "limit_exceeded",
    );
  },
);

test(
  "collector hard deadlines terminate non-cooperative workers",
  processOptions,
  async (t) => {
    const root = await tempRoot(t),
      runner = new TaskRunner(root, resolve("."), settings);
    const value = task(
      "export default async () => { setInterval(()=>{},1000); await new Promise(()=>{}); }",
    );
    value.timeoutSeconds = 1;
    await assert.rejects(
      runner.run(value, null, null, new AbortController().signal),
      (error: unknown) =>
        error instanceof IvyError && error.code === "deadline_exceeded",
    );
  },
);

test(
  "collector cancellation stops an observed live child before reporting completion",
  { ...processOptions, timeout: 40_000 },
  async (t) => {
    const root = await tempRoot(t),
      runner = new TaskRunner(root, resolve("."), settings);
    const heartbeat = join(root, "heartbeat");
    const value = task(`import {spawn} from 'node:child_process';
export default async () => {
  spawn(process.execPath,['-e',"const fs=require('node:fs');let counter=0;const beat=()=>fs.writeFileSync(process.argv[1],String(++counter));beat();const timer=setInterval(beat,40);setTimeout(()=>{clearInterval(timer)},60000)",${JSON.stringify(heartbeat)}],{stdio:'ignore'});
  await new Promise(()=>{});
}`);
    // Startup may be slow on a busy host; this test aborts only after the child
    // has produced two distinct heartbeats. The separate test covers deadlines.
    value.timeoutSeconds = 60;
    const controller = new AbortController();
    let finished = false;
    const completion = runner.run(value, null, null, controller.signal).then(
      (output) => {
        finished = true;
        return { output };
      },
      (error: unknown) => {
        finished = true;
        return { error };
      },
    );
    try {
      const deadline = Date.now() + 20_000;
      let first: string | undefined,
        ready = false;
      while (Date.now() < deadline && !finished) {
        const beat = await readFile(heartbeat, "utf8").catch(
          (error: NodeJS.ErrnoException) => {
            if (error.code === "ENOENT") return "";
            throw error;
          },
        );
        if (/^\d+$/.test(beat)) {
          if (first !== undefined && beat !== first) {
            ready = true;
            break;
          }
          first = beat;
        }
        await delay(50);
      }
      assert.ok(
        ready,
        "The child did not demonstrate readiness before cancellation.",
      );
      controller.abort();
      const outcome = await completion;
      assert.ok("error" in outcome);
      assert.ok(outcome.error instanceof Error);
      assert.equal(outcome.error.name, "AbortError");
      const last = await readFile(heartbeat, "utf8");
      await delay(200);
      assert.equal(
        await readFile(heartbeat, "utf8"),
        last,
        "The observed child must stop writing after cancellation completes.",
      );
    } finally {
      controller.abort();
      await completion;
    }
  },
);

class FixtureRunner extends TaskRunner {
  calls = 0;
  output: Output = { data: { value: "new" } };
  failure: Error | null = null;
  gate: Promise<void> | null = null;
  override async run(): Promise<Output> {
    this.calls++;
    await this.gate;
    if (this.failure) throw this.failure;
    return this.output;
  }
}

async function engineFixture(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), "ivy-collector-runner-")),
    store = new CollectorStore(join(root, "store.sqlite"));
  const runner = new FixtureRunner(root, resolve("."), settings),
    engine = new CollectorEngine(store, runner, settings);
  t.after(async () => {
    await engine.close();
    store.close();
    assert.ok(relative(tmpdir(), root).startsWith("ivy-collector-runner-"));
    await rm(root, { recursive: true, force: true });
  });
  store.save({
    task: task(),
    revision: 1,
    resultObjectId: "result",
    nextAt: Date.now() + 300_000,
    state: { seen: [1] },
    deleted: false,
    lastRunId: null,
    retentionWarning: null,
  });
  const writes: unknown[] = [];
  let onWrite: (() => Promise<void>) | undefined;
  const client = {
    request: async (method: string, params: unknown) => {
      if (method === "system.status")
        return { runtimeEpoch: "11111111-1111-4111-8111-111111111111" };
      if (method === "objects.read")
        return {
          object: {
            id: "result",
            parentId: "root",
            contractKey: "data-collector/result",
            effectivelyArchived: false,
          },
          revision: { revision: 1 },
          content: {
            encoding: "json",
            value: { runId: "previous", data: { value: "previous" } },
          },
        };
      if (method === "objects.write") {
        writes.push(params);
        await onWrite?.();
        return { object: { id: "result" } };
      }
      if (method === "objects.stat") return { currentRevision: 2 };
      if (method === "objects.pruneRevisions")
        return { protected: false, remainingBytes: 100 };
      if (method === "events.publish") return {};
      throw new Error("Unexpected fixture method: " + method);
    },
  } as unknown as RpcClient;
  engine.client = client;
  engine.rootId = "root";
  const publishing = (output: Output): Run => ({
    id: "publish-run",
    taskId: "camera",
    task: task(),
    status: "publishing",
    input: null,
    output,
    startedAt: new Date().toISOString(),
    finishedAt: new Date().toISOString(),
    eventIndex: 0,
    error: null,
  });
  return {
    store,
    runner,
    engine,
    writes,
    publishing,
    beforeWrite: (hook: () => Promise<void>) => {
      onWrite = hook;
    },
  };
}

test("deleting a task removes its private authentication cache without touching other tasks or results", async (t) => {
  const f = await engineFixture(t),
    root = f.runner.workRoot;
  for (const id of ["camera", "another-camera"]) {
    await mkdir(join(root, id, ".auth"), { recursive: true });
    await writeFile(
      join(root, id, ".auth", "provider.json"),
      "private-session",
    );
    await writeFile(join(root, id, "task.mjs"), "script");
  }
  f.engine.update({
    action: "delete",
    id: "camera",
    expectedRevision: 1,
    operationId: "delete-with-auth",
  });
  assert.equal(existsSync(join(root, "camera", ".auth")), false);
  assert.equal(
    await readFile(
      join(root, "another-camera", ".auth", "provider.json"),
      "utf8",
    ),
    "private-session",
  );
  assert.equal(
    await readFile(join(root, "camera", "task.mjs"), "utf8"),
    "script",
  );
  assert.equal(f.store.get("camera", true)?.resultObjectId, "result");
  assert.throws(
    () => f.runner.clearAuthentication("../another-camera"),
    /Invalid task/,
  );
});

test("unconfirmed termination fences every reuse until the matching stopped run is explicitly confirmed", async (t) => {
  const f = await engineFixture(t),
    cache = join(f.runner.workRoot, "camera", ".auth");
  await mkdir(cache, { recursive: true });
  await writeFile(join(cache, "provider.json"), "private-session");
  f.runner.failure = new IvyError("outcome_unknown", "Stop unconfirmed.");
  f.engine.update({
    action: "run",
    id: "camera",
    operationId: "unconfirmed-run",
  });
  await f.engine.tick();
  await Promise.all([...f.engine.active.values()].map((value) => value.work));
  const row = f.store.get("camera")!;
  assert.equal(row.task.enabled, false);
  assert.partialDeepStrictEqual(
    await f.engine.read({ view: "task", id: "camera" }),
    {
      lastRunId: "unconfirmed-run",
      error: "outcome_unknown",
    },
  );
  for (const update of [
    { action: "run", id: "camera" },
    { action: "enable", id: "camera", expectedRevision: row.revision },
    { action: "delete", id: "camera", expectedRevision: row.revision },
    { action: "save", task: row.task, expectedRevision: row.revision },
  ])
    assert.throws(
      () =>
        f.engine.update({ ...update, operationId: "blocked-" + update.action }),
      (error: unknown) =>
        error instanceof IvyError && error.code === "outcome_unknown",
    );
  f.engine.update({
    action: "disable",
    id: "camera",
    expectedRevision: row.revision,
    operationId: "ordinary-disable",
  });
  assert.equal(f.store.run("unconfirmed-run")!.error, "outcome_unknown");
  const reopened = new CollectorStore(join(f.runner.workRoot, "store.sqlite"));
  try {
    const restored = new CollectorEngine(reopened, f.runner, settings);
    assert.throws(
      () =>
        restored.update({
          action: "run",
          id: "camera",
          operationId: "after-restart",
        }),
      (error: unknown) =>
        error instanceof IvyError && error.code === "outcome_unknown",
    );
  } finally {
    reopened.close();
  }
  const confirm = {
    action: "disable",
    id: "camera",
    expectedRevision: row.revision + 1,
    confirmedStoppedRunId: "unconfirmed-run",
    operationId: "confirm-stop",
  };
  assert.throws(
    () => f.engine.update({ ...confirm, confirmedStoppedRunId: "another-run" }),
    (error: unknown) =>
      error instanceof IvyError && error.code === "operation_conflict",
  );
  assert.throws(
    () => f.engine.update({ ...confirm, expectedRevision: row.revision }),
    (error: unknown) =>
      error instanceof IvyError && error.code === "revision_conflict",
  );
  f.engine.active.set("camera", {
    controller: new AbortController(),
    work: Promise.resolve(),
  });
  try {
    assert.throws(
      () => f.engine.update(confirm),
      (error: unknown) =>
        error instanceof IvyError && error.code === "task_busy",
    );
  } finally {
    f.engine.active.delete("camera");
  }
  const receipt = f.engine.update(confirm);
  assert.equal(f.store.get("camera")!.task.enabled, false);
  assert.equal(f.store.run("unconfirmed-run")!.status, "failed");
  assert.match(
    f.store.run("unconfirmed-run")!.error!,
    /execution outcome remains unknown/,
  );
  assert.deepEqual(f.store.get("camera")!.state, { seen: [1] });
  assert.equal(existsSync(cache), true);
  f.runner.failure = null;
  f.engine.update({
    action: "run",
    id: "camera",
    operationId: "confirmed-retry",
  });
  assert.deepEqual(
    f.engine.update(confirm),
    receipt,
    "An old confirmation replay cannot affect the new run",
  );
  await f.engine.tick();
  await Promise.all([...f.engine.active.values()].map((value) => value.work));
  await f.engine.tick();
  assert.equal(f.runner.calls, 2);
  assert.equal(f.store.run("confirmed-retry")!.status, "succeeded");
});

test("disabling a task cancels its queued run before execution", async (t) => {
  const f = await engineFixture(t);
  f.engine.update({ action: "run", id: "camera", operationId: "queued-run" });
  f.engine.update({
    action: "disable",
    id: "camera",
    expectedRevision: 1,
    operationId: "disable",
  });
  await f.engine.tick();
  await f.engine.close();
  assert.equal(f.runner.calls, 0);
  assert.equal(f.store.run("queued-run")?.status, "cancelled");
  assert.equal(f.store.get("camera")?.task.enabled, false);
});

test("disabling during the final runner read cannot publish its cancelled output", async (t) => {
  const f = await engineFixture(t);
  let resume!: () => void;
  f.runner.gate = new Promise<void>((resolve) => {
    resume = resolve;
  });
  f.engine.update({
    action: "run",
    id: "camera",
    operationId: "cancelled-run",
  });
  await f.engine.tick();
  f.engine.update({
    action: "disable",
    id: "camera",
    expectedRevision: 1,
    operationId: "disable-active",
  });
  resume();
  await f.engine.close();
  assert.equal(f.store.run("cancelled-run")?.status, "cancelled");
  assert.deepEqual(f.store.get("camera")?.state, { seen: [1] });
  assert.equal(f.writes.length, 0);
});

test("publication preserves a concurrent disable and its updated revision", async (t) => {
  const f = await engineFixture(t);
  let entered!: () => void, resume!: () => void;
  const waiting = new Promise<void>((resolve) => {
    entered = resolve;
  });
  const released = new Promise<void>((resolve) => {
    resume = resolve;
  });
  f.beforeWrite(async () => {
    entered();
    await released;
  });
  f.store.saveRun(f.publishing({ data: { value: "new" } }));
  const ticking = f.engine.tick();
  await Promise.race([
    waiting,
    delay(2500).then(() => {
      throw new Error("Publication did not reach the fixture write.");
    }),
  ]);
  f.engine.update({
    action: "disable",
    id: "camera",
    expectedRevision: 1,
    operationId: "disable-during-publish",
  });
  resume();
  await ticking;
  assert.equal(f.store.get("camera")?.task.enabled, false);
  assert.equal(f.store.get("camera")?.revision, 2);
});

test("an explicit null script state resets saved collector state", async (t) => {
  const f = await engineFixture(t);
  f.store.saveRun(f.publishing({ data: { value: "new" }, state: null }));
  await f.engine.tick();
  assert.equal(f.store.get("camera")?.state, null);
});

test("failed collection preserves the last successful result and motion state", async (t) => {
  const f = await engineFixture(t);
  f.runner.failure = new IvyError("task_failed", "Fixture camera failure.");
  f.engine.update({ action: "run", id: "camera", operationId: "failed-run" });
  await f.engine.tick();
  await Promise.all([...f.engine.active.values()].map((value) => value.work));
  const row = f.store.get("camera");
  assert.equal(f.store.run(row!.lastRunId!)?.status, "failed");
  assert.equal(row?.resultObjectId, "result");
  assert.deepEqual(row?.state, { seen: [1] });
  assert.equal(f.writes.length, 0);
  assert.deepEqual(
    (
      (await f.engine.read({ view: "result", id: "camera" })) as unknown as {
        content: { value: { data: unknown } };
      }
    ).content.value.data,
    { value: "previous" },
  );
});
