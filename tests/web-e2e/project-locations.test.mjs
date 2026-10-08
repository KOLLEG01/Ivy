import test from "node:test";
import assert from "node:assert/strict";
import { expect } from "@playwright/test";
import { readFile, readdir, mkdir } from "node:fs/promises";
import { join } from "node:path";
import { agentFixture } from "./fixtures/agent-fixture.mjs";
import { taskBoardFixture } from "./fixtures/task-board-fixture.mjs";

test("AgentUI renames and removes native projects without recreating them or deleting files", { timeout: 120000 }, async t => {
  const f = await agentFixture(t);
  const path = f.nativeProjects[0].path;
  await f.open("#/host?node=browser-agent");
  await f.page.getByRole("button", { name: "Manage project Isolated project", exact: true }).click();
  await f.page.getByRole("menuitem", { name: "Rename project", exact: true }).click();
  await f.page.getByRole("dialog").getByLabel("Project name", { exact: true }).fill("Renamed native project");
  await f.page.getByRole("button", { name: "Save name", exact: true }).click();
  await expect(f.page.getByRole("button", { name: "Manage project Renamed native project", exact: true })).toBeVisible();
  await f.page.getByRole("button", { name: "Manage project Renamed native project", exact: true }).click();
  await f.page.getByRole("menuitem", { name: "Remove project", exact: true }).click();
  await f.page.getByRole("dialog").getByRole("button", { name: "Remove project", exact: true }).click();
  await expect.poll(() => f.nativeProjects.length).toBe(0);
  await f.page.reload();
  await expect(f.page.getByRole("button", { name: "Renamed native project", exact: true })).toHaveCount(0);
  assert.ok(Array.isArray(await readdir(path)));
  assert.deepEqual(f.pageErrors, []);
});

for (const viewport of [{ width: 1280, height: 900 }, { width: 390, height: 844 }]) {
  test(`IvyInternal stays collapsed outside Recent and is searchable at ${viewport.width}px`, { timeout: 120000 }, async t => {
    const f = await agentFixture(t, viewport, false, '0.154.0', false, true);
    await f.open("#/hosts");
    if (viewport.width < 768) await f.page.getByRole("button", { name: "Toggle navigation", exact: true }).click();
    const nav = f.page.getByRole("navigation", { name: "Recent tasks", exact: true });
    await expect(nav.getByRole("button", { name: "IvyInternal", exact: true })).toHaveAttribute("aria-expanded", "false");
    await expect(nav.getByRole("link", { name: "saved-task", exact: true })).toBeVisible();
    await expect(nav.getByRole("link", { name: "Internal service work", exact: true })).toBeHidden();
    await nav.getByRole("button", { name: "IvyInternal", exact: true }).click();
    await expect(nav.getByRole("link", { name: "Internal service work", exact: true })).toBeVisible();
    await f.page.getByRole("button", { name: "Search tasks Ctrl K", exact: true }).click();
    await f.page.getByRole("dialog").getByRole("textbox").fill("Internal service work");
    await expect(f.page.getByRole("dialog").getByText("Internal service work", { exact: true })).toBeVisible();
    assert.deepEqual(f.pageErrors, []);
  });
}

test(
  "AgentUI retries an uncertain project allocation without duplicate directories and starts in its exact cwd",
  { timeout: 120000 },
  async (t) => {
    const f = await agentFixture(t);
    await f.open("#/host?node=browser-agent");
    await f.page.getByLabel("Project", { exact: true }).click();
    await f.page.getByRole("option", { name: "New project…", exact: true }).click();
    await f.page
      .getByLabel("Project name", { exact: true })
      .fill("Permanent normal work");
    let lost = false;
    await f.page.route("**/api/v1/rpc", async (route) => {
      const request = route.request().postDataJSON();
      if (
        !lost &&
        request.method === "tools.call" &&
        request.params.qualifiedName === "agent.resolveProject"
      ) {
        lost = true;
        await route.fetch();
        return route.abort();
      }
      await route.continue();
    });
    await f.page
      .getByRole("button", { name: "Use this location", exact: true })
      .click();
    await expect(
      f.page.getByRole("button", {
        name: "Retry original selection",
        exact: true,
      }),
    ).toBeEnabled();
    await f.page
      .getByRole("button", { name: "Retry original selection", exact: true })
      .click();
    await expect
      .poll(async () =>
        (
          (await f.page
            .getByLabel("Project", { exact: true })
            .getAttribute("data-value")) ?? ""
        ).startsWith(f.config.settings.projectRoot),
      )
      .toBe(true);
    const cwd =
      (await f.page
        .getByLabel("Project", { exact: true })
        .getAttribute("data-value")) ?? "";
    assert.equal(cwd, join(f.config.settings.projectRoot, "Permanent normal work"));
    assert.equal((await readdir(f.config.settings.projectRoot)).length, 1);
    assert.equal(f.nativeProjects.filter(project => project.path === cwd).length, 1);
    await f.page
      .getByRole("button", { name: "Create task", exact: true })
      .click();
    await expect
      .poll(
        () =>
          f.current().sent.filter((frame) => frame.method === "thread/start")
            .length,
      )
      .toBe(1);
    assert.equal(
      f.current().sent.find((frame) => frame.method === "thread/start").params
        .cwd,
      cwd,
    );
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

for (const viewport of [{ width: 1280, height: 900 }, { width: 390, height: 844 }]) {
  test(`AgentUI preserves new project folder names and allows correcting collisions at ${viewport.width}px`,
    { timeout: 120000 }, async t => {
      const f = await agentFixture(t, viewport);
      await f.open("#/host?node=browser-agent");
      const selectNew = async () => {
        await f.page.getByLabel("Project", { exact: true }).click();
        await f.page.getByRole("option", { name: "New project…", exact: true }).click();
      };
      const use = () => f.page.getByRole("button", { name: "Use this location", exact: true }).click();
      const name = f.page.getByRole("dialog").getByLabel("Project name", { exact: true });
      await selectNew();
      await name.fill("Local Take ä");
      await use();
      const cwd = join(f.config.settings.projectRoot, "Local Take ä");
      await expect(f.page.getByLabel("Project", { exact: true })).toHaveAttribute("data-value", cwd);
      assert.equal(f.nativeProjects.filter(project => project.path === cwd).length, 1);
      await selectNew();
      await name.fill("Local Take ä");
      await use();
      await expect(f.page.getByRole("dialog").getByText(/Project directory already exists:/)).toBeVisible();
      await expect(name).toBeEnabled();
      await expect(f.page.getByRole("button", { name: "Retry original selection", exact: true })).toHaveCount(0);
      assert.equal(f.current().sent.filter(frame => frame.method === "project/create").length, 1);
      assert.deepEqual(await readdir(f.config.settings.projectRoot), ["Local Take ä"]);
      await name.fill("nested/folder");
      await use();
      await expect(f.page.getByRole("dialog").getByText(/Project name must be one valid directory name/)).toBeVisible();
      await expect(name).toBeEnabled();
      await name.fill("Other work");
      await use();
      await expect(f.page.getByLabel("Project", { exact: true })).toHaveAttribute("data-value", join(f.config.settings.projectRoot, "Other work"));
      assert.equal(f.current().sent.filter(frame => frame.method === "project/create").length, 2);
      assert.deepEqual(f.pageErrors, []);
    });
}

for (const viewport of [{ width: 1280, height: 900 }, { width: 390, height: 844 }]) {
  test(`AgentUI browses and selects an existing host directory from its project menu at ${viewport.width}px`,
    { timeout: 120000 }, async t => {
      const f = await agentFixture(t, viewport);
      const nested = join(f.root, "Browse", "Nested");
      await mkdir(nested, { recursive: true });
      await f.open("#/host?node=browser-agent");
      await expect(f.page.getByRole("button", { name: "Add project", exact: true })).toHaveCount(0);
      await f.page.getByLabel("Project", { exact: true }).click();
      await f.page.getByRole("option", { name: "Choose existing directory…", exact: true }).click();
      await expect(f.page.getByLabel("Project type", { exact: true })).toHaveAttribute("data-value", "existing");
      await f.page.getByRole("button", { name: "Browse…", exact: true }).click();
      const browse = f.page.getByRole("dialog", { name: "Choose existing directory", exact: true });
      await browse.getByLabel("Folder path").fill(f.root);
      await browse.getByRole("button", { name: "Open", exact: true }).click();
      await browse.getByRole("listitem").getByText("Browse", { exact: true }).click();
      await browse.getByRole("listitem").getByText("Nested", { exact: true }).click();
      await expect(browse.getByLabel("Folder path")).toHaveValue(nested);
      await browse.getByRole("button", { name: "Use this folder", exact: true }).click();
      await expect(f.page.getByLabel("Absolute directory on this host")).toHaveValue(nested);
      await f.page.getByRole("button", { name: "Use this location", exact: true }).click();
      await expect(f.page.getByLabel("Project", { exact: true })).toHaveAttribute("data-value", nested);
      assert.equal(f.nativeProjects.filter(project => project.path === nested).length, 1);
      assert.equal(f.current().sent.some(frame => frame.method === "fs/readDirectory"), false);
      assert.deepEqual(f.pageErrors, []);
      assert.deepEqual(f.externalRequests, []);
    });
}

test(
  "TaskBoardUI automatically executes in the deterministic internal Task workspace",
  { timeout: 180000 },
  async (t) => {
    const f = await taskBoardFixture(t, { native: true, scheduler: true });
    await f.open("#/new?node=browser-task-board");
    await f.page
      .getByLabel("Title", { exact: true })
      .fill("Internal project execution");
    await f.page
      .getByLabel("Description", { exact: true })
      .fill("Keep internal working files.");
    await f.page
      .getByRole("button", { name: "Create task", exact: true })
      .click();
    await f.page
      .getByRole("heading", {
        name: /TASK-[0-9]{4,} · Internal project execution/,
      })
      .waitFor();
    const id = new URLSearchParams(
      new URL(f.page.url()).hash.split("?")[1],
    ).get("id");
    assert.ok(id);
    const taskKey = (await f.task(id)).taskKey;
    await f.page
      .getByRole("button", { name: "Move to Todo", exact: true })
      .click();
    await expect
      .poll(
        () =>
          f.current().sent.filter((frame) => frame.method === "turn/start")
            .length,
        { timeout: 60000 },
      )
      .toBe(1);
    const cwd = f
      .current()
      .sent.find((frame) => frame.method === "thread/start").params.cwd;
    assert.equal(cwd, f.config.settings.internalProjectRoot);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);
