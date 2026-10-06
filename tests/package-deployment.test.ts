import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtemp,
  mkdir,
  readFile,
  rm,
  stat,
  utimes,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { spawnSync } from "node:child_process";
import {
  Client,
  StreamableHTTPClientTransport,
} from "@modelcontextprotocol/client";
import type { Transport } from "@modelcontextprotocol/client";
import { HiveServer } from "../services/hive/src/server.js";
import { collectBackupArtifacts } from "../services/hive/src/main.js";
import { HiveKernel } from "../services/hive/src/kernel.js";
import { digest } from "../packages/contracts/src/canonical.js";
import type { Host } from "../packages/contracts/src/generated.js";
import { createPackageArchive } from "../packages/host-runtime/src/package-archive.js";
import { portableNativeFiles } from "../packages/host-runtime/src/portable-native-files.js";
import { PackageUpdater } from "../packages/host-runtime/src/package-updater.js";
import { HostJournal } from "../packages/host-runtime/src/journal.js";
import { runtimeRequirementsSatisfied } from "../packages/host-runtime/src/package-requirements.js";
import { HiveClient } from "../packages/sdk/src/client.js";
import { readUiBundle } from "../packages/cli/src/publish-ui.js";

test("package runtime requirements stay universal unless a component declares an explicit boundary", () => {
  const universal = {
    node: ">=24.18.0 <25.0.0",
    hiveProtocol: 1 as const,
    contracts: [],
  };
  assert.equal(
    runtimeRequirementsSatisfied(universal, {
      node: "v24.18.0",
      os: "linux",
      arch: "x64",
    }),
    true,
  );
  assert.equal(
    runtimeRequirementsSatisfied(universal, {
      node: "v24.99.0",
      os: "win32",
      arch: "arm64",
    }),
    true,
  );
  assert.equal(
    runtimeRequirementsSatisfied(universal, {
      node: "v25.0.0",
      os: "linux",
      arch: "x64",
    }),
    false,
  );
  assert.equal(
    runtimeRequirementsSatisfied(
      { ...universal, os: ["win32"], arch: ["x64"] },
      { node: "v24.18.0", os: "linux", arch: "x64" },
    ),
    false,
  );
});

test(
  "the reference catalog is imported once into a retained Hive Object",
  { timeout: 20_000 },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "ivy-catalog-object-")),
      packageRoot = join(root, "packages"),
      token = "catalog-seed";
    await mkdir(packageRoot, { recursive: true });
    const buildId = digest("existing-release");
    const manifest: Host.ReleaseManifest = {
      schemaVersion: 1,
      componentId: "existing-service",
      kind: "service",
      version: "1.0.0",
      buildId,
      connectsToHive: true,
      requirements: {
        node: ">=24.18.0 <25.0.0",
        hiveProtocol: 1,
        contracts: [],
      },
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
      restart: {
        policy: "always",
        minimumDelayMs: 1000,
        maximumDelayMs: 60_000,
      },
    };
    await writeFile(
      join(packageRoot, "catalog.json"),
      JSON.stringify({
        schemaVersion: 1,
        revision: 37,
        packages: [
          {
            componentId: manifest.componentId,
            version: manifest.version,
            buildId,
            archiveHash: digest("existing-archive"),
            bytes: 123,
            manifest,
            revision: 37,
            publishedAt: new Date().toISOString(),
            publisherPrincipalId: "publisher",
          },
        ],
      }),
    );
    const options = {
      filename: join(root, "hive.sqlite"),
      packageRoot,
      publicBaseUrl: "http://127.0.0.1/ivy",
      version: "test",
      buildId: digest("catalog-object-hive"),
      credentials: [{ principalId: "publisher", digest: digest(token) }],
      listenHost: "127.0.0.1",
      listenPort: 0,
    };
    const first = new HiveServer(options),
      address = await first.start(),
      base = `http://127.0.0.1:${address.port}/ivy`;
    let firstClosed = false,
      second: HiveServer | null = null;
    t.after(async () => {
      await second?.close();
      if (!firstClosed) await first.close().catch(() => undefined);
      await rm(root, {
        recursive: true,
        force: true,
        maxRetries: 5,
        retryDelay: 100,
      });
    });
    const response = await fetch(
      base + "/api/v1/packages/catalog?after=0&include=all",
      { headers: { Authorization: "Bearer " + token } },
    );
    assert.equal(response.status, 200);
    assert.equal(
      ((await response.json()) as { revision: number }).revision,
      37,
    );
    const objectId = response.headers.get("x-ivy-object-id");
    assert.ok(objectId);
    await first.close();
    firstClosed = true;
    await rm(join(packageRoot, "catalog.json"));
    second = new HiveServer(options);
    const nextAddress = await second.start();
    const retained = await fetch(
      `http://127.0.0.1:${nextAddress.port}/ivy/api/v1/packages/catalog?after=0&include=all`,
      { headers: { Authorization: "Bearer " + token } },
    );
    assert.equal(retained.headers.get("x-ivy-object-id"), objectId);
    assert.equal(
      ((await retained.json()) as { revision: number }).revision,
      37,
    );
  },
);

test("startup keeps two app archives and removes older catalog entries and files", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-app-retention-"));
  const packageRoot = join(root, "packages");
  const packages = [] as Array<Record<string, unknown>>;
  for (const [index, version] of ["1.0.0", "1.1.0", "1.2.0"].entries()) {
    const buildId = digest("app-build-" + version);
    const archiveHash = digest("app-archive-" + version);
    const manifest: Host.ReleaseManifest = {
      schemaVersion: 1, componentId: "retained-ui", kind: "app", version, buildId,
      connectsToHive: false,
      requirements: { node: ">=24.18.0 <25.0.0", hiveProtocol: 1, contracts: [] },
      app: { appId: "retained-ui", dist: "dist/apps/retained-ui", entryPath: "index.html" },
    };
    packages.push({ componentId: "retained-ui", version, buildId, archiveHash,
      bytes: 1, manifest, revision: index + 1, publishedAt: new Date().toISOString(),
      publisherPrincipalId: "publisher" });
    const directory = join(packageRoot, "artifacts", "retained-ui", version);
    await mkdir(directory, { recursive: true });
    await writeFile(join(directory, archiveHash.slice(7) + ".tar.gz"), "x");
  }
  await writeFile(join(packageRoot, "catalog.json"), JSON.stringify({ schemaVersion: 1, revision: 3, packages }));
  const orphan = join(packageRoot, 'artifacts', 'failed-ui');
  await mkdir(join(orphan, '1.0.0'), { recursive: true });
  await writeFile(join(orphan, '.app'), '');
  await writeFile(join(orphan, '1.0.0', 'abandoned.tar.gz'), 'x');
  const incoming = join(packageRoot, 'incoming');
  await mkdir(join(incoming, '00000000-0000-4000-8000-000000000001-extract'), { recursive: true });
  const staleUpload = join(incoming, '00000000-0000-4000-8000-000000000002.tar.gz');
  await writeFile(staleUpload, 'x');
  const aged = new Date(Date.now() - 25 * 60 * 60 * 1000);
  await utimes(join(incoming, '00000000-0000-4000-8000-000000000001-extract'), aged, aged);
  await utimes(staleUpload, aged, aged);
  const token = "app-retention";
  const server = new HiveServer({ filename: join(root, "hive.sqlite"), packageRoot,
    publicBaseUrl: "http://127.0.0.1/ivy", version: "test", buildId: digest("app-retention-hive"),
    credentials: [{ principalId: "publisher", digest: digest(token) }],
    listenHost: "127.0.0.1", listenPort: 0 });
  t.after(async () => {
    await server.close();
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  });
  const address = await server.start();
  const response = await fetch(`http://127.0.0.1:${address.port}/ivy/api/v1/packages/catalog?after=0&include=all`,
    { headers: { Authorization: "Bearer " + token } });
  assert.equal(response.status, 200);
  const catalog = await response.json() as { packages: Array<{ version: string }> };
  assert.deepEqual(catalog.packages.map(value => value.version), ["1.1.0", "1.2.0"]);
  await assert.rejects(stat(join(packageRoot, "artifacts", "retained-ui", "1.0.0")), { code: "ENOENT" });
  assert.equal((await stat(join(packageRoot, "artifacts", "retained-ui", "1.1.0"))).isDirectory(), true);
  await assert.rejects(stat(orphan), { code: 'ENOENT' });
  await assert.rejects(stat(staleUpload), { code: 'ENOENT' });
  await assert.rejects(stat(join(incoming, '00000000-0000-4000-8000-000000000001-extract')), { code: 'ENOENT' });
});

test('backup cleanup removes interrupted copies and orphaned pairs', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-backup-cleanup-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const name = (id: string) => `hive-2026-09-25T12-00-00.000Z-00000000-0000-4000-8000-${id}`;
  const complete = join(root, name('000000000001'));
  const partial = join(root, name('000000000002') + '.ui.partial');
  const orphan = join(root, name('000000000003') + '.ui');
  const unfinished = join(root, name('000000000004'));
  await mkdir(complete + '.ui');
  await writeFile(complete + '.sqlite', 'db');
  await writeFile(complete + '.sqlite.json', '{}');
  await mkdir(partial);
  await mkdir(orphan);
  await mkdir(unfinished + '.ui');
  await writeFile(unfinished + '.sqlite', 'db');
  const aged = new Date(Date.now() - 25 * 60 * 60 * 1000);
  for (const path of [complete + '.ui', complete + '.sqlite', complete + '.sqlite.json', partial, orphan,
    unfinished + '.ui', unfinished + '.sqlite']) await utimes(path, aged, aged);
  await collectBackupArtifacts(root);
  assert.equal((await stat(complete + '.ui')).isDirectory(), true);
  await assert.rejects(stat(partial), { code: 'ENOENT' });
  await assert.rejects(stat(orphan), { code: 'ENOENT' });
  await assert.rejects(stat(unfinished + '.ui'), { code: 'ENOENT' });
  await assert.rejects(stat(unfinished + '.sqlite'), { code: 'ENOENT' });
});

test(
  "MCP grants one exact REST package upload and Hive exposes the immutable catalog",
  { timeout: 30_000 },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "ivy-package-")),
      candidateRoot = join(root, "candidate"),
      artifactRoot = join(candidateRoot, "artifact");
    let hostJournal: HostJournal | null = null,
      bootstrapJournal: HostJournal | null = null;
    await mkdir(join(artifactRoot, "dist"), { recursive: true });
    const buildId = digest("package-build"),
      major = Number(process.versions.node.split(".")[0]);
    const manifest: Host.ReleaseManifest = {
      schemaVersion: 1,
      componentId: "fixture-service",
      kind: "service",
      version: "1.0.0",
      buildId,
      connectsToHive: true,
      requirements: {
        node: `>=${major}.0.0 <${major + 1}.0.0`,
        hiveProtocol: 1,
        contracts: [],
      },
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
      restart: {
        policy: "always",
        minimumDelayMs: 1000,
        maximumDelayMs: 60_000,
      },
    };
    await writeFile(
      join(artifactRoot, "dist", "main.js"),
      "process.exit(0);\n",
    );
    await writeFile(
      join(artifactRoot, "dist", "health.js"),
      "process.exit(0);\n",
    );
    await writeFile(
      join(artifactRoot, "dist", "build-info.json"),
      JSON.stringify({
        schemaVersion: 1,
        componentId: manifest.componentId,
        version: manifest.version,
        buildId,
      }),
    );
    await writeFile(
      join(candidateRoot, "component.json"),
      JSON.stringify(manifest),
    );
    const candidate: Host.Candidate = {
      candidateId: buildId,
      componentId: manifest.componentId,
      buildId,
      artifactRoot,
      manifestPath: join(candidateRoot, "component.json"),
      createdAt: new Date().toISOString(),
      platform: {
        os: process.platform as "win32" | "linux",
        arch: process.arch as "x64" | "arm64",
        node: process.version,
      },
    };
    const archive = await createPackageArchive(candidate, {}),
      token = "package-publisher",
      deniedToken = "ordinary-service";
    const server = new HiveServer({
      filename: join(root, "hive.sqlite"),
      packageRoot: join(root, "packages"),
      publicBaseUrl: "http://127.0.0.1/ivy",
      version: "test",
      buildId: digest("hive"),
      credentials: [
        { principalId: "publisher", digest: digest(token) },
        { principalId: "ordinary-service", digest: digest(deniedToken) },
      ],
      packagePublisherPrincipalIds: ["publisher"],
      listenHost: "127.0.0.1",
      listenPort: 0,
    });
    const address = await server.start(),
      base = `http://127.0.0.1:${address.port}/ivy`;
    t.after(async () => {
      bootstrapJournal?.close();
      hostJournal?.close();
      await server.close();
      await rm(root, {
        recursive: true,
        force: true,
        maxRetries: 5,
        retryDelay: 100,
      });
    });
    const client = new Client({ name: "package-test", version: "1.0.0" }),
      transport = new StreamableHTTPClientTransport(new URL(base + "/mcp-dev"), {
        requestInit: { headers: { Authorization: "Bearer " + token } },
      });
    await client.connect(transport as unknown as Transport);
    t.after(() => client.close());
    const input = {
      componentId: manifest.componentId,
      version: manifest.version,
      buildId,
      archiveHash: archive.hash,
      bytes: archive.bytes,
      manifest,
    };
    const deniedClient = new Client({
        name: "denied-package-test",
        version: "1.0.0",
      }),
      deniedTransport = new StreamableHTTPClientTransport(
        new URL(base + "/mcp-dev"),
        {
          requestInit: { headers: { Authorization: "Bearer " + deniedToken } },
        },
      );
    await deniedClient.connect(deniedTransport as unknown as Transport);
    const denied = await deniedClient.callTool({
      name: "hive_authorize_package_upload",
      arguments: input,
    });
    await deniedClient.close();
    assert.equal(denied.isError, true);
    assert.equal(
      (denied.structuredContent as { error: { data: { code: string } } }).error
        .data.code,
      "forbidden",
    );
    assert.equal(
      denied._meta?.["mcp/www_authenticate"],
      undefined,
      "publisher policy denial is not an OAuth relinking condition",
    );
    const authorized = await client.callTool({
      name: "hive_authorize_package_upload",
      arguments: input,
    });
    const grant = (
      authorized.structuredContent as {
        result: { uploadId: string; uploadToken: string; uploadPath: string };
      }
    ).result;
    const uploaded = await fetch(base + grant.uploadPath.replace("/ivy", ""), {
      method: "PUT",
      headers: {
        Authorization: "Bearer " + grant.uploadToken,
        "Content-Length": String(archive.bytes),
      },
      body: await readFile(archive.path),
    });
    assert.equal(uploaded.status, 201);
    assert.equal(
      ((await uploaded.json()) as { archiveHash: string }).archiveHash,
      archive.hash,
    );
    const catalogResponse = await fetch(
      base + "/api/v1/packages/catalog?after=0",
      { headers: { Authorization: "Bearer " + token } },
    );
    assert.equal(catalogResponse.status, 200);
    const catalog = (await catalogResponse.json()) as {
      revision: number;
      packages: Array<{ componentId: string; version: string }>;
    };
    assert.equal(catalog.revision, 1);
    assert.deepEqual(
      catalog.packages.map((value) => [value.componentId, value.version]),
      [["fixture-service", "1.0.0"]],
    );
    const catalogObjectId = catalogResponse.headers.get("x-ivy-object-id");
    const catalogObjectRevision = Number(
      catalogResponse.headers.get("x-ivy-object-revision"),
    );
    assert.ok(
      catalogObjectId,
      "the package catalog has one Hive Object identity",
    );
    const hive = new HiveClient(base, { credential: token });
    const canonicalCatalog = await hive.request("packages.catalog", {
      after: 0,
      includeUis: true,
    });
    assert.equal(canonicalCatalog.objectId, catalogObjectId);
    assert.equal(canonicalCatalog.objectRevision, catalogObjectRevision);
    assert.deepEqual(
      canonicalCatalog.catalog.packages.map((value) => [
        value.componentId,
        value.version,
      ]),
      [["fixture-service", "1.0.0"]],
    );
    const mcpCatalog = await client.callTool({ name: "hive_package_catalog", arguments: { after: 0, includeUis: true } });
    assert.equal(mcpCatalog.isError, undefined);
    assert.deepEqual(
      (mcpCatalog.structuredContent as { result: typeof canonicalCatalog })
        .result,
      canonicalCatalog,
    );
    const stored = await hive.request("objects.read", {
      objectId: catalogObjectId,
    });
    assert.equal((stored.content.value as { revision: number }).revision, 1);
    const changes = await hive.request("events.read", {
      afterSequence: 0,
      filter: { topics: ["hive.object.changed"], objectIds: [catalogObjectId] },
    });
    assert.ok(
      changes.items.some(
        (item) =>
          (item.payload as { objectId?: string; revision?: number })
            .objectId === catalogObjectId &&
          (item.payload as { revision?: number }).revision ===
            stored.revision.revision,
      ),
      "accepted catalog revision emits one durable Object change",
    );
    const unrelated = await hive.request("events.read", {
      afterSequence: 0,
      filter: {
        topics: ["hive.object.changed"],
        objectIds: ["00000000-0000-0000-0000-000000000000"],
      },
    });
    assert.deepEqual(
      unrelated.items,
      [],
      "Object ID filters avoid unrelated Object changes",
    );
    assert.equal(
      (
        await fetch(base + "/api/v1/packages/catalog?after=1", {
          headers: { Authorization: "Bearer " + token },
        })
      ).status,
      204,
    );
    const downloaded = await fetch(
      base + "/api/v1/packages/fixture-service/1.0.0/artifact",
      { headers: { Authorization: "Bearer " + token } },
    );
    assert.equal(downloaded.status, 200);
    assert.equal(
      (await downloaded.arrayBuffer()).byteLength,
      (await stat(archive.path)).size,
    );
    const executable = (name: string) =>
      spawnSync(process.platform === "win32" ? "where.exe" : "which", [name], {
        encoding: "utf8",
        windowsHide: true,
        timeout: 5000,
      })
        .stdout.trim()
        .split(/\r?\n/)[0]!;
    const host: Host.HostConfig = {
      schemaVersion: 1,
      hostId: "package-consumer",
      runtimeRoot: join(root, "host-state"),
      artifactRoot: join(root, "host-artifacts"),
      stagingRoot: join(root, "host-staging"),
      publicBaseUrl: base,
      executables: { node: process.execPath, tar: executable("tar") },
      packageUpdates: { intervalSeconds: 5 },
      instances: [
        {
          instanceId: "fixture-service",
          serviceNodeId: "fixture-service",
          componentId: manifest.componentId,
          enabled: true,
          engine: "process",
          credential: token,
          settings: {},
        },
      ],
    };
    const journal = (hostJournal = new HostJournal(host));
    const legacyBuild = digest("legacy-package-build"),
      legacyCandidate: Host.Candidate = {
        ...candidate,
        candidateId: legacyBuild,
        buildId: legacyBuild,
        artifactRoot: join(root, "legacy-artifact"),
        manifestPath: join(root, "legacy-component.json"),
      };
    const { restart: _removedByOldSchema, ...legacyManifest } = {
      ...manifest,
      version: "0.9.0",
      buildId: legacyBuild,
    };
    journal.db
      .prepare("INSERT INTO candidates VALUES (?,?,?,?,?)")
      .run(
        legacyBuild,
        manifest.componentId,
        legacyBuild,
        JSON.stringify(legacyCandidate),
        JSON.stringify(legacyManifest),
      );
    journal.db.prepare("INSERT INTO installed VALUES (?,?)").run(
      "fixture-service",
      JSON.stringify({
        instanceId: "fixture-service",
        candidateId: legacyBuild,
        buildId: legacyBuild,
        enabled: true,
        installedAt: new Date().toISOString(),
      }),
    );
    await new PackageUpdater(host, journal, token).sync();
    const queued = journal.list();
    assert.equal(queued.length, 1);
    assert.equal(queued[0]!.record.phase, "prepared");
    assert.equal(queued[0]!.candidateId, buildId);
    assert.equal(journal.candidate(buildId).archiveHash, archive.hash);
    await new PackageUpdater(host, journal, token).sync();
    assert.equal(journal.list().length, 1, "a catalog replay is idempotent");
    journal.advance(queued[0]!.record.deploymentId, "prepared", "failed");
    journal.useConfiguration({
      ...host,
      packageUpdates: { intervalSeconds: 6 },
    });
    await new PackageUpdater(journal.config, journal, token).sync();
    assert.equal(
      journal.list().length,
      2,
      "a changed host configuration gets one fresh bounded package attempt",
    );
    await new PackageUpdater(journal.config, journal, token).sync();
    assert.equal(
      journal.list().length,
      2,
      "the same failed configuration does not retry forever",
    );
    const replay = await client.callTool({
      name: "hive_authorize_package_upload",
      arguments: input,
    });
    assert.equal(
      (replay.structuredContent as { result: { alreadyPublished: boolean } })
        .result.alreadyPublished,
      true,
    );

    const bootstrapCandidateRoot = join(root, "bootstrap-candidate"),
      bootstrapArtifactRoot = join(bootstrapCandidateRoot, "artifact");
    await mkdir(join(bootstrapArtifactRoot, "dist"), { recursive: true });
    const bootstrapBuild = digest("bootstrap-package-build"),
      bootstrapManifest: Host.ReleaseManifest = {
        ...manifest,
        componentId: "host-executor",
        kind: "native",
        version: "1.0.0",
        buildId: bootstrapBuild,
        connectsToHive: false,
        requirements: { ...manifest.requirements, hiveProtocol: null },
      };
    await writeFile(
      join(bootstrapArtifactRoot, "dist", "main.js"),
      "process.exit(0);\n",
    );
    await writeFile(
      join(bootstrapArtifactRoot, "dist", "health.js"),
      "process.exit(0);\n",
    );
    await writeFile(
      join(bootstrapArtifactRoot, "dist", "build-info.json"),
      JSON.stringify({
        schemaVersion: 1,
        componentId: bootstrapManifest.componentId,
        version: bootstrapManifest.version,
        buildId: bootstrapBuild,
      }),
    );
    await writeFile(
      join(bootstrapCandidateRoot, "component.json"),
      JSON.stringify(bootstrapManifest),
    );
    for (const file of portableNativeFiles(bootstrapManifest.componentId)) {
      const path = join(bootstrapArtifactRoot, file);
      await mkdir(dirname(path), { recursive: true });
      await writeFile(path, "Fixture payload; the test queues but never launches it.");
    }
    const bootstrapCandidate: Host.Candidate = {
      ...candidate,
      candidateId: bootstrapBuild,
      componentId: bootstrapManifest.componentId,
      buildId: bootstrapBuild,
      artifactRoot: bootstrapArtifactRoot,
      manifestPath: join(bootstrapCandidateRoot, "component.json"),
    };
    const bootstrapArchive = await createPackageArchive(bootstrapCandidate, {}),
      bootstrapInput = {
        componentId: bootstrapManifest.componentId,
        version: bootstrapManifest.version,
        buildId: bootstrapBuild,
        archiveHash: bootstrapArchive.hash,
        bytes: bootstrapArchive.bytes,
        manifest: bootstrapManifest,
      };
    const bootstrapAuthorization = await client.callTool({
      name: "hive_authorize_package_upload",
      arguments: bootstrapInput,
    });
    const bootstrapGrant = (
      bootstrapAuthorization.structuredContent as {
        result: { uploadToken: string; uploadPath: string };
      }
    ).result;
    const bootstrapUpload = await fetch(base + bootstrapGrant.uploadPath.replace("/ivy", ""), {
          method: "PUT",
          headers: {
            Authorization: "Bearer " + bootstrapGrant.uploadToken,
            "Content-Length": String(bootstrapArchive.bytes),
          },
          body: await readFile(bootstrapArchive.path),
        });
    assert.equal(bootstrapUpload.status, 201, await bootstrapUpload.text());
    const bootstrapHost: Host.HostConfig = {
      ...host,
      hostId: "bootstrap-package-consumer",
      runtimeRoot: join(root, "bootstrap-host-state"),
      artifactRoot: join(root, "bootstrap-host-artifacts"),
      stagingRoot: join(root, "bootstrap-host-staging"),
      instances: [
        {
          instanceId: "host-executor",
          serviceNodeId: "host-executor",
          componentId: "host-executor",
          enabled: true,
          engine: "process",
          credential: token,
          settings: {},
        },
      ],
    };
    bootstrapJournal = new HostJournal(bootstrapHost);
    const oldBootstrapBuild = digest("old-bootstrap-package"),
      oldBootstrapCandidate: Host.Candidate = {
        ...bootstrapCandidate,
        candidateId: oldBootstrapBuild,
        buildId: oldBootstrapBuild,
        artifactRoot: join(root, "old-bootstrap-artifact"),
        manifestPath: join(root, "old-bootstrap.json"),
      };
    bootstrapJournal.db
      .prepare("INSERT INTO candidates VALUES (?,?,?,?,?)")
      .run(
        oldBootstrapBuild,
        "host-executor",
        oldBootstrapBuild,
        JSON.stringify(oldBootstrapCandidate),
        JSON.stringify({
          ...bootstrapManifest,
          version: "0.9.0",
          buildId: oldBootstrapBuild,
        }),
      );
    bootstrapJournal.db.prepare("INSERT INTO installed VALUES (?,?)").run(
      "host-executor",
      JSON.stringify({
        instanceId: "host-executor",
        candidateId: oldBootstrapBuild,
        buildId: oldBootstrapBuild,
        enabled: true,
        installedAt: new Date().toISOString(),
      }),
    );
    const bootstrapUpdater = new PackageUpdater(
        bootstrapHost,
        bootstrapJournal,
        token,
      ),
      bootstrapResult = await bootstrapUpdater.sync();
    assert.deepEqual(
      bootstrapResult.bootstrap.map((value) => [
        value.instanceId,
        value.candidate.candidateId,
      ]),
      [["host-executor", bootstrapBuild]],
    );
    assert.equal(
      bootstrapJournal.list().length,
      0,
      "bootstrap packages enter stopped-owner maintenance instead of the normal deployment queue",
    );
    assert.deepEqual(
      (await bootstrapUpdater.sync()).bootstrap.map(
        (value) => value.candidate.candidateId,
      ),
      [bootstrapBuild],
      "a 204 retains the pending bootstrap handoff",
    );
  },
);

test(
  "an accepted ui package is installed into the Hive ui catalog",
  { timeout: 30_000 },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "ivy-ui-package-")),
      candidateRoot = join(root, "candidate"),
      artifactRoot = join(candidateRoot, "artifact");
    const appRoot = join(artifactRoot, "dist", "apps", "fixture-ui");
    await mkdir(appRoot, { recursive: true });
    const definition = {
      metadata: {
        uiId: "fixture-ui",
        displayName: "Fixture",
        description: "Package-installed fixture ui.",
        iconKey: "ui" as const,
      },
      entryPath: "index.html",
      requirements: { hiveProtocol: 1 as const, contracts: [], services: [] },
      dataContracts: [],
    };
    await writeFile(
      join(appRoot, "index.html"),
      "<!doctype html><title>fixture ui</title>",
    );
    await writeFile(join(appRoot, "ivy-ui.json"), JSON.stringify(definition));
    const buildId = digest("fixture-ui-package");
    const manifest: Host.ReleaseManifest = {
      schemaVersion: 1,
      componentId: "fixture-ui",
      kind: "app",
      version: "1.0.0",
      buildId,
      connectsToHive: false,
      requirements: {
        node: ">=24.18.0 <25.0.0",
        hiveProtocol: 1,
        contracts: [],
      },
      app: {
        appId: "fixture-ui",
        dist: "dist/apps/fixture-ui",
        entryPath: "index.html",
      },
    };
    await writeFile(
      join(artifactRoot, "dist", "build-info.json"),
      JSON.stringify({
        schemaVersion: 1,
        componentId: manifest.componentId,
        version: manifest.version,
        buildId,
      }),
    );
    await writeFile(
      join(candidateRoot, "component.json"),
      JSON.stringify(manifest),
    );
    const candidate: Host.Candidate = {
      candidateId: buildId,
      componentId: manifest.componentId,
      buildId,
      artifactRoot,
      manifestPath: join(candidateRoot, "component.json"),
      createdAt: new Date().toISOString(),
      platform: {
        os: process.platform as "win32" | "linux",
        arch: process.arch as "x64" | "arm64",
        node: process.version,
      },
    };
    const archive = await createPackageArchive(candidate, {}),
      token = "ui-package-publisher";
    const filename = join(root, "hive.sqlite"),
      legacy = new HiveKernel({
        filename,
        publicBaseUrl: "http://127.0.0.1/ivy",
        version: "legacy",
        buildId: digest("legacy-ui-package-hive"),
        credentials: [{ principalId: "publisher", digest: digest(token) }],
      });
    const legacyReleaseId = digest("legacy-ui-release").slice(7);
    legacy.store.run(
      "INSERT INTO apps VALUES (?,?,?,NULL)",
      "fixture-ui",
      JSON.stringify({
        appId: "fixture-ui",
        displayName: "Fixture",
        description: "Legacy fixture",
        iconKey: "ui",
      }),
      legacyReleaseId,
    );
    legacy.store.run(
      "INSERT INTO app_releases VALUES (?,?,?)",
      "fixture-ui",
      legacyReleaseId,
      JSON.stringify({
        releaseId: legacyReleaseId,
        entryPath: "index.html",
        requirements: { hiveProtocol: 1, contracts: [], tools: [] },
        assets: [],
      }),
    );
    legacy.close();
    const server = new HiveServer({
      filename,
      packageRoot: join(root, "packages"),
      publicBaseUrl: "http://127.0.0.1/ivy",
      version: "test",
      buildId: digest("ui-package-hive"),
      credentials: [{ principalId: "publisher", digest: digest(token) }],
      packagePublisherPrincipalIds: ["publisher"],
      listenHost: "127.0.0.1",
      listenPort: 0,
    });
    const address = await server.start(),
      base = `http://127.0.0.1:${address.port}/ivy`;
    t.after(async () => {
      await server.close();
      await rm(root, {
        recursive: true,
        force: true,
        maxRetries: 5,
        retryDelay: 100,
      });
    });
    const mcp = new Client({ name: "ui-package-test", version: "1.0.0" }),
      transport = new StreamableHTTPClientTransport(new URL(base + "/mcp-dev"), {
        requestInit: { headers: { Authorization: "Bearer " + token } },
      });
    await mcp.connect(transport as unknown as Transport);
    t.after(() => mcp.close());
    const authorization = await mcp.callTool({
      name: "hive_authorize_package_upload",
      arguments: {
        componentId: manifest.componentId,
        version: manifest.version,
        buildId,
        archiveHash: archive.hash,
        bytes: archive.bytes,
        manifest,
      },
    });
    const grant = (
      authorization.structuredContent as {
        result: { uploadToken: string; uploadPath: string };
      }
    ).result;
    const uploaded = await fetch(base + grant.uploadPath.replace("/ivy", ""), {
      method: "PUT",
      headers: {
        Authorization: "Bearer " + grant.uploadToken,
        "Content-Length": String(archive.bytes),
      },
      body: await readFile(archive.path),
    });
    assert.equal(uploaded.status, 201);
    const expected = await readUiBundle(appRoot, definition),
      installed = await new HiveClient(base, { credential: token }).request(
        "uis.get",
        { uiId: "fixture-ui" },
      );
    assert.equal(installed.currentReleaseId, expected.releaseId);
    assert.equal(installed.releases.length, 1);
    assert.ok(installed.releases[0]!.assets.every((asset) => !("objectId" in asset)));
    assert.equal((await stat(join(root, "ui-releases", "fixture-ui", expected.releaseId, "index.html"))).isFile(), true);
    const entry = await fetch(base + "/ui/fixture-ui/", { headers: { Authorization: "Bearer " + token } });
    assert.equal(entry.status, 200);
    assert.equal(await entry.text(), await readFile(join(appRoot, "index.html"), "utf8"));
    const hostCatalog = (await (
      await fetch(base + "/api/v1/packages/catalog?after=0", {
        headers: { Authorization: "Bearer " + token },
      })
    ).json()) as { revision: number; packages: unknown[] };
    assert.equal(hostCatalog.revision, 1);
    assert.deepEqual(
      hostCatalog.packages,
      [],
      "host polling omits Hive-installed ui packages",
    );
    const catalog = (await (
      await fetch(base + "/api/v1/packages/catalog?after=0&include=all", {
        headers: { Authorization: "Bearer " + token },
      })
    ).json()) as {
      packages: Array<{ componentId: string; manifest: { kind: string } }>;
    };
    assert.deepEqual(
      catalog.packages.map((value) => [value.componentId, value.manifest.kind]),
      [["fixture-ui", "app"]],
    );
  },
);
