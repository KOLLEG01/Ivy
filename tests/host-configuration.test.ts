import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { HiveServer } from "../services/hive/src/server.js";
import { digest, hashJson } from "../packages/contracts/src/canonical.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import type {
  Host,
  Operation,
  Wire,
} from "../packages/contracts/src/generated.js";
import { HiveClient, newOperationId } from "../packages/sdk/src/client.js";
import { atomicJson } from "../packages/host-runtime/src/config.js";
import { ConfigurationUpdater } from "../packages/host-runtime/src/configuration-updater.js";
import { HostJournal } from "../packages/host-runtime/src/journal.js";
import { HostExecutor } from "../packages/host-runtime/src/executor.js";
import { resolve } from "node:path";
import { validateHost } from "../packages/contracts/src/host-validation.js";

test("Windows process escape is an explicit process-only instance setting", () => {
  const base = {
    instanceId: "worker",
    serviceNodeId: "worker",
    componentId: "fixture",
    enabled: true,
    settings: {},
  };
  assert.doesNotThrow(() =>
    validateHost("Instance", {
      ...base,
      engine: "process",
      process: { allowWindowsBreakaway: true },
    }),
  );
  assert.throws(
    () =>
      validateHost("Instance", {
        ...base,
        engine: "docker",
        process: { allowWindowsBreakaway: true },
      }),
    (error: unknown) =>
      error instanceof IvyError && error.code === "invalid_arguments",
  );
});

test("configuration adoption removes a disabled installed instance but keeps enabled instances fenced", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-config-retire-")), configPath = join(root, "config.json");
  const initial: Host.HostConfig = {
    schemaVersion: 1, hostId: "RETIRE-HOST", configPath, runtimeRoot: join(root, "runtime"), artifactRoot: join(root, "artifacts"),
    stagingRoot: join(root, "staging"), publicBaseUrl: "http://127.0.0.1:1/ivy", executables: { node: process.execPath },
    instances: ["keeper", "retired"].map(instanceId => ({ instanceId, serviceNodeId: instanceId, componentId: "fixture", enabled: instanceId === "keeper", engine: "process" as const, settings: {} })),
  };
  await atomicJson(configPath, initial);
  const journal = new HostJournal(initial);
  t.after(async () => { journal.close(); await rm(root, { recursive: true, force: true }); });
  const next = { ...initial, instances: initial.instances.filter(instance => instance.instanceId !== "retired") };
  const remote: Operation.HostConfiguration = { hostId: initial.hostId, objectId: "configuration", revision: 1,
    contentHash: hashJson(next), updatedAt: new Date().toISOString(), byteLength: 1, configuration: next };
  const updater = new ConfigurationUpdater(configPath, journal, "unused");
  (updater as unknown as { remote: () => Promise<Operation.HostConfiguration> }).remote = async () => remote;
  const installed = { instanceId: "retired", candidateId: digest("candidate"), buildId: digest("build"), enabled: true, installedAt: new Date().toISOString() };
  journal.db.prepare("INSERT INTO installed VALUES (?,?)").run("retired", JSON.stringify(installed));
  await assert.rejects(updater.sync(), (error: unknown) => error instanceof IvyError && error.code === "restart_required");
  journal.db.prepare("UPDATE installed SET value=? WHERE instance_id=?").run(JSON.stringify({ ...installed, enabled: false }), "retired");
  assert.equal((await updater.sync()).changed, true);
  assert.deepEqual(journal.status(null).instances.map(instance => instance.instanceId), ["keeper"]);
  assert.deepEqual((JSON.parse(await readFile(configPath, "utf8")) as Host.HostConfig).instances.map(instance => instance.instanceId), ["keeper"]);
});

test("configuration adoption recovers exact lifecycle actions after persistence, partial acceptance and restart", async (t) => {
  for (const failAt of [1, 2]) {
    const root = await mkdtemp(join(tmpdir(), "ivy-config-recovery-")),
      configPath = join(root, "config.json");
    const initial: Host.HostConfig = {
      schemaVersion: 1,
      hostId: "RECOVERY-" + failAt,
      configPath,
      runtimeRoot: join(root, "runtime"),
      artifactRoot: join(root, "artifacts"),
      stagingRoot: join(root, "staging"),
      publicBaseUrl: "http://127.0.0.1:1/ivy",
      executables: { node: process.execPath },
      instances: ["first", "second"].map((instanceId) => ({
        instanceId,
        serviceNodeId: instanceId,
        componentId: "fixture",
        enabled: true,
        engine: "process" as const,
        settings: { value: 1 },
      })),
    };
    await atomicJson(configPath, initial);
    let journal = new HostJournal(initial);
    t.after(async () => {
      try {
        journal.close();
      } catch {}
      await rm(root, { recursive: true, force: true });
    });
    const plan = JSON.parse(
        await readFile(resolve("specs/examples/component.json"), "utf8"),
      ) as Host.BuildPlan,
      buildId = digest("config-recovery-build");
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
      artifactRoot: join(root, "candidate"),
      manifestPath: join(root, "candidate", "component.json"),
      createdAt: new Date().toISOString(),
      buildId,
      platform: {
        os: process.platform as "win32" | "linux",
        arch: process.arch as "x64" | "arm64",
        node: process.version,
      },
    };
    journal.saveCandidate(candidate, manifest);
    for (const instance of initial.instances)
      journal.setInstalled({
        instanceId: instance.instanceId,
        candidateId: buildId,
        buildId,
        enabled: true,
        installedAt: new Date().toISOString(),
      });
    const next = {
        ...initial,
        instances: initial.instances.map((instance, index) => ({
          ...instance,
          settings: { value: index + 2 },
        })),
      },
      remote = {
        hostId: initial.hostId,
        objectId: "configuration",
        revision: 1,
        contentHash: hashJson(next),
        updatedAt: new Date().toISOString(),
        byteLength: 1,
        configuration: next,
      };
    const updater = new ConfigurationUpdater(configPath, journal, "unused"),
      originalAccept = journal.accept.bind(journal);
    let attempts = 0;
    journal.accept = ((request: Host.LocalRequest) => {
      if (++attempts === failAt)
        throw Error("synthetic acceptance interruption");
      return originalAccept(request);
    }) as HostJournal["accept"];
    (
      updater as unknown as {
        remote: () => Promise<Operation.HostConfiguration>;
      }
    ).remote = async () => remote;
    await assert.rejects(updater.sync(), /synthetic acceptance interruption/);
    assert.ok(journal.configurationAdoption());
    journal.close();

    journal = new HostJournal(
      JSON.parse(await readFile(configPath, "utf8")) as Host.HostConfig,
    );
    const recovered = new ConfigurationUpdater(configPath, journal, "unused");
    (
      recovered as unknown as {
        remote: () => Promise<Operation.HostConfiguration>;
      }
    ).remote = async () => remote;
    assert.equal((await recovered.sync()).changed, true);
    assert.equal(journal.configurationAdoption(), null);
    assert.deepEqual(
      journal
        .list()
        .map((entry) => entry.request.instanceId)
        .sort(),
      ["first", "second"],
    );
    assert.equal((await recovered.sync()).changed, false);
    assert.equal(journal.list().length, 2);
    journal.close();
    await rm(root, { recursive: true, force: true });
  }
});

test(
  "Hive retains complete HostConfig revisions and the HostExecutor converges through the canonical SDK operation",
  { timeout: 20_000 },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "ivy-host-configuration-")),
      token = "host-configuration-token";
    const server = new HiveServer({
      filename: join(root, "hive.sqlite"),
      publicBaseUrl: "http://127.0.0.1/ivy",
      version: "test",
      buildId: digest("host-configuration-hive"),
      credentials: [
        { principalId: "configuration-editor", digest: digest(token) },
      ],
      listenHost: "127.0.0.1",
      listenPort: 0,
    });
    const address = await server.start(),
      base = `http://127.0.0.1:${address.port}/ivy`;
    const initial: Host.HostConfig = {
      schemaVersion: 1,
      hostId: "CONFIG-HOST",
      configPath: join(root, "config.json"),
      runtimeRoot: join(root, "runtime"),
      artifactRoot: join(root, "artifacts"),
      stagingRoot: join(root, "staging"),
      publicBaseUrl: base,
      executables: { node: process.execPath },
      configurationUpdates: { intervalSeconds: 2 },
      instances: [
        {
          instanceId: "worker",
          serviceNodeId: "worker",
          componentId: "fixture-worker",
          enabled: true,
          engine: "process",
          settings: { value: 1 },
        },
      ],
    };
    await atomicJson(initial.configPath!, initial);
    const journal = new HostJournal(initial),
      client = new HiveClient(base, { credential: token });
    t.after(async () => {
      journal.close();
      await server.close();
      await rm(root, {
        recursive: true,
        force: true,
        maxRetries: 5,
        retryDelay: 100,
      });
    });

    const first = await client.request("hostConfigurations.put", {
      hostId: initial.hostId,
      configuration: {
        ...initial,
        instances: [{ ...initial.instances[0]!, settings: { value: 2 } }],
      },
      mutationId: await newOperationId(client),
    });
    assert.equal(first.revision, 1);
    assert.equal(first.configuration.instances[0]!.settings["value"], 2);
    const updater = new ConfigurationUpdater(
        initial.configPath!,
        journal,
        token,
      ),
      synchronized = await updater.sync();
    assert.equal(synchronized.changed, true);
    assert.equal(journal.config.instances[0]!.settings["value"], 2);
    assert.equal(
      (
        JSON.parse(
          await readFile(initial.configPath!, "utf8"),
        ) as Host.HostConfig
      ).instances[0]!.settings["value"],
      2,
    );

    const secondConfiguration = {
      ...first.configuration,
      instances: [
        { ...first.configuration.instances[0]!, settings: { value: 3 } },
      ],
    };
    const second = await client.request("hostConfigurations.put", {
      hostId: initial.hostId,
      expectedRevision: first.revision,
      configuration: secondConfiguration,
      mutationId: await newOperationId(client),
    });
    assert.equal(second.revision, 2);
    assert.equal((await updater.sync()).changed, true);
    const history = await client.request("hostConfigurations.history", {
      hostId: initial.hostId,
      limit: 10,
    });
    assert.deepEqual(
      history.items.map((item) => item.revision),
      [2, 1],
    );
    await assert.rejects(
      client.request("objects.write", {
        objectId: second.objectId,
        expectedRevision: 2,
        contractVersion: "1.0.0",
        references: {},
        content: { encoding: "json", value: secondConfiguration },
        mutationId: await newOperationId(client),
      }),
      (error: unknown) =>
        error instanceof IvyError && error.code === "forbidden",
    );
  },
);

test("Settings editing never returns configured secrets and preserves their current values", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-settings-editor-")),
    token = "settings-editor-token";
  const server = new HiveServer({
    filename: join(root, "hive.sqlite"),
    publicBaseUrl: "http://127.0.0.1/ivy",
    version: "test",
    buildId: digest("settings-editor-hive"),
    credentials: [{ principalId: "settings-editor", digest: digest(token) }],
    listenHost: "127.0.0.1",
    listenPort: 0,
  });
  const address = await server.start(),
    base = `http://127.0.0.1:${address.port}/ivy`,
    client = new HiveClient(base, { credential: token });
  t.after(async () => {
    await server.close();
    await rm(root, {
      recursive: true,
      force: true,
      maxRetries: 5,
      retryDelay: 100,
    });
  });
  const configuration: Host.HostConfig = {
    schemaVersion: 1,
    hostId: "SECRET-HOST",
    runtimeRoot: join(root, "runtime"),
    artifactRoot: join(root, "artifacts"),
    stagingRoot: join(root, "staging"),
    publicBaseUrl: base,
    executables: { node: process.execPath },
    instances: [
      {
        instanceId: "worker",
        serviceNodeId: "worker",
        componentId: "fixture-worker",
        enabled: true,
        engine: "process",
        credential: "instance-password",
        secretPaths: ["/settings/apiToken"],
        settings: { value: 1, apiToken: "nested-password" },
      },
      {
        instanceId: "second",
        serviceNodeId: "second",
        componentId: "fixture-worker",
        enabled: true,
        engine: "process",
        credential: "second-password",
        settings: {},
      },
    ],
  };
  for (const secretPaths of [
    ["/settings/accounts/0/token"],
    ["/settings/apiToken", "/settings/apiToken"],
  ])
    await assert.rejects(
      client.request("hostConfigurations.put", {
        hostId: configuration.hostId,
        configuration: {
          ...configuration,
          instances: [
            { ...configuration.instances[0]!, secretPaths },
            configuration.instances[1]!,
          ],
        },
        mutationId: await newOperationId(client),
      }),
      (error: unknown) =>
        error instanceof IvyError && error.code === "invalid_arguments",
    );
  const createRequest = { hostId: configuration.hostId, configuration, mutationId: await newOperationId(client) };
  const created = await client.request("hostConfigurations.put", createRequest);
  assert.deepEqual(await client.request("hostConfigurations.put", createRequest), created);
  const editor = await client.request("hostConfigurations.edit", {
    hostId: configuration.hostId,
  });
  const encoded = JSON.stringify(editor);
  assert.equal(encoded.includes("instance-password"), false);
  assert.equal(encoded.includes("nested-password"), false);
  const draft = editor.configuration as Record<string, unknown>,
    instances = draft["instances"] as Array<Record<string, unknown>>;
  assert.deepEqual(instances[0]!["credential"], {
    $ivySecret: { instanceId: "worker", path: "/credential" },
  });
  assert.deepEqual(
    (instances[0]!["settings"] as Record<string, unknown>)["apiToken"],
    { $ivySecret: { instanceId: "worker", path: "/settings/apiToken" } },
  );
  (instances[0]!["settings"] as Record<string, unknown>)["value"] = 2;
  draft["instances"] = [instances[1]!, instances[0]!];
  const saveRequest = {
    hostId: configuration.hostId,
    expectedRevision: created.revision,
    configuration: editor.configuration,
    mutationId: await newOperationId(client),
  };
  const saved = await client.request("hostConfigurations.save", saveRequest);
  assert.equal(JSON.stringify(saved).includes("password"), false);
  const retained = await client.request("hostConfigurations.get", {
    hostId: configuration.hostId,
  });
  assert.equal(
    retained.configuration.instances[0]!.credential,
    "second-password",
  );
  assert.equal(
    retained.configuration.instances[1]!.credential,
    "instance-password",
  );
  assert.equal(
    retained.configuration.instances[1]!.settings["apiToken"],
    "nested-password",
  );
  assert.equal(retained.configuration.instances[1]!.settings["value"], 2);
  const unprotected = structuredClone(saved.configuration) as Record<
    string,
    unknown
  >;
  const unprotectedInstances = unprotected["instances"] as Array<
    Record<string, unknown>
  >;
  unprotectedInstances[1]!["secretPaths"] = [];
  await assert.rejects(
    client.request("hostConfigurations.save", {
      hostId: configuration.hostId,
      expectedRevision: saved.revision,
      configuration: unprotected as Host.HostConfig,
      mutationId: await newOperationId(client),
    }),
    (error: unknown) =>
      error instanceof IvyError && error.code === "invalid_arguments",
  );
  await assert.rejects(
    client.request("hostConfigurations.save", {
      hostId: configuration.hostId,
      expectedRevision: saved.revision,
      configuration: {
        ...draft,
        publicBaseUrl: {
          $ivySecret: { instanceId: "worker", path: "/credential" },
        },
      },
      mutationId: await newOperationId(client),
    }),
    (error: unknown) =>
      error instanceof IvyError && error.code === "invalid_arguments",
  );
  const invalid = async (candidate: Wire.Json) => {
    await assert.rejects(
      client.request("hostConfigurations.save", {
        hostId: configuration.hostId,
        expectedRevision: saved.revision,
        configuration: candidate,
        mutationId: await newOperationId(client),
      }),
      (error: unknown) =>
        error instanceof IvyError && error.code === "invalid_arguments",
    );
    assert.equal(
      (
        await client.request("hostConfigurations.get", {
          hostId: configuration.hostId,
        })
      ).revision,
      saved.revision,
    );
  };
  for (const candidate of [
    null,
    [],
    { ...draft, instances: [null] },
    { ...draft, instances: [1] },
    { ...draft, instances: [{ ...instances[0], secretPaths: null }] },
    {
      ...draft,
      instances: [{ ...instances[0], secretPaths: ["/settings/~2bad"] }],
    },
    {
      ...draft,
      instances: [
        {
          ...instances[0],
          credential: { $ivySecret: { instanceId: "worker" } },
        },
      ],
    },
  ] as Wire.Json[])
    await invalid(candidate);
  const rotated = await client.request("hostConfigurations.put", {
    hostId: configuration.hostId, expectedRevision: saved.revision,
    configuration: { ...retained.configuration, instances: retained.configuration.instances.map(instance => ({ ...instance, credential: "rotated-password" })) },
    mutationId: await newOperationId(client),
  });
  assert.deepEqual(await client.request("hostConfigurations.save", saveRequest), saved);
  assert.deepEqual(await client.request("hostConfigurations.put", createRequest), created);
  assert.equal((await client.request("hostConfigurations.get", { hostId: configuration.hostId })).revision, rotated.revision);

});

test(
  "a retained Object-change batch hint wakes HostExecutor before its minute safety check",
  { timeout: 20_000 },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "ivy-config-hint-")),
      token = "host-hint-token";
    const server = new HiveServer({
      filename: join(root, "hive.sqlite"),
      packageRoot: join(root, "packages"),
      publicBaseUrl: "http://127.0.0.1/ivy",
      version: "test",
      buildId: digest("hint-hive"),
      credentials: [
        { principalId: "HINT.host-executor", digest: digest(token) },
      ],
      listenHost: "127.0.0.1",
      listenPort: 0,
    });
    const address = await server.start(),
      base = `http://127.0.0.1:${address.port}/ivy`;
    const config: Host.HostConfig = {
      schemaVersion: 1,
      hostId: "HINT",
      configPath: join(root, "config.json"),
      runtimeRoot: join(root, "runtime"),
      artifactRoot: join(root, "artifacts"),
      stagingRoot: join(root, "staging"),
      publicBaseUrl: base,
      executables: { node: process.execPath },
      configurationUpdates: { intervalSeconds: 60 },
      instances: [
        {
          instanceId: "host-executor",
          serviceNodeId: "HINT.host-executor",
          componentId: "host-executor",
          enabled: true,
          engine: "process",
          credential: token,
          settings: { hostConfigPath: join(root, "config.json") },
        },
        {
          instanceId: "worker",
          serviceNodeId: "HINT.worker",
          componentId: "fixture-worker",
          enabled: true,
          engine: "process",
          settings: { value: 1 },
        },
      ],
    };
    await atomicJson(config.configPath!, config);
    const client = new HiveClient(base, { credential: token });
    const first = await client.request("hostConfigurations.put", {
      hostId: config.hostId,
      configuration: config,
      mutationId: await newOperationId(client),
    });
    const executor = new HostExecutor(
      config,
      config.configPath!,
      resolve("."),
      "host-executor",
    );
    executor.start();
    t.after(async () => {
      await executor.close();
      await server.close();
      await rm(root, {
        recursive: true,
        force: true,
        maxRetries: 5,
        retryDelay: 100,
      });
    });
    const deadline = Date.now() + 10_000;
    while (true) {
      const status = (await readFile(
        join(config.runtimeRoot, "executor.json"),
        "utf8",
      )
        .then(JSON.parse)
        .catch(() => null)) as { configurationRevision?: number } | null;
      if (status?.configurationRevision === first.revision) break;
      assert.ok(
        Date.now() < deadline,
        "initial HostConfig revision was not observed",
      );
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    await client.request("hostConfigurations.put", {
      hostId: config.hostId,
      expectedRevision: first.revision,
      configuration: {
        ...config,
        instances: [
          config.instances[0]!,
          { ...config.instances[1]!, settings: { value: 2 } },
        ],
      },
      mutationId: await newOperationId(client),
    });
    await atomicJson(join(config.runtimeRoot, "desired-state-hint.json"), {
      schemaVersion: 1,
      throughSequence: 2,
      observedAt: new Date().toISOString(),
      configuration: true,
      packages: false,
    });
    const hintedDeadline = Date.now() + 10_000;
    while (true) {
      const local = (await readFile(config.configPath!, "utf8").then(
        JSON.parse,
      )) as Host.HostConfig;
      if (local.instances[1]?.settings["value"] === 2) break;
      assert.ok(
        Date.now() < hintedDeadline,
        "batch hint did not wake HostExecutor before the minute fallback",
      );
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  },
);
