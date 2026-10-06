import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { PNG } from "pngjs";
import { chromium } from "playwright-core";
import { readFile } from "node:fs/promises";
import { HiveKernel } from "../services/hive/src/kernel.js";
import type { ConnectionContext } from "../services/hive/src/kernel.js";
import { digest, newOperationId } from "../packages/sdk/src/node.js";
import type {
  RpcClient,
  OperationName,
  Params,
  Result,
} from "../packages/sdk/src/node.js";
import {
  dashboardRegistry,
  definition,
  inputs,
  outputOptions,
} from "../services/dashboards/src/schema.js";
import { DashboardStore } from "../services/dashboards/src/store.js";
import { DashboardRenderer } from "../services/dashboards/src/render.js";
import { dashboardDocument } from "../services/dashboards/src/document.js";
import { uiContentPolicy } from "../services/hive/src/ui-security.js";
import { mcpSuccess } from "../services/hive/src/mcp.js";

test("dashboard registry, CRUD, revision conflicts, scoped reads and fresh Hive data", async (t) => {
  const credentialDigest = digest("dashboard-test"),
    principalId = "dashboard-test";
  const kernel = new HiveKernel({
    filename: ":memory:",
    publicBaseUrl: "https://hive.test/",
    version: "test",
    buildId: digest("test"),
    credentials: [{ principalId, digest: credentialDigest }],
  });
  t.after(() => kernel.close());
  let context: ConnectionContext = {
    transport: "ws",
    credentialDigest,
    principalId,
  };
  const client: RpcClient = {
    request: async <M extends OperationName>(
      method: M,
      params: Params<M>,
    ): Promise<Result<M>> => {
      const result = kernel.execute(context, {
        jsonrpc: "2.0",
        id: randomUUID(),
        method,
        params,
      });
      assert.equal(result.kind, "result");
      return (result as { value: Result<M> }).value;
    },
  };
  const connected = await client.request("service.connect", {
    serviceNodeId: "dashboards-test",
    hostId: "test",
    serviceName: "dashboards",
    version: "0.1.0",
    buildId: digest("test"),
    hiveProtocol: 1,
  });
  context = {
    ...context,
    serviceNodeId: "dashboards-test",
    generation: connected.generation,
  };
  await client.request("registry.sync", dashboardRegistry());
  const store = new DashboardStore(
    client,
    null,
    "https://hive.test/",
    "dashboards-test",
  );
  const value = definition.parse({
    title: "Test",
    html: "<p>Hello</p>",
    sources: {},
    refreshSeconds: 5,
  });
  const operation = await newOperationId(client),
    created = await store.save(value, undefined, undefined, operation);
  assert.deepEqual(
    await store.save(value, undefined, undefined, operation),
    created,
  );
  assert.match(created.url, /dashboards\/#\/view\?node=dashboards-test&id=/);
  assert.equal((await store.list()).items[0]!.title, "Test");
  const updated = await store.save(
    {
      ...value,
      title: "Updated",
      imageToken: "a".repeat(40),
      metadata: { display: "Touch", nested: { enabled: true } },
    },
    created.id,
    1,
    await newOperationId(client),
  );
  assert.equal(updated.revision, 2);
  assert.equal(
    (await store.image(created.id, "a".repeat(40))).value.metadata.display,
    "Touch",
  );
  await assert.rejects(store.image(created.id, "wrong"), /token/i);
  assert.equal(
    new URL(updated.imageUrl!).searchParams.get("token"),
    "a".repeat(40),
  );
  assert.equal(
    new URL(updated.imageUrl!).pathname,
    "/api/v1/dashboards/image.png",
  );
  await assert.rejects(
    store.save(value, created.id, 1, await newOperationId(client)),
    /revision/i,
  );
  await assert.rejects(
    new DashboardStore(
      client,
      "another-root",
      "https://hive.test/",
      "test",
    ).read(created.id),
    /workspace/i,
  );
  const data = await store.data({
    ...value,
    sources: {
      saved: { objectId: created.id },
      all: { contractKey: "dashboards/dashboard", limit: 10 },
    },
  });
  assert.equal((data.saved as { title: string }).title, "Updated");
  assert.ok(!Object.hasOwn(data.saved as object, "imageToken"));
  assert.ok(
    !(data.all as Record<string, unknown>[]).some((row) =>
      Object.hasOwn(row, "imageToken"),
    ),
  );
  assert.equal((data.all as unknown[]).length, 1);
  const deletion = await newOperationId(client);
  await store.remove(created.id, 2, deletion);
  await store.remove(created.id, 2, deletion);
  assert.equal((await store.list()).items.length, 0);
});

test("dashboard validates refresh intervals, dimensions and data source count", () => {
  assert.equal(
    definition.parse({ title: "A", html: "<p>A</p>", sources: {} })
      .refreshSeconds,
    5,
  );
  assert.throws(() =>
    definition.parse({
      title: "A",
      html: "<p>A</p>",
      sources: {},
      refreshSeconds: 1,
    }),
  );
  assert.throws(() => outputOptions.parse({ width: 4096, height: 4096 }));
  assert.throws(() => inputs.render.parse({ id: "test", width: 99 }));
  assert.throws(() =>
    inputs.render.parse({ id: "test", width: 4096, height: 4096 }),
  );
  assert.throws(() =>
    definition.parse({
      title: "A",
      html: "<p>A</p>",
      sources: Object.fromEntries(
        Array.from({ length: 21 }, (_, i) => [
          "source" + i,
          { objectId: "test" },
        ]),
      ),
    }),
  );
  assert.deepEqual(outputOptions.parse({}), {
    width: 800,
    height: 600,
    colorMode: "color",
  });
  const valid = { title: "A", html: "<p>A</p>", sources: {} };
  assert.throws(() =>
    definition.parse({ ...valid, metadata: { refreshSeconds: 30 } }),
  );
  assert.throws(() =>
    definition.parse({ ...valid, metadata: { value: "x".repeat(9000) } }),
  );
  assert.throws(() => definition.parse({ ...valid, imageToken: "short" }));
  assert.deepEqual(definition.parse(valid).metadata, {});
});

test(
  "dashboard images await async drawing, expose output settings and enforce pixel modes",
  { timeout: 60000 },
  async (t) => {
    const renderer = new DashboardRenderer();
    t.after(() => renderer.close());
    const html = `<style>body{background:white}</style><script>dashboard.onUpdate(async ({data,output})=>{await new Promise(r=>setTimeout(r,100));if(output.mode!=='image'||output.width!==120||data.mark!==42)throw Error('bad snapshot');document.body.style.background='rgb(240,20,40)';});</script>`;
    for (const colorMode of ["color", "grayscale", "monochrome"] as const) {
      const result = await renderer.render(
        html,
        { mark: 42 },
        { width: 120, height: 100, colorMode },
      );
      const png = PNG.sync.read(Buffer.from(result.data, "base64"));
      if (colorMode !== "color") {
        const bytes = Buffer.from(result.data, "base64");
        assert.equal(bytes[24], 8, "E Ink requires eight-bit pixels");
        assert.equal(
          bytes[25],
          0,
          "E Ink requires a grayscale PNG without alpha",
        );
        assert.equal(bytes[28], 0, "E Ink requires a non-interlaced PNG");
      }
      const mcp = mcpSuccess(result, true);
      assert.equal(mcp.content[1]?.type, "image");
      assert.ok(!JSON.stringify(mcp.content[0]).includes(result.data));
      assert.ok(!JSON.stringify(mcp.structuredContent).includes(result.data));
      assert.equal(png.width, 120);
      assert.equal(png.height, 100);
      if (colorMode === "color")
        assert.deepEqual([...png.data.subarray(0, 3)], [240, 20, 40]);
      else
        for (let i = 0; i < png.data.length; i += 4) {
          assert.equal(png.data[i], png.data[i + 1]);
          assert.equal(png.data[i], png.data[i + 2]);
          if (colorMode === "monochrome")
            assert.ok(png.data[i] === 0 || png.data[i] === 255);
        }
    }
    await assert.rejects(
      renderer.render(
        '<script>dashboard.onUpdate(()=>{throw Error("drawing failed")})</script>',
        {},
        outputOptions.parse({}),
      ),
      /drawing failed/,
    );
    const recovered = await renderer.render(
      "<p>Recovered</p>",
      {},
      outputOptions.parse({}),
    );
    assert.equal(recovered.mimeType, "image/png");
    await assert.rejects(
      renderer.render(
        '<script>dashboard.onUpdate(()=>{throw ""})</script>',
        {},
        outputOptions.parse({}),
      ),
      /Dashboard rendering failed/,
    );
  },
);

test(
  "published sandbox executes dashboard HTML while keeping the app origin and network isolated",
  { timeout: 60000 },
  async (t) => {
    const browser = await chromium.launch({
      headless: true,
      chromiumSandbox: true,
    });
    t.after(() => browser.close());
    const page = await browser.newPage();
    const loader = await readFile(
      "ui/dashboards-ui/public/dashboard.sandbox.html",
      "utf8",
    );
    let externalRequests = 0;
    await page.route("**/*", async (route) => {
      const url = new URL(route.request().url());
      if (url.origin !== "https://hive.test") {
        externalRequests++;
        await route.abort();
        return;
      }
      const sandbox = url.pathname.endsWith(".sandbox.html");
      await route.fulfill({
        contentType: "text/html",
        headers: { "Content-Security-Policy": uiContentPolicy(url.pathname) },
        body: sandbox
          ? loader
          : '<iframe sandbox="allow-scripts" src="/dashboard.sandbox.html"></iframe>',
      });
    });
    await page.goto("https://hive.test/index.html");
    await page
      .frameLocator("iframe")
      .locator("body")
      .waitFor({ state: "attached" });
    const html = `<p id="value"></p><script>dashboard.onUpdate(async ({data})=>{await new Promise(r=>setTimeout(r,data.delay||0));let isolated=false;try{parent.document.body}catch{isolated=true}let blocked=false;try{await fetch('https://external.test/secret')}catch{blocked=true}document.getElementById('value').textContent=data.value+':'+isolated+':'+blocked;});</script>`;
    await page.evaluate(
      ({ html }) => {
        const frame = document.querySelector("iframe")!;
        window.addEventListener("message", (event) => {
          if (
            event.source === frame.contentWindow &&
            event.data?.type === "ready"
          )
            frame.contentWindow!.postMessage(
              {
                type: "update",
                channel: "test",
                sequence: 1,
                data: { value: 42 },
                output: {
                  mode: "interactive",
                  width: 800,
                  height: 600,
                  colorMode: "color",
                },
              },
              "*",
            );
        });
        frame.contentWindow!.postMessage({ type: "document", html }, "*");
      },
      { html: dashboardDocument(html, "test") },
    );
    await page.frameLocator("iframe").getByText("42:true:true").waitFor();
    assert.equal(externalRequests, 0);
    // A slow draw must never overwrite a newer snapshot after it finishes.
    await page.evaluate(async () => {
      const frame = document.querySelector("iframe")!;
      await new Promise<void>((resolve) => {
        const finished = new Set<number>();
        const receive = (event: MessageEvent) => {
          if (
            event.source !== frame.contentWindow ||
            event.data?.type !== "rendered"
          )
            return;
          finished.add(event.data.sequence);
          if (finished.has(2) && finished.has(4)) {
            window.removeEventListener("message", receive);
            resolve();
          }
        };
        window.addEventListener("message", receive);
        for (const sequence of [2, 3, 4])
          frame.contentWindow!.postMessage(
            {
              channel: "test",
              type: "update",
              sequence,
              data: { value: sequence, delay: sequence === 2 ? 150 : 0 },
              output: {
                mode: "interactive",
                width: 800,
                height: 600,
                colorMode: "color",
              },
            },
            "*",
          );
      });
    });
    assert.equal(
      await page.frameLocator("iframe").locator("#value").textContent(),
      "4:true:true",
    );
  },
);
