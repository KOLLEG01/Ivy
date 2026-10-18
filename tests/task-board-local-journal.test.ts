import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { hashJson } from "../packages/contracts/src/canonical.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import { TaskBoardStore } from "../services/task-board/src/runtime/store.js";
import { nativeWorkflow } from "./fixtures/task-board-native.js";
import { fields, taskBoardFixture } from "./fixtures/task-board.js";

test("workspace reservations use the target path syntax independently of the server OS", (t) => {
  const { store } = taskBoardFixture(t).first;
  assert.equal(store.reserveWorkspace("windows", "C:\\Work\\Repo", "first", "one"), null);
  assert.equal(store.reserveWorkspace("windows", "c:/work/repo", "second", "two"), "first");
  assert.equal(store.reserveWorkspace("windows", "\\\\server\\share\\Repo", "unc-first", "three"), null);
  assert.equal(store.reserveWorkspace("windows", "\\\\SERVER\\SHARE\\repo", "unc-second", "four"), "unc-first");
  assert.equal(store.reserveWorkspace("linux", "/work/Repo", "linux-first", "five"), null);
  assert.equal(store.reserveWorkspace("linux", "/work/repo", "linux-second", "six"), null);
});

test("a failed continuation cannot release its Task's existing workspace reservation", (t) => {
  const { store } = taskBoardFixture(t).first;
  store.reserveWorkspace("host", "/work/task", "task", "original-run");
  store.reserveWorkspace("host", "/work/task", "task", "failed-continuation");
  store.releaseWorkspaceOperation("failed-continuation");
  assert.equal(store.reserveWorkspace("host", "/work/task", "another-task", "new-run"), "task");
});

test("an existing Windows reservation remains exclusive after path normalization", (t) => {
  const f = taskBoardFixture(t), store = f.first.store;
  const db = new DatabaseSync(join(f.plan.location!.cwd, "task-board.sqlite"));
  db.prepare("INSERT INTO workspace_reservations VALUES(?,?,?,?)").run("windows", "C:\\Work\\Repo", "task", "original-run");
  db.close();
  assert.equal(store.reserveWorkspace("windows", "c:/work/repo", "other", "other-run"), "task");
  assert.equal(store.reserveWorkspace("windows", "c:/work/repo", "task", "failed-continuation"), null);
  store.releaseWorkspaceOperation("failed-continuation");
  assert.equal(store.reserveWorkspace("windows", "C:\\WORK\\REPO", "other", "another-run"), "task");
});

test("an existing journal enables physical reclamation without losing its rows", (t) => {
  const f = taskBoardFixture(t);
  const root = mkdtempSync(join(tmpdir(), "ivy-task-board-vacuum-"));
  const path = join(root, "task-board.sqlite");
  const legacy = new DatabaseSync(path);
  legacy.exec("CREATE TABLE legacy(value TEXT); INSERT INTO legacy VALUES('retained');");
  assert.equal(legacy.prepare("PRAGMA auto_vacuum").get()?.["auto_vacuum"], 0);
  legacy.close();
  const store = new TaskBoardStore(f.first.store.client, null, root);
  t.after(() => { store.close(); rmSync(root, { recursive: true, force: true }); });
  const check = new DatabaseSync(path, { readOnly: true });
  assert.equal(check.prepare("PRAGMA auto_vacuum").get()?.["auto_vacuum"], 2);
  assert.equal(check.prepare("SELECT value FROM legacy").get()?.["value"], "retained");
  check.close();
});

test("native execution plumbing is shared through the local TaskBoard journal and absent from Hive", async (t) => {
  const workflow = await nativeWorkflow(t);
  const call = await workflow.prepare("thread");
  const first = await workflow.f.first.store.read(
    "task-board/native-call",
    call,
    false,
  );
  const replacement = await workflow.f.second.store.read(
    "task-board/native-call",
    call,
    false,
  );
  assert.deepEqual(replacement.value, first.value);
  assert.deepEqual(replacement.pin, first.pin);
  await assert.rejects(
    workflow.f.first.store.client.request("objects.query", {
      contractKey: "task-board/native-call",
      limit: 1,
    }),
    (error: unknown) =>
      error instanceof IvyError && error.code === "contract_not_found",
  );
});

test("local workflow paging includes nested run checkpoints without exposing them as domain objects", async (t) => {
  const workflow = await nativeWorkflow(t);
  const call = await workflow.prepare("thread");
  const page = await workflow.f.first.store.page("task-board/native-call", {
    where: { op: "eq", field: "object.id", value: call.objectId },
    limit: 2,
  });
  assert.equal(page.items.length, 1);
  assert.equal(page.items[0]!.objectId, call.objectId);
});

test("a cleanup marker cannot discard an unobserved native call under history pressure", async (t) => {
  const workflow = await nativeWorkflow(t);
  const call = await workflow.prepare("thread");
  workflow.f.first.store.completeLocalWorkflow(
    call.objectId,
    new Date(Date.now() - 2 * 86_400_000).toISOString(),
  );
  workflow.f.first.store.historyLimits.count = 0;
  await workflow.f.first.store.collectLocal();
  assert.ok(await workflow.f.first.store.read("task-board/native-call", call, false));
});

test("saturated history yields to requests and shares one reference scan across concurrent writers", async t => {
  const f = taskBoardFixture(t), store = f.first.store;
  const db = new DatabaseSync(join(f.plan.location!.cwd, "task-board.sqlite"));
  try {
    const at = new Date().toISOString();
    const insert = (id: string, value: unknown) => {
      db.prepare("INSERT INTO documents VALUES(?,?,?,?,?,?,?)").run(id, "task-board/native-plan", id, null, 1, at, at);
      db.prepare("INSERT INTO revisions VALUES(?,?,?,?)").run(id, 1, JSON.stringify(value), at);
    };
    for (let index = 0; index < 160; index++) {
      insert("history-" + index, { fixture: index, payload: "x".repeat(4096) });
      store.completeLocalWorkflow("history-" + index, at);
    }
    // Earlier roots are retained by a live checkpoint; later unreferenced roots
    // must still be reached without repeatedly decoding that checkpoint.
    insert("live-checkpoint", { pins: Array.from({ length: 128 }, (_, index) => "history-" + index) });
    insert("uncertain", { error: { execution: "unknown" } });
    store.completeLocalWorkflow("uncertain", at);
    store.historyLimits.count = store.localUsage().revisions.rows;
    const parsing = t.mock.method(JSON, "parse");
    let servedRequests = 0;
    const timer = setInterval(() => { servedRequests++; }, 0);
    try {
      await Promise.all(Array.from({ length: 20 }, () => store.collectLocal(Date.now(), 0, 1)));
    } finally { clearInterval(timer); }
    assert.ok(servedRequests > 0, "collection must let timers and request handlers run");
    assert.ok(parsing.mock.callCount() < 400, "concurrent collection must not repeatedly decode the journal");
    assert.ok(store.localUsage().revisions.rows < store.historyLimits.count - 10, "reclaim headroom for subsequent writes");
    assert.equal(db.prepare("SELECT COUNT(*) AS count FROM documents WHERE id='uncertain' OR id='history-0'").get()?.["count"], 2);
  } finally { db.close(); }
});

test("collection refreshes references written by another journal owner while yielding", async t => {
  const f = taskBoardFixture(t), store = f.first.store;
  const db = new DatabaseSync(join(f.plan.location!.cwd, "task-board.sqlite"));
  try {
    const at = new Date().toISOString();
    for (let index = 0; index < 96; index++) {
      db.prepare("INSERT INTO documents VALUES(?,?,?,?,?,?,?)").run("yield-" + index, "task-board/native-plan", "yield-" + index, null, 1, at, at);
      db.prepare("INSERT INTO revisions VALUES(?,?,?,?)").run("yield-" + index, 1, JSON.stringify({ fixture: index }), at);
      store.completeLocalWorkflow("yield-" + index, at);
    }
    const collection = store.collectLocal(Date.now(), 0, store.historyLimits.count);
    await new Promise<void>(resolve => setImmediate(() => {
      db.prepare("INSERT INTO documents VALUES(?,?,?,?,?,?,?)").run("new-reference", "task-board/native-plan", "new-reference", null, 1, at, at);
      db.prepare("INSERT INTO revisions VALUES(?,?,?,?)").run("new-reference", 1, JSON.stringify({ pin: "yield-95" }), at);
      resolve();
    }));
    await collection;
    assert.equal(db.prepare("SELECT COUNT(*) AS count FROM documents WHERE id IN ('new-reference','yield-95')").get()?.["count"], 2);
    assert.equal(db.prepare("SELECT COUNT(*) AS count FROM documents WHERE id='yield-0'").get()?.["count"], 0);
  } finally { db.close(); }
});

test("history collection finishes while another owner keeps writing", async t => {
  const f = taskBoardFixture(t), store = f.first.store;
  const db = new DatabaseSync(join(f.plan.location!.cwd, "task-board.sqlite"));
  let writing = true, writes = 0;
  try {
    const at = new Date().toISOString();
    for (let index = 0; index < 96; index++) {
      const id = "busy-history-" + index;
      db.prepare("INSERT INTO documents VALUES(?,?,?,?,?,?,?)").run(id, "task-board/native-plan", id, null, 1, at, at);
      db.prepare("INSERT INTO revisions VALUES(?,?,?,?)").run(id, 1, JSON.stringify({ fixture: index }), at);
      store.completeLocalWorkflow(id, at);
    }
    const write = () => {
      if (!writing) return;
      db.prepare("INSERT OR REPLACE INTO markers VALUES('busy-writer',?)").run(String(++writes));
      if (writes < 500) setImmediate(write);
    };
    setImmediate(write);
    await store.collectLocal(Date.now(), 0, store.historyLimits.count);
    assert.ok(writes > 0 && writes < 500, "cleanup must not wait for a globally idle journal");
    assert.equal(db.prepare("SELECT COUNT(*) AS count FROM documents WHERE id LIKE 'busy-history-%'").get()?.["count"], 0);
  } finally { writing = false; db.close(); }
});

test("a completed publication can release its old snapshot of another run's uncertainty", async t => {
  const f = taskBoardFixture(t), created = await f.create();
  const operation = await f.first.operations.find("user", created.operationId);
  assert.ok(operation);
  const db = new DatabaseSync(join(f.plan.location!.cwd, "task-board.sqlite"));
  try {
    const at = new Date().toISOString();
    db.prepare("INSERT INTO documents VALUES(?,?,?,?,?,?,?)").run("historical-task", "task-board/task", "historical-task", operation.pin.objectId, 1, at, at);
    db.prepare("INSERT INTO revisions VALUES(?,?,?,?)").run("historical-task", 1, JSON.stringify({ claim: { phase: "outcome_unknown" } }), at);
    f.first.store.historyLimits.count = 0;
    await f.first.store.collectLocal();
    assert.equal(db.prepare("SELECT COUNT(*) AS count FROM documents WHERE id='historical-task'").get()?.["count"], 0);
    assert.ok(f.first.store.retiredOperation("user", created.operationId));
  } finally { db.close(); }
});

test("workflow writes continue while history waits for a remote receipt check", async t => {
  const f = taskBoardFixture(t), store = f.first.store;
  const db = new DatabaseSync(join(f.plan.location!.cwd, "task-board.sqlite"));
  const at = new Date().toISOString();
  let started!: () => void, release!: () => void;
  const checking = new Promise<void>(resolve => { started = resolve; });
  const held = new Promise<void>(resolve => { release = resolve; });
  const request = store.client.request.bind(store.client);
  t.mock.method(store.client, "request", async (method: string, input: { objectId?: string }) => {
    if (method === "objects.read" && input.objectId === "slow-history-task") {
      started(); await held; throw new IvyError("not_found", "History task is gone.");
    }
    return request(method as never, input as never);
  });
  db.prepare("INSERT INTO documents VALUES(?,?,?,?,?,?,?)").run("slow-attempt", "task-board/scheduler-attempt", "slow-attempt", null, 1, at, at);
  db.prepare("INSERT INTO revisions VALUES(?,?,?,?)").run("slow-attempt", 1, JSON.stringify({ task: { objectId: "slow-history-task", revision: 1 } }), at);
  store.completeLocalWorkflow("slow-attempt", at);
  store.historyLimits.count = 0;
  const collection = store.collectLocal();
  try {
    await checking;
    const written = store.write("task-board/scan-cursor", { schemaVersion: 1, principalId: "fixture", serviceNodeId: "fixture",
      lane: "tasks", queryHash: hashJson("fixture"), cursor: null, updatedAt: at }, randomUUID(),
      { create: { parentId: null, name: "Live progress" } });
    const result = await Promise.race([written, new Promise<null>(resolve => { const timer = setTimeout(() => resolve(null), 1000); timer.unref(); })]);
    assert.ok(result, "a workflow checkpoint must not wait behind remote history collection");
  } finally { release(); await collection; db.close(); }
});

test("the local scan cursor keeps only its current revision", async (t) => {
  const f = taskBoardFixture(t),
    at = new Date().toISOString();
  const value = {
    schemaVersion: 1 as const,
    principalId: "fixture-principal",
    serviceNodeId: "fixture-node",
    lane: "tasks" as const,
    queryHash: hashJson("fixture-query"),
    cursor: null,
    updatedAt: at,
  };
  const first = await f.first.store.write(
    "task-board/scan-cursor",
    value,
    randomUUID(),
    { create: { parentId: null, name: "Fixture scan cursor" } },
  );
  const second = await f.first.store.write(
    "task-board/scan-cursor",
    { ...value, cursor: "next", updatedAt: new Date().toISOString() },
    randomUUID(),
    { objectId: first.objectId, expectedRevision: first.revision },
  );
  await assert.rejects(
    f.first.store.read("task-board/scan-cursor", first, false),
    (error: unknown) =>
      error instanceof IvyError && error.code === "task_board_evidence_mismatch",
  );
  assert.equal(
    (await f.first.store.read("task-board/scan-cursor", second, false)).value
      .cursor,
    "next",
  );
});

test("advisory signals retain their current checkpoint and reclaim superseded observations on reopen", async t => {
  const f = taskBoardFixture(t), store = f.first.store;
  const value = { schemaVersion: 1 as const, run: { objectId: "fixture-run", revision: 1 },
    serviceNodeId: "native-agent", nativeVersion: "0.154.0", epoch: "fixture-epoch", threadId: "thread", turnId: "turn",
    observedAt: new Date().toISOString(), inputsTruncated: false, inputs: [], notificationCursor: 1,
    notificationGap: false, notificationsPending: false, activity: [] };
  const first = await store.write("task-board/native-signals", value, randomUUID(),
    { create: { parentId: "fixture-run", name: "Native signals" } });
  const second = await store.write("task-board/native-signals", { ...value, notificationCursor: 2 }, randomUUID(),
    { objectId: first.objectId, expectedRevision: first.revision });
  await assert.rejects(store.read("task-board/native-signals", first, false),
    (error: unknown) => error instanceof IvyError && error.code === "task_board_evidence_mismatch");
  const db = new DatabaseSync(join(f.plan.location!.cwd, "task-board.sqlite"));
  try {
    // Reproduce a journal that accumulated earlier advisory observations.
    db.prepare("INSERT INTO revisions VALUES(?,?,?,?)").run(first.objectId, first.revision, JSON.stringify(value), value.observedAt);
    const replacement = new TaskBoardStore(store.client, null, f.plan.location!.cwd);
    try {
      assert.equal((await replacement.read("task-board/native-signals", second, false)).value.notificationCursor, 2);
      assert.equal(db.prepare("SELECT COUNT(*) AS count FROM revisions WHERE id=?").get(first.objectId)?.["count"], 1);
      assert.equal(replacement.localUsage().revisions.rows, db.prepare("SELECT COUNT(*) AS count FROM revisions").get()?.["count"]);
    } finally { replacement.close(); }
  } finally { db.close(); }
});

test("a replacement runtime replays an ambiguous first Task reservation from the shared journal", async (t) => {
  const f = taskBoardFixture(t),
    created = await f.create();
  let loseReply = true;
  f.intercept((params) => {
    if (
      loseReply &&
      !("create" in params) &&
      params.objectId === created.task!.objectId
    ) {
      loseReply = false;
      throw new IvyError(
        "synthetic_task_response_loss",
        "Synthetic first Task response loss.",
        "unknown",
      );
    }
  });
  const operationId = randomUUID();
  await assert.rejects(
    f.invoke({
      action: "edit",
      operationId,
      taskId: created.task!.objectId,
      expectedRevision: created.task!.revision,
      fields: { ...fields, title: "Recovered by replacement" },
    }),
    (error: unknown) =>
      error instanceof IvyError &&
      error.code === "synthetic_task_response_loss",
  );
  f.intercept(null);
  const recovered = await f.second.recoverPublication();
  assert.ok(recovered?.task);
  const task = await f.second.store.read("task-board/task", recovered!.task!);
  assert.equal(task.value.fields.title, "Recovered by replacement");
  assert.equal(task.value.publication, null);
});
