import test from "node:test";
import assert from "node:assert/strict";
import { IvyError } from "../packages/contracts/src/errors.js";
import { TaskBoardScheduler } from "../services/task-board/src/runtime/scheduler.js";
import { taskBoardFixture } from "./fixtures/task-board.js";

test("failed plan preparation leaves the workspace available", async (t) => {
  const f = taskBoardFixture(t), planned = await f.planned("agent");
  const write = f.first.store.write.bind(f.first.store);
  f.first.store.write = async (...args) => {
    if (args[0] === "task-board/native-plan") throw new IvyError("plan_failure", "Synthetic plan preparation failure.");
    return write(...args);
  };
  await assert.rejects(new TaskBoardScheduler(f.first).task(planned.task.objectId),
    (error: unknown) => error instanceof IvyError && error.code === "plan_failure");
  assert.equal(f.first.store.reserveWorkspace(planned.target.hostId, planned.workspace.intendedPath, "other-task", "other-run"), null);
});

test("scheduler ignores Backlog and user-controlled Tasks", async (t) => {
  const f = taskBoardFixture(t),
    scheduler = new TaskBoardScheduler(f.first);
  const backlog = await f.create({ control: "agent" });
  const user = await f.planned("user");
  assert.equal(await scheduler.task(backlog.task!.objectId), null);
  assert.equal(await scheduler.task(user.task.objectId), null);
  assert.equal((await f.first.store.page("task-board/run")).items.length, 0);
});

test("scheduler allocates a Todo Task and retains one prepared attempt", async (t) => {
  const f = taskBoardFixture(t),
    scheduler = new TaskBoardScheduler(f.first),
    planned = await f.planned("agent");
  const outcome = await scheduler.task(planned.task.objectId);
  assert.ok(outcome?.run);
  const task = await f.first.store.read("task-board/task", outcome.task!);
  const run = await f.first.store.read("task-board/run", outcome.run!);
  assert.equal(task.value.workflowState, "in_progress");
  assert.equal(run.value.workspace.hostId, "fixture-host");
  const ticket = JSON.stringify(run.value.plan.turnStart);
  assert.match(ticket, /TaskBoard TASK-/);
  assert.match(ticket, /@Ivy MCP/);
  assert.match(ticket, /Read the current ticket/);
  assert.doesNotMatch(ticket, /A real Hive-persisted workflow fixture/);
  assert.ok(
    await f.first.store.named(
      "task-board/scheduler-attempt",
      "Scheduled task " +
        planned.task.objectId +
        " revision " +
        planned.task.revision,
      planned.task.objectId,
    ),
  );
  assert.equal(await scheduler.task(planned.task.objectId), null);
  const registry = f.kernel.registry.getRegistry('native-agent')!;
  for (const tool of registry.namespaces.find(ns => ns.namespace === 'agent')!.tools)
    tool.description += ' Updated planning contract.';
  await f.clients.get('native-agent')!.client.request('registry.sync', registry);
  const second = await f.planned("agent"), concurrent = await scheduler.task(second.task.objectId);
  assert.ok(concurrent?.run);
  const other = await f.first.store.read("task-board/run", concurrent.run);
  assert.equal(other.value.workspace.canonicalCwd, run.value.workspace.canonicalCwd);
  assert.notEqual(other.value.taskId, run.value.taskId);
  assert.match(JSON.stringify(other.value.plan.turnStart), new RegExp(`outputs/${(await f.first.store.read("task-board/task", second.task)).value.taskKey}/`));
  assert.equal(await f.first.store.workspaceOwner(planned.target.hostId, planned.workspace.intendedPath, "probe"), null);
});

test("scheduler automatically allocates an unrestricted Task to a ready PC", async (t) => {
  const f = taskBoardFixture(t),
    scheduler = new TaskBoardScheduler(f.first);
  await f.invoke({
    action: "savePlan",
    operationId: "automatic-plan",
    plan: f.plan,
  });
  const created = await f.create({
    control: "agent",
    executionRequirement: null,
  });
  const moved = await f.invoke({
    action: "transition",
    operationId: "automatic-todo",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  const outcome = await scheduler.task(moved.task!.objectId);
  assert.ok(outcome?.run);
  const run = await f.first.store.read("task-board/run", outcome.run!);
  assert.equal(run.value.target.hostId, "fixture-host");
  assert.match(
    JSON.stringify(run.value.plan.turnStart),
    /assigned workspace/,
  );
});

test("scheduler applies Task model, reasoning and speed to the native plan", async (t) => {
  const f = taskBoardFixture(t),
    scheduler = new TaskBoardScheduler(f.first);
  await f.invoke({
    action: "savePlan",
    operationId: "native-options-plan",
    plan: f.plan,
  });
  const created = await f.create({
    control: "agent",
    nativeOptions: {
      model: "gpt-fixture",
      reasoningEffort: "high",
      serviceTier: "fast",
    },
  });
  const moved = await f.invoke({
    action: "transition",
    operationId: "native-options-todo",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  const outcome = await scheduler.task(moved.task!.objectId);
  assert.ok(outcome?.run);
  const run = await f.first.store.read("task-board/run", outcome.run!);
  assert.equal(run.value.plan.threadStart.model, "gpt-fixture");
  assert.equal(run.value.plan.threadStart.serviceTier, "fast");
  assert.equal(run.value.plan.turnStart.model, "gpt-fixture");
  assert.equal(run.value.plan.turnStart.effort, "high");
  assert.equal(run.value.plan.turnStart.serviceTier, "fast");
});

test("missing capability leaves first-attempt work visibly blocked before its first attempt without a fake failure", async (t) => {
  const f = taskBoardFixture(t),
    scheduler = new TaskBoardScheduler(f.first);
  const created = await f.create({
    control: "agent",
    executionRequirement: { kind: "automatic" },
    requiredCapabilities: ["video-editing"],
  });
  const moved = await f.invoke({
    action: "transition",
    operationId: "capability-todo",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  const outcome = await scheduler.task(moved.task!.objectId);
  assert.ok(outcome?.task);
  const task = await f.first.store.read("task-board/task", outcome!.task!);
  assert.equal(task.value.workflowState, "waiting");
  assert.equal(task.value.claim, null);
  assert.equal(task.value.waiting?.reason, "capability");
  assert.match(task.value.waiting?.detail ?? "", /video-editing/);
  assert.equal(f.first.condition(task.value).state, "blocked_environment");
});

test("a host missing a required capability keeps the Task in Blocked", async (t) => {
  const f = taskBoardFixture(t),
    scheduler = new TaskBoardScheduler(f.first);
  const created = await f.create({
    control: "agent",
    executionRequirement: { kind: "host", hostId: "fixture-host" },
    requiredCapabilities: ["video-editing"],
  });
  const moved = await f.invoke({
    action: "transition",
    operationId: "required-capability-todo",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  assert.deepEqual(f.first.condition((await f.first.store.read("task-board/task", moved.task!)).value), {
    state: "queued",
    detail: "Waiting for fixture-host with video-editing.",
  });
  const outcome = await scheduler.task(moved.task!.objectId);
  const task = await f.first.store.read("task-board/task", outcome!.task!);
  assert.equal(task.value.workflowState, "waiting");
  assert.equal(task.value.waiting?.reason, "capability");
  assert.match(task.value.waiting?.detail ?? "", /fixture-host does not advertise video-editing/);
});

test("an unexpected host failure is named in the waiting detail", async (t) => {
  const f = taskBoardFixture(t),
    scheduler = new TaskBoardScheduler(f.first);
  f.nativeTools(async (call) => {
    if (call.definition.name === "resolveWorkspace")
      throw new IvyError("invalid_arguments", "Selected directory must be absolute.");
    return f.nativeManagement(call);
  });
  const created = await f.create({ control: "agent" });
  const moved = await f.invoke({
    action: "transition",
    operationId: "host-failure-todo",
    taskId: created.task!.objectId,
    expectedRevision: created.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  const outcome = await scheduler.task(moved.task!.objectId);
  const task = await f.first.store.read("task-board/task", outcome!.task!);
  assert.equal(task.value.waiting?.reason, "host");
  assert.match(task.value.waiting?.detail ?? "", /No ready AgentManager is available on fixture-host\. fixture-host: .*absolute/);
});

test("future review and incomplete dependency reasons are retained before the first Run", async (t) => {
  const now = new Date("2026-09-14T06:00:00.000Z"),
    f = taskBoardFixture(t),
    scheduler = new TaskBoardScheduler(f.first, () => now);
  const timed = await f.create({
    control: "agent",
    nextReviewAt: "2099-09-14T07:00:00.000Z",
  });
  const timedTodo = await f.invoke({
    action: "transition",
    operationId: "timed-todo",
    taskId: timed.task!.objectId,
    expectedRevision: timed.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  const timedOutcome = await scheduler.task(timedTodo.task!.objectId),
    timedTask = await f.first.store.read("task-board/task", timedOutcome!.task!);
  assert.equal(timedTask.value.workflowState, "waiting");
  assert.equal(timedTask.value.waiting?.reason, "time");
  assert.match(
    timedTask.value.waiting?.detail ?? "",
    /2099-09-14T07:00:00.000Z/,
  );
  const dependency = await f.create({ control: "user" }),
    dependent = await f.create({
      control: "agent",
      dependencies: [dependency.task!.objectId],
    });
  const dependentTodo = await f.invoke({
    action: "transition",
    operationId: "dependent-todo",
    taskId: dependent.task!.objectId,
    expectedRevision: dependent.task!.revision,
    workflowState: "todo",
    detail: null,
  });
  const dependencyOutcome = await scheduler.task(dependentTodo.task!.objectId),
    dependencyTask = await f.first.store.read(
      "task-board/task",
      dependencyOutcome!.task!,
    );
  assert.equal(dependencyTask.value.workflowState, "waiting");
  assert.equal(dependencyTask.value.waiting?.reason, "dependency");
  assert.equal(
    f.first.condition(dependencyTask.value).state,
    "blocked_dependency",
  );
});

test("shared directory queues another task while a peer is running", async (t) => {
  const f = taskBoardFixture(t),
    scheduler = new TaskBoardScheduler(f.first),
    owner = await f.planned("agent", { kind: "directory_path", path: String(f.plan.threadStart.cwd) }),
    blocked = await f.planned("agent", { kind: "directory_path", path: String(f.plan.threadStart.cwd) });
  assert.ok((await scheduler.task(owner.task.objectId))?.run);
  const outcome = await scheduler.task(blocked.task.objectId),
    task = await f.first.store.read("task-board/task", outcome!.task!);
  assert.equal(task.value.workflowState, "waiting");
  assert.equal(task.value.waiting?.reason, "workspace");
  assert.deepEqual(task.value.blocker, { taskId: owner.task.objectId, until: "idle" });
  assert.equal(outcome?.run, null);
  await scheduler.task(blocked.task.objectId);
  const waiting = await f.first.store.read("task-board/task", blocked.task.objectId);
  for (let pass = 0; pass < 3; pass++) assert.equal(await scheduler.task(blocked.task.objectId), null);
  assert.deepEqual((await f.first.store.read("task-board/task", blocked.task.objectId)).pin, waiting.pin,
    "An unchanged workspace blocker must not create another Task revision on every scan.");
});

test("a conclusive failed scheduler attempt uses a new operation and allocation on retry", async (t) => {
  const f = taskBoardFixture(t), planned = await f.planned("agent");
  let now = Date.now();
  const scheduler = new TaskBoardScheduler(f.first, () => new Date(now));
  const execution = f.first.checks.execution.bind(f.first.checks);
  f.first.checks.execution = async () => { throw new IvyError("task_board_target_unavailable", "Synthetic transient outage."); };
  await assert.rejects(scheduler.task(planned.task.objectId),
    (error: unknown) => error instanceof IvyError && error.code === "task_board_target_unavailable");
  f.first.checks.execution = execution;
  const name = "Scheduled task " + planned.task.objectId + " revision " + planned.task.revision;
  const first = await f.first.store.named("task-board/scheduler-attempt", name, planned.task.objectId);
  assert.ok(first);
  const failed = await f.first.operations.find(f.settings.principalId, first.value.request.operationId);
  assert.equal(failed?.value.phase, "failed");
  now = Date.parse(failed!.value.updatedAt) + 31_000;
  const outcome = await scheduler.task(planned.task.objectId);
  assert.ok(outcome?.run);
  const successor = await f.first.store.named("task-board/scheduler-attempt", name, planned.task.objectId);
  assert.equal(successor?.value.sequence, 2);
  assert.notEqual(successor?.value.request.operationId, first.value.request.operationId);
  if (successor?.value.request.action === "start" && first.value.request.action === "start")
    assert.notDeepEqual(successor.value.request.intent, first.value.request.intent);
});
