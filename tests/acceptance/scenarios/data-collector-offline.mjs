import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { once } from "node:events";
import {
  mkdir,
  mkdtemp,
  readFile,
  rename,
  rm,
  writeFile,
} from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as delay } from "node:timers/promises";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

// Exercise exact installed/prepared artifacts against a separate loopback Hive.
// The only external effects are append-only marker files inside this fixture.
const { values } = parseArgs({
  options: Object.fromEntries(
    ["collector", "hive", "report"].map((key) => [key, { type: "string" }]),
  ),
});
for (const key of ["collector", "hive", "report"])
  assert.ok(values[key], "Missing --" + key);
const collector = resolve(values.collector),
  hive = resolve(values.hive);
const load = (root, path) =>
  import(pathToFileURL(join(root, "dist", path)).href);
const { HiveServer } = await load(hive, "services/hive/src/server.js");
const { HiveClient, serviceTools, newOperationId } = await load(
  collector,
  "packages/sdk/src/client.js",
);
const { digest } = await load(collector, "packages/contracts/src/canonical.js");
const { atomicJson } = await load(
  collector,
  "packages/host-runtime/src/config.js",
);
const build = JSON.parse(
  await readFile(join(collector, "dist/build-info.json"), "utf8"),
);
const hiveBuild = JSON.parse(
  await readFile(join(hive, "dist/build-info.json"), "utf8"),
);
assert.equal(build.componentId, "data-collector");
const parent = resolve(tmpdir()),
  root = await mkdtemp(join(parent, "ivy-collector-offline-"));
const report = {
  schemaVersion: 1,
  platform: process.platform,
  collector: build,
  hive: hiveBuild,
  startedAt: new Date().toISOString(),
  checks: [],
  processes: [],
};
let server,
  child,
  exited,
  db,
  client,
  childOutput = "";
async function until(check, detail, timeout = 40000) {
  const deadline = Date.now() + timeout;
  while (!(await check())) {
    assert.ok(Date.now() < deadline, detail + " timed out. " + childOutput);
    await delay(75);
  }
}
async function port() {
  const server = createServer().listen(0, "127.0.0.1");
  await once(server, "listening");
  const value = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return value;
}
const hivePort = await port(),
  ingressPort = await port(),
  token = randomUUID();
const base = `http://127.0.0.1:${hivePort}/ivy`,
  dataRoot = join(root, "collector"),
  configPath = join(root, "collector.json");
await mkdir(dataRoot);
await atomicJson(configPath, {
  schemaVersion: 1,
  componentId: "data-collector",
  instanceId: "acceptance",
  serviceNodeId: "acceptance.collector",
  hostId: "acceptance",
  publicBaseUrl: base,
  artifactRoot: collector,
  dataRoot,
  workRoot: join(root, "work"),
  version: build.version,
  buildId: build.buildId,
  credential: token,
  settings: {
    rootName: "Isolated acceptance results",
    secrets: { INPUT: "isolated-input-token" },
    ingress: { host: "127.0.0.1", port: ingressPort },
  },
});
async function openHive() {
  server = new HiveServer({
    filename: join(root, "hive.sqlite"),
    publicBaseUrl: base,
    credentials: [{ principalId: "acceptance", digest: digest(token) }],
    version: hiveBuild.version,
    buildId: hiveBuild.buildId,
    listenHost: "127.0.0.1",
    listenPort: hivePort,
  });
  await server.start();
  client = new HiveClient(base, { credential: token });
}
async function launch() {
  child = spawn(
    process.execPath,
    [
      join(collector, "dist/services/data-collector/src/main.js"),
      "--config",
      configPath,
    ],
    {
      cwd: collector,
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, IVY_LAUNCH_ID: randomUUID() },
    },
  );
  childOutput = "";
  child.stdout.on("data", (bytes) => {
    childOutput = (childOutput + bytes).slice(-4000);
  });
  child.stderr.on("data", (bytes) => {
    childOutput = (childOutput + bytes).slice(-4000);
  });
  exited = new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  await until(async () => {
    try {
      return (
        JSON.parse(await readFile(join(dataRoot, "health.json"), "utf8"))
          .pid === child.pid
      );
    } catch {
      return false;
    }
  }, "collector process startup");
  report.processes.push(child.pid);
}
async function stop() {
  if (!child || child.exitCode !== null) return;
  const health = JSON.parse(
    await readFile(join(dataRoot, "health.json"), "utf8"),
  );
  assert.equal(health.pid, child.pid);
  await atomicJson(join(dataRoot, "control.json"), {
    schemaVersion: 1,
    instanceId: "acceptance",
    launchId: health.launchId,
    bootId: health.bootId,
    action: "shutdown",
    requestedAt: new Date().toISOString(),
  });
  const outcome = await Promise.race([
    exited,
    delay(20000).then(() => {
      throw new Error("Collector shutdown did not finish.");
    }),
  ]);
  assert.deepEqual(outcome, { code: 0, signal: null }, childOutput);
  child = null;
}
async function ready() {
  await until(async () => {
    try {
      const node = await client.request("serviceNodes.get", {
        serviceNodeId: "acceptance.collector",
      });
      return node.ready && node.connected && node.buildId === build.buildId;
    } catch {
      return false;
    }
  }, "collector registration");
}
function row(id) {
  const value = db.prepare("SELECT document FROM tasks WHERE id=?").get(id);
  return value ? JSON.parse(value.document) : null;
}
function runs(status) {
  return db
    .prepare("SELECT document FROM runs WHERE status=? ORDER BY rowid")
    .all(status)
    .map((r) => JSON.parse(r.document));
}
const active = () => runs("running").length + runs("queued").length;
async function markers(id) {
  try {
    return (await readFile(join(root, id + ".jsonl"), "utf8"))
      .trim()
      .split("\n")
      .filter(Boolean)
      .map(JSON.parse);
  } catch (e) {
    if (e.code === "ENOENT") return [];
    throw e;
  }
}
const read = (input) =>
  serviceTools(client, "acceptance.collector", [
    { namespace: "data-collector", interfaceVersion: "1.0.0" },
  ]).call("data-collector.read", input);
async function update(input) {
  const operationId = await newOperationId(client);
  return serviceTools(client, "acceptance.collector", [
    { namespace: "data-collector", interfaceVersion: "1.0.0" },
  ]).call("data-collector.update", { ...input, operationId }, operationId);
}
async function push(sequence, key) {
  const response = await fetch(
    `http://127.0.0.1:${ingressPort}/tasks/input/run`,
    {
      method: "POST",
      headers: {
        Authorization: "Bearer isolated-input-token",
        "Content-Type": "application/json",
        "Idempotency-Key": key,
      },
      body: JSON.stringify({ sequence }),
      signal: AbortSignal.timeout(5000),
    },
  );
  assert.equal(response.status, 202);
  return response.json();
}
const script = `import {appendFile} from 'node:fs/promises';
import {setTimeout as delay} from 'node:timers/promises';
export default async ({config,state,input,signal}) => {
  const count=(typeof state==='number'?state:0)+1;
  await appendFile(config.marker, JSON.stringify({count,input,tag:config.tag})+'\\n');
  await delay(config.waitMs, undefined, {signal});
  return {data:{count,input,tag:config.tag},state:count,events:[{name:'observed',payload:{count,tag:config.tag}}]};
};`;
const task = (id) => ({
  id,
  name: id,
  enabled: false,
  intervalSeconds: id === "periodic" ? 2 : 0,
  timeoutSeconds: 15,
  memoryMb: 64,
  dependencies: {},
  script,
  config: {
    marker: join(root, id + ".jsonl"),
    tag: 1,
    waitMs: id === "periodic" ? 1500 : 0,
  },
  secretNames: [],
  ...(id === "input" ? { inputSecretName: "INPUT" } : {}),
  retention: { maximumCount: 1000, maximumAgeDays: 7, maximumBytes: 104857600 },
});
try {
  await openHive();
  await launch();
  await ready();
  db = new DatabaseSync(join(dataRoot, "data-collector.sqlite"), {
    readOnly: true,
  });
  const saved = {};
  for (const id of ["periodic", "input"]) {
    saved[id] = await update({
      action: "save",
      expectedRevision: 0,
      task: task(id),
    });
    assert.equal(row(id), null, "saving does not activate");
    await update({
      action: "activate",
      jobId: saved[id].jobId,
      definitionRevision: 1,
      expectedRevision: 0,
      enabled: true,
    });
  }
  await until(
    async () =>
      (await markers("periodic")).length >= 1 && runs("running").length > 0,
    "an actual worker starts online",
  );
  await server.close();
  server = null;
  const before = (await markers("periodic")).length;
  const firstInput = await push(1, "input-one");
  assert.deepEqual(await push(1, "input-one"), firstInput);
  await until(
    () =>
      row("periodic").state >= before + 2 &&
      row("input").state === 1 &&
      active() === 0,
    "in-flight and later offline intervals finish",
  );
  const beforeRestart = row("periodic").state,
    pendingBeforeRestart = runs("publishing").map((r) => r.id);
  assert.ok(pendingBeforeRestart.length >= 4);
  await stop();
  await launch();
  assert.notEqual(report.processes[0], report.processes[1]);
  await until(
    () => row("periodic").state >= beforeRestart + 2 && active() === 0,
    "new offline process executes cached intervals",
  );
  assert.deepEqual(
    await push(1, "input-one"),
    firstInput,
    "HTTP receipt survives process restart",
  );
  await push(2, "input-two");
  await until(
    () => row("input").state === 2 && active() === 0,
    "HTTP input executes while Hive stays unavailable",
  );
  report.offline = {
    periodicBeforeRestart: beforeRestart,
    periodicAfterRestart: row("periodic").state,
    inputExecutions: row("input").state,
    pendingResults: runs("publishing").length,
  };
  await openHive();
  await ready();
  await until(
    () => runs("publishing").length === 0 && active() === 0,
    "outbox drains after Hive epoch change",
  );
  const selected = await read({
    view: "definition",
    jobId: saved.periodic.jobId,
  });
  await update({
    action: "activate",
    jobId: saved.periodic.jobId,
    definitionRevision: 1,
    expectedRevision: selected.assignment.revision.revision,
    enabled: false,
  });
  await until(
    () => active() === 0 && runs("publishing").length === 0,
    "periodic acceptance fixture is paused",
  );
  for (const id of ["periodic", "input"]) {
    const effects = await markers(id);
    assert.deepEqual(
      effects.map((effect) => effect.count),
      effects.map((_, index) => index + 1),
      "external markers are never replayed",
    );
    assert.equal(effects.length, row(id).state);
    const result = await read({ view: "result", id });
    assert.equal(result.content.value.data.count, effects.length);
  }
  const events = await client.request("events.read", {
    afterSequence: 0,
    filter: { topics: ["data-collector.event"] },
    limit: 100,
  });
  const completed = runs("succeeded");
  assert.equal(events.items.length, completed.length);
  assert.equal(
    new Set(
      events.items.map((e) => e.payload.runId + ":" + e.payload.eventIndex),
    ).size,
    completed.length,
  );
  assert.ok(
    pendingBeforeRestart.every((id) => completed.some((r) => r.id === id)),
  );
  report.reconnected = {
    completedRuns: completed.length,
    uniqueEvents: events.items.length,
    publicationsPending: 0,
  };
  const resultId = row("periodic").resultObjectId;
  const draft = await update({
    action: "save",
    expectedRevision: 1,
    jobId: saved.periodic.jobId,
    task: {
      ...task("periodic"),
      config: { ...task("periodic").config, tag: 2, waitMs: 0 },
    },
  });
  assert.equal(row("periodic").task.config.tag, 1);
  const desired = await read({
    view: "definition",
    jobId: saved.periodic.jobId,
  });
  await update({
    action: "activate",
    jobId: saved.periodic.jobId,
    definitionRevision: draft.revision,
    expectedRevision: desired.assignment.revision.revision,
    enabled: false,
  });
  await until(
    () => row("periodic").definition.revision === 2,
    "new definition adoption",
  );
  const run = await update({ action: "run", id: "periodic" });
  await until(
    () => runs("succeeded").some((r) => r.id === run.runId),
    "new execution revision publishes",
  );
  assert.equal(
    (await read({ view: "result", id: "periodic" })).content.value.data.tag,
    2,
  );
  const latest = await read({
    view: "definition",
    jobId: saved.periodic.jobId,
  });
  await update({
    action: "activate",
    jobId: saved.periodic.jobId,
    definitionRevision: 1,
    expectedRevision: latest.assignment.revision.revision,
    enabled: false,
  });
  await until(
    () => row("periodic").definition.revision === 1,
    "full execution rollback",
  );
  assert.deepEqual(row("periodic").task.config, task("periodic").config);
  assert.equal(row("periodic").resultObjectId, resultId);
  await until(
    async () =>
      (await read({ view: "definition", jobId: saved.periodic.jobId }))
        .observation?.content.value.accepted === true,
    "adoption acknowledgement",
  );
  await stop();
  db.close();
  db = null;
  await rename(
    join(dataRoot, "data-collector.sqlite"),
    join(root, "discarded-cache.sqlite"),
  );
  for (const suffix of ["-wal", "-shm"])
    await rm(join(dataRoot, "data-collector.sqlite" + suffix), { force: true });
  await launch();
  await ready();
  await until(
    async () =>
      (await read({ view: "task", id: "periodic" })).adoptionError ===
      "recovery_required",
    "lost-cache recovery fence",
  );
  const recovery = await read({
    view: "definition",
    jobId: saved.periodic.jobId,
  });
  await update({
    action: "recover",
    jobId: saved.periodic.jobId,
    expectedRevision: recovery.assignment.revision.revision,
    confirmedStopped: true,
  });
  db = new DatabaseSync(join(dataRoot, "data-collector.sqlite"), {
    readOnly: true,
  });
  assert.equal(
    row("periodic").state,
    null,
    "source recovery cannot recreate a lost cursor",
  );
  assert.equal(row("periodic").task.enabled, false);
  report.checks = [
    "real interval worker",
    "in-flight completion during Hive outage",
    "subsequently due offline runs",
    "full collector process restart while Hive unavailable",
    "authenticated offline HTTP and durable idempotency",
    "ordered result and event recovery without external marker replay",
    "draft / activation / complete rollback",
    "lost SQLite cache fences resumption; explicit stopped-worker recovery",
  ];
  report.status = "passed";
} catch (error) {
  report.status = "failed";
  report.failure = {
    name: error.name,
    code: error.code,
    message: error.message,
  };
  process.exitCode = 1;
} finally {
  try {
    await stop();
  } catch (error) {
    report.cleanupError = error.message;
    process.exitCode = 1;
  }
  db?.close();
  await server?.close();
  if (report.status === "passed" && !report.cleanupError) {
    assert.equal(dirname(root), parent);
    assert.ok(root.startsWith(join(parent, "ivy-collector-offline-")));
    await rm(root, {
      recursive: true,
      force: true,
      maxRetries: 10,
      retryDelay: 100,
    });
  } else report.retainedFixture = root;
  report.completedAt = new Date().toISOString();
  await writeFile(
    resolve(values.report),
    JSON.stringify(report, null, 2) + "\n",
    { mode: 0o600 },
  );
  console.log(JSON.stringify(report));
}
