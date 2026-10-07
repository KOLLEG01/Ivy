import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:net";
import { once } from "node:events";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { randomUUID } from "node:crypto";
import { digest } from "../packages/contracts/src/canonical.js";
import { HiveServer } from "../services/hive/src/server.js";
import { HiveClient } from "../packages/sdk/src/client.js";
import { atomicJson } from "../packages/sdk/src/host.js";
import { startDataCollector } from "../services/data-collector/src/main.js";
import {
  defaultRetention,
  taskSchema,
} from "../services/data-collector/src/schema.js";

test("Collectors on separate hosts stay connected and route their own tasks", async (t) => {
  const parent = resolve(process.env.IVY_TEST_TEMP ?? tmpdir());
  const root = await mkdtemp(join(parent, "ivy-collector-service-"));
  const runtimes: Awaited<ReturnType<typeof startDataCollector>>[] = [];
  let server: HiveServer | undefined;
  t.after(async () => {
    for (const runtime of runtimes) await runtime.close();
    await server?.close();
    assert.equal(dirname(root), parent);
    await rm(root, { recursive: true, force: true });
  });
  const reservation = createServer().listen(0, "127.0.0.1");
  await once(reservation, "listening");
  const port = (reservation.address() as { port: number }).port;
  await new Promise<void>((resolve, reject) =>
    reservation.close((error) => (error ? reject(error) : resolve())),
  );
  const base = `http://127.0.0.1:${port}/ivy`;
  const token = "isolated-collector-service-fixture";
  server = new HiveServer({
    filename: join(root, "hive.sqlite"),
    publicBaseUrl: base,
    credentials: [{ principalId: "fixture", digest: digest(token) }],
    version: "fixture",
    buildId: digest("collector-service-fixture"),
    listenHost: "127.0.0.1",
    listenPort: port,
  });
  await server.start();
  const build = JSON.parse(await readFile("dist/build-info.json", "utf8"));
  for (const host of ["host-a", "host-b"]) {
    const path = join(root, host + ".json");
    await atomicJson(path, {
      schemaVersion: 1,
      componentId: "data-collector",
      instanceId: "collector",
      serviceNodeId: host + ".collector",
      hostId: host,
      publicBaseUrl: base,
      version: build.version,
      buildId: build.buildId,
      dataRoot: join(root, host),
      artifactRoot: resolve("."),
      credential: token,
      settings: { rootName: host + " results" },
    });
    const runtime = await startDataCollector(path);
    runtimes.push(runtime);
    await runtime.service.waitReady({ timeoutMs: 5000 });
    runtime.engine.update({
      action: "save",
      expectedRevision: 0,
      operationId: randomUUID(),
      task: taskSchema.parse({
        id: host,
        name: host,
        enabled: false,
        intervalSeconds: 0,
        timeoutSeconds: 5,
        memoryMb: 64,
        script: "export default async () => ({data:{}});",
        dependencies: {},
        config: {},
        secretNames: [],
        retention: defaultRetention,
      }),
    });
  }
  const client = new HiveClient(base, { credential: token });
  for (const host of ["host-a", "host-b"]) {
    const serviceNodeId = host + ".collector";
    const catalog = await client.request("tools.list", {
      namespace: "data-collector",
      serviceNodeId,
    });
    const read = catalog.items.find(
      (item) => item.qualifiedName === "data-collector.read",
    );
    assert.ok(read);
    const result = (await client.request("tools.call", {
      qualifiedName: read.qualifiedName,
      serviceNodeId,
      expectedDefinitionHash: read.definitionHash,
      arguments: { view: "tasks" },
    })) as { items: { task: { id: string } }[] };
    assert.deepEqual(
      result.items.map((item) => item.task.id),
      [host],
    );
  }
  assert.ok(runtimes.every((runtime) => runtime.service.ready));
  assert.notEqual(runtimes[0]!.engine.rootId, runtimes[1]!.engine.rootId);
});
