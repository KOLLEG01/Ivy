import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import {
  mkdtemp,
  mkdir,
  readFile,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { randomUUID } from "node:crypto";
import { atomicJson } from "../packages/host-runtime/src/config.js";
import { hostConfig } from "../packages/host-runtime/src/host-config.js";
import {
  bootstrapResetUis,
  configuredScopeRootIds,
  coordinatedRuntimeResetPlans,
  localRuntimeResetPlan,
  releaseResetHosts,
  republishResetUis,
  resetUiBundles,
  resetBootstrapContracts,
  restoredBootstrapTarget,
  retainedResetIdentity,
  runtimeReset,
} from "../packages/host-runtime/src/runtime-reset.js";
import { bootstrapLaunchId } from "../packages/host-runtime/src/journal.js";
import { acceptedInstanceConfigurationPath } from "../packages/host-runtime/src/accepted-configuration.js";
import { servicePaths } from "../packages/host-runtime/src/layout.js";
import type { Host } from "../packages/contracts/src/generated.js";
import { HiveStore } from "../services/hive/src/store.js";
import { Objects } from "../services/hive/src/objects.js";
import type { RuntimeResetPlan } from "../packages/host-runtime/src/runtime-reset.js";
import { HiveKernel } from "../services/hive/src/kernel.js";
import { digest } from "../packages/contracts/src/canonical.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import { operationId } from "../packages/contracts/src/operation-id.js";
import type {
  Operation,
  OperationName,
  Params,
  Result,
} from "../packages/contracts/src/generated.js";
import type { RpcClient } from "../packages/sdk/src/client.js";

const code = (expected: string) => (error: unknown) =>
  error instanceof IvyError && error.code === expected;

async function fixture(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), "ivy-reset-")),
    distribution = join(root, "distribution"),
    configPath = join(root, "config.json");
  const input: Host.HostConfigInput = {
    schemaVersion: 1,
    hostId: "reset-host",
    ivyRoot: join(root, "ivy"),
    publicBaseUrl: "http://127.0.0.1:39081/ivy",
    executables: { node: process.execPath },
    deployment: {
      identity: "reference-rollout",
      hosts: [{ hostId: "reset-host", configPath }],
    },
    instances: [
      {
        instanceId: "worker",
        serviceNodeId: "worker",
        componentId: "fixture",
        enabled: false,
        engine: "process",
        credential: "reset-fixture-token",
        settings: {
          runtimeReset: {
            coordinatorHostId: "reset-host",
            uiIds: ["reset-fixture"],
          },
        },
      },
    ],
  };
  await atomicJson(configPath, input);
  const config = await hostConfig(configPath),
    paths = servicePaths(config, "worker");
  await mkdir(join(distribution, "services/fixture"), { recursive: true });
  const component: Host.BuildPlan = {
    schemaVersion: 1,
    componentId: "fixture",
    kind: "service",
    version: "1.0.0",
    description: "Reset planning fixture.",
    connectsToHive: false,
    requirements: {
      node: ">=24.18.0 <25.0.0",
      hiveProtocol: null,
      contracts: [],
    },
    prepare: [],
    checks: [],
    entrypoint: {
      executable: "node",
      args: ["dist/fixture.js"],
      timeoutMs: 5000,
    },
    readiness: {
      timeoutMs: 5000,
      command: {
        executable: "node",
        args: ["dist/health.js"],
        timeoutMs: 1000,
      },
    },
    shutdown: { timeoutMs: 5000 },
    restart: { policy: "never", minimumDelayMs: 1000, maximumDelayMs: 1000 },
    runtimeReset: {
      delete: [
        { area: "data", path: "journal.sqlite" },
        { area: "work", path: "." },
        { area: "logs", path: "." },
      ],
      preserve: [{ area: "data", path: "settings.json" }],
      completionMarker: "runtime-reset.json",
    },
  };
  await atomicJson(
    join(distribution, "services/fixture/deploy.json"),
    component,
  );
  await writeFile(join(paths.data, "settings.json"), "settings");
  await writeFile(join(paths.data, "journal.sqlite"), "runtime");
  const ui = join(distribution, "dist/uis/reset-fixture");
  await mkdir(ui, { recursive: true });
  const definition: Operation.UiDefinition = {
    metadata: {
      uiId: "reset-fixture",
      displayName: "Reset fixture",
      description: "Reset preview fixture.",
      iconKey: "ui",
    },
    entryPath: "index.html",
    requirements: { hiveProtocol: 1, contracts: [], services: [] },
    dataContracts: [],
  };
  await writeFile(join(ui, "ivy-ui.json"), JSON.stringify(definition));
  await writeFile(
    join(ui, "index.html"),
    "<!doctype html><h1>Reset fixture</h1>",
  );
  t.after(() => rm(root, { recursive: true, force: true }));
  return { root, distribution, config, configPath, paths };
}

test("runtime reset preview resolves only fixed component paths and hashes preserved settings", async (t) => {
  const f = await fixture(t),
    plan = await localRuntimeResetPlan(f.config, f.configPath, f.distribution);
  assert.equal(plan.deploymentIdentity, "reference-rollout");
  assert.deepEqual(plan.instances, [
    { instanceId: "worker", componentId: "fixture", enabled: false },
  ]);
  assert.deepEqual(
    plan.delete.map((value) => value.path),
    ["journal.sqlite", ".", "."],
  );
  assert.equal(plan.preserve[0]?.path, "settings.json");
  assert.match(plan.preserve[0]?.checksum ?? "", /^sha256:/);
  assert.deepEqual(
    plan.completionMarkers.map((value) => ({
      instanceId: value.instanceId,
      path: value.path,
    })),
    [{ instanceId: "worker", path: "runtime-reset.json" }],
  );
  const preview = (await runtimeReset(
    f.config,
    f.configPath,
    f.distribution,
    false,
  )) as { mode: string; uis: { uiId: string }[] };
  assert.equal(preview.mode, "preview");
  assert.deepEqual(
    preview.uis.map((ui) => ui.uiId),
    ["reset-fixture"],
  );
});

test("runtime reset refuses a wrong typed deployment identity before changing data", async (t) => {
  const f = await fixture(t);
  await assert.rejects(
    runtimeReset(f.config, f.configPath, f.distribution, true, "wrong"),
    (error: unknown) =>
      (error as { code: string }).code === "invalid_arguments",
  );
  assert.equal(
    await readFile(join(f.paths.data, "journal.sqlite"), "utf8"),
    "runtime",
  );
  assert.equal(
    await readFile(join(f.paths.data, "settings.json"), "utf8"),
    "settings",
  );
});

test("runtime reset accepts only its configured coordinator and validates every ui before deletion", async (t) => {
  const f = await fixture(t),
    input = JSON.parse(
      await readFile(f.configPath, "utf8"),
    ) as Host.HostConfigInput;
  const reset = input.instances[0]!.settings["runtimeReset"] as {
    coordinatorHostId: string;
    uiIds: string[];
  };
  reset.coordinatorHostId = "other-host";
  await atomicJson(f.configPath, input);
  await assert.rejects(
    runtimeReset(
      await hostConfig(f.configPath),
      f.configPath,
      f.distribution,
      false,
    ),
    code("access_denied"),
  );
  reset.coordinatorHostId = "reset-host";
  reset.uiIds.push("missing-ui");
  await atomicJson(f.configPath, input);
  await assert.rejects(
    runtimeReset(
      await hostConfig(f.configPath),
      f.configPath,
      f.distribution,
      false,
    ),
    code("not_found"),
  );
  assert.equal(
    await readFile(join(f.paths.data, "journal.sqlite"), "utf8"),
    "runtime",
  );
});

test("runtime reset preview rejects a declared path that traverses a link", async (t) => {
  const f = await fixture(t),
    outside = join(f.root, "outside");
  await mkdir(outside);
  await symlink(
    outside,
    join(f.paths.data, "linked"),
    process.platform === "win32" ? "junction" : "dir",
  );
  const component = JSON.parse(
    await readFile(
      join(f.distribution, "services/fixture/deploy.json"),
      "utf8",
    ),
  ) as Host.BuildPlan;
  component.runtimeReset!.delete[0] = { area: "data", path: "linked/runtime" };
  await atomicJson(
    join(f.distribution, "services/fixture/deploy.json"),
    component,
  );
  await assert.rejects(
    localRuntimeResetPlan(f.config, f.configPath, f.distribution),
    (error: unknown) => (error as { code: string }).code === "target_conflict",
  );
});

test("runtime reset retains the launch identity of Linux bootstrap processes", () => {
  const plan: Host.BootstrapPlan = {
    schemaVersion: 1,
    os: "linux",
    hostId: "host",
    installationId: "ivy-next-0123456789ab",
    candidateId: "sha256:" + "1".repeat(64),
    artifactRoot: "/artifact",
    configPath: "/config.json",
    configHash: "sha256:" + "2".repeat(64),
    runtimeRoot: "/runtime",
    nodeExecutable: "/usr/bin/node",
    processes: [
      {
        instanceId: "service-manager",
        componentId: "service-manager",
        name: "ivy-next-0123456789ab-0123456789ab",
        candidateId: "sha256:" + "3".repeat(64),
        artifactRoot: "/service",
        entrypoint: {
          executable: "node",
          args: ["dist/main.js"],
          timeoutMs: 5000,
        },
        configPath: "/runtime/bootstrap/service-manager.json",
      },
    ],
  };
  const at = "2026-09-14T09:00:00.000Z",
    candidateId = plan.processes[0]!.candidateId!;
  assert.deepEqual(
    restoredBootstrapTarget(plan, "service-manager", candidateId, true, at),
    {
      schemaVersion: 1,
      instanceId: "service-manager",
      revision: bootstrapLaunchId(plan, "service-manager"),
      candidateId,
      desired: "running",
      configPath: "/runtime/bootstrap/service-manager.json",
      requestedAt: at,
    },
  );
  assert.match(
    acceptedInstanceConfigurationPath(
      { runtimeRoot: "/runtime" } as Host.HostConfig,
      bootstrapLaunchId(plan, "service-manager"),
    ),
    /[/\\]accepted-configurations[/\\]bootstrap-[0-9a-f]{64}\.instance\.json$/,
  );
  assert.equal(
    restoredBootstrapTarget(
      plan,
      "service-manager",
      "sha256:" + "4".repeat(64),
      true,
      at,
    ),
    null,
  );
});

test("an interrupted multi-host reset retains its shared identity until coordinator confirmation", () => {
  const planHash = "sha256:" + "1".repeat(64),
    previous = {
      resetId: "reset-one",
      runtimeEpoch: "epoch-one",
      phase: "complete" as const,
      planHash,
      deploymentComplete: false,
    };
  assert.deepEqual(retainedResetIdentity(previous, planHash), {
    resetId: "reset-one",
    runtimeEpoch: "epoch-one",
    resumeFromReleased: false,
  });
  assert.deepEqual(
    retainedResetIdentity({ ...previous, phase: "released" }, planHash),
    {
      resetId: "reset-one",
      runtimeEpoch: "epoch-one",
      resumeFromReleased: true,
    },
  );
  assert.equal(
    retainedResetIdentity({ ...previous, deploymentComplete: true }, planHash),
    null,
  );
  assert.equal(retainedResetIdentity(previous, "different-plan"), null);
});

test("runtime reset finds fixed scope roots nested in service settings", () => {
  const config = {
    instances: [
      {
        settings: {
          identity: { scope: { rootObjectId: "scope-b" } },
          destination: { rootObjectId: "scope-a" },
          optional: { rootObjectId: null },
        },
      },
      { settings: { rootObjectId: "scope-b" } },
    ],
  } as unknown as Pick<Host.HostConfig, "instances">;
  assert.deepEqual(configuredScopeRootIds(config), ["scope-a", "scope-b"]);
});

test("runtime reset gives every host the deployment-wide scope roots", () => {
  const base: Omit<RuntimeResetPlan, "hostId" | "scopeRootIds"> = {
    schemaVersion: 1,
    deploymentIdentity: "reference-rollout",
    configPath: "/config",
    instances: [],
    delete: [],
    preserve: [],
    completionMarkers: [],
  };
  const plans = coordinatedRuntimeResetPlans([
    { ...base, hostId: "one", scopeRootIds: ["scope-b"] },
    { ...base, hostId: "two", scopeRootIds: ["scope-a", "scope-b"] },
  ]);
  assert.deepEqual(
    plans.map((plan) => plan.scopeRootIds),
    [
      ["scope-a", "scope-b"],
      ["scope-a", "scope-b"],
    ],
  );
});

test("Hive reset bootstrap recreates empty stable scope roots idempotently", () => {
  const store = new HiveStore(":memory:");
  try {
    const objects = new Objects(store),
      at = "2026-09-14T12:00:00.000Z";
    objects.bootstrapScopeRoots(["scope-b", "scope-a"], at);
    objects.bootstrapScopeRoots(["scope-a", "scope-b"], at);
    for (const id of ["scope-a", "scope-b"]) {
      const value = objects.read({ objectId: id });
      assert.equal(value.object.id, id);
      assert.equal(value.object.parentId, null);
      assert.equal(value.object.contractKey, "ivy-reference-rollout/root");
      assert.equal(value.object.currentRevision, 1);
      assert.deepEqual(value.content, {
        encoding: "json",
        value: { schemaVersion: 1 },
      });
    }
    assert.equal(
      store.get("SELECT COUNT(*) AS count FROM objects")?.["count"],
      2,
    );
  } finally {
    store.close();
  }
});

test("a lost release response reprepares every host that may already be released", async () => {
  const hosts = [{ hostId: "one" }, { hostId: "two" }],
    released: string[] = [],
    reprepared: string[] = [];
  await assert.rejects(
    releaseResetHosts(
      hosts,
      async (host) => {
        released.push(host.hostId);
        if (host.hostId === "two")
          throw new IvyError(
            "outcome_unknown",
            "Synthetic lost release response.",
            "unknown",
          );
      },
      async (host) => {
        reprepared.push(host.hostId);
      },
    ),
    (error) =>
      error instanceof IvyError &&
      error.code === "outcome_unknown" &&
      JSON.stringify(error.details).includes("possiblyReleasedHosts"),
  );
  assert.deepEqual(released, ["one", "two"]);
  assert.deepEqual(reprepared, ["one", "two"]);
});

test("ui bootstrap stays fenced on every host and reprepares all possibly started hosts on failure", async () => {
  const hosts = [{ hostId: "one" }, { hostId: "two" }],
    events: string[] = [],
    reprepared: string[] = [];
  await bootstrapResetUis(
    hosts,
    async (host) => {
      events.push("bootstrap " + host.hostId);
    },
    async (host) => {
      reprepared.push(host.hostId);
    },
    async () => {
      events.push("uis ready");
    },
  );
  await releaseResetHosts(
    [...hosts].reverse(),
    async (host) => {
      events.push("release " + host.hostId);
    },
    async (host) => {
      reprepared.push(host.hostId);
    },
  );
  assert.deepEqual(events, [
    "bootstrap one",
    "bootstrap two",
    "uis ready",
    "release two",
    "release one",
  ]);
  assert.equal(reprepared.length, 0);
  await assert.rejects(
    bootstrapResetUis(
      hosts,
      async () => undefined,
      async (host) => {
        reprepared.push(host.hostId);
      },
      async () => {
        throw new IvyError("not_found", "Synthetic missing ui.");
      },
    ),
    code("not_found"),
  );
  assert.deepEqual(reprepared, ["one", "two"]);
});

test("runtime reset republishes every bundled ui and verifies readiness", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-reset-uis-")),
    directory = join(root, "dist/uis/reset-fixture");
  await mkdir(directory, { recursive: true });
  t.after(() => rm(root, { recursive: true, force: true }));
  const definition: Operation.UiDefinition = {
    metadata: {
      uiId: "reset-fixture",
      displayName: "Reset fixture",
      description: "Reset ui publication fixture.",
      iconKey: "ui",
    },
    entryPath: "index.html",
    requirements: {
      hiveProtocol: 1,
      contracts: [
        {
          key: "chat-bridge/input",
          readVersions: ["1.2.0"],
          writeVersions: [],
        },
      ],
      services: [],
    },
    dataContracts: [],
  };
  await writeFile(join(directory, "ivy-ui.json"), JSON.stringify(definition));
  await writeFile(
    join(directory, "index.html"),
    "<!doctype html><h1>Reset fixture</h1>",
  );
  const credential = {
      principalId: "reset-publication",
      digest: digest("reset-publication"),
    },
    kernel = new HiveKernel({
      filename: ":memory:",
      publicBaseUrl: "https://fixture.test",
      version: "fixture",
      buildId: digest("fixture"),
      credentials: [credential],
    });
  t.after(() => kernel.close());
  const client: RpcClient = {
    async request<M extends OperationName>(
      method: M,
      params: Params<M>,
    ): Promise<Result<M>> {
      const result = kernel.execute(
        {
          credentialDigest: credential.digest,
          principalId: credential.principalId,
          transport: "http",
        },
        { jsonrpc: "2.0", id: randomUUID(), method, params },
      );
      if (result.kind !== "result")
        throw new Error("Unexpected native dispatch.");
      return result.value as Result<M>;
    },
  };
  const bundles = await resetUiBundles(root, ["reset-fixture"]),
    bootstrapContracts = resetBootstrapContracts(bundles);
  new Objects(kernel.store).bootstrapServiceContracts(bootstrapContracts);
  assert.deepEqual(
    bootstrapContracts.map((contract) => contract.key),
    ["chat-bridge/input"],
  );
  await rm(join(directory, "index.html"));
  const issuedAt = Date.now(),
    published = await republishResetUis(
      client,
      bundles,
      operationId(kernel.store.runtimeEpoch, issuedAt, "runtime-reset-uis"),
    );
  assert.deepEqual(published, ["reset-fixture"]);
  assert.equal(
    (await client.request("uis.inspect", { uiId: "reset-fixture" })).status,
    "ready",
  );
  kernel.store.cleanupMutationReceipts(issuedAt + 24 * 60 * 60 * 1000);
  const resumed = await republishResetUis(
    client,
    bundles,
    operationId(
      kernel.store.runtimeEpoch,
      issuedAt + 24 * 60 * 60 * 1000,
      "runtime-reset-uis-resumed",
    ),
  );
  assert.deepEqual(resumed, ["reset-fixture"]);
  assert.equal(
    (await client.request("uis.inspect", { uiId: "reset-fixture" }))
      .currentReleaseId,
    bundles[0]!.checkedBundle.releaseId,
  );
});

test("Hive reset bootstrap accepts only the configured HTTP ui publisher", () => {
  const publisher = {
      principalId: "reset-publication",
      digest: digest("reset-publication"),
    },
    ordinary = {
      principalId: "ordinary-user",
      digest: digest("ordinary-user"),
    };
  const kernel = new HiveKernel({
    filename: ":memory:",
    publicBaseUrl: "https://fixture.test",
    version: "fixture",
    buildId: digest("fixture"),
    credentials: [publisher, ordinary],
    resetBootstrapCredentialDigest: publisher.digest,
  });
  try {
    const request = (
      credential: typeof publisher,
      method: OperationName,
      params: unknown,
      transport: "http" | "mcp" = "http",
    ) =>
      kernel.execute(
        {
          credentialDigest: credential.digest,
          principalId: credential.principalId,
          transport,
        },
        { jsonrpc: "2.0", id: randomUUID(), method, params },
      );
    assert.throws(
      () => request(ordinary, "objects.read", { objectId: "missing" }),
      code("maintenance_active"),
    );
    assert.throws(
      () => request(publisher, "contracts.get", { key: "missing" }),
      code("maintenance_active"),
    );
    assert.throws(
      () => request(publisher, "uis.inspect", { uiId: "missing" }, "mcp"),
      code("maintenance_active"),
    );
    assert.throws(
      () => request(publisher, "uis.inspect", { uiId: "missing" }),
      code("not_found"),
    );
  } finally {
    kernel.close();
  }
});
