import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { expect } from "@playwright/test";
import { fixture, fixtureToken } from "./fixtures/fixture.mjs";
import { newOperationId } from "../../dist/packages/sdk/src/client.js";
import { choose } from "./fixtures/select.mjs";

test(
  "Console lists installed apps and inspects offline dependencies on demand while launch remains guarded",
  { timeout: 60000 },
  async (t) => {
    const f = await fixture(t, true);
    const ui = await f.client.request("uis.get", { uiId: "fixture" });
    const asset = ui.releases[1].assets[0];
    const source = await fetch(f.base + "/ui/fixture/releases/r2/index.html",
      { headers: { Authorization: "Bearer " + fixtureToken } });
    assert.equal(source.status, 200);
    await f.client.request("uis.stageAsset", {
      uiId: "fixture", releaseId: "r3", asset,
      base64: Buffer.from(await source.arrayBuffer()).toString("base64"),
      mutationId: await newOperationId(f.client),
    });
    await f.client.request("uis.deploy", {
      mutationId: await newOperationId(f.client),
      metadata: ui.metadata,
      expectedReleaseId: "r2",
      release: {
        ...ui.releases[1],
        releaseId: "r3",
        requirements: {
          hiveProtocol: 1,
          contracts: [],
          services: [
            {
              serviceName: "browser-fixture",
              namespace: "fixture",
              interfaceVersion: "1.0.0",
            },
          ],
        },
      },
    });
    await f.login();
    await expect(
      f.page.getByRole("link", { name: "Open Isolated test ui", exact: true }),
    ).toHaveAttribute("href", /\/ui\/fixture\/$/);
    await f.service.stop();
    await expect
      .poll(
        async () =>
          (await f.client.request("uis.inspect", { uiId: "fixture" })).status,
      )
      .toBe("unavailable");
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(
      f.page.getByRole("link", { name: "Open Isolated test ui", exact: true }),
    ).toHaveAttribute("href", /\/ui\/fixture\/$/);
    await f.page
      .getByRole("navigation", { name: "Page navigation" })
      .getByRole("link", { name: "System", exact: true })
      .click();
    await f.page
      .getByRole("navigation", { name: "Page navigation" })
      .getByRole("link", { name: "Releases", exact: true })
      .click();
    await f.page
      .getByRole("button", { name: "Manage releases", exact: true })
      .click();
    const dialog = f.page.getByRole("dialog");
    await expect(
      dialog.getByText(
        "No compatible provider currently has an eligible catalog.",
        { exact: true },
      ),
    ).toBeVisible();
    await expect(
      dialog.getByText("service_not_ready · browser-fixture · fixture", {
        exact: true,
      }),
    ).toBeVisible();
    // Use the real browser's authenticated fetch; Node's API request context does not send Secure
    // cookies to HTTP loopback even though this browser treats that origin as potentially trustworthy.
    const stable = await f.page.evaluate(
      async (url) =>
        (await fetch(url, { credentials: "same-origin", redirect: "manual" }))
          .status,
      f.base + "/ui/fixture/",
    );
    assert.equal(stable, 503);
    const old = await f.page.evaluate(async (url) => {
      const response = await fetch(url, { credentials: "same-origin" });
      return { status: response.status, text: await response.text() };
    }, f.base + "/ui/fixture/releases/r2/index.html");
    assert.equal(old.status, 200);
    assert.match(old.text, /Isolated ui r2/);
    await choose(dialog.getByLabel("Retained release"), "r2");
    await expect(
      dialog.getByRole("button", { name: "Select this release", exact: true }),
    ).toBeEnabled();
    await dialog
      .getByRole("button", { name: "Select this release", exact: true })
      .click();
    await expect(
      dialog.getByText("Selected release r2.", { exact: true }),
    ).toBeVisible();
    assert.equal(
      (await f.client.request("uis.inspect", { uiId: "fixture" })).status,
      "ready",
    );
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "lost rollback response survives reload with the original mutation and never automatically repeats",
  { timeout: 60000 },
  async (t) => {
    const f = await fixture(t, true);
    await f.login();
    const calls = [];
    f.page.on("request", (request) => {
      if (request.method() === "POST" && request.url().endsWith("/rpc")) {
        const body = request.postDataJSON();
        if (body.method === "uis.rollback") calls.push(body.params);
      }
    });
    await f.page.route("**/api/v1/rpc", async (route) => {
      if (route.request().postDataJSON().method !== "uis.rollback")
        return route.continue();
      const response = await route.fetch();
      assert.equal(response.status(), 200);
      await route.abort();
    });
    await f.page
      .getByRole("navigation", { name: "Page navigation" })
      .getByRole("link", { name: "System", exact: true })
      .click();
    await f.page
      .getByRole("navigation", { name: "Page navigation" })
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
      f.page.getByRole("button", {
        name: "Retry original selection",
        exact: true,
      }),
    ).toBeEnabled();
    assert.equal(
      (await f.client.request("uis.get", { uiId: "fixture" })).currentReleaseId,
      "r1",
    );
    assert.equal(calls.length, 1);
    await f.page.unroute("**/api/v1/rpc");
    await f.page.reload();
    await f.page
      .getByRole("button", { name: "Manage releases", exact: true })
      .click();
    await expect(
      f.page.getByRole("button", {
        name: "Retry original selection",
        exact: true,
      }),
    ).toBeEnabled();
    assert.equal(calls.length, 1);
    await expect(f.page.getByLabel("Retained release")).toHaveAttribute(
      "data-value",
      "r1",
    );
    await f.page
      .getByRole("button", { name: "Retry original selection", exact: true })
      .click();
    await expect(
      f.page.getByText("Selected release r1.", { exact: true }),
    ).toBeVisible();
    assert.equal(calls.length, 2);
    assert.deepEqual(calls[1], calls[0]);
    assert.equal(
      await f.page.evaluate(
        () =>
          Object.keys(sessionStorage).filter((key) =>
            key.startsWith("ivy.console.pendingRollback:"),
          ).length,
      ),
      0,
    );
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "Object Browser keeps mixed historical versions exact, marks inherited archives and exports binary bytes",
  { timeout: 60000 },
  async (t) => {
    const f = await fixture(t);
    for (const version of ["1.0.0", "2.0.0"])
      await f.client.request("contracts.register", {
        mutationId: await newOperationId(f.client),
        definition: {
          key: "fixture/history",
          version,
          owner: { kind: "agent" },
          mediaType: "application/json",
          specMarkdown: "Mixed-version browser fixture.",
          retention: {
            objects: { mode: "retain" },
            revisions: { mode: "all" },
          },
          jsonSchema: {
            type: "object",
            properties: {
              title: { type: "string" },
              ...(version === "2.0.0" ? { result: { type: "integer" } } : {}),
            },
            required: version === "2.0.0" ? ["title", "result"] : ["title"],
            additionalProperties: false,
          },
        },
      });
    const create = async (name, parentId = null) =>
      f.client.request("objects.write", {
        mutationId: await newOperationId(f.client),
        contractVersion: "1.0.0",
        references: {},
        create: {
          contractKey: "fixture/history",
          name,
          parentId,
          ownerObjectId: null,
        },
        content: { encoding: "json", value: { title: "Original V1" } },
      });
    const parent = await create("Archived parent"),
      child = await create("Mixed child", parent.object.id);
    await f.client.request("objects.write", {
      mutationId: await newOperationId(f.client),
      objectId: child.object.id,
      expectedRevision: 1,
      contractVersion: "2.0.0",
      references: {},
      content: { encoding: "json", value: { title: "Current V2", result: 7 } },
    });
    await f.client.request("objects.archive", {
      mutationId: await newOperationId(f.client),
      objectId: parent.object.id,
      archived: true,
    });
    await f.client.request("contracts.register", {
      mutationId: await newOperationId(f.client),
      definition: {
        key: "fixture/binary",
        version: "1.0.0",
        owner: { kind: "agent" },
        mediaType: "application/octet-stream",
        retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
        specMarkdown: "Binary browser fixture.",
      },
    });
    const bytes = Buffer.alloc(262145);
    for (let index = 0; index < bytes.length; index++)
      bytes[index] = index % 256;
    const binary = await f.client.request("objects.write", {
      mutationId: await newOperationId(f.client),
      contractVersion: "1.0.0",
      references: {},
      create: {
        contractKey: "fixture/binary",
        name: "Exact binary",
        parentId: null,
        ownerObjectId: null,
      },
      content: { encoding: "base64", value: bytes.toString("base64") },
    });
    await f.login();
    await f.page.goto(
      f.base +
        "/#/system/objects?contract=fixture%2Fhistory&version=2.0.0&archived=yes",
    );
    const row = f.page
      .getByRole("region", { name: "Object list", exact: true })
      .getByRole("listitem")
      .filter({ has: f.page.getByRole("link", { name: /^Mixed child/ }) });
    await expect(row.getByText("Archived", { exact: true })).toBeVisible();
    await row.getByRole("link", { name: /^Mixed child/ }).click();
    await f.page
      .getByRole("navigation", { name: "Inspector views" })
      .getByRole("link", { name: "Metadata", exact: true })
      .click();
    await expect(
      f.page
        .getByRole("region", { name: "Identity and revision metadata" })
        .getByText("2.0.0", { exact: true }),
    ).toBeVisible();
    await f.page
      .getByRole("navigation", { name: "Inspector views" })
      .getByRole("link", { name: "History", exact: true })
      .click();
    await f.page.getByRole("link", { name: /^Revision 1 / }).click();
    await f.page.reload();
    await expect(
      f.page.getByText("Revision 1 · Historical", { exact: true }),
    ).toBeVisible();
    await expect(f.page.locator("main pre").first()).toContainText(
      "Original V1",
    );
    assert.equal(
      (await f.client.request("objects.stat", { objectId: child.object.id }))
        .currentRevision,
      2,
    );
    await f.page.goto(f.base + "/#/system/objects?id=" + binary.object.id);
    await expect(
      f.page.getByText(
        "(Binary content; download this exact revision to inspect it.)",
        { exact: true },
      ),
    ).toBeVisible();
    await f.page
      .getByRole("navigation", { name: "Inspector views" })
      .getByRole("link", { name: "History", exact: true })
      .click();
    await expect(
      f.page.getByRole("button", { name: "Compare content", exact: true }),
    ).toBeDisabled();
    const downloading = f.page.waitForEvent("download");
    await f.page
      .getByRole("button", { name: "Download revision", exact: true })
      .click();
    const destination = resolve(f.root, "exact-binary.bin");
    await (await downloading).saveAs(destination);
    assert.deepEqual(await readFile(destination), bytes);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);
