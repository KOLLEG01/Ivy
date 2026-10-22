import test from "node:test";
import { randomUUID } from "node:crypto";
import assert from "node:assert/strict";
import { IvyError } from "../packages/contracts/src/errors.js";
import { TaskBoardScheduler } from "../services/task-board/src/runtime/scheduler.js";
import { fields, taskBoardFixture } from "./fixtures/task-board.js";

test("failed plan preparation leaves the workspace available", async (t) => {
  const f = taskBoardFixture(t), planned = await f.planned("agent");
  const write = f.first.store.write.bind(f.first.store);
  f.first.store.write = async (...args) => {
    if (args[0] === "task-board/native-plan") throw new IvyError("plan_failure", "Synthetic plan preparation failure.");
    return write(...args);
  };
  const outcome = await new TaskBoardScheduler(f.first).task(planned.task.objectId);
  assert.ok(outcome?.task);
  const task = await f.first.store.read("task-board/task", outcome.task);
  assert.equal(task.value.claim, null);
  assert.match(task.value.runtimeWait?.detail ?? '', /Synthetic plan preparation failure/);
  assert.equal((await f.first.store.page("task-board/run")).items.length, 0);
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

test('scheduler recovers its pending admission across comment revisions and restart', async t => {
  const f = taskBoardFixture(t), planned = await f.planned('agent');
  const acquire = f.first.coordination.acquire.bind(f.first.coordination);
  f.first.coordination.acquire = async () => { throw new IvyError('task_board_publication_busy', 'Another ticket is publishing.', 'unknown'); };
  await assert.rejects(new TaskBoardScheduler(f.first).task(planned.task.objectId), /Another ticket is publishing/);
  f.first.coordination.acquire = acquire;
  const operations = await f.first.store.page('task-board/operation', { where: { op: 'eq', field: 'data:/phase', value: 'accepted' } });
  assert.equal(operations.items.length, 1);
  const original = await f.first.store.read('task-board/operation', operations.items[0]!.objectId);
  await f.invoke({ action: 'comment', operationId: randomUUID(), taskId: planned.task.objectId,
    expectedRevision: planned.task.revision, commentId: randomUUID(), body: 'A correction while allocation waits.',
    requests: [], responses: [], attachments: [], replyTo: null });
  const outcome = await new TaskBoardScheduler(f.second).task(planned.task.objectId);
  assert.ok(outcome?.run);
  const run = await f.second.store.read('task-board/run', outcome.run);
  assert.equal(run.value.operationId, original.value.operationId);
  assert.equal(run.value.originalRequest.expectedRevision, planned.task.revision);
  assert.equal((await f.second.store.page('task-board/scheduler-attempt')).items.length, 1);
  assert.equal((await f.second.store.read('task-board/operation', original.pin.objectId)).value.phase, 'succeeded');
  assert.equal((await f.second.store.read('task-board/task', planned.task.objectId)).value.comments.at(-1)?.body, 'A correction while allocation waits.');
});

test('an unfinished admission does not lock an unrelated ticket', async t => {
  const f = taskBoardFixture(t), first = await f.planned('agent'), other = await f.planned('agent');
  const operation = await f.first.operations.begin({ action: 'start', operationId: randomUUID(),
    taskId: first.task.objectId, expectedRevision: first.task.revision, target: first.target,
    intent: first.plan, workspace: first.workspace, commentIds: [] },
  { principalId: f.settings.principalId, ...f.first.owner, source: 'scheduler' });
  await f.first.coordination.acquire(operation);
  const independent = await new TaskBoardScheduler(f.second).task(other.task.objectId);
  assert.ok(independent?.run);
  const original = await f.first.recoverOperation(operation.pin.objectId);
  assert.ok(original.run);
  assert.notEqual(original.run.objectId, independent.run.objectId);
});

test("scheduler allocates a Todo Task and retains one prepared attempt", async (t) => {
  const f = taskBoardFixture(t),
    scheduler = new TaskBoardScheduler(f.first),
    planned = await f.planned("agent");
  const page = f.first.store.page.bind(f.first.store);
  let runScans = 0;
  t.mock.method(f.first.store, 'page', async (...args: Parameters<typeof page>) => {
    if (args[0] === 'task-board/run') runScans++;
    return page(...args);
  });
  const outcome = await scheduler.task(planned.task.objectId);
  assert.ok(outcome?.run);
  assert.equal(runScans, 0, 'A single eligible host needs no load-balancing scan.');
  const task = await f.first.store.read("task-board/task", outcome.task!);
  const run = await f.first.store.read("task-board/run", outcome.run!);
  assert.equal(task.value.workflowState, "in_progress");
  assert.equal(run.value.workspace.hostId, "fixture-host");
  const ticket = JSON.stringify(run.value.plan.turnStart);
  assert.match(ticket, /TaskBoard TASK-/);
  assert.match(ticket, /Ivy MCP service task-board-one/);
  assert.match(ticket, /Read the ticket and new comments/);
  assert.match(ticket, /Decide from the current ticket whether to reply, continue work or wait/);
  assert.doesNotMatch(ticket, /nextSteps|Execute only|purpose=handoff/);
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
  assert.match(JSON.stringify(other.value.plan.turnStart), new RegExp((await f.first.store.read("task-board/task", second.task)).value.taskKey));
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
    /Read the ticket and new comments/,
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
  assert.equal(task.value.workflowState, "todo");
  assert.equal(task.value.claim, null);
  assert.equal(task.value.runtimeWait?.reason, "capability");
  assert.match(task.value.runtimeWait?.detail ?? "", /video-editing/);
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
  assert.equal(f.first.condition((await f.first.store.read("task-board/task", moved.task!)).value).state, "queued");
  const outcome = await scheduler.task(moved.task!.objectId);
  const task = await f.first.store.read("task-board/task", outcome!.task!);
  assert.equal(task.value.workflowState, "todo");
  assert.equal(task.value.runtimeWait?.reason, "capability");
  assert.match(task.value.runtimeWait?.detail ?? "", /fixture-host does not advertise video-editing/);
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
  assert.equal(task.value.runtimeWait?.reason, "host");
  assert.match(task.value.runtimeWait?.detail ?? "", /No ready AgentManager is available on fixture-host\. fixture-host: .*absolute/);
});

test('appointments and watched tickets wait for their next event', async t => {
  const now = new Date('2026-09-14T06:00:00.000Z'), f = taskBoardFixture(t), scheduler = new TaskBoardScheduler(f.first, () => now);
  const dependency = await f.create({ control: 'user' });
  for (const condition of [{ kind: 'time' as const, at: '2099-09-14T07:00:00.000Z' }, { kind: 'task' as const, taskId: dependency.task!.objectId }]) {
    const created = await f.invoke({ action: 'create', operationId: randomUUID(), fields: { ...fields, control: 'agent' }, workflowState: 'waiting',
      wakeAt: condition.kind === 'time' ? condition.at : null, dependencies: condition.kind === 'task' ? [condition.taskId] : [] });
    assert.equal(await scheduler.task(created.task!.objectId), null);
    const task = await f.first.task({ task: created.task!.objectId }, 'user');
    assert.equal(task.executionCondition.state, condition.kind === 'time' ? 'deferred' : 'blocked_dependency');
    assert.equal(task.task.claim, null); assert.equal(task.task.runtimeWait, null);
  }
});

test("shared directory queues another task while a peer is running", async (t) => {
  const f = taskBoardFixture(t),
    scheduler = new TaskBoardScheduler(f.first),
    owner = await f.planned("agent", { kind: "directory_path", path: String(f.plan.threadStart.cwd) }),
    blocked = await f.planned("agent", { kind: "directory_path", path: String(f.plan.threadStart.cwd) });
  assert.ok((await scheduler.task(owner.task.objectId))?.run);
  // A rebuilt reservation index uses projected fields, not separate ticket reads.
  f.first.store.workspaceOwner = async () => null;
  const read = f.first.store.read.bind(f.first.store); let peerHeaders = 0;
  f.first.store.read = (async (...args: Parameters<typeof read>) => {
    if (args[0] === 'task-board/task' && (typeof args[1] === 'string' ? args[1] : args[1].objectId) === owner.task.objectId) {
      assert.equal(args[4], true, 'Workspace conflict detection must not hydrate a foreign comment history.');
      peerHeaders++;
    }
    return read(...args);
  }) as typeof read;
  const outcome = await scheduler.task(blocked.task.objectId),
    task = await f.first.store.read("task-board/task", outcome!.task!);
  assert.equal(task.value.workflowState, "todo");
  assert.equal(task.value.runtimeWait?.reason, "workspace");
  assert.deepEqual(task.value.dependencies, []);
  assert.equal(outcome?.run, null);
  assert.equal(peerHeaders, 0);
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
  now = Date.parse(failed!.value.updatedAt) + 4999;
  assert.equal(await scheduler.task(planned.task.objectId), null);
  now++;
  const outcome = await scheduler.task(planned.task.objectId);
  assert.ok(outcome?.run);
  const successor = await f.first.store.named("task-board/scheduler-attempt", name, planned.task.objectId);
  assert.equal(successor?.value.sequence, 2);
  assert.notEqual(successor?.value.request.operationId, first.value.request.operationId);
  if (successor?.value.request.action === "start" && first.value.request.action === "start")
    assert.notDeepEqual(successor.value.request.intent, first.value.request.intent);
  assert.equal(await scheduler.task(planned.task.objectId), null);
  assert.equal((await f.first.store.page('task-board/run')).items.length, 1);
});
