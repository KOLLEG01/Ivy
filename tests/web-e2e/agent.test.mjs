import test from "node:test";
import assert from "node:assert/strict";
import { expect } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { agentFixture } from "./fixtures/agent-fixture.mjs";
import { choose, options } from "./fixtures/select.mjs";

test(
  "AgentUI opens task search with Ctrl+K and restores a collapsed sidebar",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/task?node=browser-agent&id=saved-task");
    const sidebar = f.page.locator('[data-slot="sidebar"][data-state]');
    await f.page.getByRole("button", { name: "Toggle navigation" }).click();
    await expect(sidebar).toHaveAttribute("data-state", "collapsed");
    await f.page.keyboard.press("Control+k");
    await f.page.getByRole("textbox", { name: "Find a task" }).fill("saved");
    await expect(
      f.page
        .getByRole("navigation", { name: "Search results" })
        .getByRole("link", { name: /saved-task/ }),
    ).toBeVisible();
    await f.page.keyboard.press("Escape");
    await f.page.reload();
    await expect(sidebar).toHaveAttribute("data-state", "collapsed");
    await f.page.getByRole("button", { name: "Toggle navigation" }).click();
    await expect(sidebar).toHaveAttribute("data-state", "expanded");
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "AgentUI submits with Enter while Shift+Enter keeps a multiline draft",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/host?node=browser-agent");
    await choose(f.page
      .getByLabel("Project", { exact: true }), f.nativeProjects[0].path);
    const firstMessage = f.page.getByRole("textbox", {
      name: "First message",
    });
    await firstMessage.fill("First line");
    await firstMessage.press("Shift+Enter");
    await firstMessage.type("Second line");
    await expect(firstMessage).toHaveValue("First line\nSecond line");
    assert.equal(
      f.current().sent.filter((frame) => frame.method === "thread/start")
        .length,
      0,
    );
    await firstMessage.press("Enter");
    await expect(f.page).toHaveURL(/#\/task\?node=browser-agent&id=/);
    const firstTurn = f
      .current()
      .sent.find((frame) => frame.method === "turn/start");
    assert.deepEqual(firstTurn.params.input, [
      { type: "text", text: "First line\nSecond line" },
    ]);
    f.complete(firstTurn.params.threadId);
    const message = f.page.getByRole("textbox", { name: "Message" });
    await message.fill("Follow up");
    await expect(
      f.page.getByRole("button", { name: "Send message" }),
    ).toBeEnabled({ timeout: 15000 });
    await message.press("Enter");
    await expect
      .poll(
        () =>
          f.current().sent.filter((frame) => frame.method === "turn/start")
            .length,
      )
      .toBe(2);
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "AgentUI exposes Desktop-native rename and archive actions on the same task",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t, undefined, true);
    await f.open("#/task?node=browser-agent&id=saved-task");
    await f.page
      .getByRole("button", { name: "Task actions", exact: true })
      .click();
    await f.page
      .getByLabel("Task name", { exact: true })
      .fill("Renamed native task");
    await f.page.getByRole("button", { name: "Rename", exact: true }).click();
    await expect
      .poll(() => f.threads.get("saved-task").name)
      .toBe("Renamed native task");
    if (
      !(await f.page
        .getByRole("menuitem", { name: "Archive", exact: true })
        .isVisible())
    )
      await f.page
        .getByRole("button", { name: "Task actions", exact: true })
        .click();
    await f.page
      .getByRole("menuitem", { name: "Archive", exact: true })
      .click();
    await expect.poll(() => f.threads.get("saved-task").archived).toBe(true);
    await f.page
      .getByRole("button", { name: "Task actions", exact: true })
      .click();
    await expect(
      f.page.getByRole("menuitem", { name: "Restore", exact: true }),
    ).toBeVisible();
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "AgentUI deletes a task only after confirmation and returns to its host",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t, undefined, true);
    await f.open("#/task?node=browser-agent&id=saved-task");
    const actions = f.page.getByRole("button", { name: "Task actions", exact: true });
    const remove = f.page.getByRole("menuitem", { name: /^(Delete|Löschen)…$/ });
    const dialog = f.page.getByRole("dialog");
    await actions.click();
    await remove.click();
    await dialog.getByRole("button", { name: /^(Cancel|Abbrechen)$/ }).click();
    await expect(dialog).toHaveCount(0);
    assert.equal(f.threads.has("saved-task"), true);
    await actions.click();
    await remove.click();
    await dialog.getByRole("button", { name: /^(Delete|Löschen)$/ }).click();
    await expect.poll(() => f.threads.has("saved-task")).toBe(false);
    await expect(f.page).toHaveURL(/#\/host\?node=browser-agent$/);
    await expect(
      f.page
        .getByRole("navigation", { name: "Recent tasks" })
        .getByRole("link", { name: "saved-task", exact: true }),
    ).toHaveCount(0);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "AgentUI sets a native goal and safety profile, then attaches and sends in one action",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/task?node=browser-agent&id=saved-task");

    await f.page.getByRole("button", { name: "Add to message", exact: true }).click();
    await f.page.getByRole("menuitem", { name: "Set goal", exact: true }).click();
    await f.page
      .getByRole("textbox", { name: "Goal", exact: true })
      .fill("Ship the refined Agent UI.");
    await f.page
      .getByRole("button", { name: "Save goal", exact: true })
      .click();
    await expect
      .poll(
        () =>
          f.current().sent.find((frame) => frame.method === "thread/goal/set")
            ?.params.objective,
      )
      .toBe("Ship the refined Agent UI.");

    await choose(f.page
      .getByLabel("Safety", { exact: true }), ":danger-full-access");
    await f.page
      .getByLabel("Message", { exact: true })
      .fill("Continue from the saved task.");
    await f.page
      .getByRole("button", { name: "Send message", exact: true })
      .click();

    await expect
      .poll(
        () =>
          f.current().sent.filter((frame) => frame.method === "thread/resume")
            .length,
      )
      .toBe(1);
    await expect
      .poll(
        () =>
          f.current().sent.filter((frame) => frame.method === "turn/start")
            .length,
      )
      .toBe(1);
    const call = f
      .current()
      .sent.find((frame) => frame.method === "turn/start");
    assert.equal(call.params.permissions, ":danger-full-access");
    assert.deepEqual(call.params.input, [
      { type: "text", text: "Continue from the saved task." },
    ]);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "AgentUI keeps native reasoning and command output out of the conversation while collapsing tool steps into an expandable summary",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t);
    f.threads.get("saved-task").turns.push({
      id: "desktop-detail-turn",
      status: "completed",
      items: [
        {
          id: "desktop-user",
          type: "userMessage",
          content: [{ type: "text", text: "Show the useful result." }],
        },
        {
          id: "desktop-reasoning",
          type: "reasoning",
          summary: ["PRIVATE_REASONING_MARKER"],
          content: ["PRIVATE_REASONING_CONTENT"],
        },
        {
          id: "desktop-command",
          type: "commandExecution",
          command: "Get-Content -Raw -LiteralPath README.md",
          commandActions: [
            {
              type: "read",
              command: "Get-Content -Raw -LiteralPath README.md",
              name: "README.md",
              path: "README.md",
            },
          ],
          cwd: f.nativeProjects[0].path,
          status: "completed",
          aggregatedOutput: "RAW_COMMAND_OUTPUT_MARKER",
        },
        {
          id: "desktop-answer",
          type: "agentMessage",
          text: "The useful result is ready.",
        },
      ],
    });
    await f.open("#/task?node=browser-agent&id=saved-task");
    const output = f.page.getByRole("region", {
      name: "Saved native output",
      exact: true,
    });
    const command = output.getByText(
      "Get-Content -Raw -LiteralPath README.md",
      { exact: true },
    );
    await output
      .getByRole("button", { name: "Ran a command", exact: true })
      .click({ timeout: 35000 });
    await expect(command).toBeVisible();
    await expect(
      output.getByText("The useful result is ready.", { exact: true }),
    ).toBeVisible();
    await expect(
      output.getByText("RAW_COMMAND_OUTPUT_MARKER", { exact: true }),
    ).toHaveCount(0);
    await expect(
      output.getByText("PRIVATE_REASONING_MARKER", { exact: true }),
    ).toHaveCount(0);
    assert.deepEqual(f.pageErrors, []);
  },
);

test("AgentUI uses push without continuously polling native notifications", { timeout: 90000 }, async t => {
  const f = await agentFixture(t), thread = f.threads.get("saved-task");
  thread.turns.push({ id: "push-live-turn", status: "inProgress", items: [] });
  thread.status = { type: "active", activeFlags: [] };
  let reads = 0, taskReads = 0, inputReads = 0, inputHints = 0, lastRead = Date.now();
  const pending = new Set(), sockets = new Set();
  f.page.on("websocket", socket => {
    if (!f.page.url().includes("/ui/agent-ui/")) return;
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    socket.on("framereceived", ({ payload }) => {
      const frame = JSON.parse(String(payload));
      if (frame.params?.namespace === "agent" && frame.params.name === "inputs" && frame.params.payload.threadId === "saved-task") inputHints++;
    });
  });
  f.page.on("request", request => {
    if (request.url().endsWith("/api/v1/rpc")) {
      const body = request.postDataJSON();
      const journal = body.params?.qualifiedName === "agent.notifications";
      if (body.params?.qualifiedName === "agent.inputs") inputReads++;
      const control = body.params?.qualifiedName === "codex.thread/read" && body.params.arguments.threadId === "saved-task" && body.params.arguments.includeTurns === false;
      if (journal || control) {
        if (journal) reads++;
        if (control) taskReads++;
        lastRead = Date.now();
        pending.add(request);
      }
    }
  });
  const finished = request => { if (pending.delete(request)) lastRead = Date.now(); };
  f.page.on("requestfinished", finished);
  f.page.on("requestfailed", finished);
  await f.open("#/task?node=browser-agent&id=saved-task");
  await expect.poll(() => reads, { timeout: 15000 }).toBeGreaterThan(0);
  await expect.poll(() => !pending.size && Date.now() - lastRead > 1500, { timeout: 15000 }).toBe(true);
  const before = reads;
  f.current().emit({ method: "item/agentMessage/delta", params: {
    threadId: "saved-task", turnId: "push-live-turn", itemId: "push-live-answer", delta: "Visible through push.",
  } });
  await expect(f.page.getByRole("article", { name: "Live answer" })).toContainText("Visible through push.", { timeout: 10000 });
  await f.page.waitForTimeout(2200);
  assert.equal(reads, before, "healthy push does not run the one-second recovery poll");
  const taskBefore = taskReads;
  f.current().emit({ method: "turn/completed", params: { threadId: "foreign-task", turn: { id: "foreign-turn", status: "completed", items: [] } } });
  await f.page.waitForTimeout(1800);
  assert.equal(taskReads, taskBefore, "another task does not reload the open task's state");
  f.complete("saved-task", "One task refresh owner.");
  await expect.poll(() => taskReads).toBe(taskBefore + 1);
  await f.page.waitForTimeout(1800);
  assert.equal(taskReads, taskBefore + 1, "one lifecycle event does not also trigger a generic loader refresh");
  const inputsBefore = inputReads;
  f.current().emit({ id: "push-approval", method: "item/commandExecution/requestApproval", params: {
    threadId: "saved-task", turnId: "push-live-turn", itemId: "approval", command: "echo fixture", cwd: f.nativeProjects[0].path,
    startedAtMs: 1788690000000, availableDecisions: ["accept", "decline"],
  } });
  await expect.poll(() => inputReads, { timeout: 7000 }).toBeGreaterThan(inputsBefore);
  assert.equal(inputHints, 1, "Hive accepts and forwards the task-scoped input hint");
  await expect(f.page.getByLabel("Decision", { exact: true })).toBeVisible({ timeout: 20000 });
  assert.equal(sockets.size, 1, "task state and pending inputs share the app's WebSocket");
  assert.deepEqual(f.pageErrors, []);
});

test(
  "AgentUI updates a running answer without reload when the transient WebSocket is unavailable",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t),
      thread = f.threads.get("saved-task");
    thread.turns.push({
      id: "poll-live-turn",
      status: "inProgress",
      items: [],
    });
    thread.status = { type: "active", activeFlags: [] };
    await f.page.routeWebSocket("**/ws", (socket) => socket.close());
    await f.open("#/task?node=browser-agent&id=saved-task");
    f.current().emit({
      method: "item/agentMessage/delta",
      params: {
        threadId: "saved-task",
        turnId: "poll-live-turn",
        itemId: "poll-live-answer",
        delta: "Visible without a reload.",
      },
    });
    await expect(
      f.page.getByRole("article", { name: "Live answer" }),
    ).toContainText("Visible without a reload.", { timeout: 10000 });
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "AgentUI reads and downloads a complete saved turn when native item paging is unsupported, without mixing changed groups",
  { timeout: 150000 },
  async (t) => {
    const f = await agentFixture(t, undefined, true);
    const pendingOutput = new Set();
    let lastOutputRead = Date.now();
    f.page.on("request", (request) => {
      if (!request.url().endsWith("/api/v1/rpc")) return;
      const body = request.postDataJSON();
      if (["agent.read", "codex.thread/items/list"].includes(body.params?.qualifiedName)) {
        pendingOutput.add(request);
        lastOutputRead = Date.now();
      }
    });
    const finishedOutput = (request) => {
      if (pendingOutput.delete(request)) lastOutputRead = Date.now();
    };
    f.page.on("requestfinished", finishedOutput);
    f.page.on("requestfailed", finishedOutput);
    f.unsupportedItems();
    const large =
      "Complete output begins.\n" +
      "x".repeat(2100000) +
      "\nExact complete output ends.";
    const turn = {
      id: "complete-saved-turn",
      status: "completed",
      items: Array.from({ length: 51 }, (_, index) => ({
        id: "complete-item-" + index,
        type: "agentMessage",
        text: index === 50 ? large : "Saved entry " + index,
      })),
    };
    f.threads.get("saved-task").turns.push(turn);
    await f.open(
      "#/task?node=browser-agent&id=saved-task&turn=complete-saved-turn",
    );
    const output = f.page.getByRole("region", {
      name: "Saved native output",
      exact: true,
    });
    await expect(output.locator("article")).toHaveCount(50, { timeout: 35000 });
    await expect(
      f.page.getByText("Live activity", { exact: true }),
    ).toHaveCount(0);
    await expect(
      f.page.getByText("Native item details", { exact: true }),
    ).toHaveCount(0);
    await expect(
      f.page.getByRole("group", {
        name: "Saved output pages",
        exact: true,
      }),
    ).toHaveCount(0);
    await f.page
      .getByRole("button", { name: "Task actions", exact: true })
      .click();
    const downloaded = f.page.waitForEvent("download");
    await f.page
      .getByRole("menuitem", { name: "Download complete turn", exact: true })
      .click();
    const download = await downloaded,
      observed = JSON.parse(await readFile(await download.path(), "utf8"));
    assert.equal(
      download.suggestedFilename(),
      "native-turn-complete-saved-turn.json",
    );
    assert.equal(observed.serviceNodeId, "browser-agent");
    assert.equal(observed.epoch, f.current().epoch);
    assert.deepEqual(observed.params, {
      threadId: "saved-task",
      cursor: null,
      limit: 1,
      sortDirection: "desc",
      itemsView: "full",
    });
    assert.equal(observed.reply.result.data[0].items.length, 51);
    assert.equal(observed.reply.result.data[0].items[50].text, large);
    await f.page.locator(".agent-messages").evaluate((element) => {
      element.scrollTop = 0;
      element.dispatchEvent(new Event("scroll"));
    });
    await expect(output.locator("article")).toHaveCount(51);
    await expect(
      output.getByText(/Complete output begins/).first(),
    ).toBeVisible();
    await f.page.reload();
    await expect(output.locator("article")).toHaveCount(50, { timeout: 35000 });
    // The initial subscription also schedules a reread. Let it settle before changing the
    // fixture between pages, otherwise it can legitimately refresh the first-page hash.
    await expect.poll(() => !pendingOutput.size && Date.now() - lastOutputRead > 1000, { timeout: 15000 }).toBe(true);
    turn.items[0].text = "Output changed between groups.";
    await f.page.locator(".agent-messages").evaluate((element) => {
      element.scrollTop = 0;
      element.dispatchEvent(new Event("scroll"));
    });
    await expect(
      f.page.getByText(
        "Saved output changed or moved between item groups. Reload saved output.",
        { exact: true },
      ),
    ).toBeVisible();
    await f.page
      .getByRole("button", { name: "Task actions", exact: true })
      .click();
    await expect(
      f.page.getByRole("menuitem", {
        name: "Download complete turn",
        exact: true,
      }),
    ).toBeDisabled();
    await f.page.keyboard.press("Escape");
    await f.page.getByRole("button", { name: "Retry", exact: true }).click();
    await expect(
      output.getByText("Output changed between groups.", { exact: true }),
    ).toBeVisible();
    assert.equal(
      f
        .current()
        .sent.some((frame) =>
          [
            "thread/start",
            "thread/resume",
            "turn/start",
            "turn/interrupt",
          ].includes(frame.method),
        ),
      false,
    );
    assert.deepEqual(f.externalRequests, []);
    assert.deepEqual(f.pageErrors, []);
  },
);

for (const version of ["0.154.0"])
  test(
    "AgentUI " +
      version +
      " creates one task, sends before native history exists and retrieves complete saved output",
    { timeout: 90000 },
    async (t) => {
      const f = await agentFixture(t, undefined, false, version);
      f.unsupportedItems();
      f.unmaterializedHistory();
      await f.open();
      await f.page
        .getByRole("heading", { name: "What should we work on?", exact: true })
        .waitFor();
      await choose(f.page
        .getByLabel("Project", { exact: true }), f.nativeProjects[0].path);
      await choose(f.page
        .getByLabel("Model", { exact: true }), "fixture-model");
      assert.ok(
        (await options(f.page.getByLabel("Model", { exact: true }))).includes(
          "Second native page model",
        ),
      );
      await f.page
        .getByRole("button", { name: "Create task", exact: true })
        .click();
      await expect(f.page).toHaveURL(/#\/task\?node=browser-agent&id=/);
      await expect(
        f.page.getByText("Direct input confirmed on this native connection", {
          exact: true,
        }),
      ).toHaveCount(0);
      await expect(
        f.page.getByText("No saved output yet", { exact: true }),
      ).toBeVisible();
      await f.page
        .getByLabel("Message", { exact: true })
        .fill("An explicitly selected native prompt.");
      await choose(f.page
        .getByLabel("Model", { exact: true }), "fixture-model");
      await choose(f.page
        .getByLabel("Reasoning effort", { exact: true }), "low");
      await f.page
        .getByRole("button", { name: "Send message", exact: true })
        .click();
      await expect
        .poll(
          () =>
            f.current().sent.filter((x) => x.method === "turn/start").length,
        )
        .toBe(1);
      const call = f.current().sent.find((x) => x.method === "turn/start");
      assert.deepEqual(call.params.input, [
        { type: "text", text: "An explicitly selected native prompt." },
      ]);
      assert.equal(call.params.model, "fixture-model");
      assert.equal(call.params.effort, "low");
      assert.equal("permissions" in call.params, false);
      assert.equal("sandboxPolicy" in call.params, false);
      f.complete(
        call.params.threadId,
        "# Saved native result\n\nExact task output.\n\n<script>alert(1)</script>\n![inert](https://invalid.test/pixel)",
      );
      await expect(
        f.page.getByRole("heading", {
          name: "Saved native result",
          exact: true,
        }),
      ).toBeVisible({ timeout: 35000 });
      await f.page.reload();
      await expect(
        f.page.getByRole("heading", {
          name: "Saved native result",
          exact: true,
        }),
      ).toBeVisible({ timeout: 35000 });
      await expect(
        f.page.getByText("Live activity", { exact: true }),
      ).toHaveCount(0);
      assert.deepEqual(f.externalRequests, []);
      assert.deepEqual(f.pageErrors, []);
    },
  );

test(
  "AgentUI marks working tasks and unread results in the navigation live",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/host?node=browser-agent");
    const navigation = f.page.locator("#ivy-navigation");
    const task = navigation.getByRole("link", { name: "saved-task" });
    await expect(task).toBeVisible({ timeout: 35000 });
    const turn = { id: "indicator-turn", items: [], error: null };
    f.current().emit({
      method: "turn/started",
      params: { threadId: "saved-task", turn: { ...turn, status: "inProgress" } },
    });
    await expect(
      navigation.getByRole("img", { name: "Working", exact: true }),
    ).toBeVisible({ timeout: 10000 });
    f.current().emit({
      method: "turn/completed",
      params: { threadId: "saved-task", turn: { ...turn, status: "completed" } },
    });
    const unread = navigation.getByRole("img", {
      name: "Unread results",
      exact: true,
    });
    await expect(unread).toBeVisible({ timeout: 10000 });
    await task.click();
    await expect(unread).toHaveCount(0);
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "AgentUI keeps every host in one task tree and filters it without navigating",
  { timeout: 120000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.addAgent(undefined, "second-host");
    await f.open("#/host?node=browser-agent");
    const navigation = f.page.getByRole("navigation", { name: "Recent tasks" });
    const filter = navigation.getByRole("button", { name: "Filter tasks", exact: true });
    const tasks = navigation.getByRole("link", { name: "saved-task", exact: true });
    await expect(filter).toContainText("All hosts");
    await expect(
      navigation.getByRole("link", { name: "New task on second-host", exact: true }),
    ).toBeVisible({ timeout: 35000 });
    await expect(tasks).toHaveCount(2);
    await tasks.last().click();
    await expect(f.page).toHaveURL(/#\/task\?node=browser-second-agent&id=saved-task/);
    await expect(filter).toContainText("All hosts");
    await expect(tasks).toHaveCount(2);
    await expect(tasks.last()).toHaveAttribute("aria-current", "page");
    await expect(tasks.first()).not.toHaveAttribute("aria-current", "page");

    await filter.click();
    await f.page.getByRole("menuitemradio", { name: "isolated-agent-host", exact: true }).click();
    await expect(filter).toContainText("isolated-agent-host");
    await expect(f.page).toHaveURL(/#\/task\?node=browser-second-agent&id=saved-task/);
    await expect(tasks).toHaveCount(1);
    await expect(
      navigation.getByRole("link", { name: "New task on second-host", exact: true }),
    ).toHaveCount(0);
    await f.page.reload();
    await expect(filter).toContainText("isolated-agent-host");
    await filter.click();
    await f.page.getByRole("menuitemradio", { name: "All hosts", exact: true }).click();
    await expect(tasks).toHaveCount(2);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "AgentUI shows host usage, collapses hosts and lists tasks by latest update with row actions",
  { timeout: 120000 },
  async (t) => {
    const f = await agentFixture(t, undefined, true);
    await f.open("#/host?node=browser-agent");
    const navigation = f.page.getByRole("navigation", { name: "Recent tasks" });
    const task = navigation.getByRole("link", { name: "saved-task", exact: true });
    const host = navigation.getByRole("button", { name: "isolated-agent-host", exact: true });
    await expect(task).toBeVisible({ timeout: 35000 });

    await navigation.getByRole("button", { name: "Usage on isolated-agent-host", exact: true }).click();
    const usage = f.page.getByRole("dialog");
    await expect(usage.getByText("5-hour limit", { exact: true })).toBeVisible();
    await expect(usage.getByText("12% used", { exact: true })).toBeVisible();
    await f.page.keyboard.press("Escape");

    await host.click();
    await expect(task).toHaveCount(0);
    await f.page.reload();
    await expect(host).toHaveAttribute("aria-expanded", "false");
    await host.click();
    await expect(task).toBeVisible();

    await navigation.getByRole("button", { name: "Filter tasks", exact: true }).click();
    await f.page.getByRole("menuitemradio", { name: "By latest update", exact: true }).click();
    await expect(host).toHaveCount(0);
    await expect(task).toBeVisible();
    await navigation.getByRole("button", { name: "Actions for saved-task", exact: true }).click();
    await f.page.getByLabel("Task name", { exact: true }).fill("Renamed from sidebar");
    await f.page.getByRole("button", { name: "Rename", exact: true }).click();
    await expect.poll(() => f.threads.get("saved-task").name).toBe("Renamed from sidebar");
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "AgentUI keeps host configuration on a dedicated settings page",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/hosts");
    await expect(
      f.page.getByRole("link", { name: "Settings", exact: true }),
    ).toHaveAttribute("href", "#/settings");
    await f.page.goto(f.url + "#/host?node=browser-agent");
    await expect(
      f.page.getByRole("heading", {
        name: "What should we work on?",
        exact: true,
      }),
    ).toBeVisible();
    await expect(
      f.page.getByRole("heading", { name: "Connection", exact: true }),
    ).toHaveCount(0);
    await expect(
      f.page.getByRole("link", { name: "Hive Console", exact: true }),
    ).toHaveCount(0);
    await expect
      .poll(() =>
        f.page
          .locator(".agent-sidebar")
          .evaluate((element) => element.scrollWidth <= element.clientWidth),
      )
      .toBe(true);

    await f.page.setViewportSize({ width: 390, height: 844 });
    await f.page.locator('.ivy-toolbar').getByRole("link", { name: "Settings", exact: true }).click();
    await expect(f.page).toHaveURL(/#\/host-settings\?node=browser-agent/);
    await expect(
      f.page.getByRole("heading", {
        name: "isolated-agent-host settings",
        exact: true,
      }),
    ).toBeVisible();
    for (const name of [
      "Connection",
      "Scheduling capabilities",
      "Agent instructions",
      "MCP and skills",
    ])
      await expect(
        f.page.getByRole("heading", { name, exact: true }),
      ).toBeVisible();
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "AgentUI keeps reasoning and safety in separate desktop and mobile pills and applies the first message settings",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t, undefined, false, "0.154.0");
    f.unmaterializedHistory();
    await f.open();
    await expect(f.page).toHaveURL(/#\/host\?node=browser-agent/);
    await choose(
      f.page.getByLabel("Project", { exact: true }),
      f.nativeProjects[0].path,
    );
    await choose(f.page.getByLabel("Model", { exact: true }), "fixture-model");
    await choose(f.page.getByLabel("Reasoning effort", { exact: true }), "max");
    await choose(
      f.page.getByLabel("Safety", { exact: true }),
      ":danger-full-access",
    );
    const checkPills = async () => {
      for (const viewport of [
        { width: 1440, height: 1000 },
        { width: 360, height: 800 },
      ]) {
        await f.page.setViewportSize(viewport);
        await expect(
          f.page.getByRole("combobox", {
            name: "Reasoning effort",
            exact: true,
          }),
        ).toHaveText("Max");
        await expect(
          f.page.getByRole("combobox", { name: "Safety", exact: true }),
        ).toHaveText("Full access");
        assert.ok(
          await f.page.evaluate(
            () => document.documentElement.scrollWidth <= innerWidth + 1,
          ),
        );
      }
    };
    await checkPills();
    await f.page
      .getByLabel("First message", { exact: true })
      .fill("First turn with explicit reasoning.");
    await f.page
      .getByRole("button", { name: "Create task", exact: true })
      .click();
    await expect(f.page).toHaveURL(/#\/task\?node=browser-agent&id=/);
    await expect
      .poll(
        () =>
          f.current().sent.filter((frame) => frame.method === "turn/start")
            .length,
      )
      .toBe(1);
    const call = f
      .current()
      .sent.find((frame) => frame.method === "turn/start");
    assert.equal(call.params.effort, "max");
    const start = f
      .current()
      .sent.find((frame) => frame.method === "thread/start");
    assert.equal(start.params.permissions, ":danger-full-access");
    await choose(f.page.getByLabel("Model", { exact: true }), "fixture-model");
    await choose(f.page.getByLabel("Reasoning effort", { exact: true }), "max");
    await choose(
      f.page.getByLabel("Safety", { exact: true }),
      ":danger-full-access",
    );
    await checkPills();
    assert.deepEqual(call.params.input, [
      { type: "text", text: "First turn with explicit reasoning." },
    ]);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "AgentUI resume requires original-outcome reconciliation and a new receipt after native restart",
  { timeout: 120000 },
  async (t) => {
    const f = await agentFixture(t, undefined, false, "0.154.0");
    await f.open("#/task?node=browser-agent&id=saved-task");
    let lost = false, recoverAllowed = false;
    const loseResume = async (route) => {
      const request = route.request().postDataJSON();
      if (!recoverAllowed && request.method === "tools.call" && request.params.qualifiedName === "agent.operation") return route.abort();
      if (
        !lost &&
        request.method === "tools.call" &&
        request.params.qualifiedName === "codex.thread/resume"
      ) {
        lost = true;
        await route.fetch();
        return route.abort();
      }
      await route.continue();
    };
    await f.page.route("**/api/v1/rpc", loseResume);
    await f.page
      .getByLabel("Message", { exact: true })
      .fill("Keep this message draft.");
    await f.page
      .getByRole("button", { name: "Attach historical task", exact: true })
      .click();
    await expect.poll(() => f.page.evaluate(() => JSON.parse(Object.values(sessionStorage).find(raw => raw.includes('"label":"Attach historical task"')) ?? 'null')?.phase)).toBe('unknown');
    await expect(f.page.getByRole("button", { name: "Send message", exact: true })).toHaveAttribute("aria-busy", "true");
    await f.page.reload();
    await expect(f.page.getByLabel("Message", { exact: true })).toHaveValue(
      "Keep this message draft.",
    );
    await expect(
      f.page.getByRole("button", { name: "Send message", exact: true }),
    ).toBeDisabled();
    recoverAllowed = true;
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(
      f.page.getByRole("button", { name: "Send message", exact: true }),
    ).toBeEnabled();
    await f.page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect
      .poll(
        () =>
          f.current().sent.filter((frame) => frame.method === "turn/start")
            .length,
      )
      .toBe(1);
    await expect(f.page.getByLabel("Message", { exact: true })).toHaveValue("");
    await expect(
      f.page.getByRole("button", { name: "Request interruption", exact: true }),
    ).toBeVisible();
    await f.page
      .getByLabel("Message", { exact: true })
      .fill("Keep this message draft.");
    f.complete("saved-task");
    await f.restart();
    await f.page.reload();
    await expect(f.page.getByLabel("Message", { exact: true })).toHaveValue(
      "Keep this message draft.",
    );
    await expect(
      f.page.getByRole("button", { name: "Send message", exact: true }),
    ).toBeDisabled();
    await f.page
      .getByRole("button", { name: "Attach historical task", exact: true })
      .click();
    await expect(
      f.page.getByRole("button", { name: "Send message", exact: true }),
    ).toBeEnabled({ timeout: 15000 });
    await expect(
      f.page.getByText("A saved native result.", { exact: true }),
    ).toBeVisible();
    await expect(
      f.page.getByText("Conversation history", { exact: true }),
    ).toHaveCount(0);
    const sent = f.owners.flatMap((owner) => owner.sent);
    assert.equal(
      sent.filter((frame) => frame.method === "thread/resume").length,
      2,
    );
    assert.equal(
      sent.filter((frame) => frame.method === "turn/start").length,
      1,
    );
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "AgentUI keeps one start owner through delayed preparation and permits retry after definite pre-dispatch failure",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t, undefined, false, "0.154.0");
    await f.open("#/task?node=browser-agent&id=saved-task");
    const attach = f.page.getByRole("button", {
      name: "Attach historical task",
      exact: true,
    });
    // Hold the action's caller check after initial reads have cached the runtime epoch.
    await expect(attach).toBeEnabled();
    let statusCalls = 0,
      release,
      entered;
    const held = new Promise((resolve) => {
        release = resolve;
      }),
      enteredPromise = new Promise((resolve) => {
        entered = resolve;
      });
    t.after(() => release());
    await f.page.route("**/api/v1/rpc", async (route) => {
      const request = route.request().postDataJSON();
      if (request.method === "system.status" && ++statusCalls === 1) {
        entered();
        await held;
        return route.abort();
      }
      await route.continue();
    });
    await attach.evaluate((element) => element.click());
    await enteredPromise;
    await attach.evaluate((element) => element.click());
    release();
    await expect(attach).toBeEnabled();
    assert.equal(
      f.current().sent.filter((frame) => frame.method === "thread/resume")
        .length,
      0,
    );
    await attach.click();
    await expect
      .poll(
        () =>
          f.current().sent.filter((frame) => frame.method === "thread/resume")
            .length,
      )
      .toBe(1);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "AgentUI preserves a message after response loss and reload, reconciles without resending, and distinguishes interruption receipt",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/task?node=browser-agent&id=saved-task");
    await expect(
      f.page.getByRole("button", { name: "Send message", exact: true }),
    ).toBeDisabled();
    await f.page
      .getByRole("button", { name: "Attach historical task", exact: true })
      .click();
    await expect(f.page.getByLabel("Message", { exact: true })).toBeEditable();
    await f.page
      .getByLabel("Message", { exact: true })
      .fill("Keep this draft through a lost response.");
    await choose(f.page
      .getByLabel("Working mode", { exact: true }), "plan");
    await expect(
      f.page.getByRole("button", { name: "Send message", exact: true }),
    ).toBeEnabled();
    await choose(f.page
      .getByLabel("Model", { exact: true }), "fixture-model");
    await choose(f.page
      .getByLabel("Reasoning effort", { exact: true }), "low");
    let dropped = false, recoverAllowed = false;
    await f.page.route("**/api/v1/rpc", async (route) => {
      const request = route.request().postDataJSON();
      if (!recoverAllowed && request.method === "tools.call" && request.params.qualifiedName === "agent.operation") return route.abort();
      if (
        !dropped &&
        request.method === "tools.call" &&
        request.params.qualifiedName === "codex.turn/start"
      ) {
        dropped = true;
        await route.fetch();
        await route.abort();
      } else await route.continue();
    });
    await f.page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect.poll(() => f.page.evaluate(() => JSON.parse(Object.values(sessionStorage).find(raw => raw.includes('"label":"Send message"')) ?? 'null')?.phase)).toBe('unknown');
    await expect(f.page.getByRole("button", { name: "Send message", exact: true })).toHaveAttribute("aria-busy", "true");
    await f.page.reload();
    await expect(f.page.getByLabel("Message", { exact: true })).toHaveValue(
      "Keep this draft through a lost response.",
    );
    await expect(
      f.page.getByLabel("Working mode", { exact: true }),
    ).toHaveAttribute("data-value", "plan");
    await expect(f.page.getByLabel("Model", { exact: true })).toHaveAttribute(
      "data-value",
      "fixture-model",
    );
    await expect(
      f.page.getByLabel("Reasoning effort", { exact: true }),
    ).toHaveAttribute("data-value", "low");
    await f.page.keyboard.press("Escape");
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    assert.equal(
      f.current().sent.filter((x) => x.method === "turn/start").length,
      1,
    );
    assert.deepEqual(
      f.current().sent.find((x) => x.method === "turn/start").params
        .collaborationMode,
      {
        mode: "plan",
        settings: {
          model: "fixture-model",
          reasoning_effort: "low",
          developer_instructions: null,
        },
      },
    );
    recoverAllowed = true;
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(f.page.getByLabel("Message", { exact: true })).toHaveValue("", { timeout: 35000 });
    assert.equal(
      f.current().sent.filter((x) => x.method === "turn/start").length,
      1,
    );
    await f.page
      .getByRole("button", { name: "Request interruption", exact: true })
      .click();
    await expect
      .poll(
        () =>
          f.current().sent.filter((x) => x.method === "turn/interrupt").length,
      )
      .toBe(1);
    await expect(
      f.page.getByRole("button", { name: "Request interruption", exact: true }),
    ).toBeDisabled();
    assert.equal(f.threads.get("saved-task").turns.at(-1).status, "inProgress");
    f.complete("saved-task", "Native interruption observed.", "interrupted");
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(
      f.page.getByText("Native interruption observed.", { exact: true }),
    ).toBeVisible();
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "AgentUI preserves a mode draft when its capability disappears and requires explicit clearing before ordinary input",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/task?node=browser-agent&id=saved-task");
    await f.page
      .getByRole("button", { name: "Attach historical task", exact: true })
      .click();
    await expect(f.page.getByLabel("Message", { exact: true })).toBeEditable();
    await f.page
      .getByLabel("Message", { exact: true })
      .fill("Keep the original selected mode and draft.");
    await choose(f.page
      .getByLabel("Model", { exact: true }), "fixture-model");
    await choose(f.page
      .getByLabel("Working mode", { exact: true }), "plan");
    await expect(
      f.page.getByRole("button", { name: "Send message", exact: true }),
    ).toBeEnabled();
    await f.registryChange((registry) => {
      registry.namespaces[0].tools = registry.namespaces[0].tools.filter(
        (tool) => tool.name !== "collaborationMode/list",
      );
    });
    await f.page.reload();
    await expect(
      f.page.getByLabel("Working mode", { exact: true }),
    ).toHaveAttribute("data-value", "plan");
    await expect(f.page.getByLabel("Message", { exact: true })).toHaveValue(
      "Keep the original selected mode and draft.",
    );
    assert.ok(
      (
        await options(f.page.getByLabel("Working mode", { exact: true }))
      ).includes("Saved mode · plan (unavailable)"),
    );
    await expect(
      f.page.getByRole("button", { name: "Send message", exact: true }),
    ).toBeDisabled();
    assert.equal(
      f.current().sent.filter((frame) => frame.method === "turn/start").length,
      0,
    );
    await choose(f.page.getByLabel("Working mode", { exact: true }), "");
    await f.page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect
      .poll(
        () =>
          f.current().sent.filter((frame) => frame.method === "turn/start")
            .length,
      )
      .toBe(1);
    assert.equal(
      f.current().sent.find((frame) => frame.method === "turn/start").params
        .collaborationMode,
      undefined,
    );
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "AgentUI answers an exact native question, refuses an expired epoch, preserves focus and fits a narrow viewport",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t, { width: 390, height: 844 });
    const question = (id) => ({
      id,
      method: "item/tool/requestUserInput",
      params: {
        threadId: "saved-task",
        turnId: "question-turn",
        itemId: "question-item",
        isBlocking: true,
        questions: [
          {
            id: "choice",
            header: "Choice",
            question: "Which fixture result?",
            isOther: true,
            isSecret: false,
            options: [
              { label: "First", description: "First fixture result" },
              { label: "Second", description: "Second fixture result" },
            ],
          },
        ],
      },
    });
    f.current().emit(question("pending-one"));
    await f.open("#/task?node=browser-agent&id=saved-task");
    const installBanner = f.page.getByRole("region", { name: "Install Ivy", exact: true });
    await expect(installBanner).toBeVisible();
    const conversationBottom = async () => {
      const box = await f.page.locator(".agent-task").boundingBox();
      return Math.round(box.y + box.height);
    };
    await expect.poll(conversationBottom).toBe(844);
    const answer = f.page.getByLabel("Which fixture result?", { exact: true });
    await answer.fill("Second with a preserved draft");
    await answer.focus();
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(answer).toBeFocused();
    await expect(answer).toHaveValue("Second with a preserved draft");
    const second = await f.context.newPage();
    second.on("pageerror", (error) => f.pageErrors.push(error.message));
    await second.goto(f.page.url());
    await second
      .getByLabel("Which fixture result?", { exact: true })
      .fill("Competing answer must remain local");
    const competingDraftKey = await second.evaluate(() => Object.keys(sessionStorage).find(key =>
      key.startsWith('ivy:agent-input:') && key.endsWith(':draft')));
    assert.ok(competingDraftKey);
    await f.page
      .getByRole("button", { name: "Send response", exact: true })
      .click();
    await expect
      .poll(
        () =>
          f.current().sent.filter((x) => x.id === "pending-one" && !x.method)
            .length,
      )
      .toBe(1);
    assert.deepEqual(
      f.current().sent.find((x) => x.id === "pending-one" && !x.method).result,
      { answers: { choice: { answers: ["Second with a preserved draft"] } } },
    );
    f.current().emit({
      method: "serverRequest/resolved",
      params: { requestId: "pending-one", threadId: "saved-task" },
    });
    await expect(answer).toHaveCount(0);
    await expect(second.getByLabel("Which fixture result?", { exact: true })).toHaveCount(0);
    for (const page of [f.page, second]) {
      assert.equal(await page.evaluate(() => Object.keys(sessionStorage).some(key =>
        key.startsWith('ivy:agent-input:') && key.endsWith(':draft'))), false);
    }
    assert.equal(
      f.current().sent.filter((x) => x.id === "pending-one" && !x.method)
        .length,
      1,
    );
    // Older tabs can contain a draft without a successful local answer receipt.
    // The owner's answered state must still remove it on reload.
    await second.evaluate(key => {
      sessionStorage.setItem(key,
        JSON.stringify({ answers: { choice: 'Old answered draft' }, decision: '' }));
    }, competingDraftKey);
    await second.reload();
    await expect(second.getByLabel('Message', { exact: true })).toBeVisible();
    await expect(second.getByLabel("Which fixture result?", { exact: true })).toHaveCount(0);
    await expect.poll(() => second.evaluate(key => sessionStorage.getItem(key), competingDraftKey)).toBe(null);
    await second.close();
    f.current().emit(question("native-resolved"));
    await answer.fill("Draft for a request resolved in the native host");
    f.current().emit({ method: "serverRequest/resolved", params: { requestId: "native-resolved", threadId: "saved-task" } });
    await expect(answer).toHaveCount(0);
    f.current().emit(question("pending-two"));
    const requestCard = f.page
      .locator("article")
      .filter({ hasText: 'request "pending-two"' });
    await expect(requestCard).toBeVisible({ timeout: 10000 });
    await requestCard
      .getByLabel("Which fixture result?", { exact: true })
      .fill("Retain this expired answer");
    // Restart after the user submits but before the exact-identity check reaches the owner.
    // This deterministically exercises the stale-answer race without freezing discovery or
    // depending on whether a background refresh disables the form during the restart.
    let heldIdentityRead;
    const holdIdentityRead = async (route) => {
      const request = route.request().postDataJSON();
      if (
        request.method === "tools.call" &&
        request.params.qualifiedName === "agent.inputs" &&
        request.params.arguments.identity?.requestId === "pending-two"
      ) {
        heldIdentityRead = route;
      } else await route.continue();
    };
    await f.page.route("**/api/v1/rpc", holdIdentityRead);
    await expect(
      requestCard.getByRole("button", { name: "Send response", exact: true }),
    ).toBeEnabled();
    await requestCard
      .getByRole("button", { name: "Send response", exact: true })
      .click();
    await expect.poll(() => !!heldIdentityRead).toBe(true);
    await f.restart();
    await heldIdentityRead.continue();
    await expect(
      f.page.getByText(
        "This native request has been answered or expired. The response draft is retained.",
        { exact: true },
      ),
    ).toBeVisible();
    assert.equal(
      f.current().sent.some((x) => x.id === "pending-two" && !x.method),
      false,
    );
    await f.page.unroute("**/api/v1/rpc", holdIdentityRead);
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(
      requestCard.getByRole("button", { name: "Send response", exact: true }),
    ).toBeDisabled();
    await expect(
      requestCard.getByLabel("Which fixture result?", { exact: true }),
    ).toHaveValue("Retain this expired answer");
    const layout = await f.page.evaluate(() => ({
      width: innerWidth,
      scroll: document.documentElement.scrollWidth,
    }));
    assert.ok(layout.scroll <= layout.width, JSON.stringify(layout));
    await installBanner.getByRole("button", { name: "Dismiss installation banner" }).click();
    await expect(installBanner).toHaveCount(0);
    await expect.poll(conversationBottom).toBe(844);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "AgentUI retains the old definition and draft when a provider changes required native fields",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/task?node=browser-agent&id=saved-task");
    await f.page
      .getByRole("button", { name: "Attach historical task", exact: true })
      .click();
    await expect(f.page.getByLabel("Message", { exact: true })).toBeEditable();
    await f.page
      .getByLabel("Message", { exact: true })
      .fill("Preserve this message through an incompatible update.");
    await expect(
      f.page.getByRole("button", { name: "Send message", exact: true }),
    ).toBeEnabled();
    await f.registryChange((registry) => {
      const tool = registry.namespaces[0].tools.find(
        (t) => t.name === "turn/start",
      );
      const root = tool.inputSchema.$ref
        ? tool.inputSchema.$defs[tool.inputSchema.$ref.split("/").at(-1)]
        : tool.inputSchema;
      root.properties.newRequiredField = { type: "string" };
      root.required = [...root.required, "newRequiredField"];
    });
    await f.page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect(
      f.page.getByText(/tool_definition_changed; not_executed/),
    ).toBeVisible();
    assert.equal(
      f.current().sent.filter((x) => x.method === "turn/start").length,
      0,
    );
    await f.page.reload();
    await expect(f.page.getByLabel("Message", { exact: true })).toHaveValue(
      "Preserve this message through an incompatible update.",
    );
    await expect(
      f.page.getByRole("button", { name: "Send message", exact: true }),
    ).toBeDisabled();
    assert.equal(
      f.current().sent.filter((x) => x.method === "turn/start").length,
      0,
    );
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "AgentUI requires owner-confirmed absence before retry and retains the exact original native request",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/task?node=browser-agent&id=saved-task");
    await f.page
      .getByRole("button", { name: "Attach historical task", exact: true })
      .click();
    await expect(f.page.getByLabel("Message", { exact: true })).toBeEditable();
    await f.page
      .getByLabel("Message", { exact: true })
      .fill("An original operation which never reached the owner.");
    await f.page.clock.install();
    const requests = [];
    let recoverAllowed = false;
    await f.page.route("**/api/v1/rpc", async (route) => {
      const frame = route.request().postDataJSON();
      if (!recoverAllowed && frame.method === "tools.call" && frame.params.qualifiedName === "agent.operation") return route.abort();
      if (
        frame.method === "tools.call" &&
        frame.params.qualifiedName === "codex.turn/start"
      ) {
        requests.push(frame.params);
        if (requests.length === 1) return route.abort();
      }
      await route.continue();
    });
    await f.page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect.poll(() => f.page.evaluate(() => JSON.parse(Object.values(sessionStorage).find(raw => raw.includes('"label":"Send message"')) ?? 'null')?.phase)).toBe('unknown');
    await f.registryChange((registry) => {
      registry.namespaces[1].tools = registry.namespaces[1].tools.filter(
        (t) => t.name !== "operation",
      );
    });
    recoverAllowed = true;
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect.poll(() => f.page.evaluate(() => JSON.parse(Object.values(sessionStorage).find(raw => raw.includes('"label":"Send message"')) ?? 'null')?.phase)).toBe('unknown');
    await f.page.clock.fastForward(125000);
    await expect(f.page.getByRole("alert").filter({ hasText: "The request has not been confirmed yet." })).toBeVisible();
    assert.equal(
      await f.page
        .getByRole("button", { name: "Retry request", exact: true })
        .count(),
      0,
    );
    await f.page.clock.setSystemTime(new Date());
    await f.registryChange(() => {});
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await f.page
      .getByRole("button", { name: "Retry request", exact: true })
      .click();
    await expect(f.page.getByText("Working…", { exact: true })).toBeVisible();
    await expect(
      f.page.getByRole("button", {
        name: "Retry request",
        exact: true,
      }),
    ).toHaveCount(0);
    assert.equal(requests.length, 2);
    assert.deepEqual(requests[1], requests[0]);
    assert.equal(
      f.current().sent.filter((x) => x.method === "turn/start").length,
      1,
    );
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "AgentUI preserves a terminal failure and permits a new attached turn without admitting concurrent input",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/task?node=browser-agent&id=saved-task");
    await f.page
      .getByRole("button", { name: "Attach historical task", exact: true })
      .click();
    await expect(f.page.getByLabel("Message", { exact: true })).toBeEditable();
    const thread = f.threads.get("saved-task");
    thread.turns.push({
      id: "failed-auth-turn",
      status: "failed",
      items: [],
      error: {
        message: "Fixture authentication failed.",
        codexErrorInfo: "other",
        additionalDetails: null,
      },
    });
    thread.status = { type: "systemError" };
    const before = structuredClone(thread.turns);
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(
      f.page.getByText("Native turn failed: Fixture authentication failed.", {
        exact: true,
      }),
    ).toBeVisible();
    await f.page
      .getByLabel("Message", { exact: true })
      .fill("New work after fixing the cause.");
    await expect(
      f.page.getByRole("button", { name: "Send message", exact: true }),
    ).toBeEnabled();
    await f.page
      .getByRole("button", { name: "Send message", exact: true })
      .click();
    await expect
      .poll(
        () =>
          f.current().sent.filter((frame) => frame.method === "turn/start")
            .length,
      )
      .toBe(1);
    await expect(
      f.page.getByRole("button", { name: "Request interruption", exact: true }),
    ).toBeVisible();
    assert.deepEqual(thread.turns.slice(0, -1), before);
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "AgentUI renders native approval choices and recovers a secret answer only after explicit re-entry",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t);
    f.current().emit({
      id: "approval-one",
      method: "item/commandExecution/requestApproval",
      params: {
        threadId: "saved-task",
        turnId: "approval-turn",
        itemId: "approval-item",
        startedAtMs: 1788690000000,
        command: "echo isolated-fixture",
        cwd: f.nativeProjects[0].path,
        availableDecisions: ["accept", "decline"],
      },
    });
    await f.open("#/task?node=browser-agent&id=saved-task");
    await choose(f.page.getByLabel("Decision", { exact: true }), "accept");
    assert.equal(
      (await options(f.page.getByLabel("Decision", { exact: true }))).includes(
        "Approve for this session",
      ),
      false,
    );
    await f.page
      .getByRole("button", { name: "Send response", exact: true })
      .click();
    await expect
      .poll(
        () =>
          f.current().sent.filter((x) => x.id === "approval-one" && !x.method)
            .length,
      )
      .toBe(1);
    assert.deepEqual(
      f.current().sent.find((x) => x.id === "approval-one" && !x.method).result,
      { decision: "accept" },
    );
    f.current().emit({
      method: "serverRequest/resolved",
      params: { requestId: "approval-one", threadId: "saved-task" },
    });
    await expect(f.page.getByLabel("Decision", { exact: true })).toHaveCount(0);
    assert.equal(await f.page.evaluate(() => Object.keys(sessionStorage).some(key =>
      key.startsWith('ivy:agent-input:') && key.endsWith(':draft'))), false);
    f.current().emit({
      id: "secret-one",
      method: "item/tool/requestUserInput",
      params: {
        threadId: "saved-task",
        turnId: "secret-turn",
        itemId: "secret-item",
        isBlocking: true,
        questions: [
          {
            id: "secret",
            header: "Secret",
            question: "Fixture secret answer",
            isSecret: true,
            isOther: true,
          },
        ],
      },
    });
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    let secretCard = f.page
      .locator("article")
      .filter({ hasText: 'request "secret-one"' });
    await secretCard
      .getByLabel("Fixture secret answer", { exact: true })
      .fill("not-for-session-storage");
    const requests = [];
    await f.page.route("**/api/v1/rpc", async (route) => {
      const frame = route.request().postDataJSON();
      if (
        frame.method === "tools.call" &&
        frame.params.qualifiedName === "agent.answer" &&
        frame.params.arguments.identity.requestId === "secret-one"
      ) {
        requests.push(frame.params);
        if (requests.length === 1) return route.abort();
      }
      await route.continue();
    });
    await secretCard
      .getByRole("button", { name: "Send response", exact: true })
      .click();
    await expect.poll(() => f.page.evaluate(() => JSON.parse(Object.values(sessionStorage).find(raw => raw.includes('"secret":true') && raw.includes('"label":"Answer native request"')) ?? 'null')?.phase)).toBe('unknown');
    assert.equal(
      (
        await f.page.evaluate(() =>
          JSON.stringify(Object.entries(sessionStorage)),
        )
      ).includes("not-for-session-storage"),
      false,
    );
    await f.page.reload();
    secretCard = f.page
      .locator("article")
      .filter({ hasText: 'request "secret-one"' });
    await expect(
      secretCard.getByLabel("Fixture secret answer", { exact: true }),
    ).toHaveValue("");
    await expect(secretCard.getByText("Re-enter the original secret response in the form.", { exact: true })).toBeVisible({ timeout: 35000 });
    await secretCard
      .getByLabel("Fixture secret answer", { exact: true })
      .fill("not-for-session-storage");
    await secretCard
      .getByRole("button", { name: "Send response", exact: true })
      .click();
    await expect
      .poll(
        () =>
          f.current().sent.filter((x) => x.id === "secret-one" && !x.method)
            .length,
      )
      .toBe(1);
    assert.equal(requests.length, 2);
    assert.deepEqual(requests[1], requests[0]);
    assert.equal(
      (
        await f.page.evaluate(() =>
          JSON.stringify(Object.entries(sessionStorage)),
        )
      ).includes("not-for-session-storage"),
      false,
    );
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "AgentUI queues messages while a turn runs, steers one into it and sends staged images and files",
  { timeout: 120000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/task?node=browser-agent&id=saved-task");
    const message = f.page.getByLabel("Message", { exact: true });
    await message.fill("Start the work.");
    await f.page.getByRole("button", { name: "Send message", exact: true }).click();
    await expect.poll(() => f.current().sent.filter((frame) => frame.method === "turn/start").length).toBe(1);
    await expect(f.page.getByRole("status").filter({ hasText: "Working…" })).toBeVisible();

    const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9L+OQAAAAASUVORK5CYII=", "base64");
    await f.page.getByLabel("Choose photos and files", { exact: true }).setInputFiles([
      { name: "screen.png", mimeType: "image/png", buffer: png },
      { name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("Release notes") },
    ]);
    await expect(f.page.getByRole("img", { name: "screen.png", exact: true })).toBeVisible();
    await expect(f.page.getByRole("status", { name: /^Uploading/ })).toHaveCount(0);
    await message.fill("Also consider these files.");
    await f.page.getByRole("button", { name: "Queue message", exact: true }).click();
    await message.fill("Then write the summary.");
    await message.press("Enter");
    const queued = f.page.getByRole("region", { name: "Queued messages", exact: true });
    await expect(queued.getByText("Queued", { exact: true })).toHaveCount(2);
    assert.equal(f.current().sent.filter((frame) => frame.method === "turn/start").length, 1);

    await queued.getByRole("button", { name: "Steer with Also consider these files.", exact: true }).click();
    await expect.poll(() => f.current().sent.filter((frame) => frame.method === "turn/steer").length).toBe(1);
    const [text, image] = f.current().sent.find((frame) => frame.method === "turn/steer").params.input;
    assert.equal(text.type, "text");
    assert.match(text.text, /^Also consider these files\.\n\nAttached file notes\.txt: .*notes\.txt$/);
    assert.equal(await readFile(text.text.split(": ").at(-1), "utf8"), "Release notes");
    assert.equal(image.type, "localImage");
    assert.deepEqual(await readFile(image.path), png);
    const savedImage = f.page.getByRole("region", { name: "Saved native output", exact: true }).getByRole("img", { name: "screen.png", exact: true });
    await expect(savedImage).toBeVisible();
    await expect.poll(() => savedImage.evaluate(img => img.naturalWidth)).toBe(1);
    await expect(queued.getByText("Queued", { exact: true })).toHaveCount(1);

    // The remaining message follows once the running turn ends.
    f.complete("saved-task");
    await expect.poll(() => f.current().sent.filter((frame) => frame.method === "turn/start").length).toBe(2);
    assert.deepEqual(f.current().sent.filter((frame) => frame.method === "turn/start").at(-1).params.input, [{ type: "text", text: "Then write the summary." }]);
    await expect(queued).toHaveCount(0);
    await f.page.reload();
    await expect(savedImage).toBeVisible();
    await f.page.setViewportSize({ width: 390, height: 844 });
    await expect(savedImage).toBeVisible();
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);
