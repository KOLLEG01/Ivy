import test from "node:test";
import assert from "node:assert/strict";
import { readFile, mkdir } from "node:fs/promises";
import { createServer } from "node:net";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { setTimeout as delay } from "node:timers/promises";
import { join, resolve } from "node:path";
import { hostFixture, until } from "./fixtures/host.js";
import { HiveServer } from "../services/hive/src/server.js";
import {
  HiveClient,
  discover,
  callBound,
  newOperationId,
} from "../packages/sdk/src/client.js";
import { HostExecutor } from "../packages/host-runtime/src/executor.js";
import { atomicJson } from "../packages/host-runtime/src/config.js";
import { runtimeEnvironment } from "../packages/host-runtime/src/process.js";
import { terminalPhases } from "../packages/host-runtime/src/journal.js";
import { collectHostStorage } from "../packages/host-runtime/src/storage-retention.js";
import { digest } from "../packages/contracts/src/canonical.js";
import {
  SchemaValidators,
  admitSchema,
} from "../packages/contracts/src/schema.js";
import {
  startServiceManager,
  managerTools,
} from "../services/service-manager/src/main.js";
import type { Host, Wire } from "../packages/contracts/src/generated.js";

test(
  "real ServiceManager tools retain local operation identity and executor ownership after the manager disconnects",
  { timeout: 55_000 },
  async (t) => {
    const f = await hostFixture(t);
    const listener = createServer();
    await new Promise<void>((resolve) =>
      listener.listen(0, "127.0.0.1", resolve),
    );
    const port = (listener.address() as { port: number }).port;
    await new Promise<void>((resolve) => listener.close(() => resolve()));
    const token = "synthetic-manager-private-credential";
    f.config.publicBaseUrl = `http://127.0.0.1:${port}/ivy`;
    f.config.instances.push({
      instanceId: "manager",
      serviceNodeId: "manager-node",
      componentId: "service-manager",
      engine: "process",
      enabled: true,
      credential: token,
      settings: { hostConfigPath: f.configPath },
    });
    await atomicJson(f.configPath, f.config);
    const hive = new HiveServer({
      filename: join(f.root, "hive.sqlite"),
      packageRoot: join(f.root, "packages"),
      publicBaseUrl: f.config.publicBaseUrl,
      listenHost: "127.0.0.1",
      listenPort: port,
      version: "test",
      buildId: digest("manager-test-hive"),
      credentials: [{ principalId: "manager", digest: digest(token) }],
    });
    await hive.start();
    f.cleanups.push(() => hive.close());
    const client = new HiveClient(f.config.publicBaseUrl, {
      credential: token,
    });
    await client.request("hostConfigurations.put", {
      hostId: f.config.hostId,
      configuration: f.config,
      mutationId: await newOperationId(client),
    });
    const candidate = await f.prepare("manager-owned-request");
    await collectHostStorage(f.config);
    const retention = f.journal.storageRetentionStatus();
    assert.equal(retention?.state, "succeeded");
    const executor = new HostExecutor(f.config, f.configPath, resolve("."));
    executor.start();
    f.cleanups.push(() => executor.close());
    const build = JSON.parse(
      await readFile(resolve("dist/build-info.json"), "utf8"),
    ) as { buildId: string; version: string };
    const instance: Host.InstanceConfig = {
      schemaVersion: 1,
      instanceId: "manager",
      componentId: "service-manager",
      serviceNodeId: "manager-node",
      hostId: f.config.hostId,
      publicBaseUrl: f.config.publicBaseUrl,
      dataRoot: join(f.root, "manager-data"),
      artifactRoot: resolve("."),
      buildId: build.buildId,
      version: build.version,
      credential: token,
      settings: { hostConfigPath: f.configPath },
    };
    const path = join(f.root, "manager.json");
    await atomicJson(path, instance);
    const manager = await startServiceManager(path);
    f.cleanups.push(() => manager.close());
    try {
      await manager.service.waitReady({ timeoutMs: 15_000 });
    } catch (error) {
      t.diagnostic(
        JSON.stringify({
          health: JSON.parse(
            await readFile(join(instance.dataRoot, "health.json"), "utf8"),
          ),
          nodes: await client.request("serviceNodes.list", {}),
          diagnostics: await client.request("system.diagnostics", {}),
        }),
      );
      throw error;
    }
    // Let the manager's own health projection settle once, then prove that idle
    // timestamp pulses do not create a new local or Hive observation.
    await delay(2300);
    const idleBefore = (
      await client.request("hosts.observations", { hostId: f.config.hostId })
    ).items[0]!;
    await delay(2300);
    const idleAfter = (
      await client.request("hosts.observations", { hostId: f.config.hostId })
    ).items[0]!;
    assert.equal(idleAfter.snapshot.sequence, idleBefore.snapshot.sequence);
    assert.deepEqual(idleAfter.snapshot.status.storageRetention, retention);
    assert.equal(idleAfter.reportedAt, idleBefore.reportedAt);
    const deploy = await discover(client, "host.deploy", {
      serviceNodeId: "manager-node",
    });
    const args = {
      instanceId: "first",
      candidateId: candidate.candidateId,
      operationId: "service-handoff",
    };
    await assert.rejects(
      callBound(client, deploy, args, "different-id"),
      (error: unknown) =>
        (error as { code: string }).code === "mutation_conflict",
    );
    assert.equal(f.journal.operation(args.operationId), null);
    const accepted = (await callBound(
      client,
      deploy,
      args,
      args.operationId,
    )) as Wire.DeploymentRecord;
    // Simulate a Hive connection loss while the ServiceManager process remains
    // the single owner of its assigned child processes.
    await manager.service.stop();
    await until(() =>
      terminalPhases.has(f.journal.get(accepted.deploymentId).record.phase),
    );
    assert.equal(
      f.journal.get(accepted.deploymentId).record.phase,
      "succeeded",
    );
    await manager.close();
    const replacement = await startServiceManager(path);
    f.cleanups.push(() => replacement.close());
    await replacement.service.waitReady();
    const replay = (await callBound(
      client,
      await discover(client, "host.deploy", { serviceNodeId: "manager-node" }),
      args,
      args.operationId,
    )) as Wire.DeploymentRecord;
    assert.equal(replay.deploymentId, accepted.deploymentId);
    assert.equal(replay.createdAt, accepted.createdAt);
    assert.equal(replay.phase, "succeeded");
    const invalidResult = { ...replay, targetBuild: null };
    assert.throws(
      () =>
        new SchemaValidators().validate(
          managerTools.find((value) => value.name === "deploy")!.outputSchema,
          invalidResult,
        ),
      (error: unknown) =>
        (error as { code: string }).code === "invalid_arguments",
    );
    const status = (await callBound(
      client,
      await discover(client, "host.status", { serviceNodeId: "manager-node" }),
      {},
    )) as unknown as Host.HostStatus;
    assert.equal(status.hostId, f.config.hostId);
    assert.deepEqual(status.storageRetention, retention);
    assert.equal(JSON.stringify(status).includes(token), false);
    const disabled = (await callBound(
      client,
      await discover(client, "host.disable", { serviceNodeId: "manager-node" }),
      { instanceId: "first", operationId: "service-disable" },
      "service-disable",
    )) as Wire.DeploymentRecord;
    try {
      await until(
        () => f.journal.get(disabled.deploymentId).record.phase === "succeeded",
      );
    } catch (error) {
      t.diagnostic(
        JSON.stringify({
          operation: f.journal.get(disabled.deploymentId),
          target: f.journal.target("first"),
          observation: f.journal.observations()["first"],
        }),
      );
      throw error;
    }
    await until(
      async () =>
        (
          await client.request("hosts.observations", {
            hostId: f.config.hostId,
          })
        ).items[0]?.snapshot.status.instances.find(
          (value) => value.instanceId === "first",
        )?.desiredEnabled === false,
    );
    const reports = await client.request("deployments.list", {
      hostId: f.config.hostId,
    });
    assert.ok(
      reports.items.some(
        (value) =>
          value.record.deploymentId === disabled.deploymentId &&
          value.record.phase === "succeeded",
      ),
    );
  },
);

test(
  "ServiceManager batches Object changes for its HostConfig and the package catalog into a local hint",
  { timeout: 35_000 },
  async (t) => {
    const f = await hostFixture(t),
      token = "synthetic-batch-credential";
    const listener = createServer();
    await new Promise<void>((resolve) =>
      listener.listen(0, "127.0.0.1", resolve),
    );
    const port = (listener.address() as { port: number }).port;
    await new Promise<void>((resolve) => listener.close(() => resolve()));
    f.config.publicBaseUrl = `http://127.0.0.1:${port}/ivy`;
    f.config.instances.push({
      instanceId: "manager",
      serviceNodeId: "manager-node",
      componentId: "service-manager",
      engine: "process",
      enabled: true,
      credential: token,
      settings: { hostConfigPath: f.configPath },
    });
    await atomicJson(f.configPath, f.config);
    const hive = new HiveServer({
      filename: join(f.root, "hive.sqlite"),
      packageRoot: join(f.root, "packages"),
      publicBaseUrl: f.config.publicBaseUrl,
      listenHost: "127.0.0.1",
      listenPort: port,
      version: "test",
      buildId: digest("manager-batch-hive"),
      credentials: [{ principalId: "manager", digest: digest(token) }],
    });
    await hive.start();
    f.cleanups.push(() => hive.close());
    const client = new HiveClient(f.config.publicBaseUrl, {
      credential: token,
    });
    const first = await client.request("hostConfigurations.put", {
      hostId: f.config.hostId,
      configuration: f.config,
      mutationId: await newOperationId(client),
    });
    const executor = new HostExecutor(f.config, f.configPath, resolve("."));
    executor.start();
    f.cleanups.push(() => executor.close());
    const build = JSON.parse(
      await readFile(resolve("dist/build-info.json"), "utf8"),
    ) as { buildId: string; version: string };
    const instance: Host.InstanceConfig = {
      schemaVersion: 1,
      instanceId: "manager",
      componentId: "service-manager",
      serviceNodeId: "manager-node",
      hostId: f.config.hostId,
      publicBaseUrl: f.config.publicBaseUrl,
      dataRoot: join(f.root, "manager-data"),
      artifactRoot: resolve("."),
      buildId: build.buildId,
      version: build.version,
      credential: token,
      settings: { hostConfigPath: f.configPath },
    };
    const path = join(f.root, "manager.json");
    await atomicJson(path, instance);
    const manager = await startServiceManager(path);
    f.cleanups.push(() => manager.close());
    await manager.service.waitReady({ timeoutMs: 15_000 });
    const changed = await client.request("hostConfigurations.put", {
      hostId: f.config.hostId,
      expectedRevision: first.revision,
      configuration: { ...f.config, packageUpdates: { intervalSeconds: 60 } },
      mutationId: await newOperationId(client),
    });
    const events = await client.request("events.read", {
      afterSequence: 0,
      filter: {
        topics: ["hive.object.changed"],
        objectIds: [changed.objectId],
      },
    });
    const sequence = Math.max(...events.items.map((item) => item.sequence));
    await until(async () => {
      try {
        const hint = JSON.parse(
          await readFile(
            join(f.config.runtimeRoot, "desired-state-hint.json"),
            "utf8",
          ),
        ) as { configuration?: boolean; throughSequence?: number };
        return (
          hint.configuration === true &&
          Number(hint.throughSequence) >= sequence
        );
      } catch {
        return false;
      }
    }, 20_000);
  },
);

test("conditional schema branches retain bounded admission and reject non-consuming recursion", () => {
  assert.throws(
    () => admitSchema({ if: { unsupportedKeyword: true }, then: false }),
    (error: unknown) => (error as { code: string }).code === "registry_invalid",
  );
  assert.throws(
    () =>
      admitSchema({
        $defs: { Loop: { not: { $ref: "#/$defs/Loop" } } },
        $ref: "#/$defs/Loop",
      }),
    (error: unknown) => (error as { code: string }).code === "registry_invalid",
  );
});

test(
  "ServiceManager startup exits with failure when its health file cannot be published",
  { timeout: 15_000 },
  async (t) => {
    const f = await hostFixture(t),
      token = "synthetic-startup-credential";
    f.config.instances.push({
      instanceId: "manager",
      serviceNodeId: "manager-node",
      componentId: "service-manager",
      engine: "process",
      enabled: true,
      credential: token,
      settings: { hostConfigPath: f.configPath },
    });
    await atomicJson(f.configPath, f.config);
    const build = JSON.parse(
      await readFile(resolve("dist/build-info.json"), "utf8"),
    ) as { buildId: string; version: string };
    const dataRoot = join(f.root, "manager-data");
    // A directory at the destination deterministically refuses the atomic health-file replacement.
    await mkdir(join(dataRoot, "health.json"), { recursive: true });
    const path = join(f.root, "manager.json");
    await atomicJson(path, {
      schemaVersion: 1,
      instanceId: "manager",
      componentId: "service-manager",
      serviceNodeId: "manager-node",
      hostId: f.config.hostId,
      publicBaseUrl: f.config.publicBaseUrl,
      dataRoot,
      artifactRoot: resolve("."),
      buildId: build.buildId,
      version: build.version,
      credential: token,
      settings: { hostConfigPath: f.configPath },
    });
    await assert.rejects(
      promisify(execFile)(
        process.execPath,
        [
          resolve("dist/services/service-manager/src/main.js"),
          "--config",
          path,
        ],
        {
          timeout: 8000,
          windowsHide: true,
          maxBuffer: 4096,
          env: runtimeEnvironment(),
        },
      ),
      (error: unknown) => {
        const failure = error as {
          code: number;
          killed: boolean;
          signal: string | null;
          stderr: string;
        };
        assert.equal(
          failure.killed,
          false,
          "Startup must release its timers and exit itself.",
        );
        assert.equal(failure.code, 1);
        assert.equal(failure.signal, null);
        assert.match(failure.stderr, /ServiceManager failed to start/);
        assert.equal(failure.stderr.includes(token), false);
        return true;
      },
    );
  },
);
