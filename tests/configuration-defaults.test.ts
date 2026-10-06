import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { hashJson } from "../packages/contracts/src/canonical.js";
import type { Host, Operation } from "../packages/contracts/src/generated.js";
import { atomicJson } from "../packages/host-runtime/src/config.js";
import { ConfigurationUpdater } from "../packages/host-runtime/src/configuration-updater.js";
import { HostJournal } from "../packages/host-runtime/src/journal.js";
import { resolveHostConfiguration } from "../packages/host-runtime/src/layout.js";
import {
  validateHost,
  validateAgent,
  validateTaskBoard,
  validateSecretary,
} from "../packages/contracts/src/validation.js";

test("portable service defaults preserve local choices and leave extensions and activation untouched", () => {
  const instance = (
    componentId: string,
    settings: Host.Instance["settings"] = {},
  ): Host.Instance => ({
    instanceId: componentId,
    componentId,
    serviceNodeId: "new-host." + componentId,
    engine: "process",
    enabled: false,
    settings,
  });
  const input: Host.HostConfigInput = {
    schemaVersion: 1,
    hostId: "new-host",
    publicBaseUrl: "https://hive.example",
    executables: { node: "/usr/bin/node" },
    ivyRoot: "/srv/ivy",
    packageUpdates: { intervalSeconds: 120 },
    instances: [
      instance("hive", { credentials: [], backup: { retain: 14 } }),
      instance("host-executor"),
      instance("service-manager"),
      instance("agent-manager", {
        nativeExecutable: "/usr/bin/codex",
        nativeVersion: "0.159.2",
        nativeExecutableHash: "sha256:" + "a".repeat(64),
        capabilities: [],
        limits: { maxPendingInputs: 4 },
      }),
      instance("task-board", {
        principalId: "new-owner",
        scheduler: { enabled: false },
      }),
      instance("secretary", {
        identity: {
          hostId: "new-host",
          serviceNodeId: "new-host.secretary",
          principalId: "new-owner",
          scope: { secretaryId: "secretary", rootObjectId: "new-root" },
        },
        policy: { researchMaxMinutes: 0 },
      }),
      instance("chat-bridge", { language: "de" }),
      instance("data-collector"),
      instance("dashboards"),
      instance("phone-bridge", { binding: { port: 5080 }, registration: null }),
      instance("custom-service", { identity: "local", values: [1, 2] }),
    ],
  };
  const before = structuredClone(input);
  const result = resolveHostConfiguration(
    input,
    "/srv/ivy/config.json",
    { HOME: "/home/new-user" },
    "linux",
  );
  const settings = Object.fromEntries(
    result.instances.map((value) => [value.componentId, value.settings]),
  );
  assert.deepEqual(input, before);
  assert.equal(result.instances.length, input.instances.length);
  assert.ok(
    result.instances.every(
      (value) => !value.enabled && value.credential === undefined,
    ),
  );
  assert.deepEqual(result.packageUpdates, { intervalSeconds: 120 });
  assert.deepEqual(settings["hive"]!["backup"], {
    directory: "/srv/ivy/backups/hive",
    intervalHours: 24,
    retain: 14,
  });
  assert.deepEqual(settings["agent-manager"]!["capabilities"], []);
  assert.equal(
    (settings["agent-manager"]!["limits"] as { maxPendingInputs: number })
      .maxPendingInputs,
    4,
  );
  assert.equal(
    settings["agent-manager"]!["codexHome"],
    "/srv/ivy/codex/agent-manager",
  );
  assert.equal(
    settings["agent-manager"]!["internalProjectRoot"],
    "/srv/ivy/codex-projects",
  );
  assert.deepEqual(settings["task-board"]!["scheduler"], {
    enabled: false,
    intervalMs: 2000,
    pageSize: 50,
  });
  assert.equal(
    (settings["secretary"]!["policy"] as { researchMaxMinutes: number })
      .researchMaxMinutes,
    0,
  );
  assert.equal(settings["chat-bridge"]!["language"], "de");
  assert.equal(settings["data-collector"]!["maximumMemoryMb"], 512);
  assert.equal(settings["dashboards"]!["rootObjectId"], null);
  assert.equal(settings["phone-bridge"]!["registration"], null);
  assert.deepEqual(settings["phone-bridge"]!["policy"], {
    incoming: [],
    recipients: [],
  });
  assert.deepEqual(
    settings["custom-service"],
    before.instances.at(-1)!.settings,
  );
  validateHost("HostConfig", result);
  validateHost("HiveSettings", settings["hive"]);
  validateHost("ManagerSettings", settings["service-manager"]);
  validateAgent("Settings", settings["agent-manager"]);
  validateTaskBoard("Settings", settings["task-board"]);
  validateSecretary("Settings", settings["secretary"]);
});

test("Hive configuration hashes cover supplied input while host defaults remain local", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-config-defaults-"));
  const configPath = join(root, "host.json");
  const input: Host.HostConfigInput = {
    schemaVersion: 1,
    hostId: "DEFAULTS",
    ivyRoot: join(root, "ivy"),
    runtimeRoot: join(root, "runtime"),
    artifactRoot: join(root, "artifacts"),
    stagingRoot: join(root, "staging"),
    publicBaseUrl: "http://127.0.0.1:1/ivy",
    executables: { node: process.execPath },
    instances: [
      {
        instanceId: "agent-manager",
        serviceNodeId: "defaults.agent-manager",
        componentId: "agent-manager",
        enabled: false,
        engine: "process",
        settings: {
          appServer: { mode: "owned-stdio" },
          projectRoot: join(root, "projects"),
          internalProjectRoot: join(root, "internal"),
          value: 1,
        },
      },
    ],
  };
  const initial = resolveHostConfiguration(input, configPath);
  await atomicJson(configPath, initial);
  const journal = new HostJournal(initial);
  t.after(async () => {
    journal.close();
    await rm(root, { recursive: true, force: true });
  });
  const changed: Host.HostConfigInput = {
    ...input,
    instances: input.instances.map((instance) => ({
      ...instance,
      settings: { ...instance.settings, value: 2 },
    })),
  };
  const remote: Operation.HostConfiguration = {
    hostId: input.hostId,
    objectId: "configuration",
    revision: 1,
    contentHash: hashJson(changed),
    updatedAt: new Date().toISOString(),
    byteLength: 1,
    configuration: changed as Host.HostConfig,
  };
  const updater = new ConfigurationUpdater(configPath, journal, "unused");
  (
    updater as unknown as { remote(): Promise<Operation.HostConfiguration> }
  ).remote = async () => remote;
  const result = await updater.sync();
  assert.equal(result.changed, true);
  assert.equal(result.revision, 1);
  assert.equal(journal.config.instances[0]!.settings["value"], 2);
  assert.equal(
    journal.config.instances[0]!.settings["codexHome"],
    initial.instances[0]!.settings["codexHome"],
  );
  assert.equal(result.contentHash, hashJson(journal.config));
  assert.notEqual(result.contentHash, remote.contentHash);
  assert.equal(
    hashJson(JSON.parse(await readFile(configPath, "utf8"))),
    result.contentHash,
  );
  assert.deepEqual(await updater.sync(), {
    changed: false,
    revision: 1,
    contentHash: result.contentHash,
    bootstrapRequired: [],
  });
});
