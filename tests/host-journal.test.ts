import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { tmpdir } from "node:os";
import { join, relative, resolve } from "node:path";
import {
  HostJournal,
  ExecutorLock,
  requiresBootstrapReplacement,
} from "../packages/host-runtime/src/journal.js";
import { parallelDeployments } from "../packages/host-runtime/src/executor.js";
import { hostConfig } from "../packages/host-runtime/src/host-config.js";
import { atomicJson } from "../packages/host-runtime/src/config.js";
import { runCommand } from "../packages/host-runtime/src/process.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import {
  canonical,
  digest,
  hashJson,
} from "../packages/contracts/src/canonical.js";
import type { Host, Wire } from "../packages/contracts/src/generated.js";

async function fixture(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), "ivy-host-journal-"));
  const config: Host.HostConfig = {
    schemaVersion: 1,
    hostId: "journal-test",
    runtimeRoot: join(root, "state"),
    artifactRoot: join(root, "artifacts"),
    stagingRoot: join(root, "staging"),
    publicBaseUrl: "http://127.0.0.1:39081/ivy",
    executables: { node: process.execPath },
    instances: [
      {
        instanceId: "fixture",
        serviceNodeId: "fixture-node",
        componentId: "fixture",
        enabled: true,
        engine: "process",
        settings: {},
      },
    ],
  };
  const configPath = join(root, "config.json");
  await atomicJson(configPath, config);
  await hostConfig(configPath);
  const journal = new HostJournal(config);
  t.after(async () => {
    journal.close();
    assert.ok(relative(tmpdir(), root).startsWith("ivy-host-journal-"));
    await rm(root, { recursive: true, force: true });
  });
  const plan = JSON.parse(
    await readFile(resolve("specs/examples/component.json"), "utf8"),
  ) as Host.BuildPlan;
  const buildId = digest("synthetic-code");
  const manifest: Host.ReleaseManifest = {
    schemaVersion: 1,
    componentId: "fixture",
    kind: plan.kind,
    version: plan.version,
    buildId,
    connectsToHive: plan.connectsToHive,
    requirements: plan.requirements,
    entrypoint: plan.entrypoint!,
    readiness: plan.readiness!,
    shutdown: plan.shutdown!,
    restart: plan.restart!,
    ...(plan.storage ? { storage: plan.storage } : {}),
  };
  const candidate: Host.Candidate = {
    candidateId: buildId,
    componentId: "fixture",
    artifactRoot: join(config.artifactRoot, "candidate"),
    manifestPath: join(config.artifactRoot, "manifest.json"),
    createdAt: new Date().toISOString(),
    buildId,
    platform: {
      os: process.platform as "win32" | "linux",
      arch: process.arch as "x64" | "arm64",
      node: process.version,
    },
  };
  journal.saveCandidate(candidate, manifest);
  return { root, config, configPath, journal, candidate, manifest };
}
const code = (expected: string) => (error: unknown) =>
  error instanceof IvyError && error.code === expected;

test("normal deployment excludes exactly the OS-owned bootstrap roots", () => {
  assert.equal(
    requiresBootstrapReplacement({ componentId: "host-executor" }),
    true,
  );
  assert.equal(
    requiresBootstrapReplacement({ componentId: "service-manager" }),
    process.platform === "win32",
  );
  assert.equal(
    requiresBootstrapReplacement({ componentId: "phone-bridge" }),
    false,
  );
});

test("host action replay retains first IDs, arguments and timestamps across clients and configuration changes", async (t) => {
  const f = await fixture(t),
    request = {
      action: "deploy" as const,
      operationId: "stable-action",
      instanceId: "fixture",
      candidateId: f.candidate.candidateId,
    };
  const first = f.journal.accept(request);
  assert.deepEqual(f.journal.accept(request), first);
  assert.throws(
    () => f.journal.accept({ ...request, action: "restart" }),
    code("mutation_conflict"),
  );
  const other = new HostJournal({ ...f.config, instances: [] });
  try {
    assert.deepEqual(other.accept(request), first);
  } finally {
    other.close();
  }
  assert.throws(
    () =>
      f.journal.accept({
        ...request,
        operationId: "wrong-target",
        instanceId: "absent",
      }),
    code("not_found"),
  );
  assert.throws(
    () => new HostJournal({ ...f.config, hostId: "different-host" }),
    code("target_conflict"),
  );
});

test(
  "two real CLI-like processes accept one durable action through the same host journal",
  { timeout: 15_000 },
  async (t) => {
    const f = await fixture(t),
      module = new URL(
        "../packages/host-runtime/src/journal.js",
        import.meta.url,
      ).href;
    const script = join(f.root, "accept.mjs");
    const request = {
      action: "deploy",
      operationId: "concurrent-intent",
      instanceId: "fixture",
      candidateId: f.candidate.candidateId,
    };
    await writeFile(
      script,
      `import {readFileSync} from 'node:fs';import {HostJournal} from ${JSON.stringify(module)};const journal=new HostJournal(JSON.parse(readFileSync(${JSON.stringify(f.configPath)},'utf8')));try{console.log(JSON.stringify(journal.accept(${JSON.stringify(request)})));}finally{journal.close();}`,
    );
    const command = { executable: "node", args: [script], timeoutMs: 8000 };
    const results = await Promise.all([
      runCommand(command, resolve("."), { node: process.execPath }),
      runCommand(command, resolve("."), { node: process.execPath }),
    ]);
    assert.deepEqual(
      JSON.parse(results[0]!.stdout),
      JSON.parse(results[1]!.stdout),
    );
    assert.equal(f.journal.unfinished().length, 1);
  },
);

test("journal phase CAS rejects stale and impossible transitions and cannot reinterpret a terminal result", async (t) => {
  const f = await fixture(t),
    accepted = f.journal.accept({
      action: "deploy",
      operationId: "phase-cas",
      instanceId: "fixture",
      candidateId: f.candidate.candidateId,
    });
  const id = accepted.record.deploymentId;
  assert.throws(
    () => f.journal.advance(id, "prepared", "succeeded"),
    code("revision_conflict"),
  );
  f.journal.advance(id, "prepared", "checking");
  assert.throws(
    () => f.journal.advance(id, "prepared", "checking"),
    code("revision_conflict"),
  );
  f.journal.advance(id, "checking", "draining");
  f.journal.advance(id, "draining", "activating");
  const uncertain = f.journal.advance(id, "activating", "needs_attention", {
    readiness: {
      state: "unknown",
      message: "Observed instance cannot be identified safely.",
    },
    errorCode: "outcome_unknown",
  });
  assert.equal(uncertain.record.observedBuild, null);
  assert.equal(f.journal.unfinished().length, 0);
  assert.throws(
    () => f.journal.advance(id, "needs_attention", "succeeded"),
    code("revision_conflict"),
  );
  assert.deepEqual(f.journal.accept(accepted.request), uncertain);
});

test(
  "executor OS lock excludes another live opener and releases independently of the operation journal",
  { timeout: 10_000 },
  async (t) => {
    const f = await fixture(t),
      lock = new ExecutorLock(f.config.runtimeRoot);
    try {
      assert.throws(
        () => new ExecutorLock(f.config.runtimeRoot),
        code("executor_already_running"),
      );
      assert.equal(f.journal.list().length, 0);
    } finally {
      lock.close();
    }
    const after = new ExecutorLock(f.config.runtimeRoot);
    after.close();
  },
);

test("host roots cannot alias and release identity cannot change in the activation catalog", async (t) => {
  const f = await fixture(t);
  const invalid = join(f.root, "overlap.json");
  await atomicJson(invalid, {
    ...f.config,
    stagingRoot: join(f.config.artifactRoot, "staging"),
  });
  await assert.rejects(hostConfig(invalid), code("invalid_arguments"));
  const other = { ...f.candidate, createdAt: "2026-09-05T00:00:00.000Z" };
  assert.throws(
    () => f.journal.saveCandidate(other, f.manifest),
    code("mutation_conflict"),
  );
  assert.throws(
    () =>
      f.journal.saveCandidate(other, {
        ...f.manifest,
        buildId: digest("different-release"),
      }),
    code("build_mismatch"),
  );
});

test("accepted configuration is one private direct file and keeps credentials out of journal records", async (t) => {
  const f = await fixture(t),
    configured = structuredClone(f.config),
    secret = "isolated-configuration-token-value";
  configured.instances[0]!.credential = secret;
  const caller = new HostJournal(configured);
  const entry = caller.accept({
    action: "deploy",
    operationId: "private-snapshot",
    instanceId: "fixture",
    candidateId: f.candidate.candidateId,
  });
  caller.close();
  assert.deepEqual(
    f.journal.acceptedConfiguration(entry.configurationPath),
    configured,
  );
  assert.equal(JSON.stringify(f.journal.list()).includes(secret), false);
  for (const row of f.journal.db
    .prepare("SELECT name FROM sqlite_master WHERE type='table'")
    .all()) {
    const name = String(row["name"]);
    assert.match(name, /^[a-z_]+$/);
    assert.equal(
      JSON.stringify(
        f.journal.db.prepare("SELECT * FROM " + name).all(),
      ).includes(secret),
      false,
    );
  }
  assert.equal(
    entry.configurationPath.startsWith(
      join(configured.runtimeRoot, "accepted-configurations"),
    ),
    true,
  );
  await writeFile(entry.configurationPath, "{ damaged configuration");
  assert.throws(
    () => f.journal.acceptedConfiguration(entry.configurationPath),
    code("configuration_changed"),
  );
  const other = {
    ...f.config,
    executables: { node: join(f.root, "different-node") },
  };
  assert.throws(() => new HostJournal(other), code("target_conflict"));
});

test("multiple local manager readers allocate one durable report sequence and reconcile a restored reporting cursor", async (t) => {
  const f = await fixture(t),
    other = new HostJournal(f.config);
  try {
    const first = f.journal.managementSnapshot(null),
      second = other.managementSnapshot(null);
    assert.equal(first.sequence, 1);
    assert.equal(second.sequence, 2);
    assert.equal(other.managementSnapshot(null, 400).sequence, 401);
    assert.equal(f.journal.managementSnapshot(null).sequence, 402);
    assert.equal(first.status.instances[0]!.desiredEnabled, true);
    assert.equal(
      Object.hasOwn(first.status.instances[0]!, "credential"),
      false,
    );
  } finally {
    other.close();
  }
});

test("status projects current owner health and never exposes stale ready observations", async (t) => {
  const f = await fixture(t),
    bootId = "00000000-0000-4000-8000-000000000001",
    startedAt = new Date(Date.now() - 20_000).toISOString();
  const health: Host.Health = {
    schemaVersion: 1,
    instanceId: "fixture",
    componentId: "fixture",
    buildId: f.candidate.buildId,
    pid: process.pid,
    bootId,
    startedAt,
    observedAt: startedAt,
    ready: true,
    generation: null,
    details: "",
  };
  await atomicJson(
    join(f.config.runtimeRoot, "instances", "fixture", "data", "health.json"),
    health,
  );
  const staleExecutor: Host.ExecutorStatus = {
    schemaVersion: 1,
    hostId: f.config.hostId,
    pid: process.pid,
    bootId,
    observedAt: startedAt,
    state: "ready",
    activeDeploymentId: null,
    code: null,
    buildId: null,
  };
  const stale = f.journal.status(staleExecutor);
  assert.equal(stale.executor, null);
  assert.equal(stale.instances[0]!.observedState, "unknown");
  assert.equal(stale.instances[0]!.code, "health_stale");
  const current = { ...health, observedAt: new Date().toISOString() };
  await atomicJson(
    join(f.config.runtimeRoot, "instances", "fixture", "data", "health.json"),
    current,
  );
  const status = f.journal.status(null);
  assert.equal(status.instances[0]!.observedState, "ready");
  assert.equal(status.instances[0]!.observedBuild, f.candidate.buildId);
  f.config.instances.push({
    ...f.config.instances[0]!,
    instanceId: "executor",
    serviceNodeId: "executor",
    componentId: "host-executor",
  });
  f.journal.db.prepare("INSERT INTO installed VALUES (?,?)").run(
    "executor",
    JSON.stringify({
      instanceId: "executor",
      candidateId: f.candidate.candidateId,
      buildId: f.candidate.buildId,
      enabled: true,
      installedAt: startedAt,
    }),
  );
  const actual = {
    ...staleExecutor,
    observedAt: new Date().toISOString(),
    buildId: digest("different-running-executor"),
  };
  const mismatch = f.journal.currentObservations(actual)["executor"]!;
  assert.equal(mismatch.buildId, actual.buildId);
  assert.equal(mismatch.state, "unknown");
  assert.equal(mismatch.code, "build_mismatch");
  assert.equal(
    f.journal.currentObservations({ ...actual, buildId: f.candidate.buildId })[
      "executor"
    ]!.state,
    "ready",
  );
  assert.equal(
    f.journal.currentObservations(staleExecutor)["executor"]!.buildId,
    null,
  );
});

test("format-1 journal keeps terminal history, adopts retained candidates and fences unfinished legacy activation", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-host-journal-legacy-"));
  t.after(async () => {
    assert.ok(relative(tmpdir(), root).startsWith("ivy-host-journal-legacy-"));
    await rm(root, { recursive: true, force: true });
  });
  const config: Host.HostConfig = {
    schemaVersion: 1,
    hostId: "legacy-host",
    runtimeRoot: join(root, "state"),
    artifactRoot: join(root, "artifacts"),
    stagingRoot: join(root, "staging"),
    publicBaseUrl: "http://127.0.0.1:39081/ivy",
    executables: { node: process.execPath },
    instances: [
      {
        instanceId: "fixture",
        serviceNodeId: "fixture-node",
        componentId: "fixture",
        enabled: true,
        engine: "process",
        settings: {},
      },
    ],
  };
  const initial = new HostJournal(config);
  initial.close();
  const plan = JSON.parse(
    await readFile(resolve("specs/examples/component.json"), "utf8"),
  ) as Host.BuildPlan;
  const buildId = digest("legacy-build"),
    candidateId = digest("legacy-candidate"),
    configurationHash = hashJson(config),
    now = new Date().toISOString();
  const manifest = {
    schemaVersion: 1,
    componentId: "fixture",
    kind: plan.kind,
    version: plan.version,
    buildId,
    connectsToHive: plan.connectsToHive,
    requirements: plan.requirements,
    entrypoint: plan.entrypoint!,
    readiness: plan.readiness!,
    shutdown: plan.shutdown!,
    restart: plan.restart!,
    artifactManifestHash: digest("removed-inventory"),
  };
  const candidate = {
    candidateId,
    componentId: "fixture",
    sourceRoot: join(root, "old-source"),
    artifactRoot: join(config.artifactRoot, "legacy"),
    manifestPath: join(config.artifactRoot, "legacy/component.json"),
    createdAt: now,
    platform: {
      os: process.platform,
      arch: process.arch,
      node: process.version,
    },
    archivePath: join(config.artifactRoot, "legacy/artifact.tar.gz"),
    archiveHash: digest("archive"),
    buildId,
    fileManifestHash: digest("files"),
  };
  const request: Host.LocalRequest = {
    action: "deploy",
    operationId: "legacy-unfinished",
    instanceId: "fixture",
    candidateId,
  };
  const record: Wire.DeploymentRecord = {
    deploymentId: "legacy-deployment",
    hostId: config.hostId,
    instanceId: "fixture",
    componentId: "fixture",
    requestHash: hashJson(request),
    previousBuild: null,
    targetBuild: buildId,
    observedBuild: buildId,
    phase: "activating",
    createdAt: now,
    updatedAt: now,
    readiness: {
      state: "passed",
      message: "Old owner observed the launch before losing its reply.",
    },
  };
  await mkdir(join(config.runtimeRoot, "host-configurations"), {
    recursive: true,
  });
  await writeFile(
    join(
      config.runtimeRoot,
      "host-configurations",
      configurationHash.slice(7) + ".json",
    ),
    canonical(config) + "\n",
  );
  const db = new DatabaseSync(join(config.runtimeRoot, "deployments.sqlite"));
  db.prepare("DELETE FROM meta WHERE key='installation'").run();
  db.prepare("INSERT OR REPLACE INTO meta VALUES (?,?)").run(
    "configurationAnchor",
    configurationHash,
  );
  db.prepare("INSERT INTO candidates VALUES (?,?,?,?,?)").run(
    candidateId,
    "fixture",
    buildId,
    canonical(candidate),
    canonical(manifest),
  );
  db.prepare("INSERT INTO runtime_targets VALUES (?,?)").run(
    "fixture",
    canonical({
      schemaVersion: 1,
      instanceId: "fixture",
      revision: "legacy-revision",
      candidateId,
      desired: "running",
      configPath: join(root, "old-instance.json"),
      configHash: digest("instance"),
      hostConfigHash: configurationHash,
      configMode: "exact",
      requestedAt: now,
    }),
  );
  const oldEntry = {
    schemaVersion: 1,
    request,
    record,
    candidateId,
    previousCandidateId: null,
    configurationHash,
    steps: [
      {
        name: "launch",
        state: "observed",
        at: now,
        evidence: "old observed result",
      },
    ],
  };
  db.prepare("INSERT INTO operations VALUES (?,?,?,?,?,?)").run(
    request.operationId,
    record.deploymentId,
    record.requestHash,
    record.phase,
    now,
    canonical(oldEntry),
  );
  const insertOperation = db.prepare(
    "INSERT INTO operations VALUES (?,?,?,?,?,?)",
  );
  for (let index = 0; index < 200; index += 1) {
    const operationId = `legacy-older-${index.toString().padStart(3, "0")}`,
      deploymentId = `${operationId}-deployment`;
    const createdAt = new Date(
      Date.parse(now) - (index + 1) * 1_000,
    ).toISOString();
    const legacyRequest = { ...request, operationId },
      legacyRecord = {
        ...record,
        deploymentId,
        requestHash: hashJson(legacyRequest),
        createdAt,
        updatedAt: createdAt,
      };
    insertOperation.run(
      operationId,
      deploymentId,
      legacyRecord.requestHash,
      legacyRecord.phase,
      createdAt,
      canonical({ ...oldEntry, request: legacyRequest, record: legacyRecord }),
    );
  }
  const terminal = {
    ...oldEntry,
    request: { ...request, operationId: "legacy-terminal" },
    record: {
      ...record,
      deploymentId: "legacy-terminal-deployment",
      requestHash: hashJson({ ...request, operationId: "legacy-terminal" }),
      phase: "succeeded",
    },
  };
  db.prepare("INSERT INTO operations VALUES (?,?,?,?,?,?)").run(
    terminal.request.operationId,
    terminal.record.deploymentId,
    terminal.record.requestHash,
    terminal.record.phase,
    now,
    canonical(terminal),
  );
  const originalRows = db
    .prepare(
      "SELECT operation_id,phase,entry_json FROM operations ORDER BY operation_id",
    )
    .all();
  db.close();
  let journal: HostJournal | null = new HostJournal(config);
  try {
    const fenced = journal.get(record.deploymentId);
    assert.equal(fenced.record.phase, "needs_attention");
    assert.equal(
      fenced.record.errorCode,
      "legacy_activation_requires_reconciliation",
    );
    assert.equal(fenced.record.observedBuild, buildId);
    assert.deepEqual(fenced.record.readiness, record.readiness);
    const retainedStatus = journal.status(null).unfinished;
    assert.equal(retainedStatus.length, 200);
    assert.ok(
      retainedStatus.every((value) => value.phase === "needs_attention"),
    );
    assert.equal(journal.unfinished().length, 0);
    assert.equal(parallelDeployments(journal.unfinished(), []).length, 0);
    assert.deepEqual(
      journal.accept(request),
      fenced,
      "retry reads the unresolved result without admitting another action",
    );
    assert.equal(
      journal.get(terminal.record.deploymentId).record.phase,
      "succeeded",
    );
    assert.deepEqual(
      journal.db
        .prepare(
          "SELECT operation_id,phase,entry_json FROM operations ORDER BY operation_id",
        )
        .all(),
      originalRows,
    );
    assert.equal(
      JSON.parse(
        String(
          journal.db
            .prepare("SELECT entry_json FROM operations WHERE deployment_id=?")
            .get(record.deploymentId)!["entry_json"],
        ),
      ).steps[0].evidence,
      "old observed result",
    );
    journal.close();
    journal = new HostJournal(config);
    assert.deepEqual(
      journal.db
        .prepare(
          "SELECT operation_id,phase,entry_json FROM operations ORDER BY operation_id",
        )
        .all(),
      originalRows,
    );
    assert.deepEqual(journal.candidate(candidateId), {
      candidateId,
      componentId: "fixture",
      artifactRoot: candidate.artifactRoot,
      manifestPath: candidate.manifestPath,
      createdAt: now,
      platform: candidate.platform,
      archivePath: candidate.archivePath,
      archiveHash: candidate.archiveHash,
      buildId,
    });
    assert.equal(
      Object.hasOwn(journal.manifest(candidateId), "artifactManifestHash"),
      false,
    );
    assert.equal(
      Object.hasOwn(journal.target("fixture")!, "configHash"),
      false,
    );
    const accepted = journal.accept({
      action: "rollback",
      operationId: "use-retained-build",
      instanceId: "fixture",
      targetBuild: buildId,
    });
    assert.equal(accepted.candidateId, candidateId);
    assert.deepEqual(
      journal.unfinished().map((value) => value.record.deploymentId),
      [accepted.record.deploymentId],
      "the execution limit applies after legacy entries have been excluded",
    );
    assert.deepEqual(
      parallelDeployments(journal.unfinished(), []).map(
        (value) => value.record.deploymentId,
      ),
      [accepted.record.deploymentId],
    );
    const rowsAfterAcceptance = journal.db
      .prepare(
        "SELECT operation_id,phase,entry_json FROM operations ORDER BY operation_id",
      )
      .all();
    journal.close();
    journal = new HostJournal(config);
    assert.deepEqual(
      journal.db
        .prepare(
          "SELECT operation_id,phase,entry_json FROM operations ORDER BY operation_id",
        )
        .all(),
      rowsAfterAcceptance,
    );
    assert.deepEqual(
      journal.unfinished().map((value) => value.record.deploymentId),
      [accepted.record.deploymentId],
    );
  } finally {
    journal?.close();
  }
});
