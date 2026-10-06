import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { expect } from "@playwright/test";
import { fixture, fixtureToken } from "./fixtures/fixture.mjs";
import { newOperationId } from "../../dist/packages/sdk/src/client.js";
import { choose } from "./fixtures/select.mjs";
import { digest } from "../../dist/packages/contracts/src/canonical.js";

const navigation = (page) =>
  page.getByRole("navigation", { name: "Page navigation" });

test('UI launcher and rail share priority order and settings stay at the top right on desktop and mobile', { timeout: 60000 }, async t => {
  const f = await fixture(t);
  const ids = ['data-collector-ui', 'secretary-ui', 'dashboards-ui', 'agent-ui', 'task-board-ui', 'wiki-ui'];
  const bytes = Buffer.from('<!doctype html><h1>Navigation fixture</h1>');
  const asset = { path: 'index.html', mediaType: 'text/html', contentHash: digest(bytes), byteLength: bytes.length };
  for (const uiId of ids) {
    const { metadata } = JSON.parse(await readFile(`ui/${uiId}/ui.json`, 'utf8'));
    await f.client.request('uis.stageAsset', { uiId, releaseId: 'r1', asset, base64: bytes.toString('base64'), mutationId: await newOperationId(f.client) });
    await f.client.request('uis.deploy', { metadata, expectedReleaseId: null, mutationId: await newOperationId(f.client), release: { releaseId: 'r1', entryPath: 'index.html', assets: [asset], requirements: { hiveProtocol: 1, contracts: [], services: [] } } });
  }
  await f.login();
  const names = ['Wiki', 'TaskBoard', 'Agents', 'Dashboards', 'Secretary', 'DataCollector'];
  await expect(f.page.locator('a[aria-label^="Open "]')).toHaveCount(6);
  assert.deepEqual(await f.page.locator('a[aria-label^="Open "]').evaluateAll(items => items.map(item => item.getAttribute('aria-label'))), names.map(name => 'Open ' + name));
  await expect(f.page.getByRole('link', { name: 'Open Wiki', exact: true })).toHaveAttribute('href', f.base + '/wiki/');
  for (const width of [1440, 390]) {
    await f.page.setViewportSize({ width, height: 900 });
    const settings = f.page.locator('.ivy-toolbar').getByRole('link', { name: 'Settings', exact: true });
    await expect(settings).toBeVisible();
    const bounds = await settings.boundingBox();
    assert.ok(bounds.y < 48 && bounds.x + bounds.width > width - 60);
    await expect(f.page.getByRole('button', { name: 'Refresh', exact: true })).toHaveCount(0);
    if (width === 390) await f.page.getByRole('button', { name: 'Toggle navigation', exact: true }).click();
    const rail = f.page.locator('.ivy-rail:visible');
    await expect(rail.getByRole('link', { name: 'Wiki', exact: true })).toBeVisible();
    assert.deepEqual(await rail.locator('a[aria-label]').evaluateAll(items => items.map(item => item.getAttribute('aria-label')).filter(name => name !== 'Ivy home' && name !== 'System')), names);
    await expect(rail.getByRole('link', { name: 'Settings', exact: true })).toHaveCount(0);
    if (width === 390) await f.page.keyboard.press('Escape');
    assert.ok(await f.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
  }
  await f.page.locator('.ivy-toolbar').getByRole('link', { name: 'Settings', exact: true }).click();
  await expect(f.page.getByRole('heading', { name: 'Settings', exact: true })).toBeVisible();
  assert.deepEqual(f.pageErrors, []);
});

test('MCP direct tools show complete schemas at desktop and mobile widths', { timeout: 90000 }, async t => {
  const f = await fixture(t);
  await f.login();
  await f.page.goto(f.base + '/#/system/mcp');
  for (const viewport of [{ width: 1440, height: 1000 }, { width: 390, height: 844 }]) {
    await f.page.setViewportSize(viewport);
    const tools = f.page.locator('section[aria-labelledby="direct-heading"]');
    await expect(tools.getByRole('heading', { name: 'Direct tools' })).toBeVisible({ timeout: 20000 });
    const pageRead = tools.locator('[data-tool="wiki_read"]');
    await expect(pageRead).toBeVisible();
    if ((await pageRead.getAttribute('data-state')) !== 'open') await pageRead.locator(':scope > [data-slot="collapsible-trigger"]').click();
    await expect(pageRead.getByText('inputSchema', { exact: true })).toBeVisible();
    assert.ok(await f.page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1));
    await expect(f.page.getByRole('alert')).toHaveCount(0);
  }
  await f.page.getByRole('textbox', { name: 'Filter tools' }).fill('wiki_read');
  await expect(f.page.locator('[data-tool]')).toHaveCount(1);
  await expect(f.page.locator('[data-tool="wiki_read"]')).toBeVisible();
  assert.deepEqual(f.pageErrors, []);
});
test(
  "fresh Console loads every HTTP view, preserves exact form Origin and logs out without an ui or provider",
  { timeout: 90000 },
  async (t) => {
    const f = await fixture(t);
    await f.login();
    await expect(
      f.page.getByText("Your workspace starts here", { exact: true }),
    ).toBeVisible();
    const cookie = (await f.context.cookies()).find(
      (item) => item.name === "ivy_session",
    );
    assert.ok(cookie.httpOnly && cookie.secure);
    assert.equal(cookie.sameSite, "Lax");
    assert.equal(await f.page.evaluate(() => document.cookie), "");
    await navigation(f.page)
      .getByRole("link", { name: "System", exact: true })
      .click();
    for (const [label, heading] of [
      ["Hosts & services", "Hosts & services"],
      ["Definitions", "Definitions"],
      ["Deployments", "Deployments"],
      ["Problems", "Problems"],
      ["Object Browser", "Object Browser"],
      ["MCP Tools", "MCP Tools"],
    ]) {
      await navigation(f.page)
        .getByRole("link", { name: label, exact: true })
        .click();
      await expect(
        f.page.getByRole("heading", { name: heading, exact: true }),
      ).toBeVisible();
      await expect(f.page.getByRole("alert")).toHaveCount(0);
    }
    await expect(f.page.getByText("Protocol surface", { exact: true })).toBeVisible({ timeout: 20000 });
    const objectRead = f.page.locator('[data-service="hive"] [data-tool="hive_object_read"]');
    await expect(objectRead).toBeVisible();
    await objectRead.locator(':scope > [data-slot="collapsible-trigger"]').click();
    await expect(objectRead.getByText("inputSchema", { exact: true })).toBeVisible();
    const logout = f.page.waitForRequest(
      (request) =>
        request.method() === "POST" && request.url().endsWith("/logout"),
    );
    await f.page.getByRole("button", { name: "Account", exact: true }).click();
    await f.page.getByRole("menuitem", { name: "Sign out", exact: true }).click();
    assert.equal((await logout).headers()["origin"], f.origin);
    await expect(
      f.page.getByRole("heading", { name: "Sign in to Ivy" }),
    ).toBeVisible();
    assert.equal(
      (await f.context.cookies()).filter((item) => item.name === "ivy_session")
        .length,
      0,
    );
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "Console paginates Objects, reads exact history, compares revisions and exports verified bytes",
  { timeout: 60000 },
  async (t) => {
    const f = await fixture(t, true);
    await f.login();
    await navigation(f.page)
      .getByRole("link", { name: "System", exact: true })
      .click();
    await navigation(f.page)
      .getByRole("link", { name: "Object Browser", exact: true })
      .click();
    const rows = f.page
      .getByRole("region", { name: "Object list", exact: true })
      .locator("[role=listitem] > a:first-child");
    await expect(rows).toHaveCount(50);
    await expect(rows.first()).toContainText("Note 01");
    const firstPage = await rows.evaluateAll((links) =>
      links.map((link) => link.getAttribute("href")),
    );
    await f.page.getByRole("button", { name: "Next", exact: true }).click();
    await expect(
      f.page.getByText("6 items · Page 2", { exact: true }),
    ).toBeVisible();
    await f.page.getByRole("button", { name: "Previous", exact: true }).click();
    await expect(rows).toHaveCount(50);
    assert.deepEqual(
      await rows.evaluateAll((links) =>
        links.map((link) => link.getAttribute("href")),
      ),
      firstPage,
    );
    await f.page
      .getByRole("button", { name: "Open Object", exact: true })
      .click();
    await f.page
      .getByLabel("Object ID or absolute path", { exact: true })
      .fill(f.firstObjectId);
    await f.page.getByRole("button", { name: "Open", exact: true }).click();
    await expect(
      f.page.getByText("Revised saved result.", { exact: true }),
    ).toBeVisible();
    await f.page
      .getByRole("navigation", { name: "Inspector views" })
      .getByRole("link", { name: "History", exact: true })
      .click();
    await f.page.getByRole("link", { name: /^Revision 1 / }).click();
    await expect(
      f.page.getByText('<script>alert("unsafe")</script>', { exact: true }),
    ).toBeVisible();
    assert.equal(
      await f.page.locator("main img, main iframe, main script").count(),
      0,
    );
    const savedUrl = f.page.url();
    await f.page.reload();
    await expect(
      f.page.getByText("Original saved text.", { exact: true }),
    ).toBeVisible();
    assert.equal(f.page.url(), savedUrl);
    await f.page
      .getByRole("navigation", { name: "Inspector views" })
      .getByRole("link", { name: "History", exact: true })
      .click();
    await f.page.getByLabel("Compare with revision").fill("2");
    await f.page
      .getByRole("button", { name: "Compare content", exact: true })
      .click();
    await expect(
      f.page.getByRole("heading", {
        name: "Revision 2 · Removed span",
        exact: true,
      }),
    ).toBeVisible();
    await expect(
      f.page.getByRole("heading", {
        name: "Revision 1 · Added span",
        exact: true,
      }),
    ).toBeVisible();
    const download = f.page.waitForEvent("download");
    await f.page
      .getByRole("button", { name: "Download revision", exact: true })
      .click();
    const destination = resolve(f.root, "downloaded-revision.txt");
    await (await download).saveAs(destination);
    assert.equal(await readFile(destination, "utf8"), f.firstContent);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "Console inspects actual SDK definitions and changes a retained ui pointer through the real API",
  { timeout: 60000 },
  async (t) => {
    const f = await fixture(t, true);
    await f.login();
    await navigation(f.page)
      .getByRole("link", { name: "System", exact: true })
      .click();
    await navigation(f.page)
      .getByRole("link", { name: "Hosts & services", exact: true })
      .click();
    await expect(
      f.page.getByText("browser-fixture-service", { exact: true }),
    ).toBeVisible();
    await navigation(f.page)
      .getByRole("link", { name: "Definitions", exact: true })
      .click();
    await f.page.getByRole("button", { name: "Inspect", exact: true }).click();
    await f.page
      .locator('[data-slot="collapsible-trigger"]')
      .filter({ hasText: "fixture.read" })
      .click();
    await expect(
      f.page.getByText("Read a constant fixture marker.", { exact: true }),
    ).toBeVisible();
    const supported = f.page.getByRole("region", {
      name: "Supported Data Contracts on this node",
    });
    await expect(
      supported.getByRole("heading", { name: "fixture/markdown", exact: true }),
    ).toBeVisible();
    await expect(
      supported.getByText("Reads: 1.0.0", { exact: true }),
    ).toBeVisible();
    await expect(
      supported.getByText("Writes: None declared", { exact: true }),
    ).toBeVisible();
    await navigation(f.page)
      .getByRole("link", { name: "Problems", exact: true })
      .click();
    const resolvedDiagnostic = f.page.getByRole("heading", {
      name: "browser_fixture_observation",
      exact: true,
    });
    await expect(
      f.page.getByText("No matching diagnostics", { exact: true }),
    ).toBeVisible();
    await expect(resolvedDiagnostic).toHaveCount(0);
    await choose(f.page.getByLabel("Observation state"), "resolved");
    await expect(resolvedDiagnostic).toBeVisible();
    await navigation(f.page)
      .getByRole("link", { name: "Releases", exact: true })
      .click();
    await f.page
      .getByRole("button", { name: "Manage releases", exact: true })
      .click();
    await choose(f.page.getByLabel("Retained release"), "r1");
    await f.page
      .getByRole("button", { name: "Select this release", exact: true })
      .click();
    await expect(
      f.page.getByText("Selected release r1.", { exact: true }),
    ).toBeVisible();
    assert.equal(
      (await f.client.request("uis.get", { uiId: "fixture" })).currentReleaseId,
      "r1",
    );
    await f.page
      .getByRole("button", { name: "Close", exact: true })
      .first()
      .click();
    await f.page.getByRole("link", { name: "Ivy home", exact: true }).click();
    await f.page
      .getByRole("link", { name: "Open Isolated test ui", exact: true })
      .click();
    await expect(
      f.page.getByRole("heading", { name: "Isolated ui r1", exact: true }),
    ).toBeVisible();
    assert.match(f.page.url(), /\/ui\/fixture\/$/);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "Console retains last data during a failed refresh and recovers without a queued mutation",
  { timeout: 60000 },
  async (t) => {
    const f = await fixture(t, true);
    await f.login();
    await expect(
      f.page.getByRole("heading", { name: "Isolated test ui", exact: true }),
    ).toBeVisible();
    const mutationCalls = [];
    f.page.on("request", (request) => {
      if (request.method() !== "POST" || !request.url().endsWith("/rpc"))
        return;
      const body = request.postDataJSON();
      if (body.method === "uis.rollback") mutationCalls.push(body);
    });
    await f.page.route("**/api/v1/rpc", (route) =>
      route.request().postDataJSON().method === "uis.catalog"
        ? route.abort()
        : route.continue(),
    );
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(f.page.getByRole("alert")).toContainText(
      "Showing the last successful response",
    );
    await expect(
      f.page.getByRole("heading", { name: "Isolated test ui", exact: true }),
    ).toBeVisible();
    await f.page.unroute("**/api/v1/rpc");
    await f.page.getByRole("button", { name: "Retry", exact: true }).click();
    await expect(f.page.getByRole("alert")).toHaveCount(0);
    assert.deepEqual(mutationCalls, []);
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "Console preserves a safe configuration draft through refresh, conflict recovery and guarded navigation",
  { timeout: 60000 },
  async (t) => {
    const f = await fixture(t);
    const hostId = "browser-settings-host";
    const configuration = {
      schemaVersion: 1,
      hostId,
      runtimeRoot: resolve(f.root, "runtime"),
      artifactRoot: resolve(f.root, "artifacts"),
      stagingRoot: resolve(f.root, "staging"),
      publicBaseUrl: f.base,
      executables: { node: process.execPath },
      instances: [
        {
          instanceId: "worker",
          serviceNodeId: "settings-worker",
          componentId: "fixture-worker",
          enabled: false,
          engine: "process",
          credential: "original-instance-secret",
          secretPaths: ["/settings/apiToken"],
          settings: { value: 1, apiToken: "original-api-secret" },
        },
      ],
    };
    const created = await f.client.request("hostConfigurations.put", {
      hostId,
      configuration,
      mutationId: await newOperationId(f.client),
    });
    await f.login();
    await navigation(f.page)
      .getByRole("link", { name: "System", exact: true })
      .click();
    await f.page.getByRole("link", { name: "Settings", exact: true }).click();
    await f.page.getByRole("button", { name: "Edit", exact: true }).click();
    const editor = f.page.getByLabel("Complete HostConfig JSON", {
      exact: true,
    });
    await expect(editor).toHaveValue(/"schemaVersion": 1/);
    const initial = JSON.parse(await editor.inputValue());
    assert.deepEqual(initial.instances[0].credential, {
      $ivySecret: { instanceId: "worker", path: "/credential" },
    });
    assert.deepEqual(initial.instances[0].settings.apiToken, {
      $ivySecret: { instanceId: "worker", path: "/settings/apiToken" },
    });
    initial.instances[0].settings.value = 2;
    initial.instances[0].credential = {
      $ivySecret: { instanceId: "worker", path: "/credential" },
      value: "smuggled-instance-secret",
    };
    initial.instances[0].settings.apiToken = "replacement-api-secret";
    await editor.fill(JSON.stringify(initial, null, 2));
    await expect
      .poll(() =>
        f.page.evaluate(() =>
          sessionStorage.getItem("ivy.console.host-configuration-draft.v1"),
        ),
      )
      .not.toContain("smuggled-instance-secret");
    assert.equal(
      (
        await f.page.evaluate(() =>
          sessionStorage.getItem("ivy.console.host-configuration-draft.v1"),
        )
      ).includes("replacement-api-secret"),
      false,
    );

    f.page.once("dialog", (dialog) => dialog.accept());
    await f.page.reload();
    await expect(
      f.page.getByText(
        "Draft restored after refresh. Newly entered secrets were not stored and must be entered again.",
        { exact: true },
      ),
    ).toBeVisible();
    const restored = JSON.parse(await editor.inputValue());
    assert.equal(restored.instances[0].settings.value, 2);
    assert.equal("credential" in restored.instances[0], false);
    assert.equal("apiToken" in restored.instances[0].settings, false);
    restored.instances[0].credential = "replacement-instance-secret";
    restored.instances[0].settings.apiToken = "replacement-api-secret";
    await editor.fill(JSON.stringify(restored, null, 2));

    const external = structuredClone(configuration);
    external.instances[0].settings.value = 99;
    await f.client.request("hostConfigurations.put", {
      hostId,
      expectedRevision: created.revision,
      configuration: external,
      mutationId: await newOperationId(f.client),
    });
    await f.page
      .getByRole("button", { name: "Save new revision", exact: true })
      .click();
    await expect(
      f.page.getByText(
        "The configuration changed before this save. Your draft is preserved; reload the current revision, then explicitly reapply it.",
        { exact: true },
      ),
    ).toBeVisible();
    await f.page
      .getByRole("button", {
        name: "Reload current and keep draft",
        exact: true,
      })
      .click();
    await expect(editor).toHaveValue(/"value": 99/);
    await f.page.reload();
    await expect(
      f.page.getByRole("button", { name: "Reapply prior draft", exact: true }),
    ).toBeVisible();
    await f.page
      .getByRole("button", { name: "Reapply prior draft", exact: true })
      .click();
    const reapplied = JSON.parse(await editor.inputValue());
    assert.equal(reapplied.instances[0].settings.value, 2);
    assert.equal("credential" in reapplied.instances[0], false);
    assert.equal("apiToken" in reapplied.instances[0].settings, false);
    reapplied.instances[0].credential = "replacement-instance-secret";
    reapplied.instances[0].settings.apiToken = "replacement-api-secret";
    await editor.fill(JSON.stringify(reapplied, null, 2));
    await f.page
      .getByRole("button", { name: "Save new revision", exact: true })
      .click();
    await expect(
      f.page.getByText("Saved revision 3.", { exact: true }),
    ).toBeVisible();
    const retained = await f.client.request("hostConfigurations.get", {
      hostId,
    });
    assert.equal(
      retained.configuration.instances[0].credential,
      "replacement-instance-secret",
    );
    assert.equal(
      retained.configuration.instances[0].settings.apiToken,
      "replacement-api-secret",
    );
    assert.equal(retained.configuration.instances[0].settings.value, 2);

    const dirty = JSON.parse(await editor.inputValue());
    dirty.instances[0].settings.value = 3;
    await editor.fill(JSON.stringify(dirty, null, 2));
    f.page.once("dialog", (dialog) => dialog.dismiss());
    await navigation(f.page)
      .getByRole("link", { name: "Object Browser", exact: true })
      .click();
    await expect(
      f.page.getByRole("heading", { name: "Settings", exact: true }),
    ).toBeVisible();
    await expect(editor).toHaveValue(/"value": 3/);
    f.page.once("dialog", (dialog) => dialog.accept());
    await navigation(f.page)
      .getByRole("link", { name: "Object Browser", exact: true })
      .click();
    await expect(
      f.page.getByRole("heading", { name: "Object Browser", exact: true }),
    ).toBeVisible();
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "Console mobile navigation, keyboard skip link and theme preserve the current hash route",
  { timeout: 60000 },
  async (t) => {
    const f = await fixture(t, false, { width: 390, height: 844 });
    await f.login();
    await f.page.keyboard.press("Tab");
    await expect(
      f.page.getByRole("link", { name: "Skip to content", exact: true }),
    ).toBeFocused();
    const url = f.page.url();
    await f.page.keyboard.press("Enter");
    await expect(f.page.locator("#main-content")).toBeFocused();
    assert.equal(f.page.url(), url);
    await expect(navigation(f.page)).not.toBeVisible();
    await f.page
      .getByRole("button", { name: "Toggle navigation", exact: true })
      .click();
    await navigation(f.page)
      .getByRole("link", { name: "System", exact: true })
      .click();
    await f.page
      .getByRole("button", { name: "Toggle navigation", exact: true })
      .click();
    await navigation(f.page)
      .getByRole("link", { name: "Object Browser", exact: true })
      .click();
    await expect(
      f.page.getByRole("heading", { name: "Object Browser", exact: true }),
    ).toBeVisible();
    await expect(navigation(f.page)).not.toBeVisible();
    await f.page.locator('.ivy-toolbar').getByRole("link", { name: "Settings", exact: true }).click();
    await expect(
      f.page.getByRole("heading", { name: "Settings", exact: true }),
    ).toBeVisible();
    await expect(navigation(f.page)).not.toBeVisible();
    await f.page
      .getByRole("button", { name: "Toggle navigation", exact: true })
      .click();
    await navigation(f.page)
      .getByRole("link", { name: "Object Browser", exact: true })
      .click();
    await f.page
      .getByRole("button", { name: "Toggle navigation", exact: true })
      .click();
    await f.page.getByRole("button", { name: "Account", exact: true }).click();
    await f.page
      .getByRole("menuitem", { name: "Use dark theme", exact: true })
      .click();
    await expect(f.page.locator("html")).toHaveClass("dark");
    await f.page.reload();
    await expect(f.page.locator("html")).toHaveClass("dark");
    await expect(
      f.page.getByRole("heading", { name: "Object Browser", exact: true }),
    ).toBeVisible();
    assert.deepEqual(await f.page.evaluate(() => Object.keys(localStorage)), [
      "ivy.theme",
    ]);
    assert.ok(
      await f.page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    );
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "external UI links retain their route through login and recover an existing Strict session",
  { timeout: 60000 },
  async (t) => {
    const f = await fixture(t, true);
    const hash = "#/tasks?id=task-one&node=HOST-C.task-board";
    const target = f.base + "/ui/fixture/" + hash;
    const external = "http://external.test/";
    await f.page.route(external, route => route.fulfill({ contentType: "text/html",
      body: '<!doctype html><a href="' + target.replaceAll("&", "&amp;") + '">Open task link</a>' }));
    const openExternally = async (status, sendsCookie) => {
      await f.page.goto(external);
      const first = f.page.waitForResponse(response =>
        response.url() === f.base + "/ui/fixture/" && response.request().isNavigationRequest(),
      );
      await f.page.getByRole("link", { name: "Open task link" }).click();
      const response = await first;
      const headers = await response.request().allHeaders();
      assert.equal(Boolean(headers["cookie"]), sendsCookie);
      assert.equal(response.status(), status);
    };
    await openExternally(303, false);
    await expect(f.page.getByRole("heading", { name: "Sign in to Ivy" })).toBeVisible();
    assert.equal(new URL(f.page.url()).hash, hash);
    assert.equal(await f.page.locator('input[name="returnTo"]').inputValue(), "/ivy/ui/fixture/" + hash);
    await f.page.getByLabel("Hive credential").fill(fixtureToken);
    await f.page.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(f.page.getByRole("heading", { name: "Isolated ui r2" })).toBeVisible();
    assert.equal(new URL(f.page.url()).hash, hash);
    const cookie = (await f.context.cookies()).find(item => item.name === "ivy_session");
    assert.equal(cookie?.sameSite, "Lax");
    await f.context.addCookies([{ name: cookie.name, value: cookie.value, domain: cookie.domain,
      path: cookie.path, expires: cookie.expires, httpOnly: true, secure: true, sameSite: "Strict" }]);
    await openExternally(303, false);
    await expect(f.page.getByRole("heading", { name: "Isolated ui r2" })).toBeVisible();
    assert.equal(new URL(f.page.url()).hash, hash);
    assert.equal((await f.context.cookies()).find(item => item.name === "ivy_session")?.sameSite, "Lax");
    await openExternally(200, true);
    await expect(f.page.getByRole("heading", { name: "Isolated ui r2" })).toBeVisible();
    assert.equal(new URL(f.page.url()).hash, hash);
  },
);

test(
  "login uses the Ivy theme, retains the destination and recovers from an invalid credential",
  { timeout: 60000 },
  async (t) => {
    const f = await fixture(t);
    const target = "/ivy/#/system/objects";
    await f.page.goto(f.base + "/login?returnTo=" + encodeURIComponent(target));
    await expect(
      f.page.getByRole("heading", { name: "Sign in to Ivy" }),
    ).toBeVisible();
    await expect(f.page.locator("header svg.logo")).toBeVisible();
    await expect(f.page.getByRole("alert")).toHaveCount(0);
    await f.page.getByRole("button", { name: "Use dark theme" }).click();
    await f.page.reload();
    await expect(f.page.locator("html")).toHaveAttribute("data-theme", "dark");
    await f.page.setViewportSize({ width: 390, height: 844 });
    await f.page
      .getByLabel("Hive credential")
      .fill("incorrect-test-credential");
    const rejected = f.page.waitForResponse(
      (response) =>
        response.url().endsWith("/login") &&
        response.request().method() === "POST",
    );
    await f.page.getByRole("button", { name: "Sign in", exact: true }).click();
    assert.equal((await rejected).status(), 401);
    await expect(f.page.getByRole("alert")).toContainText(
      "That credential was not recognized",
    );
    await expect(f.page.getByLabel("Hive credential")).toHaveValue("");
    assert.equal(
      await f.page.locator('input[name="returnTo"]').inputValue(),
      target,
    );
    assert.ok(
      await f.page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    );
    await f.page.getByLabel("Hive credential").fill(fixtureToken);
    await f.page.getByRole("button", { name: "Sign in", exact: true }).click();
    await expect(
      f.page.getByRole("heading", { name: "Object Browser", exact: true }),
    ).toBeVisible();
    await expect(f.page.locator("html")).toHaveClass("dark");
    assert.equal(new URL(f.page.url()).hash, "#/system/objects");
    assert.deepEqual(await f.page.evaluate(() => Object.keys(localStorage)), [
      "ivy.theme",
    ]);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);
