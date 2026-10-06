import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { expect } from "@playwright/test";
import { publishUi } from "../../dist/packages/cli/src/publish-ui.js";
import { newOperationId } from "../../dist/packages/sdk/src/client.js";
import { fixture } from "./fixtures/fixture.mjs";
import { agentFixture } from "./fixtures/agent-fixture.mjs";
import { taskBoardFixture } from "./fixtures/task-board-fixture.mjs";
import { choose } from "./fixtures/select.mjs";

const widths = [360, 390, 430, 768, 1440];
async function fits(page) {
  assert.ok(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth + 2,
    ),
    "The page must fit its viewport",
  );
}
async function publishWiki(f) {
  const directory = resolve("dist/apps/wiki-ui"),
    definition = JSON.parse(
      await readFile(resolve(directory, "ivy-ui.json"), "utf8"),
    );
  await publishUi(f.client, {
    directory,
    definition,
    expectedReleaseId: null,
    mutationId: await newOperationId(f.client),
  });
  return f.base + "/ui/wiki-ui/";
}

test(
  "Object workspace lazily expands a deep tree and preserves its list when the inspector opens, closes and reloads",
  { timeout: 90000 },
  async (t) => {
    const f = await fixture(t);
    await f.client.request("contracts.register", {
      mutationId: await newOperationId(f.client),
      definition: {
        key: "fixture/tree",
        version: "1.0.0",
        owner: { kind: "agent" },
        mediaType: "text/markdown",
        retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
        specMarkdown: "Hierarchical browser test.",
      },
    });
    const create = async (name, parentId = null) =>
      (
        await f.client.request("objects.write", {
          mutationId: await newOperationId(f.client),
          contractVersion: "1.0.0",
          references: {},
          create: {
            contractKey: "fixture/tree",
            parentId,
            ownerObjectId: null,
            name,
          },
          content: {
            encoding: "text",
            value: "# " + name + "\n\nSaved content.",
          },
        })
      ).object.id;
    const parent = await create("Workspace"),
      child = await create("Research", parent),
      leaf = await create("A long document title with several words", child);
    const lists = [];
    f.page.on("request", (request) => {
      if (request.method() === "POST" && request.url().endsWith("/rpc")) {
        const body = request.postDataJSON();
        if (body.method === "objects.list") lists.push(body.params.parentId);
      }
    });
    await f.login();
    await f.page.goto(f.base + "/#/system/objects");
    const tree = f.page.getByRole("tree", { name: "Object tree", exact: true });
    await expect(
      tree.getByRole("treeitem", { name: "Workspace", exact: true }),
    ).toBeVisible();
    assert.equal(lists.includes(parent), false);
    await tree
      .getByRole("treeitem", { name: "Workspace", exact: true })
      .focus();
    await f.page.keyboard.press("ArrowRight");
    await f.page.keyboard.press("ArrowRight");
    await expect(
      tree.getByRole("treeitem", { name: "Research", exact: true }),
    ).toBeFocused();
    await f.page.keyboard.press("Enter");
    await expect(f.page).toHaveURL(new RegExp("parent=" + child));
    await f.page
      .getByRole("region", { name: "Object list", exact: true })
      .getByRole("link", { name: /^A long document title/ })
      .click();
    await expect(
      f.page.getByRole("region", { name: "Object inspector" }),
    ).toBeVisible();
    await f.page
      .getByRole("navigation", { name: "Inspector views" })
      .getByRole("link", { name: "Metadata" })
      .click();
    await expect(
      f.page.getByText(
        "/Workspace/Research/A long document title with several words",
        { exact: true },
      ),
    ).toBeVisible();
    await f.page
      .getByRole("link", { name: "Back to objects", exact: true })
      .click();
    assert.equal(
      new URLSearchParams(new URL(f.page.url()).hash.split("?")[1]).get(
        "parent",
      ),
      child,
    );
    await f.page.reload();
    await expect(
      tree.getByRole("treeitem", { name: "Research", exact: true }),
    ).toBeVisible();
    for (const width of widths) {
      await f.page.setViewportSize({ width, height: 844 });
      await fits(f.page);
      if (width < 1024) {
        await f.page
          .getByRole("button", { name: "Toggle navigation", exact: true })
          .click();
        await expect(
          f.page.getByRole("dialog", { name: "Sidebar", exact: true }),
        ).toBeVisible();
        await f.page.keyboard.press("Escape");
        await expect(
          f.page.getByRole("button", {
            name: "Toggle navigation",
            exact: true,
          }),
        ).toBeFocused();
      }
    }
    await f.page.goto(
      f.base + "/#/system/objects?parent=" + child + "&id=" + leaf,
    );
    await f.page.setViewportSize({ width: 360, height: 640 });
    await expect(
      f.page.getByRole("region", { name: "Object list", exact: true }),
    ).toBeHidden();
    await fits(f.page);
    await f.page
      .getByRole("button", { name: "Toggle navigation", exact: true })
      .click();
    await f.page.getByRole("button", { name: "Account", exact: true }).click();
    await f.page.getByRole("menuitem", { name: "Use dark theme" }).click();
    await expect(f.page.locator("html")).toHaveClass(/dark/);
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "Wiki document blocks, contextual location changes and mobile drafts retain exact saved content",
  { timeout: 90000 },
  async (t) => {
    const f = await fixture(t),
      url = await publishWiki(f);
    await f.login();
    await f.page.goto(url + "#/new");
    await f.page
      .getByLabel("Page title", { exact: true })
      .fill("Product notes");
    const editor = f.page.getByLabel("Markdown", { exact: true });
    await editor.click();
    await editor.pressSequentially("## Direction");
    await editor.press("Enter");
    await editor.pressSequentially("Keep the workspace quiet.");
    // Pages save automatically; edits typed during the create follow on the page view.
    await expect(f.page).toHaveURL(/#\/page\?id=/, { timeout: 15000 });
    await expect(
      f.page.getByRole("heading", { name: "Product notes", exact: true }),
    ).toBeVisible();
    const id = new URLSearchParams(
      new URL(f.page.url()).hash.split("?")[1],
    ).get("id");
    await expect.poll(async () =>
      (await f.client.request("objects.read", { objectId: id })).content.value, { timeout: 15000 },
    ).toBe("## Direction\n\nKeep the workspace quiet.\n");
    await f.page
      .getByRole("button", { name: "Page options", exact: true })
      .click();
    await f.page
      .getByRole("menuitem", { name: "New subpage", exact: true })
      .click();
    await f.page.getByLabel("Page title", { exact: true }).fill("Mobile notes");
    await f.page
      .getByLabel("Markdown", { exact: true })
      .fill("A retained child page.");
    await expect(f.page).toHaveURL(/#\/page\?id=/, { timeout: 15000 });
    await expect(f.page).not.toHaveURL(new RegExp("id=" + id + "$"));
    await expect(
      f.page.getByRole("heading", { name: "Mobile notes", exact: true }),
    ).toBeVisible();
    const child = new URLSearchParams(
      new URL(f.page.url()).hash.split("?")[1],
    ).get("id");
    await f.page
      .getByRole("button", { name: "Page options", exact: true })
      .click();
    await f.page
      .getByRole("menuitem", { name: "Move page", exact: true })
      .click();
    await f.page
      .getByRole("dialog")
      .getByLabel("Page title", { exact: true })
      .fill("Pocket notes");
    await f.page
      .getByRole("button", { name: "Wiki root", exact: true })
      .click();
    await f.page
      .getByRole("button", { name: "Save location", exact: true })
      .click();
    await expect(
      f.page.getByRole("heading", { name: "Pocket notes", exact: true }),
    ).toBeVisible();
    const moved = await f.client.request("objects.stat", { objectId: child });
    assert.equal(moved.parentId, null);
    assert.equal(moved.currentRevision, 1);
    await f.page
      .getByLabel("Markdown", { exact: true })
      .fill("Unsaved on a phone.");
    for (const width of widths) {
      await f.page.setViewportSize({ width, height: 844 });
      await fits(f.page);
    }
    await f.page.reload();
    await expect(f.page.getByLabel("Markdown", { exact: true })).toContainText(
      "Unsaved on a phone.",
    );
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "Agent workspace starts a task and its first message once, combines live chunks and keeps the composer reachable",
  { timeout: 120000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/host?node=browser-agent");
    let lost = false;
    await f.page.route("**/api/v1/rpc", async (route) => {
      const body = route.request().postDataJSON();
      if (
        !lost &&
        body.method === "tools.call" &&
        body.params.qualifiedName === "codex.turn/start"
      ) {
        lost = true;
        await route.fetch();
        await route.abort();
      } else await route.continue();
    });
    await f.page
      .getByLabel("First message", { exact: true })
      .fill("Implement the mobile navigation.");
    await choose(f.page
      .getByLabel("Project", { exact: true }), f.nativeProjects[0].path);
    await f.page
      .getByRole("button", { name: "Create task", exact: true })
      .click();
    await expect.poll(() => lost, { timeout: 35000 }).toBe(true);
    const firstMessage = f.page
      .locator('[aria-live="polite"]')
      .filter({ has: f.page.getByText("Send first message", { exact: true }) });
    await expect(
      firstMessage.getByText("unknown", { exact: true }),
    ).toBeVisible();
    await expect(
      firstMessage.getByRole("button", {
        name: "Check original outcome",
        exact: true,
      }),
    ).toBeEnabled();
    await f.page.reload();
    await f.page
      .getByRole("button", { name: "Check original outcome", exact: true })
      .click();
    await expect(f.page).toHaveURL(/#\/task\?node=browser-agent&id=/, {
      timeout: 35000,
    });
    const start = f
      .current()
      .sent.filter((frame) => frame.method === "turn/start");
    assert.equal(start.length, 1);
    assert.equal(
      f.current().sent.filter((frame) => frame.method === "thread/start")
        .length,
      1,
    );
    assert.equal(
      start[0].params.input[0].text,
      "Implement the mobile navigation.",
    );
    const id = start[0].params.threadId,
      turnId = f.threads.get(id).turns[0].id;
    f.current().emit({
      method: "item/agentMessage/delta",
      params: {
        threadId: id,
        turnId,
        itemId: "answer",
        delta: "One coherent ",
      },
    });
    f.current().emit({
      method: "item/agentMessage/delta",
      params: { threadId: id, turnId, itemId: "answer", delta: "answer." },
    });
    const liveAnswer = f.page.getByRole("article", { name: "Live answer" });
    await expect
      .poll(async () => await liveAnswer.innerText(), { timeout: 35000 })
      .toContain("One coherent answer.");
    f.complete(
      id,
      "The navigation is ready.\n\n" +
        Array.from(
          { length: 80 },
          (_, index) => "Saved paragraph " + index + ".",
        ).join("\n\n"),
    );
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await f.page
      .getByLabel("Message", { exact: true })
      .fill("Retain this next message.");
    for (const width of widths) {
      await f.page.setViewportSize({ width, height: 844 });
      await fits(f.page);
      const bounds = await f.page
        .getByRole("button", { name: "Send message", exact: true })
        .boundingBox();
      assert.ok(bounds.y + bounds.height <= 844 && bounds.y >= 0);
    }
    await f.page.setViewportSize({ width: 390, height: 500 });
    await f.page.getByLabel("Message", { exact: true }).focus();
    const button = await f.page
      .getByRole("button", { name: "Send message", exact: true })
      .boundingBox();
    assert.ok(button.y + button.height <= 500);
    await expect(
      f.page.getByText("Saved paragraph 79.", { exact: true }),
    ).toBeVisible();
    await f.page
      .locator(".agent-messages")
      .evaluate((element) => (element.scrollTop = 120));
    await expect
      .poll(() =>
        f.page.evaluate(() =>
          Object.entries(sessionStorage).some(
            ([key, value]) =>
              key.startsWith("ivy.agent.scroll:") &&
              JSON.parse(value).top === 120,
          ),
        ),
      )
      .toBe(true);
    await f.page.reload();
    await expect(f.page.getByLabel("Message", { exact: true })).toHaveValue(
      "Retain this next message.",
    );
    await expect(
      f.page.getByText("Saved paragraph 79.", { exact: true }),
    ).toBeVisible();
    await expect
      .poll(() =>
        f.page
          .locator(".agent-messages")
          .evaluate((element) => element.scrollTop),
      )
      .toBe(120);
    assert.equal(
      f.current().sent.filter((frame) => frame.method === "turn/start").length,
      1,
    );
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "TaskBoard columns page independently, show total counts and retain the board behind mobile details",
  { timeout: 120000 },
  async (t) => {
    const f = await taskBoardFixture(t);
    const fields = (title) => ({
      title,
      description: "A board task.",
      acceptanceCriteria: [],
      category: null,
      control: "user",
      priority: 0,
      executionRequirement: null,
      workspaceRequirement: { kind: "task_workspace" },
      dependencies: [],
      userContact: "ticket",
      dueAt: null,
      nextReviewAt: null,
    });
    let taskId;
    for (let index = 0; index < 23; index++) {
      const result = await f.invoke({
        action: "create",
        fields: fields("Board task " + String(index).padStart(2, "0")),
      });
      taskId ??= result.task.objectId;
    }
    await f.open("#/backlog?node=browser-task-board");
    const backlog = f.page.getByRole("region", {
      name: "Backlog",
      exact: true,
    });
    await expect(backlog.getByLabel("23 total tasks")).toBeVisible();
    await expect(backlog.locator("article")).toHaveCount(20);
    await backlog.getByRole("button", { name: "Next", exact: true }).click();
    await expect(backlog.locator("article")).toHaveCount(3);
    const title = await backlog
      .locator("article h3")
      .first()
      .innerText();
    await backlog.getByRole("link", { name: new RegExp(title) }).click();
    await expect(
      f.page
        .getByRole("dialog")
        .getByRole("heading", { name: new RegExp(title) }),
    ).toBeVisible();
    for (const width of widths) {
      await f.page.setViewportSize({ width, height: 844 });
      await fits(f.page);
    }
    await f.page.setViewportSize({ width: 390, height: 844 });
    await f.page.getByRole("button", { name: "Close", exact: true }).click();
    await expect(backlog.locator("article")).toHaveCount(3);
    await expect(backlog.getByText("Page 2", { exact: false })).toBeVisible();
    await f.page.reload();
    await expect(backlog.getByText("Page 2", { exact: false })).toBeVisible();
    await fits(f.page);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);
