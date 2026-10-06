import test from "node:test";
import assert from "node:assert/strict";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { expect } from "@playwright/test";
import { fixture, fixtureToken } from "./fixtures/fixture.mjs";
import { startDataCollector } from "../../dist/services/data-collector/src/main.js";
import { atomicJson } from "../../dist/packages/host-runtime/src/config.js";
import { publishUi } from "../../dist/packages/cli/src/publish-ui.js";
import { newOperationId } from "../../dist/packages/sdk/src/client.js";

for (const viewport of [
  { width: 1440, height: 1000 },
  { width: 390, height: 844 },
])
  test(
    `DataCollector edits a script, runs it and views retained results at ${viewport.width}px`,
    { timeout: 180000 },
    async (t) => {
      const f = await fixture(t, false, viewport),
        build = JSON.parse(await readFile("dist/build-info.json", "utf8"));
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
        artifactRoot: resolve("."),
        dataRoot,
        workRoot: join(f.root, "work"),
        buildId: build.buildId,
        version: build.version,
        credential: fixtureToken,
        settings: {},
      };
      const configPath = join(f.root, "collector.json");
      await atomicJson(configPath, config);
      const runtime = await startDataCollector(configPath);
      t.after(() => runtime.close());
      await runtime.service.waitReady();
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
      for (let n = 0; n < 2; n++) {
        const previousRunId =
          runtime.engine.store.get("browser-fixture").lastRunId;
        await f.page
          .getByRole("button", {
            name: label("Run now", "Jetzt ausführen"),
            exact: true,
          })
          .click();
        await expect
          .poll(
            async () => {
              const current = await runtime.engine.read({
                view: "task",
                id: "browser-fixture",
              });
              return {
                newRun: current.lastRunId !== previousRunId,
                status: current.status,
              };
            },
            {
              timeout: 30000,
            },
          )
          .toEqual({ newRun: true, status: "succeeded" });
        await expect(f.page.getByText("21.5", { exact: true })).toBeVisible();
      }
      const row = runtime.engine.store.get("browser-fixture");
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
      assert.ok(
        await f.page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth + 2,
        ),
        "Editor fits viewport",
      );
      await f.page
        .getByLabel("JavaScript (ESM)", { exact: true })
        .fill(
          "export default async () => { throw Object.assign(new Error('secret-fixture'), {code:'interaction_required'}); };",
        );
      await f.page
        .getByRole("button", { name: label("Save", "Speichern"), exact: true })
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
      assert.deepEqual(f.pageErrors, []);
      assert.deepEqual(f.externalRequests, []);
    },
  );
