import { gzipSync } from "node:zlib";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { HiveKernel } from "../../services/hive/src/kernel.js";
import { digest } from "../../packages/contracts/src/canonical.js";
import { operationId } from "../../packages/contracts/src/operation-id.js";
import { publishUi } from "../../packages/cli/src/publish-ui.js";
import type {
  Operation,
  OperationName,
  Params,
  Result,
  Wire,
} from "../../packages/contracts/src/generated.js";
import type { RpcClient } from "../../packages/sdk/src/client.js";

const output = resolve(
  process.argv[2] ?? "tests/fixtures/hive-current.sqlite.gz",
);
const root = await mkdtemp(join(tmpdir(), "ivy-core-baseline-"));
const database = join(root, "hive.sqlite");
const bundle = join(root, "baseline-ui");
const credential = {
  principalId: "baseline-principal",
  digest: digest("synthetic-core-baseline-credential"),
};
const issuedAt = Date.now();
const kernel = new HiveKernel({
  filename: database,
  publicBaseUrl: "https://baseline.invalid/ivy",
  version: "baseline",
  buildId: digest("baseline-build"),
  credentials: [credential],
});

try {
  const call = <M extends OperationName>(
    context: Parameters<HiveKernel["execute"]>[0],
    method: M,
    params: Params<M>,
  ): Result<M> => {
    const result = kernel.execute(context, {
      jsonrpc: "2.0",
      id: randomUUID(),
      method,
      params,
    });
    if (result.kind !== "result")
      throw new Error("Fixture generation dispatched a native operation.");
    return result.value as Result<M>;
  };
  const clientContext = {
    ...credential,
    credentialDigest: credential.digest,
    transport: "http" as const,
  };
  const client: RpcClient = {
    request: async (method, params) => call(clientContext, method, params),
  };
  const mutation = (nonce: string) =>
    operationId(kernel.store.runtimeEpoch, issuedAt, nonce);
  const definition: Wire.DataContract = {
    key: "baseline/service-note",
    version: "1.0.0",
    owner: { kind: "service", serviceName: "baseline-service" },
    mediaType: "text/markdown",
    retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
    specMarkdown: "Frozen current-format service note fixture.",
  };
  const connected = call(
    { ...clientContext, transport: "ws" },
    "service.connect",
    {
      serviceNodeId: "baseline-node",
      hostId: "baseline-host",
      serviceName: "baseline-service",
      version: "1.0.0",
      buildId: digest("baseline-service-build"),
      hiveProtocol: 1,
    },
  );
  const serviceContext = {
    ...clientContext,
    transport: "ws" as const,
    serviceNodeId: "baseline-node",
    generation: connected.generation,
  };
  call(serviceContext, "registry.sync", {
    namespaces: [],
    contracts: [definition],
    requiredContracts: [],
  });
  const parent = call(serviceContext, "objects.write", {
    mutationId: mutation("baseline-parent"),
    contractVersion: "1.0.0",
    references: {},
    create: {
      contractKey: definition.key,
      name: "baseline-root",
      parentId: null,
      ownerObjectId: null,
    },
    content: { encoding: "text", value: "Frozen baseline root." },
  });
  call(serviceContext, "objects.write", {
    mutationId: mutation("baseline-child"),
    contractVersion: "1.0.0",
    references: { source: { objectId: parent.object.id, revision: 1 } },
    create: {
      contractKey: definition.key,
      name: "baseline-child",
      parentId: parent.object.id,
      ownerObjectId: null,
    },
    content: { encoding: "text", value: "Frozen baseline child." },
  });

  await mkdir(bundle);
  await writeFile(
    join(bundle, "index.html"),
    '<!doctype html><script type="module" src="./main.js"></script>',
  );
  await writeFile(
    join(bundle, "main.js"),
    'document.body.textContent = "frozen baseline";',
  );
  const ui: Operation.UiDefinition = {
    metadata: {
      uiId: "baseline-ui",
      displayName: "Baseline UI",
      description: "Frozen current-format UI fixture.",
      iconKey: "ui",
    },
    entryPath: "index.html",
    requirements: {
      hiveProtocol: 1,
      contracts: [
        { key: definition.key, readVersions: ["1.0.0"], writeVersions: [] },
      ],
      services: [],
    },
    dataContracts: [],
  };
  await publishUi(client, {
    directory: bundle,
    definition: ui,
    expectedReleaseId: null,
    mutationId: mutation("baseline-ui"),
  });
} finally {
  kernel.close();
}

const sqlite = new DatabaseSync(database);
try {
  sqlite.exec("VACUUM");
} finally {
  sqlite.close();
}
await mkdir(dirname(output), { recursive: true });
await writeFile(output, gzipSync(await readFile(database), { level: 9 }));
await rm(root, { recursive: true, force: true });
process.stdout.write(output + "\n");
