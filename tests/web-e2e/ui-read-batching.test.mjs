import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { expect } from "@playwright/test";
import { publishUi } from "../../dist/packages/cli/src/publish-ui.js";
import { newOperationId } from "../../dist/packages/sdk/src/client.js";
import { ServiceClient } from "../../dist/packages/sdk/src/service.js";
import { digest } from "../../dist/packages/contracts/src/canonical.js";
import { fixture, fixtureToken } from "./fixtures/fixture.mjs";

test(
  "all browser apps share bounded read batches on desktop and mobile",
  { timeout: 120000 },
  async (t) => {
    const f = await fixture(t);
    const apps = [{ path: "/#/system/releases", name: "Console" }];
    const providers = new Map(), contracts = new Set();
    for (const name of [
      "wiki-ui",
      "agent-ui",
      "task-board-ui",
      "secretary-ui",
      "dashboards-ui",
      "data-collector-ui",
    ]) {
      const definition = JSON.parse(
        await readFile(`ui/${name}/ui.json`, "utf8"),
      );
      for (const required of definition.requirements.services) {
        if (providers.has(required.serviceName)) continue;
        const service = new ServiceClient({ publicBaseUrl: f.base, credential: () => fixtureToken,
          identity: { serviceNodeId: 'batch-' + required.serviceName, hostId: 'batch-host', serviceName: required.serviceName,
            version: '1.0.0', buildId: digest('batch-' + required.serviceName), hiveProtocol: 1 },
          registry: () => ({ namespaces: [{ namespace: required.namespace, description: 'Transport fixture', guideMarkdown: 'Transport fixture only.',
            tools: [{ namespace: required.namespace, name: 'read', interfaceVersion: required.interfaceVersion, description: 'Transport fixture read.',
              inputSchema: { type: 'object', properties: {}, additionalProperties: false }, outputSchema: { type: 'string' }, annotations: { readOnlyHint: true } }],
            topics: [], inventoryKinds: [] }], contracts: [], requiredContracts: [] }),
          handlers: { [required.namespace + '.read']: () => 'fixture' }, reconcile: async () => {} });
        providers.set(required.serviceName, service); t.after(() => service.stop()); service.start(); await service.waitReady();
      }
      for (const required of definition.requirements.contracts) {
        if (contracts.has(required.key) || definition.dataContracts.some(contract => contract.key === required.key)) continue;
        await f.client.request('contracts.register', { mutationId: await newOperationId(f.client), definition: {
          key: required.key, version: required.readVersions.at(-1), owner: { kind: 'agent' }, mediaType: 'text/plain',
          retention: { objects: { mode: 'retain' }, revisions: { mode: 'all' } }, specMarkdown: 'Empty transport fixture only.' } });
        contracts.add(required.key);
      }
      await publishUi(f.client, {
        directory: resolve(`dist/apps/${name}`),
        definition,
        mutationId: await newOperationId(f.client),
        expectedReleaseId: null,
      });
      apps.push({ path: "/" + definition.metadata.slug + "/", name });
    }
    await f.login();
    for (const width of [1440, 390]) {
      await f.page.setViewportSize({ width, height: 1000 });
      for (const app of apps) {
        const calls = [],
          errors = [];
        const requested = (request) => {
          if (request.url().endsWith("/api/v1/rpc"))
            calls.push(request.postDataJSON());
        };
        const failed = (error) => errors.push(error.message);
        f.page.on("request", requested);
        f.page.on("pageerror", failed);
        try {
        await f.page.goto(f.base + app.path);
        await expect(f.page.locator("main")).toBeVisible();
        await f.page.waitForTimeout(350);
        // One live burst reaches independently mounted loaders through the real socket.
        f.server.worker.onChanges?.(["services", "system", "uis", "inventory"]);
        await expect
            .poll(() => calls.filter(Array.isArray).length, {
              message: app.name + " uses shared read batching",
            })
            .toBeGreaterThan(0);
          await f.page.waitForTimeout(350);
          assert.ok(
            calls.every((call) => !Array.isArray(call) || call.length <= 8),
            app.name,
          );
          assert.ok(
            await f.page.evaluate(
              () => document.documentElement.scrollWidth <= innerWidth + 1,
            ),
            app.name,
          );
          assert.deepEqual(errors, [], app.name);
        } finally {
          f.page.off("request", requested);
          f.page.off("pageerror", failed);
        }
      }
    }
  },
);
