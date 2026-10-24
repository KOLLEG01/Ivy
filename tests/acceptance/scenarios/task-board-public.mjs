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
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { chromium, expect } from "@playwright/test";

// Public manual work, durable discussion and UI against real native packages.
// Native agent execution/resumption is a separate acceptance boundary.
const { values } = parseArgs({
  options: Object.fromEntries(
    [
      "hive",
      "task-board",
      "ui",
      "ui-publisher",
      "report",
      "browser-executable",
    ].map((key) => [key, { type: "string" }]),
  ),
});
for (const key of ["hive", "task-board", "ui", "ui-publisher", "report"])
  assert.ok(values[key], "Missing --" + key);
const { publishUi } = await import(
  pathToFileURL(resolve(values["ui-publisher"])).href
);
const json = async (path) => JSON.parse(await readFile(path, "utf8"));
const parent = resolve(tmpdir()),
  root = await mkdtemp(join(parent, "ivy-task-board-public-"));
const report = {
  platform: process.platform,
  startedAt: new Date().toISOString(),
  checks: [],
  operations: [],
  processes: [],
  nativeAgentExecutionVerified: false,
  status: "running",
};
const reportPath = resolve(values.report);
await writeFile(reportPath, "{}\n", { flag: "wx", mode: 0o600 });
async function atomicJson(path, value) {
  await mkdir(dirname(path), { recursive: true });
  const temporary = path + "." + randomUUID();
  await writeFile(temporary, JSON.stringify(value, null, 2) + "\n", {
    mode: 0o600,
  });
  await rename(temporary, path);
}
const save = () => atomicJson(reportPath, report);
const socket = createServer().listen(0, "127.0.0.1");
await once(socket, "listening");
const port = socket.address().port;
await new Promise((yes) => socket.close(yes));
const base = `http://127.0.0.1:${port}/ivy`,
  token = randomUUID(),
  principal = "acceptance";
const node = "acceptance.task-board",
  packages = new Map(),
  owned = [];
for (const component of ["hive", "task-board"]) {
  const artifact = resolve(values[component]),
    manifest = await json(join(artifact, "manifest.json"));
  assert.equal(manifest.componentId, component);
  const executable = resolve(artifact, manifest.entrypoint.executable),
    child = relative(artifact, executable);
  assert.ok(child && !isAbsolute(child) && child.split(/[\\/]/)[0] !== "..");
  assert.match(executable, new RegExp("ivy-" + component + "(?:\\.exe)?$"));
  const build = await json(join(artifact, "dist/build-info.json"));
  const data = join(root, component),
    path = join(root, component + ".json");
  const settings =
    component === "hive"
      ? {
          listenHost: "127.0.0.1",
          listenPort: port,
          credentials: [{ principalId: principal, token }],
          backup: {
            enabled: false,
            directory: join(root, "backups"),
            intervalHours: 24,
            retain: 2,
          },
        }
      : {
          principalId: principal,
          rootObjectId: null,
          scheduler: { enabled: false, intervalMs: 1000, pageSize: 10 },
          phoneTarget: null,
        };
  await atomicJson(path, {
    schemaVersion: 1,
    componentId: component,
    instanceId: component,
    serviceNodeId: component === "hive" ? "acceptance.hive" : node,
    hostId: "public-acceptance",
    publicBaseUrl: base,
    artifactRoot: artifact,
    dataRoot: data,
    workRoot: join(data, "work"),
    logsRoot: join(data, "logs"),
    version: build.version,
    buildId: build.buildId,
    ...(component === "hive" ? {} : { credential: token }),
    settings,
  });
  packages.set(component, {
    artifact,
    executable,
    args: manifest.entrypoint.args ?? [],
    build,
    data,
    path,
  });
}
report.packages = Object.fromEntries(
  [...packages].map(([key, value]) => [key, value.build]),
);
async function until(check, detail) {
  const deadline = Date.now() + 45000;
  let last;
  while (Date.now() < deadline) {
    try {
      const value = await check();
      if (value) return value;
    } catch (error) {
      last = error;
    }
    await delay(100);
  }
  throw Error(detail + " timed out: " + (last?.message ?? "not ready"));
}
async function start(component) {
  const pkg = packages.get(component),
    child = spawn(pkg.executable, [...pkg.args, "--config", pkg.path], {
      cwd: pkg.artifact,
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, IVY_LAUNCH_ID: randomUUID() },
    });
  const state = { component, child, pkg, logs: "" };
  state.closed = new Promise((yes) => {
    child.once("error", (error) => {
      state.error = error;
    });
    child.once("close", (code, signal) => yes({ code, signal }));
  });
  for (const stream of [child.stdout, child.stderr])
    stream.on("data", (bytes) => {
      state.logs = (
        state.logs + bytes.toString().replaceAll(token, "[redacted]")
      ).slice(-6000);
    });
  owned.push(state);
  report.processes.push({ component, pid: child.pid });
  const health = await until(async () => {
    if (state.error) throw state.error;
    assert.equal(child.exitCode, null, state.logs);
    const value = await json(join(pkg.data, "health.json"));
    return value.pid === child.pid && value.ready && value;
  }, component + " native startup");
  state.health = health;
  return state;
}
async function stop(state) {
  if (state.child.exitCode !== null) {
    assert.deepEqual(await state.closed, { code: 0, signal: null }, state.logs);
    return;
  }
  const health = await json(join(state.pkg.data, "health.json"));
  assert.equal(health.pid, state.child.pid);
  await atomicJson(join(state.pkg.data, "control.json"), {
    schemaVersion: 1,
    instanceId: health.instanceId,
    launchId: health.launchId,
    bootId: health.bootId,
    action: "shutdown",
    requestedAt: new Date().toISOString(),
  });
  const outcome = await Promise.race([
    state.closed,
    delay(20000, null, { ref: false }).then(() => {
      throw Error(state.component + " shutdown timed out");
    }),
  ]);
  assert.deepEqual(outcome, { code: 0, signal: null }, state.logs);
  assert.equal((await json(join(state.pkg.data, "health.json"))).ready, false);
}
async function rpc(method, params = {}, mcp = false) {
  const response = await fetch(base + (mcp ? "/mcp" : "/api/v1/rpc"), {
    method: "POST",
    headers: {
      Authorization: "Bearer " + token,
      "Content-Type": "application/json",
      Connection: "close",
      ...(mcp
        ? {
            Accept: "application/json, text/event-stream",
            "MCP-Protocol-Version": "2025-11-25",
          }
        : {}),
    },
    body: JSON.stringify({ jsonrpc: "2.0", id: randomUUID(), method, params }),
    signal: AbortSignal.timeout(15000),
  });
  const text = await response.text(),
    value = JSON.parse(
      response.headers.get("content-type")?.includes("text/event-stream")
        ? text.match(/^data: (.+)$/m)?.[1]
        : text,
    );
  if (value.error)
    throw Object.assign(Error(value.error.message), {
      code: value.error.data?.code,
      outcome: value.error.data?.outcome,
    });
  assert.equal(response.status, 200);
  return value.result;
}
async function call(name, input = {}, operationId) {
  const qualifiedName = "task-board." + name;
  const page = await rpc("tools.list", {
    namespace: "task-board",
    namePrefix: name,
    serviceNodeId: node,
  });
  const binding = page.items.find(
    (value) => value.qualifiedName === qualifiedName,
  );
  assert.ok(binding, qualifiedName);
  return rpc("tools.call", {
    qualifiedName,
    serviceNodeId: node,
    expectedDefinitionHash: binding.definitionHash,
    arguments: input,
    ...(operationId ? { operationId } : {}),
  });
}
let workspace;
async function action(input) {
  const operationId =
    (await rpc("system.status")).runtimeEpoch +
    ":" +
    Date.now() +
    ":" +
    randomUUID();
  const request = { ...input, operationId, expectedWorkspace: workspace };
  report.operations.push(request);
  await save();
  return call(request.action, request, operationId);
}
const readTask = (id) => call("read", { task: id, comments: { limit: 50 } });
const fields = (title) => ({
  title,
  description: "Isolated public native TaskBoard acceptance.",
  acceptanceCriteria: [],
  category: null,
  control: "user",
  priority: 2,
  executionRequirement: null,
  workspaceRequirement: { kind: "task_workspace" },
  nativeOptions: {
    model: null,
    reasoningEffort: null,
    serviceTier: "standard",
  },
  userContact: "ticket",
  dueAt: null,
});
const comment = (id, revision, body, extra = {}) =>
  action({
    action: "comment",
    taskId: id,
    expectedRevision: revision,
    commentId: randomUUID(),
    body,
    requests: [],
    responses: [],
    attachments: [],
    replyTo: null,
    agentDelivery: "none",
    ...extra,
  });
let board, browser;
try {
  await start("hive");
  board = await start("task-board");
  await until(async () => {
    const value = await rpc("serviceNodes.get", { serviceNodeId: node });
    return value.ready && value.connected && value.synced;
  }, "native TaskBoard registration");
  const info = await until(async () => {
    const value = await call("workspace");
    return value.recovered && value;
  }, "TaskBoard recovery");
  workspace = Object.fromEntries(
    ["principalId", "rootObjectId", "callerPrincipalId"].map((key) => [
      key,
      info[key],
    ]),
  );
  assert.equal(info.role, "user");
  const configuration = await call("configuration");
  assert.equal(configuration.object, null);
  assert.equal(configuration.configuration.schemaVersion, 1);
  const peer = await action({
    action: "create",
    workflowState: "backlog",
    fields: fields("Watched manual task"),
  });
  const created = await action({
    action: "create",
    workflowState: "todo",
    fields: fields("Native public discussion"),
  });
  const id = created.task.objectId;
  report.taskId = id;
  let task = await readTask(id);
  assert.equal(task.task.claim, null);
  assert.equal(task.task.lastRun, null);
  assert.deepEqual(task.task.fields.nativeOptions, fields("").nativeOptions);
  const question = await comment(
    id,
    task.object.revision,
    "Which synthetic option: Alpha or Beta?",
    { authorKind: "agent", purpose: "question", recipient: { kind: "user" } },
  );
  const questionRequest = report.operations.at(-1);
  const replay = await call(
    "comment",
    questionRequest,
    questionRequest.operationId,
  );
  assert.deepEqual(replay.history, question.history);
  await assert.rejects(
    call(
      "comment",
      {
        ...questionRequest,
        body: "A different request under the original identity.",
      },
      questionRequest.operationId,
    ),
    (error) => {
      assert.equal(error.outcome, "not_executed");
      assert.equal(error.code, "mutation_conflict");
      report.rejectedConflictCode = error.code;
      return true;
    },
  );
  task = await readTask(id);
  assert.equal(task.openQuestions.length, 1);
  assert.equal(
    (await call("read", { task: id, view: "card" })).task.openQuestionCount,
    1,
  );
  assert.equal(
    task.task.comments.filter(
      (value) => value.commentId === questionRequest.commentId,
    ).length,
    1,
  );
  const questions = await call("read", {
    task: id,
    comments: { questionsOnly: true, limit: 50 },
  });
  assert.equal(questions.task.comments.length, 1);
  assert.equal(questions.task.comments[0].commentId, questionRequest.commentId);
  report.checks.push(
    "Current task fields, caller-bound durable comment replay/conflict and unresolved question reads",
  );
  const mcp = await rpc(
    "tools/call",
    {
      name: "task_read",
      arguments: { serviceNodeId: node, input: { task: id } },
    },
    true,
  );
  assert.notEqual(mcp.isError, true);
  assert.ok(JSON.stringify(mcp).includes(questionRequest.commentId));
  await stop(board);
  board = await start("task-board");
  assert.notEqual(board.health.bootId, owned.at(-2).health.bootId);
  await until(
    async () => (await call("workspace")).recovered,
    "TaskBoard restart recovery",
  );
  task = await readTask(id);
  assert.equal(task.openQuestions.length, 1);
  assert.equal(task.task.comments[0].commentId, questionRequest.commentId);
  report.checks.push(
    "Actual native TaskBoard restart retains original question identity; public MCP reads it",
  );
  const ui = resolve(values.ui),
    definition = await json(join(ui, "ivy-ui.json"));
  const publicationId =
    (await rpc("system.status")).runtimeEpoch +
    ":" +
    Date.now() +
    ":" +
    randomUUID();
  report.uiOperationId = publicationId;
  await save();
  report.ui = await publishUi(
    { request: rpc },
    {
      directory: ui,
      definition,
      expectedReleaseId: null,
      mutationId: publicationId,
    },
  );
  browser = await chromium.launch({
    headless: true,
    ...(values["browser-executable"]
      ? { executablePath: resolve(values["browser-executable"]) }
      : process.platform === "win32"
        ? { channel: "msedge" }
        : {}),
  });
  report.layouts = [];
  for (const width of [1440, 390]) {
    const context = await browser.newContext({
      viewport: { width, height: 950 },
      locale: "en-US",
    });
    context.setDefaultTimeout(20000);
    const outside = [],
      errors = [];
    await context.route("**/*", (route) => {
      if (new URL(route.request().url()).origin !== new URL(base).origin) {
        outside.push(route.request().url());
        return route.abort();
      }
      return route.continue();
    });
    const page = await context.newPage();
    page.on("pageerror", (error) => errors.push(error.message));
    await page.goto(base + "/");
    await page.getByLabel("Hive credential").fill(token);
    const login = page.waitForResponse(
      (response) =>
        response.request().method() === "POST" &&
        response.url().endsWith("/login"),
    );
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    assert.equal((await login).status(), 303);
    await page.goto(
      base + "/ui/task-board-ui/#/tasks?id=" + id + "&node=" + node,
    );
    await expect(
      page.getByRole("heading", { name: /Native public discussion/ }),
    ).toBeVisible();
    await expect(
      page
        .getByRole("region", { name: "Open questions in this ticket" })
        .getByText(questionRequest.body, { exact: true }),
    ).toBeVisible();
    const body = "Browser comment " + width;
    await page
      .getByRole("textbox", { name: "Comment", exact: true })
      .fill(body);
    await page
      .getByRole("button", { name: "Add comment", exact: true })
      .click();
    await until(
      async () =>
        (await readTask(id)).task.comments.some((value) => value.body === body),
      "actual browser comment",
    );
    await page.reload();
    await expect(page.getByText(body, { exact: true })).toHaveCount(1);
    assert.equal(
      await page.evaluate(
        () => document.documentElement.scrollWidth > innerWidth,
      ),
      false,
    );
    assert.deepEqual(errors, []);
    assert.deepEqual(outside, []);
    report.layouts.push({
      width,
      comment: body,
      pageErrors: errors,
      outsideRequests: outside,
    });
    await context.close();
  }
  task = await readTask(id);
  assert.equal(
    task.openQuestions.length,
    1,
    "An unrelated comment must leave the original question open.",
  );
  await comment(id, task.object.revision, "Alpha.", {
    replyTo: questionRequest.commentId,
    authorKind: "user",
  });
  task = await readTask(id);
  assert.equal(task.openQuestions.length, 0);
  report.checks.push(
    "Desktop/mobile authenticated current UI, actual single comment publication and addressed question answer",
  );
  const wakeAt = new Date(Date.now() + 60000).toISOString();
  await action({
    action: "defer",
    taskId: id,
    expectedRevision: task.object.revision,
    dependencies: [peer.task.objectId],
    wakeAt,
  });
  task = await readTask(id);
  assert.equal(task.task.workflowState, "waiting");
  assert.equal(task.task.wakeAt, wakeAt);
  assert.deepEqual(task.task.dependencies, [peer.task.objectId]);
  await stop(board);
  const previousBoot = board.health.bootId;
  board = await start("task-board");
  assert.notEqual(board.health.bootId, previousBoot);
  await until(
    async () => (await call("workspace")).recovered,
    "waiting-state recovery",
  );
  task = await readTask(id);
  assert.equal(task.task.workflowState, "waiting");
  assert.equal(task.task.wakeAt, wakeAt);
  assert.deepEqual(task.task.dependencies, [peer.task.objectId]);
  assert.equal(task.task.lastRun, null);
  for (const workflowState of ["todo", "in_progress"]) {
    await action({
      action: "transition",
      taskId: id,
      expectedRevision: task.object.revision,
      workflowState,
      detail: null,
    });
    task = await readTask(id);
  }
  const content = {
    summary: "Verified native public manual result.",
    artifacts: [],
    codeReferences: [],
    repositoryResult: null,
    limitations: [],
  };
  const manualReview = await action({
    action: "create",
    fields: fields("Native manual result"),
    workflowState: "review",
    initialResult: content,
  });
  const reviewId = manualReview.task.objectId;
  report.reviewTaskId = reviewId;
  task = await readTask(reviewId);
  assert.equal(task.task.workflowState, "review");
  assert.ok(task.task.latestResult);
  const result = await rpc("objects.read", task.task.latestResult);
  assert.deepEqual(result.content.value.content, content);
  assert.equal(result.content.value.run, null);
  await action({
    action: "transition",
    taskId: reviewId,
    expectedRevision: task.object.revision,
    workflowState: "done",
    detail: "Checked the saved manual result through the public API.",
  });
  task = await readTask(reviewId);
  assert.equal(task.task.workflowState, "done");
  assert.deepEqual(task.task.latestResult, {
    objectId: result.object.id,
    revision: result.revision.revision,
  });
  for (const archived of [true, false]) {
    await action({
      action: "archive",
      taskId: reviewId,
      expectedRevision: task.object.revision,
      archived,
    });
    assert.equal(
      (await rpc("objects.stat", { objectId: reviewId })).effectivelyArchived,
      archived,
    );
    task = await readTask(reviewId);
  }
  report.checks.push(
    "Persisted manual Waiting/dependencies/appointment, manual resume, saved final result, explicit acceptance and archive/restore",
  );
  report.status = "passed";
} catch (error) {
  report.status = "failed";
  report.failure = {
    message: error.message,
    code: error.code,
    stack: error.stack,
  };
  process.exitCode = 1;
} finally {
  try {
    await browser?.close();
  } catch (error) {
    report.cleanupError = error.message;
    process.exitCode = 1;
  }
  for (const state of owned.toReversed()) {
    try {
      await stop(state);
    } catch (error) {
      report.cleanupError = error.message;
      process.exitCode = 1;
      if (state.child.exitCode === null) {
        state.child.kill();
        await state.closed;
      }
    }
  }
  if (report.status === "passed" && !report.cleanupError) {
    assert.equal(dirname(root), parent);
    assert.ok(root.startsWith(join(parent, "ivy-task-board-public-")));
    await rm(root, {
      recursive: true,
      force: true,
      maxRetries: 10,
      retryDelay: 100,
    });
  } else report.retainedFixture = root;
  report.completedAt = new Date().toISOString();
  await save();
  console.log(
    JSON.stringify({
      status: report.status,
      report: reportPath,
      failure: report.failure,
      cleanupError: report.cleanupError,
    }),
  );
}
