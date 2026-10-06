import test from "node:test";
import assert from "node:assert/strict";
import { expect } from "@playwright/test";
import { taskBoardFixture } from "./fixtures/task-board-fixture.mjs";
import { choose } from "./fixtures/select.mjs";

const fields = (title, category = null) => ({
  title,
  description: "Browser-verified TaskBoard Task.",
  acceptanceCriteria: ["The requested state and material are retained."],
  category,
  control: "user",
  priority: 2,
  executionRequirement: null,
  workspaceRequirement: { kind: "task_workspace" },
  dependencies: [],
  userContact: "ticket",
  nextReviewAt: null,
  dueAt: null,
});

for (const [layout, viewport] of [["desktop", { width: 1440, height: 1000 }], ["mobile", { width: 390, height: 844 }]]) {
  test(`running contact edit preserves inherited native options on ${layout}`, { timeout: 180000 }, async t => {
    const f = await taskBoardFixture(t, { native: true, scheduler: true, viewport });
    const created = await f.invoke({ action: "create", fields: { ...fields("Running contact edit"), control: "agent",
      executionRequirement: { kind: "host", hostId: "isolated-agent-host" } } });
    const id = created.task.objectId;
    await f.invoke({ action: "transition", taskId: id, expectedRevision: created.task.revision, workflowState: "todo", detail: null });
    await expect.poll(async () => (await f.task(id)).claim?.phase, { timeout: 30000 }).toBe("running");
    const before = await f.task(id);
    assert.equal(before.fields.nativeOptions, undefined);
    await f.open("#/tasks?id=" + id + "&node=browser-task-board");
    await f.page.getByRole("button", { name: "Edit task", exact: true }).click();
    await f.page.getByRole("group", { name: "Updates", exact: true }).getByRole("button", { name: "Ticket and chat", exact: true }).click();
    await f.page.getByRole("button", { name: "Save changes", exact: true }).click();
    await expect.poll(async () => (await f.task(id)).fields.userContact).toBe("chat");
    const after = await f.task(id);
    assert.equal(after.workflowState, "in_progress");
    assert.equal(after.fields.nativeOptions, undefined);
    assert.equal(after.fields.requiredCapabilities, undefined);
    assert.equal(after.claim.operationId, before.claim.operationId);
    assert.equal(after.lastRun.objectId, before.lastRun.objectId);
    assert.deepEqual(after.primaryResourceRef, before.primaryResourceRef);
    assert.equal(f.current().sent.filter(frame => frame.method === "turn/interrupt").length, 0);
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  });
}

test("TaskBoardUI receives live list changes from another client", { timeout: 180000 }, async t => {
  const f = await taskBoardFixture(t);
  await f.open("#/tasks?node=browser-task-board");
  await expect(f.page.getByRole("heading", { name: "Task board", exact: true })).toBeVisible();
  await f.invoke({ action: "create", fields: fields("An externally created live task") });
  await expect(f.page.getByRole("link", { name: /An externally created live task/ })).toBeVisible({ timeout: 7000 });
  assert.deepEqual(f.pageErrors, []);
});

test("task edits ignore runtime revisions, clear saved drafts and retain field conflicts", { timeout: 180000 }, async t => {
  const f = await taskBoardFixture(t);
  const created = await f.invoke({ action: "create", fields: fields("Edit revision checks") });
  const id = created.task.objectId;
  const read = () => f.client.request("objects.read", { objectId: id });
  const transition = async workflowState => f.invoke({ action: "transition", taskId: id,
    expectedRevision: (await read()).revision.revision, workflowState, detail: null });
  const openEditor = async () => {
    await f.page.getByRole("button", { name: "Edit task", exact: true }).click();
    await expect(f.page.getByLabel("Title", { exact: true })).toBeVisible();
  };
  const save = () => f.page.getByRole("button", { name: "Save changes", exact: true }).click();
  const draftKeys = () => f.page.evaluate(() => Object.keys(sessionStorage).filter(key => key.startsWith("ivy:task-board:draft:")));
  await f.open("#/tasks?id=" + id + "&node=browser-task-board");
  for (const [index, workflowState] of ["todo", "backlog"].entries()) {
    await f.page.setViewportSize(index ? { width: 390, height: 844 } : { width: 1280, height: 900 });
    await openEditor();
    const title = "Saved draft " + index;
    await f.page.getByLabel("Title", { exact: true }).fill(title);
    await expect.poll(draftKeys).toHaveLength(1);
    await transition(workflowState);
    if (index) {
      // Drafts written by an earlier UI contain only their revision, not a field snapshot.
      await f.page.evaluate(() => {
        const key = Object.keys(sessionStorage).find(key => key.startsWith("ivy:task-board:draft:"));
        const draft = JSON.parse(sessionStorage.getItem(key));
        delete draft.baseFields;
        sessionStorage.setItem(key, JSON.stringify(draft));
      });
      await f.page.reload();
      await openEditor();
      await expect(f.page.getByLabel("Title", { exact: true })).toHaveValue(title);
    }
    await save();
    await expect.poll(async () => (await f.task(id)).fields.title).toBe(title);
    await expect(f.page.getByLabel("Title", { exact: true })).toHaveCount(0);
    await expect.poll(draftKeys).toHaveLength(0);
    assert.equal((await f.task(id)).workflowState, workflowState);
  }
  await openEditor();
  await f.page.getByLabel("Title", { exact: true }).fill("Temporary draft");
  await expect.poll(draftKeys).toHaveLength(1);
  const savedFields = (await f.task(id)).fields;
  await f.page.evaluate(({ savedFields, revision }) => {
    const key = Object.keys(sessionStorage).find(key => key.startsWith("ivy:task-board:draft:"));
    const draft = JSON.parse(sessionStorage.getItem(key));
    draft.fields = savedFields;
    draft.revision = revision;
    delete draft.baseFields;
    sessionStorage.setItem(key, JSON.stringify(draft));
  }, { savedFields, revision: created.task.revision });
  await f.page.reload();
  await openEditor();
  await f.page.getByLabel("Title", { exact: true }).fill("Fresh edit after a stale saved draft");
  await save();
  await expect.poll(async () => (await f.task(id)).fields.title).toBe("Fresh edit after a stale saved draft");
  await expect.poll(draftKeys).toHaveLength(0);
  await openEditor();
  await f.page.getByLabel("Title", { exact: true }).fill("My retained title");
  const current = await read();
  await f.invoke({ action: "edit", taskId: id, expectedRevision: current.revision.revision,
    fields: { ...current.content.value.fields, title: "Another client's title" } });
  await save();
  await expect(f.page.getByText("The task changed while you were editing", { exact: true })).toBeVisible();
  await expect(f.page.getByLabel("Title", { exact: true })).toHaveValue("My retained title");
  assert.equal((await f.task(id)).fields.title, "Another client's title");
  await f.page.getByRole("button", { name: "Use current revision with my draft", exact: true }).click();
  await transition("todo");
  await save();
  await expect.poll(async () => (await f.task(id)).fields.title).toBe("My retained title");
  await expect.poll(draftKeys).toHaveLength(0);
  assert.deepEqual(f.pageErrors, []);
});

test("automatic native recovery stays in progress while user input stays blocked on desktop and mobile", { timeout: 180000 }, async t => {
  const f = await taskBoardFixture(t, { native: true, scheduler: true });
  const created = await f.invoke({ action: "create", fields: { ...fields("Recover the original execution"), control: "agent",
    executionRequirement: { kind: "host", hostId: "isolated-agent-host" } } });
  await f.invoke({ action: "transition", taskId: created.task.objectId, expectedRevision: created.task.revision, workflowState: "todo", detail: null });
  const runtime = f.taskBoard().runtime;
  await expect.poll(async () => (await f.task(created.task.objectId)).claim?.run?.objectId, { timeout: 30000 }).toBeTruthy();
  const admitted = await f.task(created.task.objectId);
  assert.ok(runId, JSON.stringify({ state: admitted.workflowState, waiting: admitted.waiting }));
  await runtime.native.drain(runId);
  await expect.poll(async () => (await f.task(created.task.objectId)).claim?.phase).toBe("running");
  const original = await f.task(created.task.objectId);
  const thread = f.threads.get(original.primaryResourceRef.nativeId);
  thread.status = { type: "notLoaded" };
  await runtime.native.step(runId);
  await expect.poll(async () => (await f.task(created.task.objectId)).claim?.phase).toBe("outcome_unknown");
  await f.page.addInitScript(() => Object.defineProperty(navigator, "language", { get: () => "de-DE" }));
  await f.open("#/tasks?node=browser-task-board");
  for (const viewport of [{ width: 1440, height: 1000 }, { width: 390, height: 844 }]) {
    await f.page.setViewportSize(viewport);
    const progress = f.page.getByRole("region", { name: "In progress", exact: true });
    await expect(progress.getByRole("link", { name: /Recover the original execution/ })).toBeVisible();
    await expect(progress.getByText("Statusabgleich", { exact: true })).toBeVisible();
    await expect(f.page.getByRole("region", { name: "Blocked", exact: true }).getByRole("link", { name: /Recover the original execution/ })).toHaveCount(0);
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  }
  thread.status = { type: "active", activeFlags: ["waitingOnUserInput"] };
  await runtime.native.step(runId);
  await expect(f.page.getByRole("region", { name: "Blocked", exact: true }).getByRole("link", { name: /Recover the original execution/ })).toBeVisible();
  await expect(f.page.getByRole("region", { name: "Blocked", exact: true }).getByText("Needs you", { exact: true })).toBeVisible();
  const waiting = await f.task(created.task.objectId);
  assert.equal(waiting.workflowState, "waiting");
  assert.equal(waiting.waiting.reason, "user");
  assert.equal(waiting.attemptCount, original.attemptCount);
  assert.deepEqual(waiting.primaryResourceRef, original.primaryResourceRef);
  for (const method of ["thread/start", "turn/start"]) assert.equal(f.current().sent.filter(frame => frame.method === method).length, 1);
  assert.deepEqual(f.pageErrors, []);
});

test("task project names resolve on their host with desktop and mobile layouts", { timeout: 180000 }, async t => {
  const f = await taskBoardFixture(t, { native: true });
  const created = await f.invoke({ action: "create", fields: { ...fields("Named project"), control: "agent",
    executionRequirement: { kind: "host", hostId: "isolated-agent-host" },
    workspaceRequirement: { kind: "existing_project", projectId: "fixture-project", path: null, useWorktree: false } } });
  await f.open("#/tasks?id=" + created.task.objectId + "&node=browser-task-board");
  await expect(f.page.getByText("Isolated project", { exact: true })).toBeVisible();
  await expect(f.page.getByText("fixture-project", { exact: true })).toHaveCount(0);
  await f.page.getByRole("button", { name: "Edit task", exact: true }).click();
  await expect(f.page.getByLabel("Project", { exact: true })).toContainText("Isolated project");
  await f.page.getByRole("button", { name: "Cancel", exact: true }).click();
  await f.page.setViewportSize({ width: 390, height: 844 });
  await expect(f.page.getByText("Isolated project", { exact: true })).toBeVisible();
  assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(f.pageErrors, []);
});

test(
  "agent tasks preselect a host and its effective native choices",
  { timeout: 180000 },
  async (t) => {
    const f = await taskBoardFixture(t, { native: true });
    await f.addAgent([{ id: "other-model-id", model: "other-provider-model", displayName: "Other provider model",
      description: "Isolated second provider", hidden: false, isDefault: true, defaultReasoningEffort: "low",
      supportedReasoningEfforts: [{ reasoningEffort: "low", description: "Low" }] }]);
    await f.open();
    await f.page.getByRole("link", { name: "New task", exact: true }).click();
    await expect(f.page.getByRole("button", { name: "Advanced options" })).toHaveCount(0);
    await expect(f.page.getByLabel("Acceptance criteria", { exact: true })).toHaveCount(0);
    await expect(f.page.getByLabel("Due", { exact: true })).toHaveCount(0);
    const host = f.page.getByLabel("Host", { exact: true });
    await expect(host).toHaveAttribute("data-value", "isolated-agent-host", { timeout: 20000 });
    const model = f.page.getByLabel("Model", { exact: true });
    await expect(model).toHaveAttribute("data-value", "fixture-model");
    const reasoning = f.page.getByRole("group", { name: "Reasoning", exact: true });
    await expect(reasoning.getByRole("button", { name: "High", exact: true })).toHaveAttribute("aria-pressed", "true");
    await choose(model, "other-provider-model");
    await expect(model).toHaveAttribute("data-value", "other-provider-model");
    await expect(reasoning.getByRole("button", { name: "High", exact: true })).toHaveCount(0);
    await choose(model, "second-native-model");
    await reasoning.getByRole("button", { name: "Low", exact: true }).click();
    await f.page.getByRole("group", { name: "Speed", exact: true }).getByRole("button", { name: "Fast", exact: true }).click();
    await f.page.getByRole("group", { name: "Priority", exact: true }).getByRole("button", { name: "Urgent", exact: true }).click();
    const dependencies = f.page.getByRole("combobox", { name: "Dependencies", exact: true });
    await dependencies.fill("Video editing");
    await f.page.getByRole("option", { name: /Require .video-editing./ }).click();
    await expect(f.page.getByText("isolated-agent-host does not provide video-editing.")).toBeVisible();
    await expect(host).toHaveAttribute("aria-invalid", "true");
    await f.page.keyboard.press("Escape");
    await f.page.getByLabel("Project", { exact: true }).click();
    await f.page.getByRole("option", { name: "Add project…" }).click();
    const browse = f.page.getByRole("dialog", { name: "Add project" });
    await browse.getByRole("listitem").getByText("project", { exact: true }).click();
    await expect(browse.getByLabel("Folder path")).toHaveValue(/[\\/]project$/);
    await browse.getByRole("button", { name: "Use this folder" }).click();
    await expect(f.page.getByLabel("Project", { exact: true })).toHaveAttribute("data-value", "directory");
    await f.page.getByLabel("Title", { exact: true }).fill("Native choices");
    await f.page.getByRole("button", { name: "Create task", exact: true }).click();
    await expect(f.page.getByRole("heading", { name: /Native choices/ })).toBeVisible();
    const id = new URLSearchParams(new URL(f.page.url()).hash.split("?")[1]).get("id");
    const saved = (await f.task(id)).fields;
    assert.deepEqual(saved.executionRequirement, { kind: "host", hostId: "isolated-agent-host" });
    assert.deepEqual(saved.requiredCapabilities, ["video-editing"]);
    assert.deepEqual(saved.nativeOptions, { model: "second-native-model", reasoningEffort: "low", serviceTier: "fast" });
    assert.equal(saved.priority, 4);
    assert.equal(saved.workspaceRequirement.kind, "directory_path");
    assert.match(saved.workspaceRequirement.path, /[\\/]project$/);
    await f.page.getByRole("button", { name: "Edit task", exact: true }).click();
    await f.page.getByLabel("Project", { exact: true }).click();
    await f.page.getByRole("option", { name: "No project" }).click();
    await expect(f.page.getByLabel("Reason", { exact: true })).toHaveCount(0);
    await f.page.getByRole("button", { name: "Save changes", exact: true }).click();
    await expect.poll(async () => (await f.task(id)).fields.workspaceRequirement.kind).toBe("task_workspace");
    await f.page.setViewportSize({ width: 390, height: 844 });
    await f.page.goto(f.url + "#/new?node=browser-task-board");
    await expect(f.page.getByLabel("Host", { exact: true })).toHaveAttribute("data-value", "isolated-agent-host", { timeout: 20000 });
    await choose(f.page.getByLabel("Model", { exact: true }), "other-provider-model");
    await f.page.getByLabel("Title", { exact: true }).fill("Other provider on mobile");
    await f.page.getByRole("button", { name: "Create task", exact: true }).click();
    await expect(f.page.getByRole("heading", { name: /Other provider on mobile/ })).toBeVisible();
    const mobileId = new URLSearchParams(new URL(f.page.url()).hash.split("?")[1]).get("id");
    assert.equal((await f.task(mobileId)).fields.nativeOptions.model, "other-provider-model");
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test('TaskBoard settings persist across service restarts and expose inherited choices on desktop and mobile', { timeout: 180000 }, async t => {
  const f = await taskBoardFixture(t, { native: true });
  await f.open('#/settings?node=browser-task-board');
  await expect(f.page.getByRole('heading', { name: /^(TaskBoard settings|TaskBoard-Einstellungen)$/ })).toBeVisible();
  const model = f.page.getByLabel(/^(Default model|Standardmodell)$/);
  await expect(model).toBeEnabled({ timeout: 20000 });
  await choose(f.page.getByLabel('Host', { exact: true }), 'isolated-agent-host');
  await choose(model, 'fixture-model');
  await choose(f.page.getByLabel('Reasoning', { exact: true }), 'high');
  await choose(f.page.getByLabel('Speed', { exact: true }), 'fast');
  await choose(f.page.getByLabel('Updates', { exact: true }), 'chat');
  await f.page.getByRole('checkbox', { name: /^(Use a dedicated worktree|Eigenen Worktree verwenden)$/ }).click();
  await f.page.getByRole('checkbox', { name: /^(Allow parallel tickets in the same project|Parallele Tickets im selben Projekt erlauben)$/ }).click();
  await f.page.reload();
  await expect(model).toHaveAttribute('data-value', 'fixture-model');
  await expect(f.page.getByLabel('Speed', { exact: true })).toHaveAttribute('data-value', 'fast');
  await f.page.getByRole('button', { name: /^(Save|Speichern)$/ }).click();
  await expect(f.page.getByRole('status').filter({ hasText: /Defaults apply to new executions|Defaults gelten für neue Ausführungen/ })).toBeVisible();
  await f.restartTaskBoard();
  await f.page.reload();
  await expect(f.page.getByLabel(/^(Default model|Standardmodell)$/)).toHaveAttribute('data-value', 'fixture-model');
  await expect(f.page.getByLabel('Host', { exact: true })).toHaveAttribute('data-value', 'isolated-agent-host');
  await expect(f.page.getByLabel('Reasoning', { exact: true })).toHaveAttribute('data-value', 'high');
  await expect(f.page.getByLabel('Speed', { exact: true })).toHaveAttribute('data-value', 'fast');
  await expect(f.page.getByLabel('Updates', { exact: true })).toHaveAttribute('data-value', 'chat');
  await expect(f.page.getByRole('checkbox', { name: /^(Use a dedicated worktree|Eigenen Worktree verwenden)$/ })).toBeChecked();
  await expect(f.page.getByRole('checkbox', { name: /^(Allow parallel tickets in the same project|Parallele Tickets im selben Projekt erlauben)$/ })).toBeChecked();
  await f.page.goto(f.url + '#/new?node=browser-task-board');
  await expect(f.page.getByLabel('Host', { exact: true })).toHaveAttribute('data-value', 'isolated-agent-host');
  await expect(f.page.getByLabel('Model', { exact: true })).toHaveAttribute('data-value', 'fixture-model');
  await expect(f.page.getByRole('group', { name: 'Updates', exact: true }).getByRole('button', { name: 'Ticket and chat' })).toHaveAttribute('aria-pressed', 'true');
  await f.page.getByLabel('Project', { exact: true }).click();
  await f.page.getByRole('option', { name: /Isolated project/ }).click();
  await expect(f.page.getByRole('checkbox', { name: 'Use a dedicated worktree' })).toBeChecked();
  await f.page.getByRole('checkbox', { name: 'Use a dedicated worktree' }).click();
  await expect(f.page.getByRole('checkbox', { name: 'Allow parallel tickets in this project' })).toBeChecked();
  const speed = f.page.getByRole('group', { name: 'Speed', exact: true });
  await expect(speed.getByRole('button', { name: 'Fast', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await speed.getByRole('button', { name: 'Standard', exact: true }).click();
  await f.page.getByLabel('Title', { exact: true }).fill('Explicit standard speed');
  await f.page.getByRole('button', { name: 'Create task', exact: true }).click();
  await expect(f.page.getByRole('heading', { name: /Explicit standard speed/ })).toBeVisible();
  const id = new URLSearchParams(new URL(f.page.url()).hash.split('?')[1]).get('id');
  assert.equal((await f.task(id)).fields.nativeOptions.serviceTier, 'standard');
  assert.equal((await f.task(id)).fields.userContact, 'chat');
  assert.equal((await f.task(id)).fields.allowParallel, true);
  assert.equal((await f.task(id)).fields.workspaceRequirement.useWorktree, false);
  await f.page.setViewportSize({ width: 390, height: 844 });
  await f.page.goto(f.url + '#/settings?node=browser-task-board');
  await expect(f.page.getByLabel(/^(Default model|Standardmodell)$/)).toBeVisible();
  assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await choose(f.page.getByLabel('Speed', { exact: true }), 'standard');
  await f.page.getByRole('button', { name: /^(Save|Speichern)$/ }).click();
  await expect(f.page.getByRole('status').filter({ hasText: /Defaults apply to new executions|Defaults gelten für neue Ausführungen/ })).toBeVisible();
  assert.deepEqual(f.pageErrors, []);
  assert.deepEqual(f.externalRequests, []);
});

test(
  "TaskBoardUI creates a Backlog Task and completes manual work with a verified attachment comment",
  { timeout: 180000 },
  async (t) => {
    const f = await taskBoardFixture(t);
    await f.open();
    await f.page
      .getByRole("heading", { name: "Task board", exact: true })
      .waitFor();
    await f.page.getByRole("link", { name: "New task", exact: true }).click();
    await f.page
      .getByLabel("Title", { exact: true })
      .fill("Browser task lifecycle");
    await f.page
      .getByLabel("Description", { exact: true })
      .fill("## Manual work\n\nKeep the useful result and comment attachment.");
    await f.page
      .getByRole("button", { name: "Add acceptance criteria", exact: true })
      .click();
    await f.page
      .getByLabel("Acceptance criteria", { exact: true })
      .fill("Complete the explicit workflow.\nRetain the attachment.");
    await f.page.getByRole("radio", { name: "Me", exact: true }).click();
    await expect(f.page.getByLabel("Due", { exact: true })).toBeVisible();
    await f.page.getByLabel("Category", { exact: true }).fill("Video");
    await f.page.keyboard.press("Tab");
    await f.page
      .getByRole("button", { name: "Create task", exact: true })
      .click();
    await f.page
      .getByRole("heading", { name: /TASK-[0-9]{4,} · Browser task lifecycle/ })
      .waitFor();
    await expect(
      f.page.getByRole("heading", { name: "Description", exact: true }),
    ).toBeVisible();
    await expect(
      f.page.getByRole("complementary", { name: "Task details", exact: true }),
    ).toBeVisible();
    await expect(
      f.page.getByRole("tab", { name: "Comments (0)", exact: true }),
    ).toBeVisible();
    await expect(
      f.page.getByRole("tab", { name: "History", exact: true }),
    ).toBeVisible();
    const id = new URLSearchParams(
      new URL(f.page.url()).hash.split("?")[1],
    ).get("id");
    assert.ok(id);
    assert.equal((await f.task(id)).workflowState, "backlog");
    await f.page
      .getByRole("button", { name: "Move to Todo", exact: true })
      .click();
    await expect
      .poll(async () => (await f.task(id)).workflowState)
      .toBe("todo");
    await f.page.getByRole("button", { name: "More actions" }).click();
    await f.page.getByRole("menuitem", { name: "Move to Backlog" }).click();
    await expect
      .poll(async () => (await f.task(id)).workflowState)
      .toBe("backlog");
    await f.page.getByRole("tab", { name: "History", exact: true }).click();
    await expect(f.page.getByText("created the task")).toBeVisible();
    await expect(f.page.getByRole("definition").filter({ hasText: /Todo.*Backlog/ })).toBeVisible();
    await f.page.getByRole("tab", { name: /^Comments/ }).click();
    await f.page
      .getByRole("button", { name: "Move to Todo", exact: true })
      .click();
    await expect
      .poll(async () => (await f.task(id)).workflowState)
      .toBe("todo");
    assert.equal((await f.task(id)).fields.category, "Video");
    let current = await f.client.request("objects.read", { objectId: id });
    await f.invoke({
      action: "transition",
      taskId: id,
      expectedRevision: current.object.currentRevision,
      workflowState: "in_progress",
      detail: null,
    });
    await f.page.reload();
    await f.page.getByRole('button', { name: /^(Move to Done|Als erledigt markieren)$/ }).click();
    await expect
      .poll(async () => (await f.task(id)).workflowState)
      .toBe("done");
    await expect(f.page.getByRole('checkbox', { name: /^(Move to Todo|Nach Todo verschieben)$/ })).not.toBeChecked();
    await expect(f.page.getByText('Latest result', { exact: true })).toHaveCount(0);
    const comment = f.page.getByRole("textbox", { name: "Comment", exact: true });
    await comment.fill("Saved user evidence. ");
    await f.page.locator("input[type=file]").setInputFiles({
      name: "evidence.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("Exact browser evidence."),
    });
    await expect(comment.getByRole("link", { name: "evidence.txt" })).toBeVisible();
    await f.page
      .getByRole("button", { name: "Add comment", exact: true })
      .click();
    await expect.poll(async () => (await f.task(id)).comments.length).toBe(1);
    const saved = await f.task(id);
    assert.equal(saved.workflowState, "done");
    assert.equal(saved.attachments.length, 1);
    assert.equal(saved.comments[0].attachments.length, 1);
    assert.ok(saved.comments[0].body.includes("attachment:" + saved.attachments[0].attachmentId));
    await f.page
      .getByRole("button", { name: "Preview", exact: true })
      .first()
      .click();
    await expect(
      f.page.getByText("Exact browser evidence.", { exact: true }).first(),
    ).toBeVisible();
    await f.page.keyboard.press('Escape');
    await f.page.setViewportSize({ width: 390, height: 844 });
    await f.page.getByRole('checkbox', { name: /^(Move to Todo|Nach Todo verschieben)$/ }).check();
    await f.page.getByRole('textbox', { name: 'Comment', exact: true }).fill('Reopen this task.');
    await f.page.getByRole('button', { name: 'Add comment', exact: true }).click();
    await expect.poll(async () => (await f.task(id)).workflowState).toBe('todo');
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    await f.page.getByRole("button", { name: "More actions" }).click();
    await f.page.getByRole("menuitem", { name: "Delete permanently…" }).click();
    const deleteDialog = f.page.getByRole("dialog", { name: /Are you sure you want to permanently delete/ });
    await expect(deleteDialog).toContainText("All revisions will be removed");
    await deleteDialog.getByRole("button", { name: "Cancel" }).click();
    assert.equal((await f.task(id)).workflowState, "todo");
    await f.page.getByRole("button", { name: "More actions" }).click();
    await f.page.getByRole("menuitem", { name: "Delete permanently…" }).click();
    await deleteDialog.getByRole("button", { name: "Delete permanently" }).click();
    await expect(f.page.getByRole("dialog", { name: "Task details" })).toHaveCount(0);
    await assert.rejects(f.client.request("objects.stat", { objectId: id }), (error) => error.code === "not_found");
    await assert.rejects(f.client.request("objects.stat", { objectId: saved.attachments[0].object.objectId }), (error) => error.code === "not_found");
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "category normalization and the per-user swimlane preference survive refresh",
  { timeout: 180000 },
  async (t) => {
    const f = await taskBoardFixture(t);
    const first = await f.invoke({
      action: "create",
      fields: fields("First category task", "Video"),
    });
    const second = await f.invoke({
      action: "create",
      fields: fields("Second category task", "video"),
    });
    await f.invoke({ action: "create", fields: fields("Uncategorized task") });
    assert.equal((await f.task(second.task.objectId)).fields.category, "Video");
    await f.open("#/tasks?node=browser-task-board");
    await f.page
      .getByRole("button", { name: "Display options", exact: true })
      .click();
    await choose(f.page
      .getByLabel("Backlog swimlanes", { exact: true }), "category");
    await expect(f.page.getByLabel("Board swimlanes", { exact: true })).toHaveAttribute("data-value", "none");
    await expect(
      f.page.getByRole("heading", { name: "Video", exact: true }),
    ).toBeVisible();
    await expect(
      f.page.getByRole("heading", { name: "Uncategorized", exact: true }),
    ).toBeVisible();
    const backlog = f.page.getByRole("region", {
      name: "Backlog",
      exact: true,
    });
    for (const title of [
      "First category task",
      "Second category task",
      "Uncategorized task",
    ])
      await expect(
        backlog.getByRole("heading", { name: title, exact: true }),
      ).toHaveCount(1);
    await f.page.reload();
    await f.page
      .getByRole("button", { name: "Display options", exact: true })
      .click();
    await expect(
      f.page.getByLabel("Backlog swimlanes", { exact: true }),
    ).toHaveAttribute("data-value", "category");
    await f.page.keyboard.press("Escape");
    await f.invoke({
      action: "transition",
      taskId: first.task.objectId,
      expectedRevision: first.task.revision,
      workflowState: "todo",
      detail: null,
    });
    await f.page.reload();
    await expect(
      backlog.getByRole("heading", {
        name: "First category task",
        exact: true,
      }),
    ).toHaveCount(0);
    await expect(
      f.page
        .getByRole("region", { name: "Todo", exact: true })
        .getByRole("heading", { name: "First category task", exact: true }),
    ).toBeVisible();
    const blocked = await f.invoke({
      action: "create",
      fields: { ...fields("Blocked by the first task"), dependencies: [first.task.objectId] },
    });
    await f.page.getByRole("link", { name: /First category task/ }).first().click();
    await f.page.getByRole("dialog").getByRole("link", { name: "Open full page" }).click();
    await expect(f.page.getByRole("link", { name: "Back to board" })).toBeVisible();
    await f.page.getByRole("button", { name: "Edit task", exact: true }).click();
    const blockers = f.page.getByRole("combobox", { name: "Blockers", exact: true });
    await blockers.fill("task");
    await expect(f.page.getByRole("option", { name: /Second category task/ })).toBeVisible();
    await expect(f.page.getByRole("option", { name: /Blocked by the first task/ })).toHaveCount(0);
    await expect(f.page.getByRole("option", { name: /First category task/ })).toHaveCount(0);
    await f.page.getByRole("option", { name: /Second category task/ }).click();
    await f.page.keyboard.press("Escape");
    await f.page.getByRole("button", { name: "Save changes", exact: true }).click();
    await expect
      .poll(async () => (await f.task(first.task.objectId)).fields.dependencies)
      .toEqual([second.task.objectId]);
    assert.ok(blocked.task);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "the backlog is ranked by hand through its menu and by dragging",
  { timeout: 180000 },
  async (t) => {
    const f = await taskBoardFixture(t);
    for (const title of ["First ranked", "Second ranked", "Third ranked"])
      await f.invoke({ action: "create", fields: fields(title) });
    await f.open("#/tasks?node=browser-task-board");
    const backlog = f.page.getByRole("region", { name: "Backlog", exact: true });
    const order = () => backlog.locator("article h3").allTextContents().then((values) => values.map((value) => value.trim()));
    await expect.poll(order).toEqual(["First ranked", "Second ranked", "Third ranked"]);
    await backlog.getByRole("button", { name: "Actions for Third ranked" }).click();
    await f.page.getByRole("menuitem", { name: "Move to top", exact: true }).click();
    await expect.poll(order).toEqual(["Third ranked", "First ranked", "Second ranked"]);
    await backlog.getByRole("button", { name: "Actions for Third ranked" }).click();
    await f.page.getByRole("menuitem", { name: "Move down", exact: true }).click();
    await expect.poll(order).toEqual(["First ranked", "Third ranked", "Second ranked"]);
    await backlog.getByRole("button", { name: "Actions for First ranked" }).click();
    await f.page.getByRole("menuitem", { name: "Move to bottom", exact: true }).click();
    await expect.poll(order).toEqual(["Third ranked", "Second ranked", "First ranked"]);
    // Dropping on the lower half of a row places the dragged Task after it.
    const target = backlog.locator("article").filter({ hasText: "Second ranked" });
    const box = await target.boundingBox();
    await backlog.locator("article").filter({ hasText: "Third ranked" }).dragTo(target, { targetPosition: { x: 20, y: box.height - 4 } });
    await expect.poll(order).toEqual(["Second ranked", "Third ranked", "First ranked"]);
    await f.page.reload();
    await expect.poll(order).toEqual(["Second ranked", "Third ranked", "First ranked"]);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);
