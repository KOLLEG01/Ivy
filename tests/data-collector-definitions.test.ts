import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:net";
import { once } from "node:events";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { randomUUID } from "node:crypto";
import { digest, HiveClient, IvyError } from "../packages/sdk/src/node.js";
import type {
  Operation,
  Params,
  RpcClient,
  Wire,
} from "../packages/sdk/src/node.js";
import { atomicJson } from "../packages/sdk/src/host.js";
import { HiveServer } from "../services/hive/src/server.js";
import { startDataCollector } from "../services/data-collector/src/main.js";
import {
  defaultRetention,
  taskSchema,
} from "../services/data-collector/src/schema.js";
import type { Task } from "../services/data-collector/src/schema.js";

type Runtime = Awaited<ReturnType<typeof startDataCollector>>;
type Saved = { jobId: string; revision: number };
type Activated = { assignment: { objectId: string; revision: number } };
const delay = (ms = 20) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(
  check: () => boolean | Promise<boolean>,
  detail: string,
  timeout = 12000,
) {
  const deadline = Date.now() + timeout;
  while (!(await check())) {
    assert.ok(Date.now() < deadline, detail);
    await delay();
  }
}
const task = (patch: Partial<Task> = {}): Task =>
  taskSchema.parse({
    id: "observation",
    name: "Observation",
    enabled: true,
    intervalSeconds: 0,
    timeoutSeconds: 5,
    memoryMb: 64,
    script: "export default async()=>({data:1})",
    dependencies: {},
    config: { version: 1 },
    secretNames: [],
    retention: defaultRetention,
    ...patch,
  });
const action = (runtime: Runtime, input: object) =>
  runtime.definitions.update({ operationId: randomUUID(), ...input });
async function fixture(t: TestContext) {
  const parent = resolve(process.env.IVY_TEST_TEMP ?? tmpdir()),
    root = await mkdtemp(join(parent, "ivy-collector-central-"));
  const reservation = createServer().listen(0, "127.0.0.1");
  await once(reservation, "listening");
  const port = (reservation.address() as { port: number }).port;
  await new Promise<void>((resolve) => reservation.close(() => resolve()));
  const base = `http://127.0.0.1:${port}/ivy`,
    token = "central-collector-fixture";
  let server: HiveServer | null = null;
  const runtimes: Runtime[] = [],
    paths: string[] = [],
    executions = [0, 0];
  async function openHive() {
    server = new HiveServer({
      filename: join(root, "hive.sqlite"),
      publicBaseUrl: base,
      credentials: [{ principalId: "fixture", digest: digest(token) }],
      version: "fixture",
      buildId: digest("central-collector"),
      listenHost: "127.0.0.1",
      listenPort: port,
    });
    await server.start();
  }
  t.after(async () => {
    for (const runtime of runtimes) await runtime.close();
    await server?.close();
    assert.equal(dirname(root), parent);
    await rm(root, { recursive: true, force: true });
  });
  await openHive();
  const build = JSON.parse(await readFile("dist/build-info.json", "utf8"));
  function instrument(runtime: Runtime, index: number) {
    runtime.engine.runner.run = async (_task, state, input) => {
      executions[index] = executions[index]! + 1;
      const count = typeof state === "number" ? state + 1 : 1;
      return {
        data: { count, input: input as Wire.Json },
        state: count,
        events: [{ name: "observed", payload: { count } }],
      };
    };
  }
  for (const index of [0, 1]) {
    const path = join(root, "host-" + index + ".json");
    paths.push(path);
    await atomicJson(path, {
      schemaVersion: 1,
      componentId: "data-collector",
      instanceId: "collector",
      serviceNodeId: "host-" + index + ".collector",
      hostId: "host-" + index,
      publicBaseUrl: base,
      version: build.version,
      buildId: build.buildId,
      dataRoot: join(root, "data-" + index),
      workRoot: join(root, "work-" + index),
      artifactRoot: resolve("."),
      credential: token,
      settings: {
        rootName: "Results " + index,
        secrets: { KNOWN: "do-not-archive-this-token" },
      },
    });
    const runtime = await startDataCollector(path);
    instrument(runtime, index);
    runtimes.push(runtime);
    await runtime.service.waitReady({ timeoutMs: 15000 });
  }
  return {
    root,
    runtimes,
    executions,
    client: new HiveClient(base, { credential: token }),
    disconnect: async () => {
      await server!.close();
      server = null;
      await until(
        () => runtimes.every((r) => !r.service.ready),
        "both instances observe disconnection",
      );
    },
    reconnect: async () => {
      await openHive();
      await Promise.all(
        runtimes.map((r) => r.service.waitReady({ timeoutMs: 20000 })),
      );
    },
    restart: async (index: number) => {
      await runtimes[index]!.close();
      const runtime = await startDataCollector(paths[index]!);
      instrument(runtime, index);
      runtimes[index] = runtime;
      return runtime;
    },
  };
}

test("Central drafts, activation, full rollback, shared scripts and stale edits remain distinct", async (t) => {
  const rig = await fixture(t),
    runtime = rig.runtimes[0]!,
    other = rig.runtimes[1]!;
  const saved = (await action(runtime, {
    action: "save",
    task: task(),
    expectedRevision: 0,
  })) as Saved;
  assert.equal(
    runtime.engine.store.get("observation"),
    null,
    "saving never activates a new task",
  );
  const first = (await action(runtime, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: saved.revision,
    expectedRevision: 0,
    enabled: true,
  })) as Activated;
  assert.deepEqual(runtime.engine.store.get("observation")!.task.config, {
    version: 1,
  });
  const next = (await action(runtime, {
    action: "save",
    jobId: saved.jobId,
    task: task({
      config: { version: 2 },
      script: "export default async()=>({data:2})",
    }),
    expectedRevision: saved.revision,
  })) as Saved;
  assert.deepEqual(
    runtime.engine.store.get("observation")!.task.config,
    { version: 1 },
    "drafts do not replace the accepted execution",
  );
  await assert.rejects(
    action(runtime, {
      action: "save",
      jobId: saved.jobId,
      task: task(),
      expectedRevision: saved.revision,
    }),
    (e) => e instanceof IvyError && e.code === "revision_conflict",
  );
  const second = (await action(runtime, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: next.revision,
    expectedRevision: first.assignment.revision,
    enabled: true,
  })) as Activated;
  assert.equal(
    runtime.engine.store.get("observation")!.task.script,
    "export default async()=>({data:2})",
  );
  await action(runtime, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: saved.revision,
    expectedRevision: second.assignment.revision,
    enabled: true,
  });
  assert.deepEqual(runtime.engine.store.get("observation")!.task.config, {
    version: 1,
  });
  assert.equal(
    runtime.engine.store.get("observation")!.definition!.revision,
    saved.revision,
  );
  const definition = (await runtime.definitions.read({
    view: "definition",
    jobId: saved.jobId,
    revision: saved.revision,
  })) as { task: Task };
  const scriptRead = await rig.client.request("objects.read", {
    objectId: saved.jobId,
    revision: saved.revision,
  });
  const source = (
    scriptRead.content as {
      encoding: "json";
      value: { script: { objectId: string; revision: number } };
    }
  ).value.script;
  const reused = (await action(other, {
    action: "save",
    task: { ...definition.task, id: "independent-job" },
    expectedRevision: 0,
    scriptRef: source,
  })) as Saved;
  assert.notEqual(reused.jobId, saved.jobId);
  await assert.rejects(
    action(runtime, {
      action: "save",
      task: task({
        id: "secret",
        config: { token: "do-not-archive-this-token" },
      }),
      expectedRevision: 0,
    }),
    (e) => e instanceof IvyError && e.code === "invalid_arguments",
  );
});

test("Unchanged adoption observations do not consume receipts after time passes or a restart", async (t) => {
  const rig = await fixture(t);
  let runtime = rig.runtimes[0]!;
  const saved = (await action(runtime, {
    action: "save",
    task: task(),
    expectedRevision: 0,
  })) as Saved;
  const assignment = (await action(runtime, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: saved.revision,
    expectedRevision: 0,
    enabled: false,
  })) as Activated;
  const observation = async () =>
    (
      (await runtime.definitions.read({
        view: "definition",
        jobId: saved.jobId,
      })) as { observation: Operation.ObjectRead }
    ).observation;
  await runtime.definitions.sync();
  const original = await observation();
  t.mock.timers.enable({ apis: ["Date"], now: Date.now() + 31000 });
  await runtime.definitions.sync();
  assert.deepEqual(await observation(), original);
  runtime = await rig.restart(0);
  await runtime.service.waitReady({ timeoutMs: 15000 });
  await runtime.definitions.sync();
  assert.deepEqual(await observation(), original);
  await action(runtime, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: saved.revision,
    expectedRevision: assignment.assignment.revision,
    enabled: true,
  });
  await runtime.definitions.sync();
  const changed = await observation();
  assert.equal(changed.revision.revision, original.revision.revision + 1);
  assert.equal(
    (changed.content.value as { quiesced: boolean }).quiesced,
    false,
  );
});

test("Offline due runs and a disconnected restart advance durable state then publish in order without reexecution", async (t) => {
  const rig = await fixture(t);
  let runtime = rig.runtimes[0]!;
  const saved = (await action(runtime, {
    action: "save",
    task: task({ intervalSeconds: 1 }),
    expectedRevision: 0,
  })) as Saved;
  await action(runtime, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: saved.revision,
    expectedRevision: 0,
    enabled: true,
  });
  await until(
    () => runtime.engine.store.runs("succeeded").length >= 1,
    "initial online observation publishes",
  );
  const resultId = runtime.engine.store.get("observation")!.resultObjectId;
  await rig.disconnect();
  const before = rig.executions[0]!;
  await until(
    () => rig.executions[0]! >= before + 2,
    "new due runs continue while disconnected",
  );
  assert.ok(runtime.engine.store.runs("publishing").length >= 2);
  assert.equal(
    runtime.engine.store.get("observation")!.state,
    rig.executions[0],
  );
  runtime = await rig.restart(0);
  const restarted = rig.executions[0]!;
  await until(
    () => rig.executions[0]! > restarted,
    "cached schedule runs after restart without Hive",
  );
  const pending = runtime.engine.store.runs("publishing").map((r) => ({
    id: r.id,
    count: (r.output!.data as { count: number }).count,
  }));
  assert.ok(
    pending.every((r, i) => i === 0 || r.count > pending[i - 1]!.count),
  );
  const performed = rig.executions[0]!;
  // Keep the cached definition but prevent a new scheduled execution during publication verification.
  const row = runtime.engine.store.get("observation")!;
  row.nextAt = Date.now() + 60000;
  runtime.engine.store.save(row);
  await rig.reconnect();
  await until(
    () => runtime.engine.pendingPublications() === 0,
    "all offline results and events publish after reconnect",
  );
  assert.equal(
    rig.executions[0],
    performed,
    "publishing never reruns a script",
  );
  assert.equal(
    runtime.engine.store.get("observation")!.resultObjectId,
    resultId,
  );
  const result = await rig.client.request("objects.read", {
    objectId: resultId!,
  });
  assert.equal(
    (result.content as unknown as { value: { data: { count: number } } }).value
      .data.count,
    performed,
  );
  assert.equal(runtime.engine.store.get("observation")!.state, performed);
});

test("A missing cache requires explicit stopped-worker recovery; an unreachable old assignment cannot fail over", async (t) => {
  const rig = await fixture(t),
    other = rig.runtimes[1]!;
  let runtime = rig.runtimes[0]!;
  const saved = (await action(runtime, {
    action: "save",
    task: task(),
    expectedRevision: 0,
  })) as Saved;
  const first = (await action(runtime, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: saved.revision,
    expectedRevision: 0,
    enabled: true,
  })) as Activated;
  await runtime.definitions.sync();
  await assert.rejects(
    action(other, {
      action: "activate",
      jobId: saved.jobId,
      definitionRevision: saved.revision,
      expectedRevision: first.assignment.revision,
      enabled: true,
      serviceNodeId: "host-1.collector",
    }),
    (e) => e instanceof IvyError && e.code === "outcome_unknown",
  );
  runtime.engine.store.db
    .prepare("DELETE FROM tasks WHERE id=?")
    .run("observation");
  await runtime.definitions.sync();
  assert.equal(runtime.engine.store.get("observation"), null);
  await runtime.definitions.sync();
  assert.equal(
    runtime.engine.store.get("observation"),
    null,
    "a rejected observation does not erase the recovery fence",
  );
  const status = (await runtime.definitions.read({
    view: "definition",
    jobId: saved.jobId,
  })) as { observation: Operation.ObjectRead };
  assert.equal(
    (status.observation.content as unknown as { value: { error: string } })
      .value.error,
    "recovery_required",
  );
  await action(runtime, {
    action: "recover",
    jobId: saved.jobId,
    expectedRevision: first.assignment.revision,
    confirmedStopped: true,
  });
  assert.equal(
    runtime.engine.store.get("observation")!.task.script,
    task().script,
  );
  const row = runtime.engine.store.get("observation")!;
  row.state = 41;
  row.nextAt = Date.now() + 60000;
  runtime.engine.store.save(row);
  await action(runtime, { action: "run", id: "observation" });
  await until(
    () =>
      runtime.engine.store.get("observation")!.state === 42 &&
      runtime.engine.pendingPublications() === 0,
    "runtime state and result exist before handoff",
  );
  await runtime.definitions.sync();
  const bound = (await runtime.definitions.read({
    view: "definition",
    jobId: saved.jobId,
  })) as { assignment: Operation.ObjectRead };
  const resultId = runtime.engine.store.get("observation")!.resultObjectId;
  const stopped = (await action(runtime, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: saved.revision,
    expectedRevision: bound.assignment.revision.revision,
    enabled: false,
  })) as Activated;
  await runtime.definitions.sync();
  await action(runtime, {
    action: "freezeHandoff",
    jobId: saved.jobId,
    expectedRevision: stopped.assignment.revision,
  });
  await assert.rejects(
    action(runtime, { action: "run", id: "observation" }),
    (e) => e instanceof IvyError && e.code === "task_busy",
  );
  const auth = join(runtime.engine.runner.workRoot, "observation", ".auth"),
    targetAuth = join(other.engine.runner.workRoot, "observation", ".auth");
  await mkdir(auth, { recursive: true });
  await writeFile(join(auth, "session"), "fixture-private-authentication");
  await assert.rejects(
    action(other, {
      action: "activate",
      jobId: saved.jobId,
      definitionRevision: saved.revision,
      expectedRevision: stopped.assignment.revision,
      enabled: true,
      serviceNodeId: "host-1.collector",
    }),
    (e) => e instanceof IvyError && e.code === "authentication_required",
  );
  await runtime.close();
  await assert.rejects(
    action(other, {
      action: "activate",
      jobId: saved.jobId,
      definitionRevision: saved.revision,
      expectedRevision: stopped.assignment.revision,
      enabled: true,
      serviceNodeId: "host-1.collector",
    }),
    "an old idle acknowledgement cannot substitute for contacting the frozen source",
  );
  const unchanged = (await other.definitions.read({
    view: "definition",
    jobId: saved.jobId,
  })) as { assignment: Operation.ObjectRead };
  assert.equal(
    unchanged.assignment.revision.revision,
    stopped.assignment.revision,
  );
  runtime = await rig.restart(0);
  await runtime.service.waitReady({ timeoutMs: 20000 });
  await mkdir(targetAuth, { recursive: true });
  await writeFile(
    join(targetAuth, "session"),
    "fixture-private-authentication",
  );
  const nextAt = runtime.engine.store.get("observation")!.nextAt;
  await action(other, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: saved.revision,
    expectedRevision: stopped.assignment.revision,
    enabled: true,
    serviceNodeId: "host-1.collector",
  });
  await other.definitions.sync();
  assert.equal(runtime.engine.store.get("observation")!.task.enabled, false);
  assert.equal(other.engine.store.get("observation")!.task.enabled, true);
  assert.equal(other.engine.store.get("observation")!.state, 42);
  assert.equal(other.engine.store.get("observation")!.nextAt, nextAt);
  assert.equal(other.engine.store.get("observation")!.resultObjectId, resultId);
});

test("Lost control-write replies reconcile across Hive restart without duplicate definitions or script revisions", async (t) => {
  const rig = await fixture(t),
    runtime = rig.runtimes[0]!;
  const original = runtime.engine.client!;
  let lose = "definition";
  const lossy: RpcClient = {
    async request(method, params, options) {
      const response = await original.request(method, params, options);
      if (method === "objects.write") {
        const write = params as Params<"objects.write">;
        if (
          write.create?.contractKey === "data-collector/" + lose ||
          (lose === "script-update" && write.objectId)
        ) {
          lose = "";
          throw new IvyError(
            "connection_lost",
            "Fixture lost the committed reply.",
            "unknown",
          );
        }
      }
      return response;
    },
  };
  runtime.engine.client = lossy;
  const request = {
    action: "save",
    task: task(),
    expectedRevision: 0,
    operationId: randomUUID(),
  };
  await assert.rejects(action(runtime, request));
  await rig.disconnect();
  await rig.reconnect();
  const saved = (await action(runtime, request)) as Saved;
  const definitions = await rig.client.request("objects.query", {
    contractKey: "data-collector/definition",
    limit: 100,
  });
  assert.equal(definitions.items.length, 1);
  const document = await rig.client.request("objects.read", {
    objectId: saved.jobId,
  });
  const source = (
    document.content as unknown as {
      value: { script: { objectId: string; revision: number } };
    }
  ).value.script;
  const update = {
    action: "saveScript",
    objectId: source.objectId,
    expectedRevision: source.revision,
    script: "export default async()=>({data:2})",
    operationId: randomUUID(),
  };
  const connected = runtime.engine.client!;
  const lostScript: RpcClient = {
    async request(method, params, options) {
      const response = await connected.request(method, params, options);
      if (
        method === "objects.write" &&
        (params as Params<"objects.write">).objectId === source.objectId &&
        lose
      ) {
        lose = "";
        throw new IvyError(
          "connection_lost",
          "Fixture lost the committed reply.",
          "unknown",
        );
      }
      return response;
    },
  };
  lose = "script-update";
  runtime.engine.client = lostScript;
  await assert.rejects(action(runtime, update));
  runtime.engine.client = connected;
  const retried = (await action(runtime, update)) as Operation.ObjectRead;
  assert.equal(retried.revision.revision, source.revision + 1);
  assert.equal(
    (await rig.client.request("objects.history", { objectId: source.objectId }))
      .items.length,
    2,
  );
});

test("Rejected adoption preserves the old cache and remains visible until disabled or rolled back", async (t) => {
  const rig = await fixture(t),
    runtime = rig.runtimes[0]!;
  const saved = (await action(runtime, {
    action: "save",
    task: task(),
    expectedRevision: 0,
  })) as Saved;
  const first = (await action(runtime, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: saved.revision,
    expectedRevision: 0,
    enabled: true,
  })) as Activated;
  const draft = (await action(runtime, {
    action: "save",
    task: task({ config: { version: 2 }, secretNames: ["UNAVAILABLE"] }),
    expectedRevision: saved.revision,
  })) as Saved;
  const rejected = (await action(runtime, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: draft.revision,
    expectedRevision: first.assignment.revision,
    enabled: true,
  })) as Activated & { adoptionError: string };
  assert.equal(rejected.adoptionError, "secret_missing");
  await runtime.definitions.sync();
  const visible = (await runtime.definitions.read({
    view: "task",
    id: "observation",
  })) as {
    desiredRevision: number;
    adoptedRevision: number;
    adoptionError: string;
  };
  assert.equal(visible.desiredRevision, draft.revision);
  assert.equal(visible.adoptedRevision, saved.revision);
  assert.equal(visible.adoptionError, "secret_missing");
  assert.deepEqual(runtime.engine.store.get("observation")!.task.config, {
    version: 1,
  });
  const paused = (await action(runtime, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: draft.revision,
    expectedRevision: rejected.assignment.revision,
    enabled: false,
  })) as Activated;
  assert.equal(runtime.engine.store.get("observation")!.task.enabled, false);
  await action(runtime, {
    action: "activate",
    jobId: saved.jobId,
    definitionRevision: saved.revision,
    expectedRevision: paused.assignment.revision,
    enabled: true,
  });
  assert.deepEqual(runtime.engine.store.get("observation")!.task.config, {
    version: 1,
  });
  await action(runtime, {
    action: "delete",
    id: "observation",
    expectedRevision: draft.revision,
  });
  assert.equal(runtime.engine.store.get("observation"), null);
  assert.equal(runtime.engine.store.get("observation", true)!.task.script, "");
  assert.equal(
    (
      (await runtime.definitions.read({
        view: "definitionHistory",
        jobId: saved.jobId,
      })) as { items: unknown[] }
    ).items.length,
    2,
  );
  const recreated = (await action(runtime, {
    action: "save",
    task: task(),
    expectedRevision: 0,
  })) as Saved;
  assert.notEqual(recreated.jobId, saved.jobId);
  await action(runtime, {
    action: "activate",
    jobId: recreated.jobId,
    definitionRevision: recreated.revision,
    expectedRevision: 0,
    enabled: false,
  });
  assert.equal(runtime.engine.store.get("observation")!.state, null);
});
