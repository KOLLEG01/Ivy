import test from "node:test";
import assert from "node:assert/strict";
import { expect } from "@playwright/test";
import { agentFixture } from "./fixtures/agent-fixture.mjs";

for (const width of [1280, 390]) {
  test(`AgentUI waits on the send button, warns after two minutes and recovers the original task at ${width}px`, { timeout: 150000 }, async t => {
    const f = await agentFixture(t, { width, height: 900 });
    await f.context.addInitScript(() => Object.defineProperty(navigator, 'language', { value: 'en-US' }));
    const release = f.holdNativeOnce("thread/start");
    await f.open("#/host?node=browser-agent");
    await f.page.clock.install();
    await f.page.getByLabel("First message", { exact: true }).fill("Keep the first message while creating.");
    const send = f.page.getByRole("button", { name: "Create task", exact: true });
    await send.click();
    await expect(send).toHaveAttribute("aria-busy", "true");
    await expect(send.locator("svg.animate-spin")).toHaveCount(1);
    await expect(f.page.getByText("Waiting for the original native outcome.", { exact: true })).toHaveCount(0);
    await expect(f.page.getByRole("alert")).toHaveCount(0);
    await expect.poll(() => f.current().sent.filter(x => x.method === "thread/start").length, { timeout: 35000 }).toBe(1);
    await f.page.clock.fastForward(40000);
    await expect.poll(() => f.page.evaluate(() => {
      const key = Object.keys(sessionStorage).find(key => key.startsWith("ivy:agent-create:") && !key.startsWith("ivy:agent-create-draft:"));
      return JSON.parse(sessionStorage.getItem(key)).phase;
    }), { timeout: 35000 }).toBe("pending");
    await expect(f.page.getByRole("alert")).toHaveCount(0);
    await f.page.clock.fastForward(85000);
    await expect(f.page.getByRole("alert")).toContainText("The request is still being processed");
    await expect(f.page.getByRole("alert")).not.toHaveClass(/text-destructive/);
    await expect(send).toHaveAttribute("aria-busy", "true");
    await expect(f.page.getByRole("alert")).not.toContainText("Operation");
    assert.equal(f.current().sent.filter(x => x.method === "thread/start").length, 1);
    // Fresh mutation IDs use wall time; only the waiting UI above runs two minutes ahead.
    await f.page.clock.setSystemTime(new Date());
    release();
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(f.page).toHaveURL(/#\/task\?node=browser-agent&id=/, { timeout: 35000 });
    const sent = f.current().sent.filter(x => x.method === "turn/start");
    assert.equal(sent.length, 1);
    assert.equal(sent[0].params.input[0].text, "Keep the first message while creating.");
    assert.equal(f.current().sent.filter(x => x.method === "thread/start").length, 1);
    await expect(f.page.getByRole("alert")).toHaveCount(0);
    assert.ok(await f.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  });
}
