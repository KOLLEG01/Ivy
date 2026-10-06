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

test("completed local workflows expire after their replay window", async (t) => {
  const workflow = await nativeWorkflow(t);
  const call = await workflow.prepare("thread");
  workflow.f.first.store.completeLocalWorkflow(
    call.objectId,
    new Date(Date.now() - 2 * 86_400_000).toISOString(),
  );
  await workflow.f.first.store.collectLocal();
  await assert.rejects(
    workflow.f.first.store.read("task-board/native-call", call, false),
    (error: unknown) =>
      error instanceof IvyError && error.code === "task_board_scope_mismatch",
  );
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
