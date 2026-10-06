import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { expect } from "@playwright/test";
import { fixture, fixtureToken } from "./fixtures/fixture.mjs";
import { startDashboards } from "../../dist/services/dashboards/src/main.js";
import { atomicJson } from "../../dist/packages/host-runtime/src/config.js";
import { publishUi } from "../../dist/packages/cli/src/publish-ui.js";
import { newOperationId } from "../../dist/packages/sdk/src/client.js";

test(
  "dashboard saves retain identity and revision when subsequent reads are unavailable",
  { timeout: 120000 },
  async (t) => {
    const f = await fixture(t);
    const build = JSON.parse(await readFile("dist/build-info.json", "utf8"));
    const config = {
      schemaVersion: 1,
      componentId: "dashboards",
      instanceId: "dashboards",
      serviceNodeId: "browser-dashboards",
      hostId: "browser-host",
      publicBaseUrl: f.base + "/",
      artifactRoot: join(f.root, "artifact"),
      dataRoot: join(f.root, "dashboards"),
      buildId: build.buildId,
      version: build.version,
      credential: fixtureToken,
      settings: {},
    };
    await atomicJson(join(config.artifactRoot, "dist/build-info.json"), build);
    const path = join(f.root, "dashboards.json");
    await atomicJson(path, config);
    const service = await startDashboards(path);
    t.after(() => service.close());
    await service.service.waitReady();
    const definition = JSON.parse(
      await readFile("ui/dashboards-ui/ui.json", "utf8"),
    );
    await publishUi(f.client, {
      directory: resolve("dist/apps/dashboards-ui"),
      definition,
      mutationId: await newOperationId(f.client),
      expectedReleaseId: null,
    });
    await f.login();
    let liveSubscriptions = 0;
    f.page.on("websocket", (socket) => {
      if (!f.page.url().includes("/ui/dashboards-ui/")) return;
      socket.on("framereceived", ({ payload }) => {
        if (JSON.parse(String(payload)).result?.changes) liveSubscriptions++;
      });
    });
    await f.page.goto(f.base + "/ui/dashboards-ui/");
    await expect.poll(() => liveSubscriptions).toBeGreaterThan(0);
    const writes = [];
    await f.page.route("**/api/v1/rpc", (route) => {
      const request = route.request().postDataJSON();
      if (request.method === "tools.call") {
        if (request.params.qualifiedName === "dashboards.read")
          return route.fulfill({
            status: 503,
            body: "Read temporarily unavailable",
          });
        if (request.params.qualifiedName === "dashboards.save")
          writes.push(request.params.arguments);
      }
      return route.continue();
    });
    for (const width of [1440, 390]) {
      await f.page.setViewportSize({ width, height: 900 });
      await f.page.goto(f.base + "/ui/dashboards-ui/#/home");
      if (width < 768)
        await f.page
          .getByRole("button", { name: "Toggle navigation", exact: true })
          .click();
      await (
        width < 768
          ? f.page.getByRole("dialog")
          : f.page.locator("#ivy-navigation")
      )
        .getByRole("link", { name: /^(New dashboard|Neues Dashboard)$/ })
        .click();
      await f.page
        .getByLabel(/^(Title|Titel)$/, { exact: true })
        .fill("Dashboard " + width);
      await f.page
        .getByRole("tab", { name: /^(Metadata & PNG|Metadaten & PNG)$/ })
        .click();
      await f.page
        .getByLabel(/^(Metadata \(JSON\)|Metadaten \(JSON\))$/)
        .fill('{"display":"Touch ä"}');
      await f.page
        .getByRole("button", {
          name: /^(Enable PNG access|PNG-Zugriff aktivieren)$/,
        })
        .click();
      const save = f.page.getByRole("button", {
        name: /^(Save|Speichern)$/,
        exact: true,
      });
      await save.click();
      const open = f.page.getByRole("link", {
        name: /^(Open frameless|Rahmenlos öffnen)$/,
        exact: true,
      });
      await expect(open).toBeVisible();
      const url = await open.getAttribute("href");
      const id = new URLSearchParams(new URL(url).hash.split("?")[1]).get("id");
      assert.ok(id);
      await f.page.getByRole("tab", { name: /^(Metadata & PNG|Metadaten & PNG)$/ }).click();
      const imageUrl = await f.page
        .getByLabel(/^(PNG URL|PNG-URL)$/)
        .inputValue();
      const endpoint = new URL(imageUrl);
      endpoint.searchParams.set("width", "120");
      endpoint.searchParams.set("height", "100");
      endpoint.searchParams.set("colorMode", "grayscale");
      const anonymous = await fetch(endpoint);
      assert.equal(
        anonymous.status,
        200,
        "PNG access must not need a Hive session",
      );
      assert.equal(anonymous.headers.get("content-type"), "image/png");
      assert.equal(
        JSON.parse(
          decodeURIComponent(anonymous.headers.get("x-dashboard-metadata")),
        ).display,
        "Touch ä",
      );
      const bytes = Buffer.from(await anonymous.arrayBuffer());
      assert.equal(bytes[25], 0);
      const etag = anonymous.headers.get("etag");
      const rejected = new URL(endpoint);
      rejected.searchParams.set("token", "wrong");
      assert.equal((await fetch(rejected)).status, 401);
      rejected.searchParams.delete("token");
      assert.equal((await fetch(rejected)).status, 401);
      rejected.searchParams.set("token", endpoint.searchParams.get("token"));
      rejected.searchParams.set("id", "another-dashboard");
      assert.equal((await fetch(rejected)).status, 404);
      await expect(save).toBeEnabled();
      await f.page
        .getByLabel(/^(Title|Titel)$/, { exact: true })
        .fill("Updated " + width);
      await f.page
        .getByRole("tab", { name: /^(Refresh|Aktualisierung)$/ })
        .click();
      await f.page
        .getByLabel(/^(Refresh in seconds|Aktualisierung in Sekunden)$/)
        .fill("60");
      await save.click();
      await expect
        .poll(
          async () =>
            (await f.client.request("objects.read", { objectId: id })).revision
              .revision,
        )
        .toBe(2);
      await expect(save).toBeEnabled();
      assert.equal(writes.at(-1).id, id);
      assert.equal(writes.at(-1).expectedRevision, 1);
      await expect(open).toHaveAttribute("href", url);
      const unchanged = await fetch(endpoint, {
        headers: { "If-None-Match": etag },
      });
      assert.equal(unchanged.status, 304);
      assert.equal(unchanged.headers.get("x-dashboard-refresh-seconds"), "60");
      assert.equal(
        JSON.parse(
          decodeURIComponent(unchanged.headers.get("x-dashboard-metadata")),
        ).refreshSeconds,
        60,
      );
      await f.page
        .getByRole("tab", { name: /^(Metadata & PNG|Metadaten & PNG)$/ })
        .click();
      await f.page
        .getByRole("button", { name: /^(Rotate token|Token erneuern)$/ })
        .click();
      await save.click();
      await expect
        .poll(
          async () =>
            (await f.client.request("objects.read", { objectId: id })).revision
              .revision,
        )
        .toBe(3);
      assert.equal((await fetch(endpoint)).status, 401);
      const rotated = new URL(
        await f.page.getByLabel(/^(PNG URL|PNG-URL)$/).inputValue(),
      );
      assert.equal((await fetch(rotated, { method: "HEAD" })).status, 200);
      assert.equal((await fetch(rotated, { method: "POST" })).status, 405);
      await f.page
        .getByRole("button", { name: /^(Disable|Deaktivieren)$/ })
        .click();
      await save.click();
      await expect
        .poll(
          async () =>
            (await f.client.request("objects.read", { objectId: id })).revision
              .revision,
        )
        .toBe(4);
      assert.equal((await fetch(rotated)).status, 401);
      await expect(f.page.getByRole("alert")).toHaveCount(0);
      await f.page
        .getByRole("button", { name: /^(Close|Schließen)$/, exact: true })
        .click();
      await expect(f.page).toHaveURL(new RegExp("#/dashboard\\?id=" + id));
      await expect(
        f.page.getByRole("heading", { name: "Updated " + width, exact: true }),
      ).toBeVisible();
    }
    assert.equal(
      (
        await f.client.request("objects.query", {
          contractKey: "dashboards/dashboard",
        })
      ).items.length,
      2,
    );
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);
