import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { IvyError } from "../packages/contracts/src/errors.js";
import type { TaskBoard } from "../packages/contracts/src/generated.js";
import { scopedOperationId } from "../packages/sdk/src/client.js";
import { TaskBoardCategoryProjection } from "../services/task-board/src/runtime/category-projection.js";
import type { TaskBoardStore } from "../services/task-board/src/runtime/store.js";
import { TaskBoardScheduler } from "../services/task-board/src/runtime/scheduler.js";
import { content, context, fields, taskBoardFixture } from "./fixtures/task-board.js";

const code = (expected: string) => (error: unknown) =>
  error instanceof IvyError && error.code === expected;

test("existing projects must belong to the requested host before a task is saved", async (t) => {
  const f = taskBoardFixture(t);
  const workspaceRequirement = { kind: "existing_project" as const, projectId: "fixture-task-project", path: null, useWorktree: false };
  await assert.rejects(f.create({ workspaceRequirement, executionRequirement: { kind: "host", hostId: "another-host" } }),
    code("task_board_project_unavailable"));
  await assert.rejects(f.create({ workspaceRequirement: { ...workspaceRequirement, projectId: "unknown-project" } }),
    code("task_board_project_unavailable"));
  const created = await f.create({ workspaceRequirement });
  assert.ok(created.task);
  await assert.rejects(f.invoke({ action: "edit", operationId: "wrong-host-edit", taskId: created.task!.objectId,
    expectedRevision: created.task!.revision, fields: { ...fields, workspaceRequirement,
      executionRequirement: { kind: "host", hostId: "another-host" } } }), code("task_board_project_unavailable"));
  assert.deepEqual((await f.first.store.read("task-board/task", created.task!)).value.fields.executionRequirement, fields.executionRequirement);
  await f.clients.get("native-agent")!.client.request("service.heartbeat", { ready: false, diagnostics: [] });
  assert.ok((await f.create({ workspaceRequirement })).task, "Retained projects remain selectable while their host is not ready");
});

test("Task key allocation is scoped to the caller's operation", async (t) => {
  const f = taskBoardFixture(t);
  const request = { action: "create" as const, operationId: "shared-id", fields };
  const first = await f.invoke(request, "first-user");
  const second = await f.invoke(request, "second-user");
  assert.notEqual(first.task!.objectId, second.task!.objectId);
  assert.notEqual((await f.first.store.read("task-board/task", first.task!)).value.taskKey,
    (await f.first.store.read("task-board/task", second.task!)).value.taskKey);
  assert.deepEqual(await f.invoke(request, "first-user"), first);
});

test("archiving serializes a concurrent Task transition", async (t) => {
  const f = taskBoardFixture(t), created = await f.create();
  let changed = false;
  f.interceptRead(async (params) => {
    if (changed || params.objectId !== created.task!.objectId) return;
    changed = true;
    await assert.rejects(f.invoke({ action: "transition", operationId: "concurrent-transition", taskId: created.task!.objectId,
      expectedRevision: created.task!.revision, workflowState: "todo", detail: null }), code("task_board_publication_busy"));
  });
  await f.first.archive({ action: "archive", operationId: "archive-race", taskId: created.task!.objectId,
    expectedRevision: created.task!.revision, archived: true }, context("archive-race"));
  const saved = await f.first.store.read("task-board/task", created.task!.objectId, true, true);
  assert.equal(saved.metadata.effectivelyArchived, true);
  assert.equal(saved.value.workflowState, "backlog");
});

test("archiving cannot interrupt a prepared Task publication", async (t) => {
  const f = taskBoardFixture(t), created = await f.create(), task = created.task!;
  const write = f.first.store.writePrepared.bind(f.first.store);
  let archived = false;
  f.first.store.writePrepared = async (...args: Parameters<TaskBoardStore["writePrepared"]>) => {
    if (!archived && args[0] === "task-board/task") {
      archived = true;
      await assert.rejects(f.first.archive({ action: "archive", operationId: "concurrent-archive", taskId: task.objectId,
        expectedRevision: task.revision, archived: true }, context("concurrent-archive")), code("task_board_publication_busy"));
    }
    return write(...args);
  };
  const transitioned = await f.invoke({ action: "transition", operationId: "archived-transition", taskId: task.objectId,
    expectedRevision: task.revision, workflowState: "todo", detail: null });
  const saved = await f.first.store.client.request("objects.read", { objectId: task.objectId });
  assert.equal(saved.object.effectivelyArchived, false);
  assert.equal(saved.object.currentRevision, transitioned.task!.revision);
  assert.equal((await f.first.operations.find("user", "archived-transition"))!.value.phase, "succeeded");
  await f.first.archive({ action: "archive", operationId: "archive-after-publication", taskId: task.objectId,
    expectedRevision: transitioned.task!.revision, archived: true }, context("archive-after-publication"));
  assert.equal((await f.first.store.client.request("objects.stat", { objectId: task.objectId })).effectivelyArchived, true);
});

test("read markers converge across concurrent callers and exact retries", async (t) => {
  const f = taskBoardFixture(t), created = await f.create();
  const saved = await f.invoke({ action: "saveResult", operationId: "read-result", taskId: created.task!.objectId,
    expectedRevision: created.task!.revision, run: null, kind: "final", content });
  const request = { operationId: "read-comments", taskId: saved.task!.objectId, expectedTaskRevision: saved.task!.revision };
  const write = f.first.store.write.bind(f.first.store);
  let writes = 0;
  f.first.store.write = async (...args) => {
    if (args[0] === "task-board/comment-read-state") assert.ok(++writes <= 4, "Read-marker retry must terminate");
    return write(...args);
  };
  const [first, second] = await Promise.all([
    f.first.markCommentsRead(request, context(request.operationId)),
    f.first.markCommentsRead({ ...request, operationId: "concurrent-read" }, context("concurrent-read")),
  ]);
  assert.deepEqual(first.readState, second.readState);
  assert.deepEqual(await f.first.markCommentsRead(request, context(request.operationId)), first);
  const other = await f.first.markCommentsRead(request, context(request.operationId, "other-user"));
  assert.notEqual(other.readState.objectId, first.readState.objectId);
});

test("invalid attachment input is rejected before reserving a Task", async (t) => {
  const f = taskBoardFixture(t), created = await f.create();
  const base = { taskId: created.task!.objectId, expectedRevision: created.task!.revision };
  await assert.rejects(f.invoke({ action: "uploadAttachment", ...base, operationId: "bad-base64", attachmentId: "bad-file",
    filename: "file.txt", mediaType: "text/plain", bytesBase64: "YR==" }), code("invalid_arguments"));
  assert.deepEqual((await f.first.store.read("task-board/task", base.taskId)).pin, created.task);
  await assert.rejects(f.invoke({ action: "comment", ...base, operationId: "missing-attachment", commentId: "missing-file",
    body: "Missing file", requests: [], responses: [], replyTo: null, attachments: [{ attachmentId: "missing-file",
      object: { objectId: "missing-object", revision: 1 }, filename: "missing.txt", mediaType: "text/plain", byteLength: 1,
      contentHash: "sha256:" + "0".repeat(64), uploader: { principalId: "user", source: "user", ...f.first.owner },
      createdAt: new Date().toISOString() }] }), code("not_found"));
  assert.deepEqual((await f.first.store.read("task-board/task", base.taskId)).pin, created.task);
});

test("attachment identities do not become Hive reference names and leave room for results", async (t) => {
  const f = taskBoardFixture(t), created = await f.create();
  const uploaded = await f.invoke({ action: "uploadAttachment", operationId: "attachment-reference", taskId: created.task!.objectId,
    expectedRevision: created.task!.revision, attachmentId: "file: " + "x".repeat(200), filename: "file.txt", mediaType: "text/plain",
    bytesBase64: Buffer.from("saved content").toString("base64") });
  assert.equal(uploaded.attachment?.filename, "file.txt");
  const current = await f.first.store.read("task-board/task", uploaded.task!);
  const attachments = Array.from({ length: 254 }, (_, index) => ({ ...uploaded.attachment!, attachmentId: "file-" + index,
    object: { objectId: "capacity-file-" + index, revision: 1 } }));
  // Capacity validation runs before any attempt to dereference or upload these files.
  f.first.checks.aggregate = async () => undefined;
  const read = f.first.store.read.bind(f.first.store);
  f.first.store.read = (async (...args) => {
    const document = await read(...args);
    if (args[0] === "task-board/task" && document.pin.objectId === current.pin.objectId)
      return { ...document, value: { ...current.value, attachments } };
    return document;
  }) as typeof f.first.store.read;
  await assert.rejects(f.invoke({ action: "uploadAttachment", operationId: "attachment-capacity", taskId: current.pin.objectId,
    expectedRevision: current.pin.revision, attachmentId: "overflow", filename: "overflow.txt", mediaType: "text/plain", bytesBase64: "" }), code("limit_exceeded"));
  assert.deepEqual((await read("task-board/task", current.pin.objectId)).pin, current.pin);
});

for (const bound of ["count", "bytes"] as const) {
  test(`a manual final Result publishes when comments reach the ${bound} limit`, async (t) => {
    const f = taskBoardFixture(t), created = await f.create({ userContact: "chat" });
    const task = await f.first.store.read("task-board/task", created.task!);
    const comments: TaskBoard.Comment[] = Array.from({ length: bound === "count" ? 512 : 15 }, (_, index) => ({
      commentId: randomUUID(), sequence: index + 1, author: { principalId: "user", source: "user", ...f.first.owner },
      authorKind: "user", body: bound === "count" ? "Retained comment" : "x".repeat(65536), requests: [], responses: [],
      delivery: null, attachments: [], run: null, turnId: null, replyTo: null, createdAt: new Date().toISOString(), operationId: randomUUID(),
    }));
    const pin = await f.first.store.write("task-board/task", { ...task.value, comments }, randomUUID(),
      { objectId: task.pin.objectId, expectedRevision: task.pin.revision });
    const summary = bound === "count" ? "Complete final result" : "r".repeat(65536);
    const result = await f.invoke({ action: "saveResult", operationId: randomUUID(), taskId: pin.objectId,
      expectedRevision: pin.revision, run: null, kind: "final", content: { ...content, summary } });
    const saved = await f.first.store.read("task-board/task", result.task!);
    assert.equal(saved.value.publication, null);
    assert.equal(saved.value.workflowState, "review");
    assert.deepEqual(saved.value.comments, comments);
    assert.equal((await f.first.store.read("task-board/result", result.result!)).value.content.summary, summary);
    assert.equal((await f.first.store.page("task-board/delivery")).items.length, 0);
  });
}

test("Tasks receive stable keys, start in Backlog and expose category projections", async (t) => {
  const f = taskBoardFixture(t);
  const { userContact: _defaultContact, ...withoutContact } = fields;
  const firstId = randomUUID(),
    first = await f.first.invoke(
      {
        action: "create",
        operationId: firstId,
        fields: {
          ...withoutContact,
          category: "Engineering",
          control: "agent",
        },
      },
      {
        callerPrincipalId: "user",
        operationId: firstId,
        generation: 1,
        signal: new AbortController().signal,
      },
    );
  const secondId = randomUUID();
  const second = await f.second.invoke(
    {
      action: "create",
      operationId: secondId,
      fields: { ...fields, category: "engineering", control: "agent" },
    },
    {
      callerPrincipalId: "user",
      operationId: secondId,
      generation: 1,
      signal: new AbortController().signal,
    },
  );
  const a = await f.first.store.read("task-board/task", first.task!);
  const b = await f.first.store.read("task-board/task", second.task!);
  assert.equal(a.value.taskKey, "TASK-0001");
  assert.equal(b.value.taskKey, "TASK-0002");
  assert.equal(a.value.workflowState, "backlog");
  assert.equal(a.value.fields.control, "agent");
  assert.equal(a.value.fields.userContact, "ticket");
  assert.deepEqual(
    (await f.first.task({ task: { taskKey: "TASK-0001" } }, "user")).object,
    a.pin,
  );
  assert.deepEqual((await f.first.categories()).categories, ["Engineering"]);
});

test("parallel categorized creates revalidate canonical spelling under the publication gate", async (t) => {
  const f = taskBoardFixture(t);
  await Promise.all([f.first.categories(), f.second.categories()]);
  let secondAtGate!: () => void, releaseSecond!: () => void;
  const secondNormalized = new Promise<void>((resolve) => {
      secondAtGate = resolve;
    }),
    firstPublished = new Promise<void>((resolve) => {
      releaseSecond = resolve;
    }),
    firstAcquire = f.first.coordination.acquire.bind(f.first.coordination),
    secondAcquire = f.second.coordination.acquire.bind(f.second.coordination);
  f.first.coordination.acquire = async (operation) => {
    await secondNormalized;
    return firstAcquire(operation);
  };
  f.second.coordination.acquire = async (operation) => {
    secondAtGate();
    await firstPublished;
    return secondAcquire(operation);
  };
  const firstId = randomUUID(),
    secondId = randomUUID(),
    firstPending = f.invoke({
      action: "create",
      operationId: firstId,
      fields: { ...fields, category: "Engineering" },
    }),
    secondPending = f.invoke(
      {
        action: "create",
        operationId: secondId,
        fields: { ...fields, category: "engineering" },
      },
      "user",
      f.second,
    );
  let first: TaskBoard.ActionOutcome;
  try {
    first = await firstPending;
  } finally {
    releaseSecond();
  }
  const second = await secondPending,
    firstTask = await f.first.store.read("task-board/task", first.task!),
    secondTask = await f.first.store.read("task-board/task", second.task!),
    filtered = await f.first.store.page("task-board/task", {
      limit: 10,
      where: {
        op: "eq",
        field: "data:/fields/category",
        value: "Engineering",
      },
    });
  assert.equal(firstTask.value.fields.category, "Engineering");
  assert.equal(secondTask.value.fields.category, "Engineering");
  assert.deepEqual((await f.first.categories()).categories, ["Engineering"]);
  assert.equal(filtered.items.length, 2);
});

test("category projection replays external edits and excludes archived Tasks across runtime instances", async (t) => {
  const f = taskBoardFixture(t),
    created = await f.create({ category: "Operations" });
  assert.deepEqual((await f.first.categories()).categories, ["Operations"]);
  const removedId = randomUUID();
  const removed = await f.invoke(
    {
      action: "edit",
      operationId: removedId,
      taskId: created.task!.objectId,
      expectedRevision: created.task!.revision,
      fields: { ...fields, category: null },
    },
    "user",
    f.second,
  );
  assert.deepEqual((await f.first.categories()).categories, []);
  const restoredId = randomUUID();
  const restored = await f.invoke(
    {
      action: "edit",
      operationId: restoredId,
      taskId: removed.task!.objectId,
      expectedRevision: removed.task!.revision,
      fields: { ...fields, category: "Support" },
    },
    "user",
    f.second,
  );
  assert.deepEqual((await f.first.categories()).categories, ["Support"]);
  await f.second.store.client.request("objects.archive", {
    mutationId: await scopedOperationId(f.second.store.client, [
      "category-archive",
      restored.task!.objectId,
    ]),
    objectId: restored.task!.objectId,
    archived: true,
  });
  assert.deepEqual((await f.first.categories()).categories, []);
  await f.second.store.client.request("objects.archive", {
    mutationId: await scopedOperationId(f.second.store.client, [
      "category-restore",
      restored.task!.objectId,
    ]),
    objectId: restored.task!.objectId,
    archived: false,
  });
  assert.deepEqual((await f.first.categories()).categories, ["Support"]);
  assert.deepEqual((await f.second.categories()).categories, ["Support"]);
});

test("category projection bounds results and avoids full-family scans after a 10,000 Task warm-up", async () => {
  const total = 10_000,
    pageSize = 200;
  let fullScans = 0,
    gap = false;
  const store = {
    rootObjectId: null,
    async page(_key: string, options: { cursor?: string; where?: unknown }) {
      assert.equal(options.where, undefined);
      fullScans++;
      const offset = Number(options.cursor ?? 0),
        end = Math.min(total, offset + pageSize);
      return {
        items: Array.from({ length: end - offset }, (_, index) => {
          const id = offset + index;
          return {
            objectId: "task-" + String(id).padStart(5, "0"),
            revision: 1,
            values: {
              "data:/fields/category":
                "Category " + String(id).padStart(5, "0"),
              "data:/createdAt": new Date(1_700_000_000_000 + id).toISOString(),
            },
          };
        }),
        nextCursor: end < total ? String(end) : null,
      };
    },
    client: {
      async request(method: string) {
        if (method === "events.subscribe") {
          if (gap) {
            gap = false;
            return {
              items: [],
              throughSequence: 1,
              hasMore: false,
              gap: { prunedThroughSequence: 1, resumeAfterSequence: 1 },
            };
          }
          return { items: [], throughSequence: 1, hasMore: false, gap: null };
        }
        if (method === "events.head") return { throughSequence: 1 };
        if (method === "events.ack") return { acknowledgedSequence: 1 };
        throw new Error("Unexpected fake projection request: " + method);
      },
    },
  } as unknown as TaskBoardStore;
  const projection = new TaskBoardCategoryProjection(
    store,
    "projection-fixture",
  );
  assert.equal((await projection.categories()).categories.length, 256);
  const warmScans = fullScans;
  for (let id = 0; id < total; id++)
    projection.observe("task-" + String(id).padStart(5, "0"), 2, {
      schemaVersion: 1,
      taskKey: "TASK-" + id,
      fields: { ...fields, category: "Updated " + (id % 300) },
      workflowState: "backlog",
      waiting: null,
      publication: null,
      primaryResourceRef: null,
      claim: null,
      attemptCount: 0,
      lastRun: null,
      lastHistory: null,
      latestResult: null,
      acceptedReview: null,
      comments: [],
      commentDeliveries: [],
      agentCommentCount: 0,
      workRevision: 0,
      attachments: [],
      createdAt: new Date(1_700_000_000_000 + id).toISOString(),
      updatedAt: new Date().toISOString(),
    });
  assert.equal((await projection.categories()).categories.length, 256);
  assert.equal(fullScans, warmScans);
  gap = true;
  await projection.categories();
  assert.equal(fullScans, warmScans * 2);
});

test("category catch-up reaches changes behind unrelated event pages before listing or normalizing", async () => {
  let category = "Operations",
    sequence = 0;
  const batches: Array<{
    items: Array<{ topic: string; payload: { objectId: string } }>;
    throughSequence: number;
    hasMore: boolean;
    gap: null;
  }> = [];
  const item = () => ({
    objectId: "task-category",
    revision: category === "Operations" ? 1 : 2,
    values: {
      "data:/fields/category": category,
      "data:/createdAt": "2026-01-01T00:00:00.000Z",
    },
  });
  const store = {
    rootObjectId: null,
    async page(_key: string, options: { where?: { value?: string[] } }) {
      return {
        items:
          !options.where || options.where.value?.includes("task-category")
            ? [item()]
            : [],
        nextCursor: null,
      };
    },
    client: {
      async request(method: string, params: { throughSequence?: number }) {
        if (method === "events.subscribe")
          return (
            batches.shift() ?? {
              items: [],
              throughSequence: sequence,
              hasMore: false,
              gap: null,
            }
          );
        if (method === "events.ack")
          return { acknowledgedSequence: params.throughSequence };
        if (method === "events.head") return { throughSequence: sequence };
        throw new Error("Unexpected fake projection request: " + method);
      },
    },
  } as unknown as TaskBoardStore;
  const projection = new TaskBoardCategoryProjection(store, "backlog-fixture");
  assert.deepEqual((await projection.categories()).categories, ["Operations"]);
  category = "Support";
  batches.push(
    {
      items: Array.from({ length: 100 }, (_, index) => ({
        topic: "hive.object.changed",
        payload: { objectId: "unrelated-" + index },
      })),
      throughSequence: ++sequence,
      hasMore: true,
      gap: null,
    },
    {
      items: [
        {
          topic: "hive.object.changed",
          payload: { objectId: "task-category" },
        },
      ],
      throughSequence: ++sequence,
      hasMore: false,
      gap: null,
    },
  );
  assert.equal(await projection.normalize("support"), "Support");
  assert.deepEqual((await projection.categories()).categories, ["Support"]);
});

test("category warm-up replays changes committed while the Task snapshot is loading", async () => {
  let category = "Before snapshot",
    loading = true,
    afterSnapshot = false;
  const store = {
    rootObjectId: null,
    async page() {
      const value = category;
      if (loading) {
        loading = false;
        category = "During snapshot";
        afterSnapshot = true;
      }
      return {
        items: [
          {
            objectId: "task-loading",
            revision: value === category ? 2 : 1,
            values: {
              "data:/fields/category": value,
              "data:/createdAt": "2026-01-01T00:00:00.000Z",
            },
          },
        ],
        nextCursor: null,
      };
    },
    client: {
      async request(method: string, params: { throughSequence?: number }) {
        if (method === "events.subscribe") {
          if (afterSnapshot) {
            afterSnapshot = false;
            return {
              items: [
                {
                  topic: "hive.object.changed",
                  payload: { objectId: "task-loading" },
                },
              ],
              throughSequence: 1,
              hasMore: false,
              gap: null,
            };
          }
          return { items: [], throughSequence: 0, hasMore: false, gap: null };
        }
        if (method === "events.head")
          return { throughSequence: afterSnapshot ? 1 : 0 };
        if (method === "events.ack")
          return { acknowledgedSequence: params.throughSequence };
        throw new Error("Unexpected fake projection request: " + method);
      },
    },
  } as unknown as TaskBoardStore;
  const projection = new TaskBoardCategoryProjection(store, "loading-fixture");
  assert.deepEqual((await projection.categories()).categories, [
    "During snapshot",
  ]);
});

test("category catch-up stops at its captured boundary while newer events continue", async () => {
  let subscribeCalls = 0,
    continuous = false,
    boundary = 0;
  const store = {
    rootObjectId: null,
    async page() {
      return { items: [], nextCursor: null };
    },
    client: {
      async request(method: string, params: { throughSequence?: number }) {
        if (method === "events.subscribe") {
          subscribeCalls++;
          return {
            items: [],
            throughSequence: subscribeCalls,
            hasMore: continuous,
            gap: null,
          };
        }
        if (method === "events.ack")
          return { acknowledgedSequence: params.throughSequence };
        if (method === "events.head") {
          if (continuous) boundary ||= subscribeCalls + 3;
          return { throughSequence: continuous ? boundary : subscribeCalls };
        }
        throw new Error("Unexpected fake projection request: " + method);
      },
    },
  } as unknown as TaskBoardStore;
  const projection = new TaskBoardCategoryProjection(store, "bounded-fixture");
  await projection.categories();
  const before = subscribeCalls;
  continuous = true;
  await projection.categories();
  assert.equal(subscribeCalls - before, 3);
});

test("category catch-up drains more than eight unrelated event pages to its captured boundary", async () => {
  let subscribeCalls = 0,
    backlog = false;
  const store = {
    rootObjectId: null,
    async page() {
      return { items: [], nextCursor: null };
    },
    client: {
      async request(method: string, params: { throughSequence?: number }) {
        if (method === "events.subscribe") {
          subscribeCalls++;
          return {
            items: [],
            throughSequence: subscribeCalls * 100,
            hasMore: backlog,
            gap: null,
          };
        }
        if (method === "events.ack")
          return { acknowledgedSequence: params.throughSequence };
        if (method === "events.head")
          return { throughSequence: backlog ? 1_250 : subscribeCalls * 100 };
        throw new Error("Unexpected fake projection request: " + method);
      },
    },
  } as unknown as TaskBoardStore;
  const projection = new TaskBoardCategoryProjection(store, "budget-fixture");
  await projection.categories();
  const before = subscribeCalls;
  backlog = true;
  await projection.categories();
  assert.equal(subscribeCalls - before, 12);
});

test("an oversized comment fails before the Task publication marker is reserved", async (t) => {
  const f = taskBoardFixture(t), created = await f.create({ control: "user" });
  let current = await f.first.store.read("task-board/task", created.task!);
  let rejected = false;
  for (let index = 0; index < 20; index++) {
    try {
      const result = await f.invoke({ action: "comment", operationId: randomUUID(), taskId: current.pin.objectId,
        expectedRevision: current.pin.revision, commentId: randomUUID(), body: "x".repeat(65536),
        requests: [], responses: [], attachments: [], replyTo: null });
      current = await f.first.store.read("task-board/task", result.task!);
    } catch (error) {
      assert.ok(error instanceof IvyError && error.code === "content_too_large" && error.outcome === "not_executed");
      rejected = true;
      break;
    }
  }
  assert.equal(rejected, true);
  current = await f.first.store.read("task-board/task", current.pin.objectId);
  assert.equal(current.value.publication, null);
  const cancelled = await f.invoke({ action: "transition", operationId: randomUUID(), taskId: current.pin.objectId,
    expectedRevision: current.pin.revision, workflowState: "cancelled", detail: "Close the full ticket." });
  assert.equal((await f.first.store.read("task-board/task", cancelled.task!)).value.workflowState, "cancelled");
});

test("category revalidation never waits behind an out-of-gate rebuild", async () => {
  let entered!: () => void, release!: () => void;
  const rebuilding = new Promise<void>((resolve) => {
      entered = resolve;
    }),
    resume = new Promise<void>((resolve) => {
      release = resolve;
    });
  const store = {
    rootObjectId: null,
    async page() {
      entered();
      await resume;
      return { items: [], nextCursor: null };
    },
    client: {
      async request(method: string, params: { throughSequence?: number }) {
        if (method === "events.subscribe")
          return {
            items: [],
            throughSequence: 0,
            hasMore: false,
            gap: null,
          };
        if (method === "events.head") return { throughSequence: 0 };
        if (method === "events.ack")
          return { acknowledgedSequence: params.throughSequence };
        throw new Error("Unexpected fake projection request: " + method);
      },
    },
  } as unknown as TaskBoardStore;
  const projection = new TaskBoardCategoryProjection(store, "busy-fixture"),
    rebuild = projection.categories();
  await rebuilding;
  await assert.rejects(
    projection.revalidate("Engineering"),
    code("task_board_category_projection_pending"),
  );
  release();
  await rebuild;
});

test("queued category work remains visible after immediate revalidation completes", async () => {
  let releaseImmediate!: () => void,
    releaseQueued!: () => void,
    immediateEntered!: () => void,
    queuedEntered!: () => void,
    sequence = 0,
    subscriptions = 0,
    activeSubscriptions = 0,
    maximumActiveSubscriptions = 0;
  const immediateStarted = new Promise<void>((resolve) => {
      immediateEntered = resolve;
    }),
    queuedStarted = new Promise<void>((resolve) => {
      queuedEntered = resolve;
    }),
    continueImmediate = new Promise<void>((resolve) => {
      releaseImmediate = resolve;
    }),
    continueQueued = new Promise<void>((resolve) => {
      releaseQueued = resolve;
    }),
    store = {
      rootObjectId: null,
      async page() {
        return { items: [], nextCursor: null };
      },
      client: {
        async request(method: string, params: { throughSequence?: number }) {
          if (method === "events.subscribe") {
            subscriptions++;
            activeSubscriptions++;
            maximumActiveSubscriptions = Math.max(
              maximumActiveSubscriptions,
              activeSubscriptions,
            );
            if (activeSubscriptions > 1)
              throw new IvyError(
                "invalid_ack",
                "Synthetic overlapping subscription access.",
              );
            try {
              if (subscriptions === 2) {
                immediateEntered();
                await continueImmediate;
              } else if (subscriptions === 3) {
                queuedEntered();
                await continueQueued;
              }
              sequence++;
              return {
                items: [],
                throughSequence: sequence,
                hasMore: false,
                gap: null,
              };
            } finally {
              activeSubscriptions--;
            }
          }
          if (method === "events.head") return { throughSequence: sequence };
          if (method === "events.ack")
            return { acknowledgedSequence: params.throughSequence };
          throw new Error("Unexpected fake projection request: " + method);
        },
      },
    } as unknown as TaskBoardStore,
    projection = new TaskBoardCategoryProjection(store, "overlap-fixture");
  await projection.categories();
  const immediate = projection.revalidate("Engineering");
  await immediateStarted;
  const queued = projection.categories();
  releaseImmediate();
  await immediate;
  await queuedStarted;
  await assert.rejects(
    projection.revalidate("Engineering"),
    code("task_board_category_projection_pending"),
  );
  releaseQueued();
  await queued;
  assert.equal(maximumActiveSubscriptions, 1);
});

test("a category catch-up retry reports an unknown outcome for its retained operation", async (t) => {
  const f = taskBoardFixture(t),
    projection = f.first.categoryProjection,
    normalize = projection.normalize.bind(projection);
  let pending = true;
  projection.normalize = async (category) => {
    if (pending) {
      pending = false;
      throw new IvyError(
        "task_board_category_projection_pending",
        "Synthetic category catch-up budget exhaustion.",
        "not_executed",
      );
    }
    return normalize(category);
  };
  const operationId = randomUUID(),
    request: TaskBoard.ActionInput = {
      action: "create",
      operationId,
      fields: { ...fields, category: "Retained category" },
    };
  await assert.rejects(
    f.invoke(request),
    (error) =>
      error instanceof IvyError &&
      error.code === "task_board_category_projection_pending" &&
      error.outcome === "unknown",
  );
  const accepted = await f.first.operations.find("user", operationId);
  assert.equal(accepted?.value.phase, "accepted");
  assert.equal(accepted?.value.writes.length, 0);
  const recovered = await f.invoke(request);
  assert.ok(recovered.task);
  assert.equal(
    (await f.first.store.page("task-board/task", { limit: 10 })).items.length,
    1,
  );
});

test("a bounded category revalidation retry releases the unstarted publication gate", async (t) => {
  const f = taskBoardFixture(t),
    projection = f.first.categoryProjection,
    revalidate = projection.revalidate.bind(projection);
  await projection.categories();
  let pending = true;
  projection.revalidate = async (category) => {
    if (pending) {
      pending = false;
      throw new IvyError(
        "task_board_category_projection_pending",
        "Synthetic in-gate replay budget exhaustion.",
        "not_executed",
      );
    }
    return revalidate(category);
  };
  const operationId = randomUUID(),
    request: TaskBoard.ActionInput = {
      action: "create",
      operationId,
      fields: { ...fields, category: "Deferred category" },
    };
  await assert.rejects(
    f.invoke(request),
    (error) =>
      error instanceof IvyError &&
      error.code === "task_board_category_projection_pending" &&
      error.outcome === "unknown",
  );
  assert.equal(await f.first.coordination.pending(), null);
  assert.equal(
    (await f.first.operations.find("user", operationId))?.value.phase,
    "accepted",
  );
  assert.ok((await f.create()).task);
  assert.ok((await f.invoke(request)).task);
});

test("parallel recovery joins the active execution before an unstarted gate can be released", async (t) => {
  const f = taskBoardFixture(t),
    projection = f.first.categoryProjection,
    revalidate = projection.revalidate.bind(projection);
  await projection.categories();
  let entered!: () => void,
    release!: () => void,
    calls = 0;
  const revalidating = new Promise<void>((resolve) => {
      entered = resolve;
    }),
    resume = new Promise<void>((resolve) => {
      release = resolve;
    });
  projection.revalidate = async (category) => {
    calls++;
    entered();
    await resume;
    return revalidate(category);
  };
  const operationId = randomUUID(),
    running = f.invoke({
      action: "create",
      operationId,
      fields: { ...fields, category: "Single execution" },
    });
  await revalidating;
  const operation = await f.first.operations.find("user", operationId);
  assert.ok(operation);
  const recovery = f.first.recoverOperation(operation.pin.objectId);
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(calls, 1);
  assert.equal(
    (await f.first.coordination.pending())?.objectId,
    operation.pin.objectId,
  );
  await assert.rejects(f.create(), code("task_board_publication_busy"));
  release();
  const [original, recovered] = await Promise.all([running, recovery]);
  assert.deepEqual(recovered.task, original.task);
  assert.equal(await f.first.coordination.pending(), null);
});

test("uncategorized writes do not initialize the category projection", async (t) => {
  const f = taskBoardFixture(t),
    client = f.first.store.client,
    request = client.request.bind(client);
  let subscriptions = 0;
  client.request = ((method: string, params: unknown, options?: unknown) => {
    if (method === "events.subscribe") subscriptions++;
    return request(method as never, params as never, options as never);
  }) as typeof client.request;
  await f.create();
  assert.equal(subscriptions, 0);
  await f.create({ category: "Demand driven" });
  assert.ok(subscriptions > 0);
});

test("category catch-up completes before the publication gate admits unrelated Task changes", async (t) => {
  const f = taskBoardFixture(t),
    independent = await f.create(),
    page = f.first.store.page.bind(f.first.store);
  let entered!: () => void,
    release!: () => void,
    delayed = false;
  const loading = new Promise<void>((resolve) => {
    entered = resolve;
  });
  const continueLoading = new Promise<void>((resolve) => {
    release = resolve;
  });
  f.first.store.page = (async (key: string, options: { select?: string[] }) => {
    if (
      !delayed &&
      key === "task-board/task" &&
      options.select?.includes("data:/fields/category")
    ) {
      delayed = true;
      entered();
      await continueLoading;
    }
    return page(key as never, options as never);
  }) as typeof f.first.store.page;
  const categorizedId = randomUUID();
  const categorized = f.invoke({
    action: "create",
    operationId: categorizedId,
    fields: { ...fields, category: "Slow category" },
  });
  await loading;
  const editId = randomUUID();
  const edited = await f.invoke({
    action: "edit",
    operationId: editId,
    taskId: independent.task!.objectId,
    expectedRevision: independent.task!.revision,
    fields: {
      ...fields,
      title: "Independent while categories load",
      category: null,
    },
  });
  assert.equal(edited.task?.objectId, independent.task!.objectId);
  release();
  assert.ok((await categorized).task);
});

test("Backlog-to-Todo is explicit and agent execution is scheduler-owned", async (t) => {
  const f = taskBoardFixture(t),
    automatic = await f.create({
      control: "agent",
      executionRequirement: null,
    }),
    created = await f.create({ control: "agent" });
  const automaticTodo = await f.invoke({
    action: "transition",
    operationId: "automatic-execution",
    taskId: automatic.task!.objectId,
    expectedRevision: automatic.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  const automaticTask = await f.first.store.read(
    "task-board/task",
    automaticTodo.task!,
  );
  assert.deepEqual(f.first.condition(automaticTask.value), {
    state: "queued",
    detail: "Waiting for the next available execution PC.",
  });
  await assert.rejects(
    f.first.schedule({
      action: "start",
      operationId: "early",
      taskId: created.task!.objectId,
      expectedRevision: created.task!.revision,
      intent: { objectId: "missing", revision: 1 },
      target: { hostId: "fixture-host", serviceNodeId: "native-agent" },
      workspace: {
        hostId: "fixture-host",
        serviceNodeId: "native-agent",
        canonicalCwd: "/missing",
        taskRoot: "/missing",
        bootstrapPath: "/missing",
        intendedPath: "/missing",
        sourcePath: null,
        project: null,
        useWorktree: false,
        repository: null,
      },
      commentIds: [],
    }),
    code("task_board_user_controlled"),
  );
  const moved = await f.invoke({
    action: "transition",
    operationId: "to-todo",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  assert.equal(
    (await f.first.store.read("task-board/task", moved.task!)).value
      .workflowState,
    "todo",
  );
  const manual = await f.create({
    control: "user",
    executionRequirement: null,
  });
  const manualTodo = await f.invoke({
    action: "transition",
    operationId: "manual-to-todo",
    taskId: manual.task!.objectId,
    expectedRevision: manual.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  const manualTask = await f.first.store.read(
    "task-board/task",
    manualTodo.task!,
  );
  assert.deepEqual(f.first.condition(manualTask.value), {
    state: "needs_user",
    detail: "Ready for you to start.",
  });
  const backlog = await f.invoke({
    action: "transition",
    operationId: "manual-to-backlog",
    taskId: manual.task!.objectId,
    expectedRevision: manualTodo.task!.revision,
    workflowState: "backlog",
    detail: null,
  });
  assert.equal(
    (await f.first.store.read("task-board/task", backlog.task!)).value
      .workflowState,
    "backlog",
  );
});

test("comments are immutable task material without scheduling user-controlled work", async (t) => {
  const f = taskBoardFixture(t),
    created = await f.create({ control: "user" });
  const final = await f.invoke({
    action: "saveResult",
    operationId: "manual-result",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    run: null,
    kind: "final",
    content,
  });
  const accepted = await f.invoke({
    action: "review",
    operationId: "accept",
    taskId: created.task!.objectId,
    expectedRevision: final.task!.revision,
    result: final.result!,
    acceptance: "Checked.",
  });
  const comment = await f.invoke({
    action: "comment",
    operationId: "comment-op",
    taskId: created.task!.objectId,
    expectedRevision: accepted.task!.revision,
    commentId: "comment-1",
    body: "Please improve this.",
    requests: [],
    responses: [],
    attachments: [],
    replyTo: null,
  });
  const task = await f.first.store.read("task-board/task", comment.task!);
  assert.equal(task.value.workflowState, "done");
  assert.equal(task.value.workRevision, 0);
  assert.equal(task.value.comments.at(-1)?.body, "Please improve this.");
  assert.deepEqual(task.value.commentDeliveries, []);
});

test("editing an agent ticket queues a system event instead of a user comment", async (t) => {
  const f = taskBoardFixture(t), created = await f.create({ control: "agent" });
  const before = await f.first.store.read("task-board/task", created.task!);
  const edited = await f.invoke({ action: "edit", operationId: randomUUID(), taskId: created.task!.objectId,
    expectedRevision: created.task!.revision, fields: { ...before.value.fields, title: "Updated title" } });
  const after = (await f.first.store.read("task-board/task", edited.task!)).value;
  assert.equal(after.comments.length, 1);
  assert.equal(after.comments[0]?.authorKind, "system");
  assert.equal(after.comments[0]?.body, "Ticket edited");
  assert.equal(after.commentDeliveries[0]?.state, "queued");
});

test("scheduler start retains resolved target, workspace and inline plan", async (t) => {
  const f = taskBoardFixture(t),
    planned = await f.planned("agent");
  const request: TaskBoard.StartRequest = {
    action: "start",
    operationId: randomUUID(),
    taskId: planned.task.objectId,
    expectedRevision: planned.task.revision,
    intent: planned.plan,
    target: planned.target,
    workspace: planned.workspace,
    commentIds: [],
  };
  const started = await f.first.schedule(request);
  const run = await f.first.store.read("task-board/run", started.run!);
  assert.deepEqual(run.value.target, planned.target);
  assert.deepEqual(run.value.workspace, planned.workspace);
  assert.equal(run.value.plan.location?.cwd, planned.workspace.canonicalCwd);
  assert.equal(
    (await f.first.store.read("task-board/task", started.task!)).value
      .workflowState,
    "in_progress",
  );
});

test("failed actions only release their own workspace reservation", async (t) => {
  const f = taskBoardFixture(t), planned = await f.planned("agent", { kind: "directory_path", path: String(f.plan.threadStart.cwd) }), other = await f.create();
  const request: TaskBoard.StartRequest = { action: "start", operationId: "shared-workspace-operation",
    taskId: planned.task.objectId, expectedRevision: planned.task.revision, intent: planned.plan,
    target: planned.target, workspace: planned.workspace, commentIds: [] };
  const write = f.first.store.writePrepared.bind(f.first.store);
  f.first.store.writePrepared = async (...args: Parameters<TaskBoardStore["writePrepared"]>) => {
    if (args[0] === "task-board/task") throw new IvyError("revision_conflict", "Concurrent Task write.");
    return write(...args);
  };
  await assert.rejects(f.first.schedule(request), code("revision_conflict"));
  assert.equal(f.first.store.reserveWorkspace(planned.target.hostId, planned.workspace.intendedPath,
    other.task!.objectId, "probe"), null);
  f.first.store.releaseWorkspace(other.task!.objectId);
  f.first.store.writePrepared = write;
  await f.first.schedule({ ...request, operationId: "active-workspace-operation" });
  f.first.store.reserveWorkspace(planned.target.hostId, planned.workspace.intendedPath, planned.task.objectId, "legacy-reservation");
  await assert.rejects(f.invoke({ action: "transition", operationId: "active-workspace-operation",
    taskId: other.task!.objectId, expectedRevision: other.task!.revision, workflowState: "waiting", detail: null }),
  code("task_board_invalid_transition"));
  assert.equal(f.first.store.reserveWorkspace(planned.target.hostId, planned.workspace.intendedPath,
    other.task!.objectId, "another-probe"), planned.task.objectId);
});

test("parallel opt-in admits another Task and explicit reservations still transfer atomically", async (t) => {
  const f = taskBoardFixture(t),
    first = await f.planned("agent", { kind: "directory_path", path: String(f.plan.threadStart.cwd) }),
    second = await f.planned("agent", { kind: "directory_path", path: String(f.plan.threadStart.cwd) });
  for (const value of [first, second]) {
    const current = await f.first.store.read("task-board/task", value.task);
    const edited = await f.invoke({ action: "edit", operationId: randomUUID(), taskId: value.task.objectId,
      expectedRevision: value.task.revision, fields: { ...current.value.fields, allowParallel: true } });
    value.task = edited.task!;
  }
  const start = (
    value: typeof first,
    operationId: string,
  ): TaskBoard.StartRequest => ({
    action: "start",
    operationId,
    taskId: value.task.objectId,
    expectedRevision: value.task.revision,
    intent: value.plan,
    target: value.target,
    workspace: value.workspace,
    commentIds: [],
  });
  const active = await f.first.schedule(start(first, "first-workspace"));
  const concurrent = await f.first.schedule(start(second, "second-workspace"));
  assert.ok(concurrent.run);
  const task = await f.first.store.read("task-board/task", active.task!);
  const reassigned = await f.invoke({
    action: "reassign",
    operationId: "release-workspace",
    taskId: task.pin.objectId,
    expectedRevision: task.pin.revision,
    executionRequirement: task.value.fields.executionRequirement,
    workspaceRequirement: task.value.fields.workspaceRequirement,
    reason: "Move unresolved work.",
    previousRun: active.run,
    acknowledgeUnresolved: true,
  });
  assert.equal(
    (await f.first.store.read("task-board/task", reassigned.task!)).value
      .workflowState,
    "todo",
  );
  assert.deepEqual((await f.first.store.read("task-board/task", second.task.objectId)).value.lastRun, concurrent.run);
  const nextPath = first.workspace.intendedPath + "-new";
  assert.equal(
    f.first.store.reserveWorkspace(
      first.target.hostId,
      nextPath,
      first.task.objectId,
      "transfer-workspace",
    ),
    null,
  );
  assert.equal(
    f.first.store.reserveWorkspace(
      first.target.hostId,
      nextPath,
      second.task.objectId,
      "collision",
    ),
    first.task.objectId,
  );
});

test("host and workspace are ticket fields until the first run, then need reassign", async (t) => {
  const f = taskBoardFixture(t),
    created = await f.create({ control: "agent" });
  const moved = await f.invoke({
    action: "edit",
    operationId: "unstarted-assignment-edit",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    fields: { ...fields, control: "agent", executionRequirement: { kind: "automatic" } },
  });
  assert.deepEqual(
    (await f.first.store.read("task-board/task", moved.task!)).value.fields.executionRequirement,
    { kind: "automatic" },
  );
  const planned = await f.planned("agent");
  const started = await new TaskBoardScheduler(f.first).task(planned.task.objectId);
  assert.ok(started?.run);
  const current = await f.first.store.read("task-board/task", planned.task.objectId);
  await assert.rejects(
    f.invoke({
      action: "edit",
      operationId: "started-assignment-edit",
      taskId: planned.task.objectId,
      expectedRevision: current.pin.revision,
      fields: { ...current.value.fields, executionRequirement: { kind: "automatic" } },
    }),
    code("task_board_reassign_required"),
  );
});

test("a blocked Task keeps its marker while an independent Task publication progresses", async (t) => {
  const f = taskBoardFixture(t),
    blocked = await f.create(),
    independent = await f.create();
  let loseHistory = true;
  f.intercept((params) => {
    if (
      loseHistory &&
      "create" in params &&
      params.create.contractKey === "task-board/history"
    ) {
      loseHistory = false;
      throw new IvyError(
        "synthetic_history_response_loss",
        "Synthetic response loss after the Task reservation.",
        "unknown",
      );
    }
  });
  const blockedId = randomUUID();
  await assert.rejects(
    f.invoke({
      action: "edit",
      operationId: blockedId,
      taskId: blocked.task!.objectId,
      expectedRevision: blocked.task!.revision,
      fields: { ...fields, title: "Blocked publication" },
    }),
    code("synthetic_history_response_loss"),
  );
  const marked = await f.first.store.read(
    "task-board/task",
    blocked.task!.objectId,
  );
  assert.equal(
    marked.value.publication?.objectId,
    (await f.first.operations.find("user", blockedId))!.pin.objectId,
  );
  assert.equal(await f.first.coordination.pending(), null);
  const independentId = randomUUID(),
    progressed = await f.invoke({
      action: "edit",
      operationId: independentId,
      taskId: independent.task!.objectId,
      expectedRevision: independent.task!.revision,
      fields: { ...fields, title: "Independent publication" },
    });
  assert.equal(
    (await f.first.store.read("task-board/task", progressed.task!)).value.fields
      .title,
    "Independent publication",
  );
  await assert.rejects(
    f.invoke({
      action: "edit",
      operationId: randomUUID(),
      taskId: marked.pin.objectId,
      expectedRevision: marked.pin.revision,
      fields: { ...fields, title: "Conflicting publication" },
    }),
    code("revision_conflict"),
  );
  f.intercept(null);
  const recovered = await f.first.recoverOperation(
    (await f.first.operations.find("user", blockedId))!.pin.objectId,
  );
  assert.equal(
    (await f.first.store.read("task-board/task", recovered.task!)).value
      .publication,
    null,
  );
});

test("an ambiguous first Task write does not block another Task", async (t) => {
  const f = taskBoardFixture(t),
    first = await f.create(),
    second = await f.create();
  let loseReservation = true;
  f.intercept((params) => {
    if (
      loseReservation &&
      !("create" in params) &&
      params.objectId === first.task!.objectId
    ) {
      loseReservation = false;
      throw new IvyError(
        "synthetic_reservation_response_loss",
        "Synthetic first-write response loss.",
        "unknown",
      );
    }
  });
  const firstId = randomUUID();
  await assert.rejects(
    f.invoke({
      action: "edit",
      operationId: firstId,
      taskId: first.task!.objectId,
      expectedRevision: first.task!.revision,
      fields: { ...fields, title: "Ambiguous reservation" },
    }),
    code("synthetic_reservation_response_loss"),
  );
  assert.equal(
    (await f.first.coordination.pending())?.objectId,
    (await f.first.operations.find("user", firstId))!.pin.objectId,
  );
  const secondId = randomUUID();
  const progressed = await f.invoke({
    action: "edit",
    operationId: secondId,
    taskId: second.task!.objectId,
    expectedRevision: second.task!.revision,
    fields: { ...fields, title: "Independent publication" },
  });
  f.intercept(null);
  await f.first.recoverPublication();
  assert.equal(await f.first.coordination.pending(), null);
  assert.equal(
    (await f.first.store.read("task-board/task", progressed.task!)).value.fields
      .title,
    "Independent publication",
  );
});

test("dependency intents become visible in the first Task reservation and reject a concurrent cycle", async (t) => {
  const f = taskBoardFixture(t),
    a = await f.create(),
    b = await f.create();
  let release!: () => void, entered!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const reservationEntered = new Promise<void>((resolve) => {
    entered = resolve;
  });
  let hold = true;
  f.intercept(async (params) => {
    if (hold && !("create" in params) && params.objectId === a.task!.objectId) {
      hold = false;
      entered();
      await held;
    }
  });
  const aId = randomUUID(),
    bId = randomUUID();
  const editA = f.invoke({
    action: "edit",
    operationId: aId,
    taskId: a.task!.objectId,
    expectedRevision: a.task!.revision,
    fields: { ...fields, dependencies: [b.task!.objectId] },
  });
  await reservationEntered;
  const editB = f.invoke({
    action: "edit",
    operationId: bId,
    taskId: b.task!.objectId,
    expectedRevision: b.task!.revision,
    fields: { ...fields, dependencies: [a.task!.objectId] },
  });
  await assert.rejects(editB, code("task_board_publication_busy"));
  release();
  await editA;
  f.intercept(null);
  await assert.rejects(
    f.first.recoverOperation(
      (await f.first.operations.find("user", bId))!.pin.objectId,
    ),
    code("task_board_dependency_cycle"),
  );
  assert.deepEqual(
    (await f.first.store.read("task-board/task", a.task!.objectId)).value.fields
      .dependencies,
    [b.task!.objectId],
  );
  assert.deepEqual(
    (await f.first.store.read("task-board/task", b.task!.objectId)).value.fields
      .dependencies,
    [],
  );
});

test("agent comments have a per-user durable read marker", async (t) => {
  const f = taskBoardFixture(t),
    created = await f.create({ control: "user" });
  const commented = await f.worker({
    action: "comment",
    operationId: "agent-comment",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    commentId: "agent-comment-1",
    body: "Agent progress update.",
    requests: [],
    responses: [],
    attachments: [],
    replyTo: null,
  });
  assert.equal(
    (await f.first.task({ task: commented.task!.objectId }, "user"))
      .unreadAgentComments,
    1,
  );
  const marked = await f.first.markCommentsRead(
    {
      operationId: "mark-comments",
      taskId: commented.task!.objectId,
      expectedTaskRevision: commented.task!.revision,
    },
    {
      callerPrincipalId: "user",
      operationId: "mark-comments",
      generation: 1,
      signal: new AbortController().signal,
    },
  );
  assert.equal(marked.unreadAgentComments, 0);
  assert.equal(
    (await f.first.task({ task: commented.task!.objectId }, "user"))
      .unreadAgentComments,
    0,
  );
});

test("a plain worker question needs no typed request and any user comment resumes waiting agent work", async (t) => {
  const f = taskBoardFixture(t),
    created = await f.create({ control: "agent", userContact: "chat" });
  const ready = await f.invoke({
    action: "transition",
    operationId: "plain-ready",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  const waiting = await f.invoke({
    action: "transition",
    operationId: "plain-wait",
    taskId: created.task!.objectId,
    expectedRevision: ready.task!.revision,
    workflowState: "waiting",
    detail: "Waiting for user information.",
  });
  const asked = await f.worker({
    action: "comment",
    operationId: "plain-question",
    taskId: created.task!.objectId,
    expectedRevision: waiting.task!.revision,
    commentId: "plain-question-comment",
    purpose: "question",
    contactUser: true,
    body: "Which variant should I use?",
    requests: [],
    responses: [],
    attachments: [],
    replyTo: null,
  });
  const askedTask = await f.first.store.read("task-board/task", asked.task!);
  assert.equal(askedTask.value.workflowState, "waiting");
  assert.deepEqual(askedTask.value.comments.at(-1)?.requests, []);
  assert.ok(askedTask.value.comments.at(-1)?.delivery);
  const replied = await f.invoke({
    action: "comment",
    operationId: "plain-answer",
    taskId: created.task!.objectId,
    expectedRevision: asked.task!.revision,
    commentId: "plain-answer-comment",
    body: "Use variant B.",
    requests: [],
    responses: [],
    attachments: [],
    replyTo: "plain-question-comment",
  });
  const repliedTask = await f.first.store.read("task-board/task", replied.task!);
  assert.equal(repliedTask.value.workflowState, "todo");
  assert.equal(repliedTask.value.waiting, null);
  assert.equal(
    repliedTask.value.commentDeliveries.at(-1)?.commentId,
    "plain-answer-comment",
  );
});

test("a user can record a TaskBoard comment without scheduling agent work", async (t) => {
  const f = taskBoardFixture(t), created = await f.create({ control: "agent" });
  const ready = await f.invoke({ action: "transition", operationId: "silent-ready", taskId: created.task!.objectId,
    expectedRevision: created.task!.revision, workflowState: "todo", detail: null });
  const waiting = await f.invoke({ action: "transition", operationId: "silent-wait", taskId: created.task!.objectId,
    expectedRevision: ready.task!.revision, workflowState: "waiting", detail: "Waiting for user." });
  const before = await f.first.store.read("task-board/task", waiting.task!);
  const noted = await f.invoke({ action: "comment", operationId: "silent-note", taskId: created.task!.objectId,
    expectedRevision: waiting.task!.revision, commentId: "silent-comment", body: "For the record.",
    requests: [], responses: [], attachments: [], replyTo: null, agentDelivery: "none" });
  const after = await f.first.store.read("task-board/task", noted.task!);
  assert.equal(after.value.workflowState, "waiting");
  assert.equal(after.value.workRevision, before.value.workRevision);
  assert.deepEqual(after.value.commentDeliveries, before.value.commentDeliveries);
  assert.equal(after.value.comments.at(-1)?.body, "For the record.");
});

test("typed approval responses reference the exact request while remaining ordinary comments", async (t) => {
  const f = taskBoardFixture(t),
    created = await f.create({ control: "agent" });
  const asked = await f.invoke(
    {
      action: "comment",
      operationId: "approval-question",
      taskId: created.task!.objectId,
      expectedRevision: created.task!.revision,
      commentId: "approval-question-comment",
      body: "Deployment needs approval.",
      requests: [
        {
          requestId: "deploy-approval",
          type: "approval",
          title: "Deploy",
          detail: "Deploy the saved build.",
        },
      ],
      responses: [],
      attachments: [],
      replyTo: null,
    },
    "worker",
  );
  await assert.rejects(
    f.invoke({
      action: "comment",
      operationId: "wrong-approval",
      taskId: created.task!.objectId,
      expectedRevision: asked.task!.revision,
      commentId: "wrong-approval-comment",
      body: "Approved.",
      requests: [],
      responses: [
        {
          requestId: "missing",
          type: "approval",
          decision: "approved",
          detail: "",
        },
      ],
      attachments: [],
      replyTo: null,
    }),
    code("task_board_response_request_missing"),
  );
  const response = await f.invoke({
    action: "comment",
    operationId: "approval-answer",
    taskId: created.task!.objectId,
    expectedRevision: asked.task!.revision,
    commentId: "approval-answer-comment",
    body: "Approved.",
    requests: [],
    responses: [
      {
        requestId: "deploy-approval",
        type: "approval",
        decision: "approved",
        detail: "",
      },
    ],
    attachments: [],
    replyTo: "approval-question-comment",
  });
  assert.equal(
    (
      await f.first.store.read("task-board/task", response.task!)
    ).value.comments.at(-1)?.responses[0]?.decision,
    "approved",
  );
});

test("material field edits during an active claim are rejected", async (t) => {
  const f = taskBoardFixture(t),
    planned = await f.planned();
  const started = await f.first.schedule({
    action: "start",
    operationId: "claim",
    taskId: planned.task.objectId,
    expectedRevision: planned.task.revision,
    intent: planned.plan,
    target: planned.target,
    workspace: planned.workspace,
    commentIds: [],
  });
  const task = await f.first.store.read("task-board/task", started.task!);
  await assert.rejects(
    f.invoke({
      action: "edit",
      operationId: "active-edit",
      taskId: task.pin.objectId,
      expectedRevision: task.pin.revision,
      fields: { ...fields, title: "Changed while active" },
    }),
    code("task_board_claim_active"),
  );
});

test("contact edits during an active claim preserve the running assignment with omitted optional fields", async (t) => {
  const f = taskBoardFixture(t), planned = await f.planned();
  const started = await f.first.schedule({
    action: "start", operationId: "contact-claim", taskId: planned.task.objectId,
    expectedRevision: planned.task.revision, intent: planned.plan, target: planned.target,
    workspace: planned.workspace, commentIds: [],
  });
  const before = await f.first.store.read("task-board/task", started.task!);
  assert.equal(before.value.workflowState, "in_progress");
  assert.ok(before.value.claim);
  assert.equal(before.value.fields.nativeOptions, undefined);
  assert.equal(before.value.fields.requiredCapabilities, undefined);
  const request = {
    action: "edit" as const, operationId: "contact-edit", taskId: before.pin.objectId,
    expectedRevision: before.pin.revision, fields: { ...before.value.fields, userContact: "chat" as const },
  };
  const edited = await f.invoke(request);
  assert.deepEqual(await f.invoke(request), edited);
  const after = (await f.first.store.read("task-board/task", edited.task!)).value;
  assert.equal(after.fields.userContact, "chat");
  assert.equal(after.workflowState, "in_progress");
  for (const key of ["claim", "lastRun", "primaryResourceRef", "attemptCount", "waiting", "workRevision", "commentDeliveries"] as const)
    assert.deepEqual(after[key], before.value[key], key);
  for (const change of [{ requiredCapabilities: ["audio"] }, { nativeOptions: { model: null, reasoningEffort: "low", serviceTier: null } }]) {
    await assert.rejects(f.invoke({
      ...request, operationId: randomUUID(), expectedRevision: edited.task!.revision,
      fields: { ...after.fields, ...change },
    }), code("task_board_claim_active"));
  }
});
