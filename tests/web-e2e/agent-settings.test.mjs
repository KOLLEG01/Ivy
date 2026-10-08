import test from "node:test";
import assert from "node:assert/strict";
import { expect } from "@playwright/test";
import { agentFixture } from "./fixtures/agent-fixture.mjs";
import { choose, options } from "./fixtures/select.mjs";
import { newOperationId } from "../../dist/packages/sdk/src/client.js";

for (const width of [1280, 390])
  test(
    "AgentUI resolves composer defaults, retries missing modes and keeps server profiles independent at " +
      width,
    { timeout: 150000 },
    async (t) => {
      const f = await agentFixture(t, { width, height: 900 });
      f.emptyModelsOnce();
      const profile = {
        schemaVersion: 1,
        codex: {
          model: "fixture-model",
          effort: "low",
          mode: "plan",
          permission: ":workspace",
        },
        claude: {
          model: "claude-profile-model",
          effort: "high",
          mode: "default",
          permission: ":read-only",
        },
      };
      await f.client.request("objects.write", {
        mutationId: await newOperationId(f.client),
        contractVersion: "1.0.0",
        references: {},
        content: { encoding: "json", value: profile },
        create: {
          contractKey: "agent/execution-defaults",
          parentId: null,
          ownerObjectId: null,
          name: "ivy-agent-execution-defaults",
        },
      });
      let failed = false;
      await f.page.route("**/api/v1/rpc", async (route) => {
        const frame = route.request().postDataJSON();
        if (
          !failed &&
          frame.method === "tools.call" &&
          frame.params.qualifiedName === "codex.collaborationMode/list"
        ) {
          failed = true;
          await route.abort();
        } else await route.continue();
      });
      await f.open("#/host?node=browser-agent");
      await expect(f.page.getByLabel("Model", { exact: true })).toHaveText(
        /Native fixture model/,
        { timeout: 35000 },
      );
      await expect(
        f.page.getByLabel("Reasoning effort", { exact: true }),
      ).toHaveText(/low/i);
      await expect(f.page.getByLabel("Safety", { exact: true })).toHaveText(
        /Workspace/,
      );
      await expect
        .poll(
          async () =>
            (
              await options(f.page.getByLabel("Working mode", { exact: true }))
            ).includes("Plan"),
          { timeout: 30000 },
        )
        .toBe(true);
      await f.page
        .getByLabel("First message", { exact: true })
        .fill("Use the configured server profile.");
      await f.page
        .getByRole("button", { name: "Create task", exact: true })
        .click();
      await expect(f.page).toHaveURL(/#\/task\?node=browser-agent&id=/);
      const sent = f
        .current()
        .sent.find((frame) => frame.method === "turn/start");
      assert.deepEqual(sent.params.collaborationMode, {
        mode: "plan",
        settings: {
          model: "fixture-model",
          reasoning_effort: "low",
          developer_instructions: null,
        },
      });
      f.complete(sent.params.threadId);
      await expect(f.page.getByLabel("Model", { exact: true })).toHaveText(
        /Native fixture model/,
        { timeout: 35000 },
      );
      await expect(
        f.page.getByLabel("Working mode", { exact: true }),
      ).toHaveText(/Plan/);
      await f.page.goto(
        f.url +
          "#/task?node=browser-agent&id=" +
          sent.params.threadId +
          "&turn=" +
          f.threads.get(sent.params.threadId).turns[0].id,
      );
      await expect(
        f.page.getByText("Showing a linked turn.", { exact: false }),
      ).toHaveCount(0);
      await f.page.goto(f.url + "#/settings");
      await expect(
        f.page.getByLabel("Default model for server type"),
      ).toHaveText(/Native fixture model/);
      await f.page.getByRole("tab", { name: "Claude", exact: true }).click();
      await expect(
        f.page.getByLabel("Default model for server type"),
      ).toHaveText(/claude-profile-model/);
      await f.page.getByRole("tab", { name: "Codex", exact: true }).click();
      await expect(
        f.page.getByLabel("Default effort for server type"),
      ).toHaveText(/low/i);
      await choose(f.page.getByLabel("Default effort for server type"), "high");
      await f.page
        .getByRole("button", { name: /Defaults speichern|Save defaults/ })
        .click();
      await expect(
        f.page.getByText(
          /Gespeichert. Neue Tasks verwenden|Saved. New tasks use/,
        ),
      ).toBeVisible();
      const object = await f.client.request("objects.stat", {
        path: "/ivy-agent-execution-defaults",
      });
      const saved = await f.client.request("objects.read", {
        objectId: object.id,
        revision: object.currentRevision,
      });
      assert.equal(saved.content.value.codex.effort, "high");
      assert.deepEqual(saved.content.value.claude, profile.claude);
      assert.equal(failed, true);
      assert.ok(
        f.current().sent.filter(frame => frame.method === 'config/read').every(
          frame => typeof frame.params.cwd === 'string' && frame.params.cwd.length > 0,
        ),
      );
      assert.deepEqual(f.pageErrors, []);
      assert.deepEqual(f.externalRequests, []);
    },
  );
