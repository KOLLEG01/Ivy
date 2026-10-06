import test from "node:test";
import assert from "node:assert/strict";
import { fields, taskBoardFixture } from "./fixtures/task-board.js";
import { TaskBoardScheduler } from "../services/task-board/src/runtime/scheduler.js";
import {
  effectiveExecution,
  initialConfiguration,
} from "../services/task-board/src/runtime/configuration.js";
import type { TaskBoard } from "../packages/contracts/src/generated.js";
import { IvyError } from "../packages/sdk/src/node.js";

test("new TaskBoard preferences use the deployment model without binding a personal host", () => {
  const { defaults } = initialConfiguration();
  assert.equal(defaults.executionRequirement, null);
  assert.deepEqual(defaults.nativeOptions, {
    model: "gpt-6.1-sol",
    reasoningEffort: "xhigh",
    serviceTier: "standard",
  });
  assert.equal(defaults.userContact, "chat");
  assert.equal(defaults.useWorktree, false);
  assert.equal(defaults.allowParallel, false);
});

test("TaskBoard settings are read without creation, saved durably and revision checked", async (t) => {
  const f = taskBoardFixture(t);
  assert.deepEqual(await f.first.configuration(), {
    object: null,
    configuration: initialConfiguration(),
  });
  assert.equal(
    (await f.first.store.page("task-board/configuration")).items.length,
    0,
  );
  const value: TaskBoard.Configuration = {
    schemaVersion: 1,
    defaults: {
      executionRequirement: { kind: "host", hostId: "fixture-host" },
      nativeOptions: {
        model: "fixture-model",
        reasoningEffort: "high",
        serviceTier: "fast",
      },
      userContact: "chat",
      useWorktree: true,
      allowParallel: true,
    },
  };
  const request = {
    action: "configure" as const,
    operationId: "save-defaults",
    configuration: null,
    value,
  };
  const saved = await f.invoke(request);
  assert.ok(saved.configuration);
  assert.deepEqual(await f.second.configuration(), {
    object: saved.configuration,
    configuration: value,
  });
  const {
    userContact: _contact,
    allowParallel: _parallel,
    ...submitted
  } = fields;
  const created = await f.invoke({
    action: "create",
    operationId: "configured-create",
    fields: submitted,
  });
  const savedTask = (await f.first.store.read("task-board/task", created.task!))
    .value;
  assert.equal(savedTask.fields.userContact, "chat");
  assert.equal(savedTask.fields.allowParallel, true);
  const explicit = await f.create({ userContact: "ticket" });
  assert.equal(
    (await f.first.store.read("task-board/task", explicit.task!)).value.fields
      .userContact,
    "ticket",
  );
  assert.deepEqual(await f.invoke(request), saved);
  await assert.rejects(f.invoke({ ...request, operationId: "stale-create" }), {
    code: "revision_conflict",
  });
  const changed = await f.invoke({
    ...request,
    operationId: "change-defaults",
    configuration: saved.configuration!,
    value: initialConfiguration(),
  });
  assert.equal(
    changed.configuration!.revision,
    saved.configuration!.revision + 1,
  );
  await assert.rejects(
    f.invoke({
      ...request,
      operationId: "stale-update",
      configuration: saved.configuration!,
    }),
    { code: "revision_conflict" },
  );
  assert.deepEqual(
    (await f.first.configuration()).configuration,
    initialConfiguration(),
  );
  const edited = await f.invoke({
    action: "edit",
    operationId: "configured-edit",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    fields: { ...submitted, title: "Keep the task contact setting" },
  });
  assert.equal(
    (await f.first.store.read("task-board/task", edited.task!)).value.fields
      .userContact,
    "chat",
  );
});

test("TaskBoard validates native model, reasoning, registered host and workflow scope", async (t) => {
  const f = taskBoardFixture(t),
    value = initialConfiguration();
  value.defaults.nativeOptions = {
    model: "missing-model",
    reasoningEffort: "high",
    serviceTier: "standard",
  };
  await assert.rejects(
    f.invoke({
      action: "configure",
      operationId: "missing-model",
      configuration: null,
      value,
    }),
    { code: "task_board_model_unavailable" },
  );
  value.defaults.nativeOptions.model = "fixture-model";
  value.defaults.nativeOptions.reasoningEffort = "impossible";
  await assert.rejects(
    f.invoke({
      action: "configure",
      operationId: "invalid-effort",
      configuration: null,
      value,
    }),
    { code: "task_board_model_unavailable" },
  );
  value.defaults.executionRequirement = {
    kind: "host",
    hostId: "unknown-host",
  };
  await assert.rejects(
    f.invoke({
      action: "configure",
      operationId: "unknown-host",
      configuration: null,
      value,
    }),
    { code: "task_board_target_unavailable" },
  );
  await assert.rejects(
    f.invoke({
      action: "configure",
      operationId: "wrong-scope",
      configuration: null,
      value: initialConfiguration(),
      expectedWorkspace: {
        principalId: f.settings.principalId,
        rootObjectId: "other-root",
        callerPrincipalId: "user",
      },
    }),
    { code: "task_board_workspace_changed" },
  );
});

test("task creation rejects invalid execution selections before publishing a ticket", async (t) => {
  const f = taskBoardFixture(t);
  for (const [operationId, overrides, code] of [
    [
      "invalid-model",
      {
        nativeOptions: {
          model: "sol-6",
          reasoningEffort: "high",
          serviceTier: "standard",
        },
      },
      "task_board_model_unavailable",
    ],
    [
      "invalid-reasoning",
      {
        nativeOptions: {
          model: "fixture-model",
          reasoningEffort: "max",
          serviceTier: "standard",
        },
      },
      "task_board_model_unavailable",
    ],
    [
      "invalid-host",
      { executionRequirement: { kind: "host", hostId: "unknown-host" } },
      "task_board_target_unavailable",
    ],
  ] as const) {
    await assert.rejects(
      f.invoke({
        action: "create",
        operationId,
        fields: { ...fields, control: "agent", ...overrides },
      }),
      { code },
    );
    const operation = await f.first.operations.find("user", operationId);
    assert.equal(operation!.value.phase, "failed");
    assert.equal(operation!.value.writes.length, 0);
  }
  assert.equal((await f.first.store.page("task-board/task")).items.length, 0);
  assert.equal(
    (await f.first.store.page("task-board/history")).items.length,
    0,
  );
  const created = await f.create({
    control: "agent",
    nativeOptions: {
      model: "fixture-model",
      reasoningEffort: "high",
      serviceTier: "standard",
    },
  });
  assert.equal(
    (await f.first.store.read("task-board/task", created.task!)).value.taskKey,
    "TASK-0001",
  );
});

test("task edits and reassignment reject invalid selections without changing the saved revision", async (t) => {
  const f = taskBoardFixture(t),
    created = await f.create({ control: "agent" });
  const invalidFields = {
    ...fields,
    control: "agent" as const,
    nativeOptions: {
      model: "sol-6",
      reasoningEffort: "high",
      serviceTier: "standard" as const,
    },
  };
  await assert.rejects(
    f.invoke({
      action: "edit",
      operationId: "invalid-edit",
      taskId: created.task!.objectId,
      expectedRevision: created.task!.revision,
      fields: invalidFields,
    }),
    (error: unknown) =>
      error instanceof IvyError &&
      error.code === "task_board_model_unavailable" &&
      /model=sol-6/.test(error.message),
  );
  await assert.rejects(
    f.invoke({
      action: "reassign",
      operationId: "invalid-reassignment",
      taskId: created.task!.objectId,
      expectedRevision: created.task!.revision,
      executionRequirement: { kind: "host", hostId: "unknown-host" },
      workspaceRequirement: fields.workspaceRequirement,
      previousRun: null,
      acknowledgeUnresolved: false,
      reason: "Select another host.",
    }),
    { code: "task_board_target_unavailable" },
  );
  const task = await f.first.store.read(
    "task-board/task",
    created.task!.objectId,
  );
  assert.deepEqual(task.pin, created.task);
  assert.equal(task.value.fields.nativeOptions, undefined);
  assert.deepEqual(
    task.value.fields.executionRequirement,
    fields.executionRequirement,
  );
  const edited = await f.invoke({
    action: "edit",
    operationId: "valid-edit",
    taskId: task.pin.objectId,
    expectedRevision: task.pin.revision,
    fields: {
      ...invalidFields,
      nativeOptions: { ...invalidFields.nativeOptions, model: "fixture-model" },
    },
  });
  assert.equal(
    (await f.first.store.read("task-board/task", edited.task!)).value.fields
      .nativeOptions!.model,
    "fixture-model",
  );
});

test("save validation resolves inherited reasoning and discovers models on later catalog pages", async (t) => {
  const f = taskBoardFixture(t),
    value = initialConfiguration();
  value.defaults.nativeOptions = {
    model: "fixture-model",
    reasoningEffort: "high",
    serviceTier: "standard",
  };
  await f.invoke({
    action: "configure",
    operationId: "inherited-options",
    configuration: null,
    value,
  });
  const pages: (string | undefined)[] = [];
  f.nativeTools(async (call) => {
    if (call.definition.name !== "model/list") return f.nativeManagement(call);
    const params = call.arguments as {
      cursor?: string;
      includeHidden?: boolean;
    };
    assert.equal(params.includeHidden, true);
    pages.push(params.cursor);
    const page = (await f.nativeManagement(call)) as {
      data: {
        id: string;
        model: string;
        supportedReasoningEfforts: {
          reasoningEffort: string;
          description: string;
        }[];
      }[];
      nextCursor: string | null;
    };
    return params.cursor
      ? {
          data: [
            {
              ...page.data[0]!,
              id: "limited-model",
              model: "limited-model",
              supportedReasoningEfforts: [
                { reasoningEffort: "low", description: "Low" },
              ],
            },
          ],
          nextCursor: null,
        }
      : { data: page.data, nextCursor: "more-models" };
  });
  await assert.rejects(
    f.create({
      control: "agent",
      nativeOptions: {
        model: "limited-model",
        reasoningEffort: null,
        serviceTier: null,
      },
    }),
    { code: "task_board_model_unavailable" },
  );
  const selected = {
    model: "limited-model",
    reasoningEffort: "low",
    serviceTier: null,
  };
  const created = await f.create({ control: "agent", nativeOptions: selected });
  assert.deepEqual(
    (await f.first.store.read("task-board/task", created.task!)).value.fields
      .nativeOptions,
    selected,
  );
  assert.deepEqual(pages, [undefined, "more-models", undefined, "more-models"]);
});

test("offline hosts reject unverified concrete defaults and native options but permit ordinary edits", async (t) => {
  const f = taskBoardFixture(t),
    created = await f.create({
      control: "agent",
      nativeOptions: {
        model: "fixture-model",
        reasoningEffort: "high",
        serviceTier: "standard",
      },
    });
  await f.clients
    .get("native-agent")!
    .client.request("service.heartbeat", { ready: false, diagnostics: [] });
  const value = initialConfiguration();
  value.defaults.nativeOptions.model = "unverified-model";
  await assert.rejects(
    f.invoke({
      action: "configure",
      operationId: "offline-options",
      configuration: null,
      value,
    }),
    { code: "task_board_target_unavailable" },
  );
  await assert.rejects(
    f.create({ control: "agent", nativeOptions: value.defaults.nativeOptions }),
    { code: "task_board_target_unavailable" },
  );
  await assert.rejects(f.create({ control: "agent" }), {
    code: "task_board_target_unavailable",
  });
  const task = (await f.first.store.read("task-board/task", created.task!))
    .value;
  assert.ok(
    (
      await f.invoke({
        action: "edit",
        operationId: "offline-title",
        taskId: created.task!.objectId,
        expectedRevision: created.task!.revision,
        fields: { ...task.fields, title: "Renamed while offline" },
      })
    ).task,
  );
});

test("TaskBoard recovers an uncertain configuration write without another revision", async (t) => {
  const f = taskBoardFixture(t),
    value = initialConfiguration();
  value.defaults.nativeOptions.serviceTier = "fast";
  let lost = false;
  f.intercept((params) => {
    if (
      !lost &&
      "create" in params &&
      params.create.contractKey === "task-board/configuration"
    ) {
      lost = true;
      throw new IvyError(
        "response_lost",
        "The write reply was lost.",
        "unknown",
      );
    }
  });
  const request = {
    action: "configure" as const,
    operationId: "recover-defaults",
    configuration: null,
    value,
  };
  await assert.rejects(f.invoke(request), { code: "response_lost" });
  const before = await f.first.configuration();
  f.intercept(null);
  const outcome = await f.invoke(request, "user", f.second);
  assert.deepEqual(outcome.configuration, before.object);
  assert.deepEqual((await f.second.configuration()).configuration, value);
  assert.equal(
    (await f.first.store.page("task-board/configuration")).items.length,
    1,
  );
});

test("an explicit Standard override clears inherited Fast in both native start calls", async (t) => {
  const f = taskBoardFixture(t),
    value = initialConfiguration();
  value.defaults.nativeOptions.serviceTier = "fast";
  await f.invoke({
    action: "configure",
    operationId: "fast-default",
    configuration: null,
    value,
  });
  const created = await f.create({
    control: "agent",
    executionRequirement: null,
    nativeOptions: {
      model: null,
      reasoningEffort: null,
      serviceTier: "standard",
    },
  });
  const ready = await f.invoke({
    action: "transition",
    operationId: "ready-standard",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  const started = await new TaskBoardScheduler(f.first).task(
    ready.task!.objectId,
  );
  const run = await f.first.store.read("task-board/run", started!.run!);
  assert.equal(run.value.plan.threadStart.serviceTier, null);
  assert.equal(run.value.plan.turnStart.serviceTier, null);
});

test("a model removed after saving remains blocked with a specific explanation and no native run", async (t) => {
  const f = taskBoardFixture(t),
    created = await f.create({
      control: "agent",
      nativeOptions: {
        model: "fixture-model",
        reasoningEffort: null,
        serviceTier: "standard",
      },
    });
  const ready = await f.invoke({
    action: "transition",
    operationId: "ready-unsupported",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  f.nativeTools(async (call) =>
    call.definition.name === "model/list"
      ? { data: [], nextCursor: null }
      : f.nativeManagement(call),
  );
  await new TaskBoardScheduler(f.first).task(ready.task!.objectId);
  const task = await f.first.store.read(
    "task-board/task",
    ready.task!.objectId,
  );
  assert.equal(task.value.workflowState, "waiting");
  assert.match(
    task.value.waiting!.detail,
    /selected model, reasoning and speed/,
  );
  assert.equal((await f.first.store.page("task-board/run")).items.length, 0);
});

test("MCP-created tasks inherit defaults at first allocation, explicit overrides and existing runs retain their values", async (t) => {
  const f = taskBoardFixture(t),
    scheduler = new TaskBoardScheduler(f.first);
  const created = await f.create({
    control: "agent",
    executionRequirement: null,
  });
  const value: TaskBoard.Configuration = {
    schemaVersion: 1,
    defaults: {
      executionRequirement: { kind: "host", hostId: "fixture-host" },
      nativeOptions: {
        model: "fixture-model",
        reasoningEffort: "high",
        serviceTier: "fast",
      },
    },
  };
  const saved = await f.invoke({
    action: "configure",
    operationId: "before-allocation",
    configuration: null,
    value,
  });
  const ready = await f.invoke({
    action: "transition",
    operationId: "ready-inherited",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  const started = await scheduler.task(ready.task!.objectId);
  const run = await f.first.store.read("task-board/run", started!.run!);
  assert.equal(run.value.target.hostId, "fixture-host");
  assert.equal(run.value.plan.turnStart.model, "fixture-model");
  assert.equal(run.value.plan.turnStart.effort, "high");
  assert.equal(run.value.plan.turnStart.serviceTier, "fast");
  assert.equal(run.value.plan.location?.hostId, "fixture-host");
  const task = (await f.first.store.read("task-board/task", started!.task!))
    .value;
  const continuing = {
    ...task,
    primaryResourceRef: {
      serviceNodeId: "native-agent",
      namespace: "codex",
      kind: "thread",
      nativeId: "existing-thread",
    },
  };
  await f.invoke({
    action: "configure",
    operationId: "after-allocation",
    configuration: saved.configuration!,
    value: initialConfiguration(),
  });
  assert.deepEqual(
    await effectiveExecution(f.first.store, continuing),
    value.defaults,
  );
  await f.first.checks.executionOptions(continuing.fields, continuing);
  await assert.rejects(
    f.first.checks.executionOptions(continuing.fields, {
      ...continuing,
      primaryResourceRef: {
        ...continuing.primaryResourceRef,
        serviceNodeId: "unavailable-owner",
      },
    }),
    { code: "task_board_target_unavailable" },
    "An existing context cannot borrow another ready AgentManager's model catalog.",
  );
  const override = await effectiveExecution(f.first.store, {
    ...continuing,
    fields: {
      ...task.fields,
      executionRequirement: { kind: "automatic" },
      nativeOptions: {
        model: null,
        reasoningEffort: "low",
        serviceTier: "standard",
      },
    },
  });
  assert.deepEqual(override, {
    executionRequirement: { kind: "automatic" },
    nativeOptions: {
      model: "fixture-model",
      reasoningEffort: "low",
      serviceTier: "standard",
    },
  });
  assert.equal(
    (await f.first.store.read("task-board/run", started!.run!)).value.plan
      .turnStart.serviceTier,
    "fast",
  );
});
