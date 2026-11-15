import test from "node:test";
import assert from "node:assert/strict";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { expect } from "@playwright/test";
import { fixture, fixtureToken } from "./fixtures/fixture.mjs";
import { startNativeService } from "../../tools/testing/native-service.mjs";
import { publishUi } from "../../dist/packages/cli/src/publish-ui.js";
import {
  newOperationId,
  discover,
  callBound,
} from "../../dist/packages/sdk/src/client.js";

for (const viewport of [
  { width: 1440, height: 1000 },
  { width: 390, height: 844 },
])
  test(
    `DataCollector edits a script, runs it and views retained results at ${viewport.width}px`,
    { timeout: 180000 },
    async (t) => {
      const f = await fixture(t, false, viewport);
      const german = viewport.width === 390;
      const label = (english, deutsch) => (german ? deutsch : english);
      await f.context.addInitScript(
        (language) => {
          Object.defineProperty(Navigator.prototype, "language", {
            get: () => language,
          });
        },
        german ? "de-DE" : "en-US",
      );
      const dataRoot = join(f.root, "collector");
      await mkdir(dataRoot);
      const config = {
        schemaVersion: 1,
        hostId: "fixture",
        instanceId: "collector",
        serviceNodeId: "fixture.collector",
        componentId: "data-collector",
        publicBaseUrl: f.base,
        dataRoot,
        workRoot: join(f.root, "work"),
        credential: fixtureToken,
        settings: {},
      };
      const runtime = await startNativeService(config, {
        env: { IVY_NODE_EXECUTABLE: process.execPath },
      });
      t.after(() => runtime.close());
      const readTask = async () =>
        callBound(
          f.client,
          await discover(f.client, "data-collector.read", {
            serviceNodeId: config.serviceNodeId,
          }),
          { view: "task", id: "browser-fixture" },
        );
      await publishUi(f.client, {
        directory: resolve("dist/apps/data-collector-ui"),
        definition: JSON.parse(
          await readFile("dist/apps/data-collector-ui/ivy-ui.json", "utf8"),
        ),
        mutationId: await newOperationId(f.client),
        expectedReleaseId: null,
      });
      await f.login();
      await f.page.goto(f.base + "/ui/data-collector-ui/");
      await f.page
        .getByRole("link", {
          name: label("New task", "Neuer Task"),
          exact: true,
        })
        .first()
        .click()
        .catch(async (error) => {
          throw new Error(
            `${error.message}\nPage errors: ${JSON.stringify(f.pageErrors)}\nPage: ${(await f.page.locator("body").innerText()).slice(0, 3000)}`,
          );
        });
      await f.page
        .getByLabel(label("Name", "Name"), { exact: true })
        .fill("Browser collection");
      await f.page
        .getByLabel(label("Task ID", "Task-ID"), { exact: true })
        .fill("browser-fixture");
      await f.page
        .getByLabel(label("Interval (seconds)", "Intervall (Sekunden)"), {
          exact: true,
        })
        .fill("0");
      await f.page
        .getByLabel(label("Keep results", "Ergebnisse behalten"), {
          exact: true,
        })
        .fill("1");
      await f.page
        .getByLabel("JavaScript (ESM)", { exact: true })
        .fill("export default async () => ({data:{temperature:21.5}});");
      await f.page
        .getByRole("button", { name: label("Save", "Speichern"), exact: true })
        .click();
      await expect(
        f.page.getByRole("heading", {
          name: "Browser collection",
          exact: true,
        }),
      ).toBeVisible();
      assert.equal(
        (await readTask()).status,
        "draft",
        "saving creates a draft",
      );
      assert.equal((await readTask()).adoptedRevision, null);
      await f.page
        .getByRole("button", {
          name: label("Apply revision", "Stand übernehmen"),
          exact: true,
        })
        .click();
      await expect
        .poll(async () => {
          const task = await readTask();
          return task.adoptedRevision === task.revision;
        })
        .toBe(true);
      for (let n = 0; n < 2; n++) {
        const previousRunId = (await readTask()).lastRunId;
        await f.page
          .getByRole("button", {
            name: label("Run now", "Jetzt ausführen"),
            exact: true,
          })
          .click();
        await expect
          .poll(
            async () => {
              const current = await readTask();
              return {
                newRun: current.lastRunId !== previousRunId,
                status: current.status,
                error: current.error,
              };
            },
            {
              timeout: 30000,
            },
          )
          .toEqual({ newRun: true, status: "succeeded", error: null });
        await expect(f.page.getByText("21.5", { exact: true })).toBeVisible();
      }
      const row = await readTask();
      await expect
        .poll(
          async () =>
            (
              await f.client.request("objects.history", {
                objectId: row.resultObjectId,
              })
            ).items.map((entry) => entry.revision),
          { timeout: 30000 },
        )
        .toEqual([2]);
      await f.page
        .getByRole("button", { name: label("Edit", "Bearbeiten"), exact: true })
        .click();
      await expect(
        f.page.getByLabel("JavaScript (ESM)", { exact: true }),
      ).toBeEditable();
      await f.page
        .getByLabel(
          label(
            "Grant this script MCP access",
            "MCP-Zugriff für dieses Script freigeben",
          ),
          { exact: true },
        )
        .click();
      await f.page
        .getByRole("combobox", {
          name: label("Access", "Zugriff"),
          exact: true,
        })
        .click();
      await f.page
        .getByRole("option", {
          name: label("Ivy services", "Ivy-Dienste"),
          exact: true,
        })
        .click();
      const principal = (await f.client.request("system.status", {}))
        .callerPrincipalId;
      await f.page
        .getByLabel(label("Collector principal", "Collector-Principal"), {
          exact: true,
        })
        .fill(principal);
      await f.page
        .getByLabel(
          label(
            "Granted Ivy bindings (JSON)",
            "Freigegebene Ivy-Bindungen (JSON)",
          ),
          { exact: true },
        )
        .fill('[{"kind":"core","tool":"hive_status"}]');
      assert.ok(
        await f.page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth + 2,
        ),
        "Editor fits viewport",
      );
      await f.page
        .getByLabel("JavaScript (ESM)", { exact: true })
        .fill(
          "export default async ({mcp}) => { if (!(await mcp.call('hive_status', {})).result.runtimeEpoch) throw Error('status missing'); throw Object.assign(new Error('secret-fixture'), {code:'interaction_required'}); };",
        );
      await f.page
        .getByRole("button", { name: label("Save", "Speichern"), exact: true })
        .click();
      await expect
        .poll(async () => (await readTask()).revision)
        .not.toBe(row.adoptedRevision);
      const draft = await readTask();
      assert.equal(
        draft.adoptedRevision,
        row.adoptedRevision,
        "saved draft leaves accepted execution unchanged",
      );
      assert.notEqual(draft.revision, draft.adoptedRevision);
      await f.page
        .getByRole("button", {
          name: label("Apply revision", "Stand übernehmen"),
          exact: true,
        })
        .click();
      await f.page
        .getByRole("button", {
          name: label("Run now", "Jetzt ausführen"),
          exact: true,
        })
        .click();
      await expect(
        f.page.getByRole("alert").filter({
          hasText: label(
            "Complete provider verification as described in the setup guide.",
            "Anbieter-Verifizierung gemäß Einrichtungsanleitung abschließen.",
          ),
        }),
      ).toBeVisible({ timeout: 30000 });
      assert.equal(
        (await f.page.locator("body").innerText()).includes("secret-fixture"),
        false,
      );
      await expect(f.page.getByText("21.5", { exact: true })).toBeVisible();
      await f.page
        .getByRole("button", { name: label("Edit", "Bearbeiten"), exact: true })
        .click();
      await expect(
        f.page.getByLabel(label("Collector principal", "Collector-Principal"), {
          exact: true,
        }),
      ).toHaveValue(principal);
      await expect(
        f.page.getByLabel(
          label(
            "Granted Ivy bindings (JSON)",
            "Freigegebene Ivy-Bindungen (JSON)",
          ),
          { exact: true },
        ),
      ).toHaveValue(/hive_status/);
      assert.ok(
        await f.page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth + 2,
        ),
      );
      assert.deepEqual(f.pageErrors, []);
      assert.deepEqual(f.externalRequests, []);
    },
  );

for (const viewport of [
  { width: 1440, height: 1000 },
  { width: 390, height: 844 },
])
  test(
    `DataCollector preserves unchanged details, pages history and ignores stale results at ${viewport.width}px`,
    { timeout: 180000 },
    async (t) => {
      const f = await fixture(t, false, viewport);
      const dataRoot = join(f.root, "collector");
      await mkdir(dataRoot);
      const serviceNodeId = "fixture.collector";
      const runtime = await startNativeService(
        {
          schemaVersion: 1,
          hostId: "fixture",
          instanceId: "collector",
          serviceNodeId,
          componentId: "data-collector",
          publicBaseUrl: f.base,
          dataRoot,
          workRoot: join(f.root, "work"),
          credential: fixtureToken,
          settings: {},
        },
        {
          env: { IVY_NODE_EXECUTABLE: process.execPath },
        },
      );
      t.after(() => runtime.close());
      const binding = async (name) =>
        discover(f.client, "data-collector." + name, { serviceNodeId });
      const readBinding = await binding("read"),
        updateBinding = await binding("update");
      const read = (input) => callBound(f.client, readBinding, input);
      const update = async (input) => {
        const operationId = await newOperationId(f.client);
        return callBound(
          f.client,
          updateBinding,
          { ...input, operationId },
          operationId,
        );
      };
      const long = "x".repeat(2048) + "END-SEEN";
      const image =
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=";
      const rows = new Map();
      for (const [id, name, data] of [
        [
          "details",
          "Detail collection",
          {
            marker: "FIRST-RESULT",
            longValue: long,
            images: [{ name: "Camera pixel", dataUrl: image }],
          },
        ],
        ["other", "Other collection", { marker: "SECOND-RESULT" }],
      ]) {
        const saved = await update({
          action: "save",
          expectedRevision: 0,
          task: {
            id,
            name,
            enabled: false,
            intervalSeconds: 0,
            timeoutSeconds: 15,
            memoryMb: 64,
            dependencies: {},
            config: {},
            secretNames: [],
            script:
              "export default async()=>({data:" + JSON.stringify(data) + "});",
            retention: {
              maximumCount: 1000,
              maximumAgeDays: 7,
              maximumBytes: 104857600,
            },
          },
        });
        await update({
          action: "activate",
          jobId: saved.jobId,
          definitionRevision: 1,
          expectedRevision: 0,
          enabled: false,
        });
        await update({ action: "run", id });
        await expect
          .poll(async () => (await read({ view: "task", id })).status, {
            timeout: 30000,
          })
          .toBe("succeeded");
        rows.set(id, await read({ view: "task", id }));
        await expect
          .poll(
            async () => {
              const definition = await read({
                view: "definition",
                jobId: saved.jobId,
              });
              const current = await read({ view: "task", id });
              return (
                definition.assignment.content.value.resultObjectId ===
                  current.resultObjectId &&
                definition.assignment.revision.revision ===
                  current.definition.assignmentRevision
              );
            },
            { timeout: 20000 },
          )
          .toBe(true);
      }
      const resultId = rows.get("details").resultObjectId;
      const original = await f.client.request("objects.read", {
        objectId: resultId,
      });
      for (let revision = 2; revision <= 53; revision++)
        await f.client.request("objects.write", {
          objectId: resultId,
          expectedRevision: revision - 1,
          contractVersion: original.revision.contractVersion,
          references: original.revision.references,
          content: original.content,
          mutationId: await newOperationId(f.client),
        });
      await publishUi(f.client, {
        directory: resolve("dist/apps/data-collector-ui"),
        definition: JSON.parse(
          await readFile("dist/apps/data-collector-ui/ivy-ui.json", "utf8"),
        ),
        mutationId: await newOperationId(f.client),
        expectedReleaseId: null,
      });
      const reads = [];
      const calls = (request) => {
        if (
          !request.url().endsWith("/api/v1/rpc") ||
          request.method() !== "POST"
        )
          return;
        const body = request.postDataJSON();
        return (Array.isArray(body) ? body : [body]).filter(
          (item) =>
            item?.method === "tools.call" &&
            item.params?.qualifiedName === "data-collector.read",
        );
      };
      f.page.on("request", (request) => {
        for (const item of calls(request) ?? [])
          reads.push(item.params.arguments);
      });
      await f.login();
      await f.page.goto(f.base + "/ui/data-collector-ui/#/task?id=details");
      await expect(
        f.page.getByText("FIRST-RESULT", { exact: false }),
      ).toBeVisible();
      await expect(
        f.page.getByRole("button", { name: "Load older", exact: true }),
      ).toBeVisible();
      const detailReads = () =>
        reads.filter((input) =>
          ["task", "definitionHistory", "result", "history"].includes(
            input.view,
          ),
        );
      const initial = detailReads().map((input) => JSON.stringify(input)),
        refreshes = reads.filter((input) => input.view === "tasks").length;
      await expect
        .poll(() => reads.filter((input) => input.view === "tasks").length, {
          timeout: 16000,
        })
        .toBeGreaterThanOrEqual(refreshes + 2);
      assert.deepEqual(
        detailReads().map((input) => JSON.stringify(input)),
        initial,
        "unchanged list refreshes do not reload definition, result or history",
      );
      const beforePage = reads.length;
      await f.page
        .getByRole("button", { name: "Load older", exact: true })
        .click();
      await expect(
        f.page.getByRole("button", { name: "Load older", exact: true }),
      ).toHaveCount(0);
      const pageReads = reads
        .slice(beforePage)
        .filter((input) => input.view !== "tasks");
      assert.equal(pageReads.length, 1);
      assert.equal(pageReads[0].view, "history");
      assert.ok(pageReads[0].cursor);
      assert.equal(
        (await f.page.locator("body").innerText()).includes("END-SEEN"),
        false,
      );
      await f.page
        .getByRole("button", { name: "Show full value", exact: true })
        .click();
      await expect(
        f.page.getByText('"' + long + '"', { exact: false }),
      ).toBeVisible();
      await f.page
        .getByRole("button", { name: "Enlarge Camera pixel", exact: true })
        .click();
      await expect(
        f.page
          .getByRole("dialog")
          .getByRole("img", { name: "Camera pixel", exact: true }),
      ).toBeVisible();
      await expect(
        f.page
          .getByRole("dialog")
          .getByRole("img", { name: "Camera pixel", exact: true }),
      ).toHaveJSProperty("naturalWidth", 1);
      await f.page.keyboard.press("Escape");
      await expect(f.page.getByRole("dialog")).toBeHidden();
      assert.ok(
        await f.page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth + 2,
        ),
      );
      let release,
        observed = false,
        delivered;
      const blocked = new Promise((yes) => {
        release = yes;
      });
      const completed = new Promise((yes) => {
        delivered = yes;
      });
      t.after(() => release());
      await f.page.route("**/api/v1/rpc", async (route) => {
        if (
          !(calls(route.request()) ?? []).some(
            (item) =>
              item.params.arguments.view === "result" &&
              item.params.arguments.id === "details" &&
              item.params.arguments.revision === 1,
          )
        )
          return route.continue();
        const response = await route.fetch();
        observed = true;
        await blocked;
        await route.fulfill({ response });
        delivered();
      });
      try {
        await f.page
          .getByRole("combobox", { name: "Result revision", exact: true })
          .click();
        await f.page.getByRole("option", { name: /^#1\s*·/ }).click();
        await expect.poll(() => observed, { timeout: 10000 }).toBe(true);
        if (viewport.width < 768)
          await f.page
            .getByRole("button", { name: "Toggle navigation", exact: true })
            .first()
            .click();
        await f.page
          .getByRole("link", { name: /Other collection/ })
          .first()
          .click();
        await expect(
          f.page.getByText("SECOND-RESULT", { exact: false }),
        ).toBeVisible();
      } finally {
        release();
      }
      await completed;
      await expect(
        f.page.getByText("FIRST-RESULT", { exact: false }),
      ).toHaveCount(0);
      await expect(
        f.page.getByText("SECOND-RESULT", { exact: false }),
      ).toBeVisible();
      assert.deepEqual(f.pageErrors, []);
      assert.deepEqual(f.externalRequests, []);
    },
  );
