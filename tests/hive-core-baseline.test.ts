import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import { gunzipSync } from "node:zlib";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, relative, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { HiveKernel } from "../services/hive/src/kernel.js";
import { digest } from "../packages/contracts/src/canonical.js";
import { operationId } from "../packages/contracts/src/operation-id.js";
import { publishUi } from "../packages/cli/src/publish-ui.js";
import type {
  Operation,
  OperationName,
  Params,
  Result,
  Wire,
} from "../packages/contracts/src/generated.js";
import type { RpcClient } from "../packages/sdk/src/client.js";

const credential = {
  principalId: "baseline-principal",
  digest: digest("synthetic-core-baseline-credential"),
};
type Context = Parameters<HiveKernel["execute"]>[0];

async function uiText(kernel: HiveKernel, uiId: string, releaseId: string, path: string) {
  const selected = kernel.uis.asset(uiId, releaseId, path);
  return (await kernel.uiFiles.read(uiId, releaseId, selected.asset)).toString();
}

function clientFor(kernel: HiveKernel, context: Context): RpcClient {
  return {
    async request<M extends OperationName>(
      method: M,
      params: Params<M>,
    ): Promise<Result<M>> {
      const result = kernel.execute(context, {
        jsonrpc: "2.0",
        id: randomUUID(),
        method,
        params,
      });
      if (result.kind !== "result")
        throw new Error("Baseline check dispatched a native operation.");
      return result.value as Result<M>;
    },
  };
}

function fixture(t: TestContext) {
  const root = mkdtempSync(join(tmpdir(), "ivy-core-baseline-check-"));
  const database = join(root, "hive.sqlite");
  writeFileSync(
    database,
    gunzipSync(readFileSync(resolve("tests/fixtures/hive-current.sqlite.gz"))),
  );
  let active: HiveKernel | null = null;
  t.after(() => {
    if (active?.store.db.isOpen) active.close();
    assert.ok(relative(tmpdir(), root).startsWith("ivy-core-baseline-check-"));
    rmSync(root, { recursive: true, force: true });
  });
  const open = () =>
    (active = new HiveKernel({
      filename: database,
      publicBaseUrl: "https://baseline.invalid/ivy",
      version: "candidate",
      buildId: digest("candidate-build"),
      credentials: [credential],
    }));
  return { root, open };
}

test("current retained-state fixture accepts a service and UI extension without changing pinned data", async (t) => {
  const f = fixture(t);
  let kernel = f.open();
  const context: Context = {
    principalId: credential.principalId,
    credentialDigest: credential.digest,
    transport: "http",
  };
  let client = clientFor(kernel, context);
  const originalContract = await client.request("contracts.get", {
    key: "baseline/service-note",
    version: "1.0.0",
  });
  assert.deepEqual(originalContract.owner, {
    kind: "service",
    serviceName: "baseline-service",
  });
  const parent = await client.request("objects.read", {
    path: "/baseline-root",
  });
  const child = await client.request("objects.read", {
    path: "/baseline-root/baseline-child",
  });
  assert.equal(parent.revision.contentHash, digest("Frozen baseline root."));
  assert.deepEqual(child.revision.references, {
    source: { objectId: parent.object.id, revision: 1 },
  });
  const originalUi = await client.request("uis.get", { uiId: "baseline-ui" });
  const originalReleaseId = originalUi.currentReleaseId!;
  assert.equal(
    (await client.request("uis.inspect", { uiId: "baseline-ui" })).status,
    "ready",
  );
  assert.equal(
    await uiText(kernel, "baseline-ui", originalReleaseId, "main.js"),
    'document.body.textContent = "frozen baseline";',
  );

  const serviceDefinition: Wire.DataContract = {
    key: "baseline/extension",
    version: "1.0.0",
    owner: { kind: "service", serviceName: "extension-service" },
    mediaType: "application/json",
    retention: { objects: { mode: "retain" }, revisions: { mode: "current" } },
    specMarkdown: "Compatible service extension.",
    jsonSchema: {
      type: "object",
      properties: { enabled: { const: true } },
      required: ["enabled"],
      additionalProperties: false,
    },
  };
  const ws = clientFor(kernel, { ...context, transport: "ws" });
  const connected = await ws.request("service.connect", {
    serviceNodeId: "extension-node",
    hostId: "baseline-host",
    serviceName: "extension-service",
    version: "1.0.0",
    buildId: digest("extension-build"),
    hiveProtocol: 1,
  });
  const service = clientFor(kernel, {
    ...context,
    transport: "ws",
    serviceNodeId: "extension-node",
    generation: connected.generation,
  });
  await service.request("registry.sync", {
    namespaces: [],
    contracts: [serviceDefinition],
    requiredContracts: [],
  });
  const mutation = (nonce: string) =>
    operationId(kernel.store.runtimeEpoch, Date.now(), nonce);
  await service.request("objects.write", {
    mutationId: mutation("extension-object"),
    contractVersion: "1.0.0",
    references: {},
    create: {
      contractKey: serviceDefinition.key,
      name: "extension-object",
      parentId: null,
      ownerObjectId: null,
    },
    content: { encoding: "json", value: { enabled: true } },
  });

  const bundle = join(f.root, "extension-ui");
  mkdirSync(bundle);
  writeFileSync(
    join(bundle, "index.html"),
    '<!doctype html><script type="module" src="./main.js"></script>',
  );
  writeFileSync(
    join(bundle, "main.js"),
    'document.body.textContent = "compatible extension";',
  );
  const definition: Operation.UiDefinition = {
    metadata: {
      uiId: "extension-ui",
      displayName: "Extension UI",
      description: "Compatible extension fixture.",
      iconKey: "ui",
    },
    entryPath: "index.html",
    requirements: {
      hiveProtocol: 1,
      contracts: [
        {
          key: serviceDefinition.key,
          readVersions: ["1.0.0"],
          writeVersions: ["1.0.0"],
        },
      ],
      services: [],
    },
    dataContracts: [],
  };
  const extension = await publishUi(client, {
    directory: bundle,
    definition,
    expectedReleaseId: null,
    mutationId: mutation("extension-ui"),
  });
  kernel.close();

  kernel = f.open();
  client = clientFor(kernel, context);
  assert.deepEqual(
    await client.request("contracts.get", {
      key: "baseline/service-note",
      version: "1.0.0",
    }),
    originalContract,
  );
  assert.deepEqual(
    await client.request("objects.read", {
      objectId: parent.object.id,
      revision: 1,
    }),
    parent,
  );
  assert.deepEqual(
    (
      await client.request("objects.read", {
        path: "/baseline-root/baseline-child",
      })
    ).revision.references,
    { source: { objectId: parent.object.id, revision: 1 } },
  );
  assert.equal(
    (await client.request("uis.get", { uiId: "baseline-ui" })).currentReleaseId,
    originalReleaseId,
  );
  assert.equal(
    await uiText(kernel, "baseline-ui", originalReleaseId, "main.js"),
    'document.body.textContent = "frozen baseline";',
  );
  assert.deepEqual(
    (
      await client.request("contracts.get", {
        key: serviceDefinition.key,
        version: "1.0.0",
      })
    ).owner,
    serviceDefinition.owner,
  );
  assert.deepEqual(
    (await client.request("objects.read", { path: "/extension-object" }))
      .content,
    { encoding: "json", value: { enabled: true } },
  );
  assert.equal(
    (await client.request("uis.get", { uiId: "extension-ui" }))
      .currentReleaseId,
    extension.releaseId,
  );
  assert.equal(
    await uiText(kernel, "extension-ui", extension.releaseId, "main.js"),
    'document.body.textContent = "compatible extension";',
  );
});
