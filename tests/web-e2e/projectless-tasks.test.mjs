import test from "node:test";
import assert from "node:assert/strict";
import { expect } from "@playwright/test";
import { readdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { agentFixture } from "./fixtures/agent-fixture.mjs";

for (const viewport of [{ width: 1280, height: 900 }, { width: 390, height: 844 }]) {
  test(`projectless tasks retain their workspace and support later project assignment at ${viewport.width}px`, { timeout: 120000 }, async t => {
    const f = await agentFixture(t, viewport, false, "0.159.2", false, true);
    await f.context.addInitScript(() => Object.defineProperty(navigator, 'language', { value: 'en-US' }));
    await f.open("#/host?node=browser-agent");
    const picker = f.page.getByLabel("Project", { exact: true });
    await expect(picker).toHaveText("No project");
    await picker.click();
    await f.page.getByRole("option", { name: /Isolated project/ }).click();
    await picker.click();
    await f.page.getByRole("option", { name: "No project", exact: true }).click();
    const projectsBefore = f.nativeProjects.length;
    await f.page.getByRole("button", { name: "Create task", exact: true }).click();
    await expect(f.page).toHaveURL(/#\/task\?/);
    const threadId = new URLSearchParams(new URL(f.page.url()).hash.split("?")[1]).get("id");
    const start = f.current().sent.find(frame => frame.method === "thread/start");
    assert.equal(start.params.projectId, null);
    assert.equal(f.threads.get(threadId).projectId, null);
    assert.ok(start.params.cwd.startsWith(join(f.config.settings.internalProjectRoot, "unassigned")));
    assert.equal(f.nativeProjects.length, projectsBefore);
    await writeFile(join(start.params.cwd, "result.txt"), "persistent task work");
    await f.restart();
    await f.page.reload();
    await expect(f.page.getByRole("button", { name: "Task actions", exact: true })).toBeVisible();
    await f.page.getByRole("button", { name: "Task actions", exact: true }).click();
    await f.page.getByRole("menuitem", { name: "Change project…", exact: true }).click();
    const dialog = f.page.getByRole("dialog", { name: "Change project", exact: true });
    await dialog.getByLabel("Project", { exact: true }).click();
    await f.page.getByRole("option", { name: "Isolated project", exact: true }).click();
    await dialog.getByRole("button", { name: "Save", exact: true }).click();
    await expect(dialog).toBeHidden();
    await expect.poll(() => f.threads.get(threadId).projectId).toBe("fixture-project");
    assert.equal(f.threads.get(threadId).cwd, start.params.cwd);
    assert.equal(await readFile(join(start.params.cwd, "result.txt"), "utf8"), "persistent task work");
    await f.page.getByRole("button", { name: "Task actions", exact: true }).click();
    await f.page.getByRole("menuitem", { name: "Change project…", exact: true }).click();
    await dialog.getByLabel("Project", { exact: true }).click();
    await f.page.getByRole("option", { name: "No project", exact: true }).click();
    await dialog.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => f.threads.get(threadId).projectId).toBeNull();
    await expect(dialog).toBeHidden();
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  });
}

test("an uncertain projectless allocation survives reload without another folder or task", { timeout: 120000 }, async t => {
  const f = await agentFixture(t, undefined, false, "0.159.2");
  await f.context.addInitScript(() => Object.defineProperty(navigator, 'language', { value: 'en-US' }));
  await f.open("#/host?node=browser-agent");
  let lost = false;
  const keys = [];
  await f.page.route("**/api/v1/rpc", async route => {
    const request = route.request().postDataJSON();
    if (request.method === "tools.call" && request.params.qualifiedName === "agent.resolveProject") {
      keys.push(request.params.arguments.selection.key);
      if (!lost) { lost = true; await route.fetch(); return route.abort(); }
    }
    await route.continue();
  });
  await f.page.getByRole("button", { name: "Create task", exact: true }).click();
  await expect(f.page.getByRole("alert").filter({ hasText: /fetch|network|failed/i }).first()).toBeVisible();
  await f.page.reload();
  await expect(f.page.getByRole("button", { name: "Create task", exact: true })).toBeEnabled();
  await f.page.getByRole("button", { name: "Create task", exact: true }).click();
  await expect(f.page).toHaveURL(/#\/task\?/);
  assert.equal(keys.length, 2);
  assert.equal(keys[0], keys[1]);
  assert.equal((await readdir(join(f.config.settings.internalProjectRoot, "unassigned"))).length, 1);
  assert.equal(f.current().sent.filter(frame => frame.method === "thread/start").length, 1);
  assert.equal(f.current().sent.some(frame => frame.method === "project/create"), false);
  await f.page.goto(f.url + "#/host?node=browser-agent");
  await f.page.getByRole("button", { name: "Create task", exact: true }).click();
  await expect(f.page).toHaveURL(/#\/task\?/);
  const starts = f.current().sent.filter(frame => frame.method === "thread/start");
  assert.equal(starts.length, 2);
  assert.notEqual(starts[0].params.cwd, starts[1].params.cwd);
  assert.deepEqual(f.pageErrors, []);
  assert.deepEqual(f.externalRequests, []);
});
