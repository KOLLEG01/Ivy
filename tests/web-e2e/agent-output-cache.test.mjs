import test from "node:test";
import assert from "node:assert/strict";
import { expect } from "@playwright/test";
import { agentFixture } from "./fixtures/agent-fixture.mjs";

const target = "#/task?node=browser-agent&id=saved-task";
const answer = (id, text) => ({ id, type: "agentMessage", text });
const output = (f) =>
  f.page.getByRole("region", { name: "Saved native output", exact: true });
const navigate = (f, hash) =>
  f.page.evaluate((hash) => {
    location.hash = hash;
  }, hash);

for (const viewport of [
  { width: 1440, height: 1000 },
  { width: 390, height: 844 },
]) {
  test(
    `AgentUI immediately restores a cached conversation and replaces edited/deleted messages (${viewport.width}px)`,
    { timeout: 90000 },
    async (t) => {
      const f = await agentFixture(t, viewport),
        thread = f.threads.get("saved-task");
      const turn = {
        id: "cached-turn",
        status: "completed",
        items: [
          answer("a", "Original answer."),
          answer("b", "Later deleted answer."),
        ],
      };
      thread.turns.push(turn);
      await f.open(target);
      await expect(output(f)).toContainText("Later deleted answer.");
      await navigate(f, "#/home");
      await expect(f.page.locator(".agent-task")).toHaveCount(0);
      turn.items = [answer("a", "Edited answer."), answer("c", "New answer.")];
      let release,
        pending = 0;
      const gate = new Promise((resolve) => {
        release = resolve;
      });
      t.after(() => release());
      await f.page.route("**/api/v1/rpc", async (route) => {
        const body = route.request().postDataJSON();
        if (body.params?.qualifiedName === "codex.thread/items/list") {
          pending++;
          await gate;
        }
        await route.continue();
      });
      await navigate(f, target);
      // No response is allowed through: this must be the synchronous preview, not a fast read.
      await expect(output(f)).toContainText("Original answer.", {
        timeout: 1500,
      });
      await expect.poll(() => pending).toBeGreaterThan(0);
      await expect(output(f)).not.toContainText("Edited answer.");
      release();
      await expect(output(f)).toContainText("Edited answer.");
      await expect(output(f)).toContainText("New answer.");
      await expect(output(f)).not.toContainText("Later deleted answer.");
      turn.items = [answer("c", "Updated while open.")];
      f.current().emit({
        method: "item/completed",
        params: { threadId: thread.id, turnId: turn.id, item: turn.items[0] },
      });
      await expect(output(f)).toContainText("Updated while open.");
      await expect(output(f)).not.toContainText("Edited answer.");
      assert.equal(
        await f.page.evaluate(
          () => document.documentElement.scrollWidth > innerWidth,
        ),
        false,
      );
      assert.deepEqual(f.pageErrors, []);
    },
  );
}

test(
  "AgentUI switches linked turns during an in-flight read without showing the previous turn",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t),
      thread = f.threads.get("saved-task");
    thread.turns.push(
      {
        id: "a",
        status: "completed",
        items: [answer("a-message", "Turn A only.")],
      },
      {
        id: "b",
        status: "completed",
        items: [answer("b-message", "Turn B only.")],
      },
    );
    await f.open(target + "&turn=b");
    await expect(output(f)).toContainText("Turn B only.");
    let release,
      pending = 0;
    const gate = new Promise((resolve) => {
      release = resolve;
    });
    t.after(() => release());
    await f.page.route("**/api/v1/rpc", async (route) => {
      const body = route.request().postDataJSON();
      if (
        body.params?.qualifiedName === "codex.thread/items/list" &&
        body.params.arguments.turnId === "a"
      ) {
        pending++;
        await gate;
      }
      try {
        await route.continue();
      } catch {
        /* The old view deliberately cancels this request. */
      }
    });
    await navigate(f, target + "&turn=a");
    await expect.poll(() => pending).toBeGreaterThan(0);
    await expect(output(f)).not.toContainText("Turn B only.");
    await navigate(f, target + "&turn=b");
    await expect(output(f)).toContainText("Turn B only.");
    release();
    await expect(output(f)).not.toContainText("Turn A only.");
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "AgentUI invalidates inactive conversations on history events",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t),
      thread = f.threads.get("saved-task");
    const turn = {
      id: "away-turn",
      status: "completed",
      items: [answer("away-message", "Before leaving.")],
    };
    thread.turns.push(turn);
    let observed = false;
    f.page.on("websocket", (socket) =>
      socket.on("framereceived", ({ payload }) => {
        if (String(payload).includes("Changed while away.")) observed = true;
      }),
    );
    await f.open(target);
    await expect(output(f)).toContainText("Before leaving.");
    await navigate(f, "#/home");
    await expect(f.page.locator(".agent-task")).toHaveCount(0);
    turn.items = [answer("away-message", "Changed while away.")];
    f.current().emit({
      method: "item/completed",
      params: { threadId: thread.id, turnId: turn.id, item: turn.items[0] },
    });
    await expect.poll(() => observed).toBe(true);
    let release,
      pending = 0;
    const gate = new Promise((resolve) => {
      release = resolve;
    });
    t.after(() => release());
    await f.page.route("**/api/v1/rpc", async (route) => {
      if (
        route.request().postDataJSON().params?.qualifiedName ===
        "codex.thread/items/list"
      ) {
        pending++;
        await gate;
      }
      await route.continue();
    });
    await navigate(f, target);
    await expect.poll(() => pending).toBeGreaterThan(0);
    await expect(output(f)).not.toContainText("Before leaving.");
    release();
    await expect(output(f)).toContainText("Changed while away.");
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "AgentUI refreshes loaded pages with fresh cursors after insertions and deletions",
  { timeout: 90000 },
  async (t) => {
    const f = await agentFixture(t, undefined, true),
      thread = f.threads.get("saved-task");
    const turn = {
      id: "paged-turn",
      status: "completed",
      items: [
        answer("a", "First saved answer."),
        answer("b", "Removed middle answer."),
        answer("c", "Latest saved answer."),
      ],
    };
    thread.turns.push(turn);
    await f.open(target);
    await expect(output(f)).toContainText("Latest saved answer.");
    const earlier = () =>
      f.page.locator(".agent-messages").evaluate((element) => {
        element.scrollTop = 0;
        element.dispatchEvent(new Event("scroll"));
      });
    await earlier();
    await expect(output(f)).toContainText("Removed middle answer.");
    await earlier();
    await expect(output(f)).toContainText("First saved answer.");
    turn.items = [
      answer("a", "Edited first answer."),
      answer("c", "Latest saved answer."),
      answer("d", "New latest answer."),
    ];
    f.current().emit({
      method: "item/completed",
      params: { threadId: thread.id, turnId: turn.id, item: turn.items.at(-1) },
    });
    await expect(output(f)).toContainText("New latest answer.");
    await expect(output(f)).toContainText("Edited first answer.");
    await expect(output(f)).not.toContainText("Removed middle answer.");
    await expect(output(f).locator("article")).toHaveCount(3);
    assert.deepEqual(f.pageErrors, []);
  },
);
