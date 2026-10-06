import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtemp,
  mkdir,
  rm,
  writeFile,
  symlink,
  readFile,
} from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import {
  resolveHostConfiguration,
  servicePaths,
  defaultHostConfigPath,
  hostStorageAreas,
} from "../packages/host-runtime/src/layout.js";
import { hostConfig } from "../packages/host-runtime/src/host-config.js";
import { sameInstallation } from "../packages/host-runtime/src/accepted-configuration.js";
import { HostJournal } from "../packages/host-runtime/src/journal.js";
import { materializeTarget } from "../packages/host-runtime/src/target.js";
import { publishBootstrapInstanceConfigurations } from "../packages/host-runtime/src/bootstrap.js";
import { serviceLog } from "../packages/host-runtime/src/service-log.js";
import type { Host } from "../packages/contracts/src/generated.js";

const input: Host.HostConfigInput = {
  schemaVersion: 1,
  hostId: "layout-fixture",
  publicBaseUrl: "http://127.0.0.1:39081/ivy",
  executables: { node: process.execPath },
  instances: [
    {
      componentId: "agent-manager",
      instanceId: "logical-a",
      serviceNodeId: "node-a",
      engine: "process",
      enabled: false,
      settings: {},
    },
  ],
};

test("new installations isolate AgentManager homes and select the platform connection mode", () => {
  for (const platform of ["win32", "linux"] as const) {
    const environment =
      platform === "win32"
        ? {
            USERPROFILE: "D:\\Accounts\\Ivy",
            HOME: "/wrong",
            CODEX_HOME: "C:\\Other\\.codex",
          }
        : { HOME: "/home/service", CODEX_HOME: "/different/.codex" };
    const path = defaultHostConfigPath(environment, platform),
      config = resolveHostConfiguration(input, path, environment, platform);
    assert.equal(
      path,
      platform === "win32"
        ? "D:\\Accounts\\Ivy\\.ivy\\config.json"
        : "/home/service/.ivy/config.json",
    );
    assert.equal(config.configPath, path);
    assert.ok(
      config.servicesRoot!.endsWith(
        platform === "win32" ? ".ivy\\services" : ".ivy/services",
      ),
    );
    assert.equal(config.instances[0]!.instanceId, "logical-a");
    assert.deepEqual(config.packageUpdates, { intervalSeconds: 60 });
    assert.deepEqual(config.configurationUpdates, { intervalSeconds: 60 });
    assert.ok(
      String(config.instances[0]!.settings["internalProjectRoot"]).endsWith(
        platform === "win32" ? ".ivy\\codex-projects" : ".ivy/codex-projects",
      ),
    );
    const nativeHome =
      platform === "win32"
        ? "D:\\Accounts\\Ivy\\.ivy\\codex\\logical-a"
        : "/home/service/.ivy/codex/logical-a";
    assert.equal(config.instances[0]!.settings["codexHome"], nativeHome);
    assert.equal(
      config.instances[0]!.settings["skillsRoot"],
      nativeHome + (platform === "win32" ? "\\skills" : "/skills"),
    );
    assert.deepEqual(config.instances[0]!.settings["appServer"], {
      mode: platform === "win32" ? "external-proxy" : "owned-stdio",
    });
  }
});

test("explicit AgentManager homes and connection modes take precedence over account defaults", () => {
  const environment = { USERPROFILE: "D:\\Accounts\\Ivy" },
    path = defaultHostConfigPath(environment, "win32");
  const owned = resolveHostConfiguration(
    {
      ...input,
      instances: input.instances.map((instance) => ({
        ...instance,
        settings: {
          nativeHome: "D:\\Isolated\\codex",
          appServer: { mode: "owned-stdio" },
        },
      })),
    },
    path,
    environment,
    "win32",
  );
  assert.equal(owned.instances[0]!.settings["codexHome"], undefined);
  assert.equal(
    owned.instances[0]!.settings["nativeHome"],
    "D:\\Isolated\\codex",
  );
  assert.deepEqual(owned.instances[0]!.settings["appServer"], {
    mode: "owned-stdio",
  });
});

test("two AgentManager instances keep Claude skills and service storage separate", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-layout-dual-agent-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const configPath = join(root, "config.json"),
    claudeData = join(root, "claude-data");
  const raw: Host.HostConfigInput = {
    ...input,
    ivyRoot: root,
    instances: [
      {
        ...input.instances[0]!,
        paths: {
          data: join(root, "codex-data"),
          work: join(root, "codex-work"),
          logs: join(root, "codex-logs"),
        },
        settings: { codexHome: join(root, "codex-home") },
      },
      {
        ...input.instances[0]!,
        instanceId: "logical-claude",
        serviceNodeId: "node-claude",
        paths: {
          data: claudeData,
          work: join(root, "claude-work"),
          logs: join(root, "claude-logs"),
        },
        settings: {
          codexHome: join(claudeData, "codex-home"),
          appServer: { mode: "claude-adapter" },
        },
      },
    ],
  };
  await writeFile(configPath, JSON.stringify(raw));
  const config = await hostConfig(configPath);
  const codex = config.instances[0]!,
    claude = config.instances[1]!;
  assert.equal(typeof codex.settings["skillsRoot"], "string");
  assert.equal(claude.settings["skillsRoot"], undefined);
  assert.equal(claude.settings["codexHome"], join(claudeData, "codex-home"));
  assert.notEqual(
    servicePaths(config, codex).data,
    servicePaths(config, claude).data,
  );
  const candidateId = "sha256:" + "1".repeat(64);
  const journal = {
    config,
    instance: (id: string) =>
      config.instances.find((instance) => instance.instanceId === id),
    candidate: () => ({
      candidateId,
      artifactRoot: join(root, "artifact"),
      buildId: candidateId,
    }),
    manifest: () => ({ version: "0.2.44" }),
  } as unknown as HostJournal;
  for (const instance of [codex, claude]) {
    const { config: target } = await materializeTarget(
      journal,
      instance.instanceId,
      candidateId,
      false,
    );
    assert.equal(
      typeof target.settings["skillsRoot"] === "string",
      instance === codex,
    );
  }
  const plan = {
    candidateId,
    processes: [codex, claude].map((instance) => ({
      instanceId: instance.instanceId,
      componentId: instance.componentId,
      name: instance.instanceId,
      configPath: join(root, instance.instanceId + "-bootstrap.json"),
    })),
  } as Host.BootstrapPlan;
  await publishBootstrapInstanceConfigurations(plan, config, journal);
  for (const instance of [codex, claude]) {
    const path = plan.processes.find(
      (process) => process.instanceId === instance.instanceId,
    )!.configPath!;
    const written = JSON.parse(
      await readFile(path, "utf8"),
    ) as Host.InstanceConfig;
    assert.equal(
      typeof written.settings["skillsRoot"] === "string",
      instance === codex,
    );
  }
});

test("legacy explicit paths remain unchanged and service path edits require an explicit restore", () => {
  const root = join(tmpdir(), "ivy-layout-legacy"),
    config = resolveHostConfiguration(
      {
        ...input,
        runtimeRoot: join(root, "runtime"),
        artifactRoot: join(root, "artifacts"),
        stagingRoot: join(root, "staging"),
      },
      join(root, "user.json"),
    );
  assert.equal(config.servicesRoot, undefined);
  assert.equal(config.configPath, undefined);
  assert.equal(
    servicePaths(config, "logical-a").data,
    join(root, "runtime/instances/logical-a/data"),
  );
  const changed = structuredClone(config);
  changed.instances[0]!.paths = {
    data: join(root, "elsewhere/data"),
    work: join(root, "elsewhere/work"),
    logs: join(root, "elsewhere/logs"),
  };
  assert.throws(
    () => sameInstallation(config, changed),
    (error: unknown) => (error as { code: string }).code === "target_conflict",
  );
});

test("flat service paths reject duplicate component collisions and linked artifact storage", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-layout-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const configPath = join(root, "config.json"),
    raw: Host.HostConfigInput = {
      ...input,
      ivyRoot: root,
      instances: input.instances.map((instance) => ({
        ...instance,
        settings: {
          codexHome: join(root, "codex"),
          projectRoot: join(root, "normal-projects"),
          internalProjectRoot: join(root, "internal-projects"),
        },
      })),
    };
  await writeFile(configPath, JSON.stringify(raw));
  const config = await hostConfig(configPath);
  assert.deepEqual(servicePaths(config, "logical-a"), {
    data: join(root, "services/agent-manager/data"),
    work: join(root, "services/agent-manager/work"),
    logs: join(root, "services/agent-manager/logs"),
  });
  assert.ok(
    hostStorageAreas(config).some((area) => area.key === "projects/logical-a"),
  );
  const duplicate = structuredClone(raw);
  duplicate.instances = [
    ...raw.instances,
    { ...raw.instances[0]!, instanceId: "logical-b", serviceNodeId: "node-b" },
  ];
  await writeFile(configPath, JSON.stringify(duplicate));
  await assert.rejects(
    hostConfig(configPath),
    (error: unknown) => (error as { code: string }).code === "target_conflict",
  );
  const linked = join(root, "linked");
  await symlink(
    config.artifactRoot,
    linked,
    process.platform === "win32" ? "junction" : "dir",
  );
  const explicit = structuredClone(raw);
  explicit.instances[0]!.paths = {
    data: join(linked, "data"),
    work: join(root, "safe-work"),
    logs: join(root, "safe-logs"),
  };
  await writeFile(configPath, JSON.stringify(explicit));
  await assert.rejects(
    hostConfig(configPath),
    (error: unknown) => (error as { code: string }).code === "target_conflict",
  );
});

test("process log rotation is bounded to logs and preserves service work and data", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ivy-layout-log-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(join(root, "work"));
  await mkdir(join(root, "data"));
  await writeFile(join(root, "work/permanent.txt"), "Retain");
  await writeFile(join(root, "data/journal.txt"), "Retain");
  const log = serviceLog(join(root, "logs"));
  for (let i = 0; i < 40; i++) log("stderr", "x".repeat(65536));
  assert.ok((await readFile(join(root, "logs/process.log"))).length <= 1048576);
  assert.ok(
    (await readFile(join(root, "logs/process.previous.log"))).length <= 1048576,
  );
  assert.equal(
    await readFile(join(root, "work/permanent.txt"), "utf8"),
    "Retain",
  );
  assert.equal(
    await readFile(join(root, "data/journal.txt"), "utf8"),
    "Retain",
  );
});
