import test from "node:test";
import assert from "node:assert/strict";
import { digest } from "../packages/contracts/src/canonical.js";
import type { Host } from "../packages/contracts/src/generated.js";
import { validatePackageCatalog } from "../packages/host-runtime/src/package-archive.js";
import type { PackageCatalogEntry } from "../packages/host-runtime/src/package-archive.js";
import { PackageUpdater } from "../packages/host-runtime/src/package-updater.js";
import type { HostJournal } from "../packages/host-runtime/src/journal.js";

test("a newer HostExecutor bootstraps before an unknown manifest field in another package is parsed", async () => {
  const oldBuild = digest("old-executor"),
    newBuild = digest("new-executor");
  const manifest: Host.ReleaseManifest = {
    schemaVersion: 1,
    componentId: "host-executor",
    kind: "service",
    version: "0.2.2",
    buildId: newBuild,
    connectsToHive: true,
    requirements: { node: ">=24.18.0 <25.0.0", hiveProtocol: 1, contracts: [] },
    entrypoint: {
      executable: "node",
      args: ["dist/main.js"],
      timeoutMs: 30_000,
    },
    readiness: {
      timeoutMs: 30_000,
      command: {
        executable: "node",
        args: ["dist/health.js"],
        timeoutMs: 5000,
      },
    },
    shutdown: { timeoutMs: 15_000 },
    restart: { policy: "always", minimumDelayMs: 1000, maximumDelayMs: 60_000 },
  };
  const executorEntry: PackageCatalogEntry = {
    componentId: manifest.componentId,
    version: manifest.version,
    buildId: newBuild,
    archiveHash: digest("new-executor-archive"),
    bytes: 100,
    manifest,
    revision: 2,
    publishedAt: new Date().toISOString(),
    publisherPrincipalId: "fixture-publisher",
  };
  const agentBuild = digest("future-agent-manager");
  const catalog = {
    schemaVersion: 1,
    revision: 2,
    packages: [
      executorEntry,
      {
        ...executorEntry,
        componentId: "agent-manager",
        version: "9.0.0",
        buildId: agentBuild,
        manifest: {
          ...manifest,
          componentId: "agent-manager",
          version: "9.0.0",
          buildId: agentBuild,
          futureManifestField: true,
        },
      },
    ],
  };
  assert.throws(
    () => validatePackageCatalog(catalog),
    "the full catalog is unreadable by the old executor",
  );
  const config = {
    publicBaseUrl: "https://example.invalid/ivy",
    instances: [{ instanceId: "host-executor", componentId: "host-executor" }],
  } as Host.HostConfig;
  const installed = {
    instanceId: "host-executor",
    candidateId: oldBuild,
    buildId: oldBuild,
  } as NonNullable<ReturnType<HostJournal["installed"]>>;
  const journal = {
    config,
    installed: () => installed,
    manifest: () => ({ ...manifest, version: "0.2.1", buildId: oldBuild }),
  } as unknown as HostJournal;
  const candidate = {
    candidateId: newBuild,
    componentId: "host-executor",
    buildId: newBuild,
  } as Host.Candidate;
  const updater = new PackageUpdater(config, journal, "fixture-credential");
  let imported = 0;
  (
    updater as unknown as {
      importPackage(entry: PackageCatalogEntry): Promise<Host.Candidate>;
    }
  ).importPackage = async (entry) => {
    assert.deepEqual(
      entry,
      executorEntry,
      "only the fully validated HostExecutor package is imported",
    );
    imported++;
    return candidate;
  };
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (input) =>
    String(input).includes("after=2")
      ? new Response(null, { status: 204 })
      : new Response(JSON.stringify(catalog), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
  try {
    assert.deepEqual(
      (await updater.sync()).bootstrap.map(
        (value) => value.candidate.candidateId,
      ),
      [newBuild],
    );
    assert.deepEqual(
      (await updater.sync()).bootstrap.map(
        (value) => value.candidate.candidateId,
      ),
      [newBuild],
    );
    assert.equal(imported, 1);
  } finally {
    globalThis.fetch = originalFetch;
  }
});
