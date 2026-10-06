import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { createServer, request as httpRequest } from "node:http";
import type { ClientRequest } from "node:http";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { ServiceClient } from "../packages/sdk/src/node.js";
import type { Host } from "../packages/sdk/src/host.js";
import { atomicJson } from "../packages/sdk/src/host.js";
import { startDataCollector } from "../services/data-collector/src/main.js";
import { taskSchema } from "../services/data-collector/src/schema.js";
import type { Task } from "../services/data-collector/src/schema.js";

async function fixture(
  t: TestContext,
  taskFields: Partial<Task> = {},
  routes: Record<string, string> = {},
) {
  const parent = resolve(process.env.IVY_TEST_TEMP ?? tmpdir());
  const root = await mkdtemp(join(parent, "ivy-collector-ingress-"));
  let runtime: Awaited<ReturnType<typeof startDataCollector>> | undefined;
  t.after(async () => {
    await runtime?.close();
    assert.equal(dirname(root), parent);
    await rm(root, { recursive: true, force: true });
  });
  const listener = createServer();
  await new Promise<void>((resolve) =>
    listener.listen(0, "127.0.0.1", resolve),
  );
  const port = (listener.address() as { port: number }).port;
  await new Promise<void>((resolve, reject) =>
    listener.close((error) => (error ? reject(error) : resolve())),
  );
  const build = JSON.parse(await readFile("dist/build-info.json", "utf8")) as {
    version: string;
    buildId: string;
  };
  const config: Host.InstanceConfig = {
    schemaVersion: 1,
    componentId: "data-collector",
    instanceId: "collector-ingress-fixture",
    serviceNodeId: "collector-ingress-fixture",
    hostId: "fixture-host",
    publicBaseUrl: "http://127.0.0.1:1/ivy",
    version: build.version,
    buildId: build.buildId,
    dataRoot: join(root, "data"),
    artifactRoot: resolve("."),
    credential: "fixture-service-token",
    settings: {
      ingress: { host: "127.0.0.1", port, routes },
      secrets: { OLD: "old-fixture-token", NEW: "new-fixture-token" },
    },
  };
  const path = join(root, "instance.json");
  await atomicJson(path, config);
  // Exercise the actual HTTP handler without connecting to Hive or running scripts.
  t.mock.method(ServiceClient.prototype, "start", () => undefined);
  runtime = await startDataCollector(path);
  const task = taskSchema.parse({
    id: "fixture-task",
    name: "Ingress fixture",
    enabled: true,
    intervalSeconds: 0,
    timeoutSeconds: 5,
    memoryMb: 64,
    script: "export default async () => ({data:{}});",
    dependencies: {},
    config: {},
    secretNames: [],
    inputSecretName: "OLD",
    retention: {
      maximumCount: 1000,
      maximumAgeDays: 7,
      maximumBytes: 104857600,
    },
    ...taskFields,
  });
  runtime.engine.update({
    action: "save",
    task,
    expectedRevision: 0,
    operationId: randomUUID(),
  });
  return {
    runtime,
    task,
    port,
    restart: async () => {
      await runtime!.close();
      runtime = await startDataCollector(path);
      return runtime;
    },
  };
}

function beginRequest(
  port: number,
  token: string | null = "old-fixture-token",
) {
  let request: ClientRequest | undefined;
  const response = new Promise<number>((resolve, reject) => {
    request = httpRequest(
      {
        host: "127.0.0.1",
        port,
        method: "POST",
        path: "/tasks/fixture-task/run",
        headers: {
          ...(token ? { Authorization: "Bearer " + token } : {}),
          "Content-Type": "application/json",
        },
      },
      (incoming) => {
        incoming.resume();
        incoming.once("end", () => resolve(incoming.statusCode ?? 0));
      },
    );
    request.once("error", reject);
  });
  void response.catch(() => undefined);
  request!.write('{"sequence":');
  return { request: request!, response };
}

function observeInitialRead(
  t: TestContext,
  runtime: Awaited<ReturnType<typeof startDataCollector>>,
) {
  const original = runtime.engine.store.get.bind(runtime.engine.store);
  let observed: (() => void) | undefined;
  const ready = new Promise<void>((resolve) => {
    observed = resolve;
  });
  t.mock.method(runtime.engine.store, "get", (id: string) => {
    const row = original(id);
    if (id === "fixture-task") observed!();
    return row;
  });
  return ready;
}

async function post(
  port: number,
  path: string,
  input: unknown,
  options: { token?: string; operationId?: string } = {},
) {
  const response = await fetch(`http://127.0.0.1:${port}${path}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...(options.token ? { Authorization: "Bearer " + options.token } : {}),
      ...(options.operationId
        ? { "Idempotency-Key": options.operationId }
        : {}),
    },
    body: JSON.stringify(input),
    signal: AbortSignal.timeout(5000),
  });
  const body = await response.text();
  return { status: response.status, body: body ? JSON.parse(body) : null };
}

test("Appliances push directly without credentials on the collector port and configured device paths", async (t) => {
  const { runtime, port } = await fixture(
    t,
    {},
    {
      "/api/pushStateWM": "washer",
      "/api/pushStateD": "dryer",
      "/protected": "fixture-task",
    },
  );
  assert.equal(
    runtime.service.ready,
    false,
    "No Hive connection is needed to accept a push",
  );
  const directory = resolve("docs/examples/data-collector/home");
  const script = await readFile(join(directory, "laundry.mjs"), "utf8");
  for (const [name, path, input] of [
    [
      "washer",
      "/api/pushStateWM",
      { cycle_st: 3, tte_showed: 120, timestamp: 1000 },
    ],
    [
      "dryer",
      "/api/pushStateD",
      { cycle_timer: 90, step_timer: 30, tte_showed: 90, timestamp: 1000 },
    ],
  ] as const) {
    const template = JSON.parse(
      await readFile(join(directory, name + ".task.json"), "utf8"),
    );
    runtime.engine.update({
      action: "save",
      task: { ...template, script },
      expectedRevision: 0,
      operationId: randomUUID(),
    });
    assert.equal(
      (await post(port, path, input)).status,
      401,
      "Disabled input tasks reject pushes",
    );
    runtime.engine.update({
      action: "enable",
      id: name,
      expectedRevision: 1,
      operationId: randomUUID(),
    });
    const accepted = await post(port, path, input);
    assert.equal(accepted.status, 202);
    assert.deepEqual(
      runtime.engine.store.run(accepted.body.runId)!.input,
      input,
    );
  }
  assert.equal((await post(port, "/protected", {})).status, 401);
  assert.equal((await post(port, "/tasks/fixture-task/run", {})).status, 401);
  assert.equal((await post(port, "/tasks/missing/run", {})).status, 401);
  assert.equal(runtime.engine.store.runs("queued").length, 2);
});

test("Input stays disabled without an explicit mode and token-protected tasks still require their own token", async (t) => {
  const { runtime, task, port } = await fixture(t);
  const path = "/tasks/fixture-task/run";
  assert.equal((await post(port, path, {}, { token: "wrong" })).status, 401);
  assert.equal((await post(port, path, {})).status, 401);
  assert.equal(
    (await post(port, path, {}, { token: "old-fixture-token" })).status,
    202,
  );
  runtime.engine.update({
    action: "save",
    task: { ...task, id: "no-input", inputSecretName: null },
    expectedRevision: 0,
    operationId: randomUUID(),
  });
  assert.equal((await post(port, "/tasks/no-input/run", {})).status, 401);
  assert.equal(
    (
      await post(
        port,
        "/tasks/no-input/run",
        {},
        { token: "old-fixture-token" },
      )
    ).status,
    401,
  );
  assert.equal(
    taskSchema.safeParse({ ...task, allowUnauthenticatedInput: true }).success,
    false,
  );
  assert.equal(runtime.engine.store.runs("queued").length, 1);
});

test("Anonymous input is durable and idempotent without Hive, with one outstanding observation per task", async (t) => {
  const f = await fixture(t, {
    inputSecretName: null,
    allowUnauthenticatedInput: true,
  });
  const path = "/tasks/fixture-task/run";
  const options = { operationId: "offline-observation" };
  const accepted = await post(f.port, path, { sequence: 1 }, options);
  assert.equal(accepted.status, 202);
  const restarted = await f.restart();
  assert.equal(restarted.service.ready, false);
  assert.deepEqual(
    await post(f.port, path, { sequence: 1 }, options),
    accepted,
  );
  assert.equal((await post(f.port, path, { sequence: 2 })).status, 409);
  await restarted.engine.tick();
  const queued = restarted.engine.store.runs("queued");
  assert.equal(queued.length, 1);
  assert.deepEqual(queued[0]!.input, { sequence: 1 });
});

test("Anonymous input rechecks a task switched to token authentication while the body is arriving", async (t) => {
  const { runtime, task, port } = await fixture(t, {
    inputSecretName: null,
    allowUnauthenticatedInput: true,
  });
  const observed = observeInitialRead(t, runtime);
  const pending = beginRequest(port, null);
  t.after(() => pending.request.destroy());
  await observed;
  runtime.engine.update({
    action: "save",
    task: { ...task, allowUnauthenticatedInput: false, inputSecretName: "NEW" },
    expectedRevision: 1,
    operationId: randomUUID(),
  });
  pending.request.end("1}");
  assert.equal(await pending.response, 401);
  assert.deepEqual(runtime.engine.store.runs("queued"), []);
  assert.equal(
    (
      await post(
        port,
        "/tasks/fixture-task/run",
        { sequence: 2 },
        { token: "new-fixture-token" },
      )
    ).status,
    202,
  );
});

test(
  "Ingress rejects a request disabled after its initial authentication",
  { timeout: 15000 },
  async (t) => {
    const { runtime, port } = await fixture(t);
    const observed = observeInitialRead(t, runtime);
    const pending = beginRequest(port);
    t.after(() => pending.request.destroy());
    await observed;
    runtime.engine.update({
      action: "disable",
      id: "fixture-task",
      expectedRevision: 1,
      operationId: randomUUID(),
    });
    pending.request.end("1}");
    assert.equal(await pending.response, 401);
    assert.deepEqual(runtime.engine.store.runs("queued"), []);
  },
);

test(
  "Ingress rejects the old secret after a task's input token changes",
  { timeout: 15000 },
  async (t) => {
    const { runtime, task, port } = await fixture(t);
    const observed = observeInitialRead(t, runtime);
    const pending = beginRequest(port);
    t.after(() => pending.request.destroy());
    await observed;
    runtime.engine.update({
      action: "save",
      task: { ...task, inputSecretName: "NEW" },
      expectedRevision: 1,
      operationId: randomUUID(),
    });
    pending.request.end("1}");
    assert.equal(await pending.response, 401);
    assert.deepEqual(runtime.engine.store.runs("queued"), []);
    const current = beginRequest(port, "new-fixture-token");
    current.request.end("2}");
    assert.equal(await current.response, 202);
    assert.deepEqual(runtime.engine.store.runs("queued")[0]!.input, {
      sequence: 2,
    });
  },
);

test(
  "Ingress rejects a changed task revision even when the token remains valid",
  { timeout: 15000 },
  async (t) => {
    const { runtime, task, port } = await fixture(t);
    const observed = observeInitialRead(t, runtime);
    const pending = beginRequest(port);
    t.after(() => pending.request.destroy());
    await observed;
    runtime.engine.update({
      action: "save",
      task: { ...task, config: { account: "replacement" } },
      expectedRevision: 1,
      operationId: randomUUID(),
    });
    pending.request.end("1}");
    assert.equal(await pending.response, 401);
    assert.deepEqual(runtime.engine.store.runs("queued"), []);
  },
);
