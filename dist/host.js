import {
  validateShared
} from "./chunk-TYPU3LZR.js";
import {
  wireSchema
} from "./chunk-C4DMFRYX.js";
import {
  HiveClient,
  baseUrl
} from "./chunk-YZRDUBD3.js";
import {
  atomicJson,
  configurationPath,
  inside,
  instanceConfig,
  jsonFile,
  releaseManifest,
  requireRuntimeRequirements,
  verifyLaunchCandidate
} from "./chunk-T4CIHWMU.js";
import {
  hostSchema,
  host_schema_default,
  pointerParts,
  validateHost
} from "./chunk-XNKMI6K4.js";
import {
  hashJson,
  hive_wire_schema_default
} from "./chunk-ZEQOMSP4.js";
import {
  IvyError,
  canonical,
  requireThat
} from "./chunk-62CEFMQW.js";

// packages/host-runtime/src/health.ts
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { randomUUID } from "node:crypto";
var HealthFile = class {
  constructor(config) {
    this.config = config;
  }
  config;
  startedAt = (/* @__PURE__ */ new Date()).toISOString();
  bootId = randomUUID();
  launchId = process.env["IVY_LAUNCH_ID"];
  async write(ready, details = "", generation = null) {
    const value = {
      schemaVersion: 1,
      instanceId: this.config.instanceId,
      componentId: this.config.componentId,
      buildId: this.config.buildId,
      pid: process.pid,
      bootId: this.bootId,
      startedAt: this.startedAt,
      observedAt: (/* @__PURE__ */ new Date()).toISOString(),
      ready,
      generation,
      ...this.launchId ? { launchId: this.launchId } : {},
      details: details.slice(0, 4096)
    };
    validateHost("Health", value);
    await atomicJson(join(this.config.dataRoot, "health.json"), value);
  }
  async control() {
    try {
      const value = await jsonFile(join(this.config.dataRoot, "control.json"));
      validateHost("RuntimeControl", value);
      return value.instanceId === this.config.instanceId && value.launchId === this.launchId && value.bootId === this.bootId ? value : null;
    } catch (error) {
      if (error.code === "ENOENT") return null;
      throw error;
    }
  }
};
function localHiveEndpoint(config, docker) {
  const settings = config.settings;
  validateHost("HiveSettings", settings);
  const mappings = docker?.ports.filter((port) => port.containerPort === settings.listenPort && ["127.0.0.1", "::1", "0.0.0.0", "::"].includes(port.host));
  requireThat(!mappings || mappings.length === 1, "invalid_arguments", "Hive health needs exactly one loopback-accessible host port mapping for its listener.");
  const binding = mappings?.[0], hostname = binding?.host ?? settings.listenHost;
  const endpoint = new URL(config.publicBaseUrl);
  endpoint.protocol = "http:";
  endpoint.hostname = hostname === "0.0.0.0" ? "127.0.0.1" : hostname === "::" ? "[::1]" : hostname.includes(":") ? "[" + hostname + "]" : hostname;
  endpoint.port = String(binding?.port ?? settings.listenPort);
  return endpoint;
}
async function checkHealth(config, hiveEndpoint, verifyRegistration = true) {
  const value = await jsonFile(join(config.dataRoot, "health.json"));
  validateHost("Health", value);
  requireThat(value.ready && value.instanceId === config.instanceId && value.buildId === config.buildId && Date.now() - Date.parse(value.observedAt) < 15e3 && Date.parse(value.observedAt) <= Date.now() + 1e3, "service_not_ready", "Instance health is stale, mismatched or not ready.");
  if (config.componentId === "hive") {
    const settings = config.settings;
    validateHost("HiveSettings", settings);
    const endpoint = hiveEndpoint ? new URL(hiveEndpoint) : localHiveEndpoint(config);
    const status = await new HiveClient(endpoint.href, { credential: settings.credentials[0].token }).request("system.status", {}, { timeoutMs: 5e3 });
    requireThat(status.ready && status.buildId === config.buildId, "service_not_ready", "Hive API/storage identity is not ready.");
  } else if (config.credential && verifyRegistration) {
    const node = await new HiveClient(config.publicBaseUrl, { credential: config.credential }).request("serviceNodes.get", { serviceNodeId: config.serviceNodeId }, { timeoutMs: 5e3 });
    requireThat(node.connected && node.synced && node.ready && node.buildId === config.buildId, "service_not_ready", "The current Hive service registration is not synchronized and ready.");
  }
  return value;
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const health = await checkHealth(await instanceConfig(configurationPath()), process.env["IVY_HIVE_HEALTH_URL"]);
    process.stdout.write(JSON.stringify({ ok: true, buildId: health.buildId, generation: health.generation }) + "\n");
  } catch (error) {
    process.stdout.write(JSON.stringify({ ok: false, code: IvyError.from(error).code }) + "\n");
    process.exitCode = 1;
  }
}

// packages/host-runtime/src/journal.ts
import { DatabaseSync } from "node:sqlite";
import { existsSync, lstatSync as lstatSync2, mkdirSync as mkdirSync2, readFileSync as readFileSync2 } from "node:fs";
import { join as join4 } from "node:path";
import { randomUUID as randomUUID3 } from "node:crypto";

// packages/host-runtime/src/accepted-configuration.ts
import { closeSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, writeFileSync } from "node:fs";
import { dirname as dirname2, join as join3 } from "node:path";
import { randomUUID as randomUUID2 } from "node:crypto";

// packages/host-runtime/src/layout.ts
import { homedir } from "node:os";
import { join as join2, resolve, isAbsolute, posix, win32, dirname, basename } from "node:path";
import { realpath } from "node:fs/promises";
function accountHome(environment = process.env, platform = process.platform) {
  const paths = platform === "win32" ? win32 : posix;
  const home = environment[platform === "win32" ? "USERPROFILE" : "HOME"] || homedir();
  requireThat(paths.isAbsolute(home), "invalid_arguments", "The executing account must have an absolute user home.");
  return paths.normalize(home);
}
function defaultHostConfigPath(environment = process.env, platform = process.platform) {
  return (platform === "win32" ? win32 : posix).join(accountHome(environment, platform), ".ivy", "config.json");
}
async function resolvedFuturePath(path) {
  let ancestor = resolve(path);
  const suffix = [];
  while (true) {
    try {
      return join2(await realpath(ancestor), ...suffix.reverse());
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
      const parent = dirname(ancestor);
      requireThat(parent !== ancestor, "target_conflict", "Storage path has no existing ancestor.");
      suffix.push(basename(ancestor));
      ancestor = parent;
    }
  }
}
function servicePaths(config, instance) {
  const selected = typeof instance === "string" ? config.instances.find((value) => value.instanceId === instance) : instance;
  requireThat(selected, "not_found", "Unknown service instance for path resolution.");
  const base = config.servicesRoot ? join2(config.servicesRoot, selected.componentId) : join2(config.runtimeRoot, "instances", selected.instanceId);
  return selected.paths ?? { data: join2(base, "data"), work: join2(base, "work"), logs: join2(base, "logs") };
}
function resolveHostConfiguration(input, sourcePath, environment = process.env, platform = process.platform) {
  const paths = platform === "win32" ? win32 : posix;
  const home = accountHome(environment, platform);
  const modern = input.ivyRoot !== void 0 || input.servicesRoot !== void 0 || input.runtimeRoot === void 0 || paths.normalize(sourcePath) === defaultHostConfigPath(environment, platform);
  const ivyRoot = input.ivyRoot ?? paths.join(home, ".ivy");
  const config = {
    ...input,
    runtimeRoot: input.runtimeRoot ?? paths.join(ivyRoot, "runtime"),
    artifactRoot: input.artifactRoot ?? paths.join(ivyRoot, "artifacts"),
    stagingRoot: input.stagingRoot ?? paths.join(ivyRoot, "staging"),
    ...modern ? { ivyRoot, servicesRoot: input.servicesRoot ?? paths.join(ivyRoot, "services"), configPath: input.configPath ?? sourcePath } : {}
  };
  config.instances = input.instances.map((instance) => instance.componentId === "agent-manager" ? {
    ...instance,
    settings: {
      ...instance.settings,
      appServer: instance.settings["appServer"] ?? { mode: "external-proxy" },
      projectRoot: instance.settings["projectRoot"] ?? paths.join(home, "projects"),
      internalProjectRoot: instance.settings["internalProjectRoot"] ?? paths.join(home, ".ivy", "codex-projects"),
      skillsRoot: instance.settings["skillsRoot"] ?? paths.join(home, ".agents", "skills")
    }
  } : instance);
  for (const value of [config.runtimeRoot, config.artifactRoot, config.stagingRoot, config.ivyRoot, config.servicesRoot, config.configPath])
    requireThat(value === void 0 || paths.isAbsolute(value), "invalid_arguments", "Host directories and configuration path must be absolute.");
  return config;
}
function hostStorageAreas(config) {
  const areas = [{ key: "state", path: config.runtimeRoot }];
  const contains = (root, path) => {
    const local = (process.platform === "win32" ? win32 : posix).relative(root, path);
    return !local.startsWith("..") && !isAbsolute(local);
  };
  for (const instance of [...config.instances].sort((a, b) => a.instanceId.localeCompare(b.instanceId))) {
    for (const [kind, path] of Object.entries(servicePaths(config, instance)))
      if (!areas.some((area) => contains(area.path, path))) areas.push({ key: "services/" + instance.instanceId + "/" + kind, path });
    if (instance.componentId === "agent-manager") {
      const configured = instance.settings["internalProjectRoot"];
      const path = typeof configured === "string" ? configured : join2(accountHome(), ".ivy", "codex-projects");
      if (!areas.some((area) => contains(area.path, path))) areas.push({ key: "projects/" + instance.instanceId, path });
    }
  }
  return areas;
}

// packages/host-runtime/src/accepted-configuration.ts
var limit = 1024 * 1024;
function saveAcceptedConfiguration(config) {
  validateHost("HostConfig", config);
  const directory = join3(config.runtimeRoot, "accepted-configurations");
  mkdirSync(directory, { recursive: true, mode: 448 });
  const path = join3(directory, randomUUID2() + ".json"), file = openSync(path, "wx", 384);
  try {
    writeFileSync(file, canonical(config, limit - 1) + "\n");
    fsyncSync(file);
  } finally {
    closeSync(file);
  }
  if (process.platform !== "win32") {
    const descriptor = openSync(directory, "r");
    try {
      fsyncSync(descriptor);
    } finally {
      closeSync(descriptor);
    }
  }
  return path;
}
function acceptedInstanceConfigurationPath(config, revision) {
  const fileRevision = /^sha256:[0-9a-f]{64}$/.test(revision) ? "bootstrap-" + revision.slice(7) : revision;
  requireThat(/^[a-zA-Z0-9_-]{1,128}$/.test(fileRevision), "target_conflict", "Runtime target revision cannot name an accepted configuration.");
  return join3(config.runtimeRoot, "accepted-configurations", fileRevision + ".instance.json");
}
function readAcceptedConfiguration(anchor, path) {
  requireThat(inside(join3(anchor.runtimeRoot, "accepted-configurations"), path), "target_conflict", "Accepted configuration leaves host runtime data.");
  const metadata = lstatSync(path);
  requireThat(
    metadata.isFile() && !metadata.isSymbolicLink() && metadata.size <= limit && dirname2(path) === join3(anchor.runtimeRoot, "accepted-configurations"),
    "configuration_changed",
    "Accepted configuration is not a bounded direct file."
  );
  try {
    const config = JSON.parse(readFileSync(path, "utf8"));
    validateHost("HostConfig", config);
    sameInstallation(anchor, config);
    return config;
  } catch (error) {
    if (error instanceof IvyError && error.code === "target_conflict") throw error;
    throw new IvyError("configuration_changed", "Accepted configuration is unreadable or invalid.");
  }
}
function sameInstallation(anchor, next) {
  requireThat(
    ["hostId", "runtimeRoot", "artifactRoot", "stagingRoot", "servicesRoot", "configPath"].every((key) => anchor[key] === next[key]) && anchor.executables["node"] === next.executables["node"],
    "target_conflict",
    "Host identity, operational roots and bootstrap Node executable cannot change within an installation."
  );
  requireThat(hashJson(anchor.restoredFrom ?? null) === hashJson(next.restoredFrom ?? null), "target_conflict", "An installation cannot discard or substitute its original recovery identity.");
  for (const instance of anchor.instances) {
    const other = next.instances.find((value) => value.instanceId === instance.instanceId);
    requireThat(!other || other.componentId === instance.componentId && other.serviceNodeId === instance.serviceNodeId, "target_conflict", "An existing instance identity cannot be reused for another component or Service Node.");
    requireThat(!other || hashJson(servicePaths(anchor, instance)) === hashJson(servicePaths(next, other)), "target_conflict", "Existing service storage cannot move through a configuration edit; use an explicit isolated restore.");
  }
}
function installationRecord(config) {
  return canonical({
    runtimeRoot: config.runtimeRoot,
    artifactRoot: config.artifactRoot,
    stagingRoot: config.stagingRoot,
    servicesRoot: config.servicesRoot ?? null,
    configPath: config.configPath ?? null,
    node: config.executables["node"],
    restoredFrom: config.restoredFrom ?? null
  });
}

// packages/host-runtime/src/journal.ts
var terminalPhases = /* @__PURE__ */ new Set([
  "succeeded",
  "rolled_back",
  "failed",
  "needs_attention"
]);
var requiresBootstrapReplacement = (instance) => instance.componentId === "host-executor" || process.platform === "win32" && instance.componentId === "service-manager";
var transitions = {
  preparing: ["prepared", "failed", "needs_attention"],
  prepared: ["checking", "failed", "needs_attention"],
  checking: ["draining", "failed", "needs_attention"],
  draining: ["activating", "rolling_back", "failed", "needs_attention"],
  activating: ["verifying", "rolling_back", "needs_attention"],
  verifying: ["succeeded", "rolling_back", "needs_attention"],
  rolling_back: ["rolled_back", "needs_attention"],
  succeeded: [],
  rolled_back: [],
  failed: [],
  needs_attention: []
};
function legacyConfigurationPath(runtimeRoot, hash) {
  requireThat(
    /^sha256:[0-9a-f]{64}$/.test(hash),
    "configuration_changed",
    "Retained configuration identity is invalid."
  );
  return join4(runtimeRoot, "host-configurations", hash.slice(7) + ".json");
}
function normalizeCandidate(input) {
  const value = input;
  const candidate = {
    candidateId: value.candidateId,
    componentId: value.componentId,
    artifactRoot: value.artifactRoot,
    manifestPath: value.manifestPath,
    createdAt: value.createdAt,
    platform: value.platform,
    buildId: value.buildId,
    ...value.archivePath === void 0 ? {} : { archivePath: value.archivePath },
    ...value.archiveHash === void 0 ? {} : { archiveHash: value.archiveHash },
    ...value.dockerImage === void 0 ? {} : { dockerImage: value.dockerImage }
  };
  validateHost("Candidate", candidate);
  return candidate;
}
function normalizeTarget(input) {
  const value = input;
  const target = {
    schemaVersion: value.schemaVersion,
    instanceId: value.instanceId,
    revision: value.revision,
    candidateId: value.candidateId,
    desired: value.desired,
    configPath: value.configPath,
    requestedAt: value.requestedAt
  };
  validateHost("RuntimeTarget", target);
  return target;
}
var freshTimestamp = (value) => {
  const at = Date.parse(value);
  return Number.isFinite(at) && Date.now() - at < 15e3 && at <= Date.now() + 1e3;
};
function freshExecutorStatus(value) {
  return freshTimestamp(value.observedAt);
}
function bootstrapLaunchId(plan, instanceId) {
  return hashJson({ planHash: hashJson(plan), instanceId });
}
var ExecutorLock = class {
  db;
  constructor(root) {
    mkdirSync2(root, { recursive: true });
    const db = new DatabaseSync(join4(root, "executor-lock.sqlite"));
    try {
      db.exec(
        "PRAGMA busy_timeout=100; PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; CREATE TABLE IF NOT EXISTS owner(pid INTEGER); DELETE FROM owner;"
      );
      db.prepare("INSERT INTO owner VALUES (?)").run(process.pid);
      db.exec("COMMIT");
    } catch {
      db.close();
      throw new IvyError(
        "executor_already_running",
        "Another executor holds the host lock."
      );
    }
    this.db = db;
  }
  close() {
    this.db.close();
  }
};
var HostJournal = class {
  db;
  activeConfig;
  get config() {
    return this.activeConfig;
  }
  constructor(config) {
    validateHost("HostConfig", config);
    this.activeConfig = config;
    mkdirSync2(config.runtimeRoot, { recursive: true });
    const db = new DatabaseSync(join4(config.runtimeRoot, "deployments.sqlite"));
    this.db = db;
    try {
      db.exec(
        "PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;"
      );
      const format = Number(
        db.prepare("PRAGMA user_version").get()["user_version"]
      );
      requireThat(
        format === 0 || format === 1,
        "unsupported_storage",
        "Host journal format is not supported by this executor."
      );
      db.exec(`BEGIN IMMEDIATE;
        CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS operations(operation_id TEXT PRIMARY KEY, deployment_id TEXT NOT NULL UNIQUE, request_hash TEXT NOT NULL, phase TEXT NOT NULL, created_at TEXT NOT NULL, entry_json TEXT NOT NULL) STRICT;
        CREATE INDEX IF NOT EXISTS operations_by_phase ON operations(phase,created_at,deployment_id);
        CREATE TABLE IF NOT EXISTS candidates(candidate_id TEXT PRIMARY KEY, component_id TEXT NOT NULL, build_id TEXT NOT NULL, candidate_json TEXT NOT NULL, manifest_json TEXT NOT NULL) STRICT;
        CREATE INDEX IF NOT EXISTS candidates_by_build ON candidates(component_id,build_id);
        CREATE TABLE IF NOT EXISTS installed(instance_id TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS observations(instance_id TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS runtime_targets(instance_id TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
        PRAGMA user_version=1; COMMIT;`);
      this.transaction(() => {
        const prior = db.prepare("SELECT value FROM meta WHERE key=?").get("hostId");
        requireThat(
          !prior || prior["value"] === config.hostId,
          "target_conflict",
          "A host journal cannot be reused for another host identity."
        );
        db.prepare("INSERT OR IGNORE INTO meta VALUES (?,?)").run(
          "hostId",
          config.hostId
        );
        const installation = installationRecord(config);
        const anchor = db.prepare("SELECT value FROM meta WHERE key=?").get("installation");
        if (!anchor) {
          const legacy = db.prepare("SELECT value FROM meta WHERE key=?").get("configurationAnchor");
          if (legacy) {
            const hash = String(legacy["value"]), path = legacyConfigurationPath(config.runtimeRoot, hash);
            try {
              const metadata = lstatSync2(path);
              requireThat(
                metadata.isFile() && !metadata.isSymbolicLink() && metadata.size <= 1024 * 1024,
                "configuration_changed",
                "Retained installation configuration is not a bounded regular file."
              );
              const prior2 = JSON.parse(
                readFileSync2(path, "utf8")
              );
              validateHost("HostConfig", prior2);
              requireThat(
                hashJson(prior2) === hash,
                "configuration_changed",
                "Retained installation configuration changed."
              );
              sameInstallation(prior2, config);
            } catch (error) {
              if (error instanceof IvyError) throw error;
              throw new IvyError(
                "configuration_changed",
                "Retained installation configuration is missing or invalid."
              );
            }
          }
        }
        requireThat(
          !anchor || anchor["value"] === installation,
          "target_conflict",
          "Host operational roots and recovery identity cannot change within an installation."
        );
        db.prepare("INSERT OR IGNORE INTO meta VALUES (?,?)").run(
          "installation",
          installation
        );
        for (const instance of config.instances) {
          const key = "instanceIdentity:" + instance.instanceId, identity = canonical({
            componentId: instance.componentId,
            serviceNodeId: instance.serviceNodeId
          });
          const prior2 = db.prepare("SELECT value FROM meta WHERE key=?").get(key);
          requireThat(
            !prior2 || prior2["value"] === identity,
            "target_conflict",
            "A previously configured instance identity cannot be reused."
          );
          db.prepare("INSERT OR IGNORE INTO meta VALUES (?,?)").run(
            key,
            identity
          );
        }
      });
    } catch (error) {
      db.close();
      throw error;
    }
  }
  close() {
    this.db.close();
  }
  acceptedConfiguration(path) {
    return readAcceptedConfiguration(this.config, path);
  }
  useConfiguration(config) {
    sameInstallation(this.config, config);
    this.transaction(() => {
      for (const instance of config.instances) {
        const key = "instanceIdentity:" + instance.instanceId, identity = canonical({
          componentId: instance.componentId,
          serviceNodeId: instance.serviceNodeId
        });
        const prior = this.db.prepare("SELECT value FROM meta WHERE key=?").get(key);
        requireThat(
          !prior || prior["value"] === identity,
          "target_conflict",
          "A previously configured instance identity cannot be reused."
        );
        this.db.prepare("INSERT OR IGNORE INTO meta VALUES (?,?)").run(key, identity);
      }
    });
    this.activeConfig = config;
  }
  configurationAdoption() {
    const row = this.db.prepare("SELECT value FROM meta WHERE key=?").get("configurationAdoption");
    if (!row) return null;
    const value = JSON.parse(String(row["value"]));
    validateHost("HostConfig", value.configuration);
    sameInstallation(this.config, value.configuration);
    requireThat(
      Number.isSafeInteger(value.revision) && value.revision >= 1 && /^sha256:[0-9a-f]{64}$/.test(value.contentHash) && Array.isArray(value.actions) && value.actions.every(
        (action) => ["restart", "enable", "disable"].includes(action.action) && typeof action.instanceId === "string" && typeof action.operationId === "string"
      ) && Array.isArray(value.bootstrapRequired) && value.bootstrapRequired.every(
        (instanceId) => typeof instanceId === "string"
      ),
      "configuration_changed",
      "Retained configuration adoption is invalid."
    );
    return value;
  }
  stageConfigurationAdoption(value) {
    validateHost("HostConfig", value.configuration);
    sameInstallation(this.config, value.configuration);
    requireThat(
      hashJson(value.configuration) === value.contentHash,
      "configuration_changed",
      "Configuration adoption hash does not match its exact input."
    );
    this.transaction(() => {
      const prior = this.db.prepare("SELECT value FROM meta WHERE key=?").get("configurationAdoption"), encoded = canonical(value);
      requireThat(
        !prior || prior["value"] === encoded,
        "service_busy",
        "Another configuration adoption is still pending."
      );
      this.db.prepare(
        "INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value"
      ).run("configurationAdoption", encoded);
    });
  }
  finishConfigurationAdoption(value) {
    this.transaction(() => {
      const prior = this.db.prepare("SELECT value FROM meta WHERE key=?").get("configurationAdoption");
      requireThat(
        prior && prior["value"] === canonical(value),
        "revision_conflict",
        "Configuration adoption changed before completion."
      );
      this.db.prepare("DELETE FROM meta WHERE key=?").run("configurationAdoption");
    });
  }
  status(executor) {
    const currentExecutor = executor && freshExecutorStatus(executor) ? executor : null;
    const observed = this.currentObservations(currentExecutor);
    const value = {
      schemaVersion: 1,
      hostId: this.config.hostId,
      observedAt: (/* @__PURE__ */ new Date()).toISOString(),
      executor: currentExecutor,
      instances: this.config.instances.map((instance) => {
        const installed = this.installed(instance.instanceId), actual = observed[instance.instanceId];
        return {
          instanceId: instance.instanceId,
          serviceNodeId: instance.serviceNodeId,
          componentId: instance.componentId,
          engine: instance.engine,
          desiredEnabled: installed?.enabled ?? instance.enabled,
          installedBuild: installed?.buildId ?? null,
          installedCandidateId: installed?.candidateId ?? null,
          observedBuild: actual?.buildId ?? null,
          observedState: actual?.state ?? null,
          observedAt: actual?.observedAt ?? null,
          code: actual?.code ?? null,
          message: actual?.message.slice(0, 512) ?? ""
        };
      }),
      unfinished: this.pendingEntries().map((value2) => value2.record)
    };
    validateHost("HostStatus", value);
    return value;
  }
  /** Read the current owner files for a status projection without changing the journal. */
  currentObservations(executor = null) {
    const prior = this.observations(), result = {};
    for (const instance of this.config.instances) {
      const installed = this.installed(instance.instanceId), target = this.target(instance.instanceId), previous = prior[instance.instanceId];
      if (instance.componentId === "host-executor") {
        if (!executor || !freshExecutorStatus(executor)) {
          result[instance.instanceId] = {
            schemaVersion: 1,
            instanceId: instance.instanceId,
            ownerPid: previous?.ownerPid ?? process.pid,
            ownerBootId: previous?.ownerBootId ?? "00000000-0000-4000-8000-000000000000",
            observedAt: previous?.observedAt ?? (/* @__PURE__ */ new Date(0)).toISOString(),
            targetRevision: target?.revision ?? null,
            candidateId: installed?.candidateId ?? target?.candidateId ?? null,
            buildId: null,
            state: "unknown",
            health: null,
            restartCount: previous?.restartCount ?? 0,
            nextRestartAt: previous?.nextRestartAt ?? null,
            code: "health_stale",
            message: "Host executor status is missing or stale; readiness is not current."
          };
          continue;
        }
        const buildMatches = Boolean(
          executor.buildId && (!installed || executor.buildId === installed.buildId)
        );
        const state = !buildMatches ? "unknown" : executor.state === "ready" ? "ready" : executor.state === "stopping" ? "stopped" : executor.state === "failed" ? "failed" : "starting";
        result[instance.instanceId] = {
          schemaVersion: 1,
          instanceId: instance.instanceId,
          ownerPid: executor.pid,
          ownerBootId: executor.bootId,
          observedAt: executor.observedAt,
          targetRevision: target?.revision ?? null,
          candidateId: buildMatches ? installed?.candidateId ?? target?.candidateId ?? null : null,
          buildId: executor.buildId,
          state,
          health: null,
          restartCount: 0,
          nextRestartAt: null,
          code: buildMatches ? executor.code : "build_mismatch",
          message: !buildMatches ? "Running executor release does not match the installed release." : executor.code ? "Host executor reported " + executor.code + "." : ""
        };
        continue;
      }
      try {
        const health = JSON.parse(
          readFileSync2(
            join4(servicePaths(this.config, instance).data, "health.json"),
            "utf8"
          )
        );
        validateHost("Health", health);
        requireThat(
          health.instanceId === instance.instanceId && health.componentId === instance.componentId,
          "target_conflict",
          "Health file belongs to another configured instance."
        );
        const expectedBuild = target ? this.candidate(target.candidateId).buildId : installed?.buildId;
        const launchMatches = !health.launchId || !target || health.launchId === target.revision;
        const runningTarget = target?.desired !== "stopped";
        const currentReady = freshTimestamp(health.observedAt) && health.ready && runningTarget && launchMatches && (!expectedBuild || health.buildId === expectedBuild);
        const state = currentReady ? "ready" : !freshTimestamp(health.observedAt) ? "unknown" : !runningTarget ? "stopped" : health.ready ? "unknown" : "starting";
        const code = currentReady ? null : !freshTimestamp(health.observedAt) ? "health_stale" : !launchMatches || expectedBuild && health.buildId !== expectedBuild ? "build_mismatch" : !runningTarget ? null : "service_not_ready";
        result[instance.instanceId] = {
          schemaVersion: 1,
          instanceId: instance.instanceId,
          ownerPid: health.pid,
          ownerBootId: health.bootId,
          observedAt: health.observedAt,
          targetRevision: target?.revision ?? health.launchId ?? null,
          candidateId: target?.candidateId ?? installed?.candidateId ?? null,
          buildId: health.buildId,
          state,
          health,
          restartCount: previous?.restartCount ?? 0,
          nextRestartAt: previous?.nextRestartAt ?? null,
          code,
          message: currentReady ? health.details : code === "health_stale" ? "Owner health is stale; readiness is not current." : health.details || "Owner health is not ready."
        };
      } catch {
        if (previous)
          result[instance.instanceId] = {
            ...previous,
            state: target?.desired === "stopped" ? "stopped" : "unknown",
            code: "health_stale",
            message: "Current owner health is missing or invalid; the last ready observation is not current."
          };
      }
    }
    return result;
  }
  managementSnapshot(executor, minimumSequence = 0, status = this.status(executor)) {
    requireThat(
      Number.isSafeInteger(minimumSequence) && minimumSequence >= 0,
      "invalid_arguments",
      "Invalid host observation sequence."
    );
    requireThat(
      status.hostId === this.config.hostId,
      "target_conflict",
      "Management status belongs to another host."
    );
    return this.transaction(() => {
      const prior = this.db.prepare("SELECT value FROM meta WHERE key=?").get("hostReportSequence");
      const sequence = Math.max(Number(prior?.["value"] ?? 0), minimumSequence) + 1;
      requireThat(
        Number.isSafeInteger(sequence),
        "limit_exceeded",
        "Host observation sequence is exhausted."
      );
      const snapshot = { sequence, status };
      validateHost("ManagementSnapshot", snapshot);
      this.db.prepare(
        "INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value"
      ).run("hostReportSequence", String(sequence));
      return snapshot;
    });
  }
  transaction(work) {
    this.db.exec("BEGIN IMMEDIATE");
    try {
      const value = work();
      this.db.exec("COMMIT");
      return value;
    } catch (error) {
      this.db.exec("ROLLBACK");
      throw error;
    }
  }
  instance(instanceId) {
    const instance = this.config.instances.find(
      (instance2) => instance2.instanceId === instanceId
    );
    requireThat(
      instance,
      "not_found",
      "Instance is not in this explicit host configuration."
    );
    return instance;
  }
  normalizeEntry(input) {
    const value = input;
    if (!value.configurationHash) {
      validateHost("JournalEntry", value);
      return value;
    }
    validateHost("LocalRequest", value.request);
    validateShared("DeploymentRecord", value.record);
    const unresolved = !terminalPhases.has(value.record.phase);
    const record = unresolved ? {
      ...value.record,
      phase: "needs_attention",
      errorCode: "legacy_activation_requires_reconciliation"
    } : value.record;
    const entry = {
      schemaVersion: 1,
      request: value.request,
      record,
      candidateId: value.candidateId,
      previousCandidateId: value.previousCandidateId,
      configurationPath: legacyConfigurationPath(
        this.config.runtimeRoot,
        value.configurationHash
      ),
      ...value.sourceSnapshot === void 0 ? {} : { sourceSnapshot: value.sourceSnapshot },
      activation: {
        previousTarget: null,
        previousEnabled: true,
        targetEnabled: value.request.action !== "disable",
        rollbackTargetRevision: null
      }
    };
    validateHost("JournalEntry", entry);
    return entry;
  }
  get(deploymentId) {
    const row = this.db.prepare("SELECT entry_json FROM operations WHERE deployment_id=?").get(deploymentId);
    requireThat(row, "not_found", "Host-local deployment was not found.");
    return this.normalizeEntry(JSON.parse(String(row["entry_json"])));
  }
  operation(operationId) {
    const row = this.db.prepare("SELECT entry_json FROM operations WHERE operation_id=?").get(operationId);
    return row ? this.normalizeEntry(JSON.parse(String(row["entry_json"]))) : null;
  }
  list(limit2 = 200) {
    return this.db.prepare(
      "SELECT entry_json FROM operations ORDER BY created_at DESC,deployment_id DESC LIMIT ?"
    ).all(Math.max(1, Math.min(200, limit2))).map((row) => this.normalizeEntry(JSON.parse(String(row["entry_json"]))));
  }
  pendingEntries() {
    return this.db.prepare(
      "SELECT entry_json FROM operations WHERE phase NOT IN ('succeeded','rolled_back','failed','needs_attention') ORDER BY created_at,deployment_id LIMIT 200"
    ).all().map((row) => this.normalizeEntry(JSON.parse(String(row["entry_json"]))));
  }
  unfinished() {
    return this.db.prepare(
      "SELECT entry_json FROM operations WHERE phase NOT IN ('succeeded','rolled_back','failed','needs_attention') AND json_extract(entry_json,'$.configurationHash') IS NULL ORDER BY created_at,deployment_id LIMIT 200"
    ).all().map((row) => this.normalizeEntry(JSON.parse(String(row["entry_json"]))));
  }
  installed(instanceId) {
    const row = this.db.prepare("SELECT value FROM installed WHERE instance_id=?").get(instanceId);
    return row ? JSON.parse(String(row["value"])) : null;
  }
  setInstalled(value) {
    this.instance(value.instanceId);
    const candidate = this.candidate(value.candidateId);
    requireThat(
      candidate.buildId === value.buildId,
      "build_mismatch",
      "Installed pointer must identify its exact candidate build."
    );
    this.db.prepare(
      "INSERT INTO installed VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value"
    ).run(value.instanceId, canonical(value));
  }
  /** Record the candidates that the reviewed OS bootstrap actually installed. */
  syncBootstrap(plan, clearTargets = false) {
    validateHost("BootstrapPlan", plan);
    this.transaction(() => {
      const requestedAt = (/* @__PURE__ */ new Date()).toISOString();
      for (const process2 of plan.processes) {
        if (!process2.candidateId) continue;
        const instance = this.instance(process2.instanceId);
        requireThat(
          instance.componentId === process2.componentId,
          "target_conflict",
          "Bootstrap process belongs to another configured component."
        );
        const candidate = this.candidate(process2.candidateId);
        this.setInstalled({
          instanceId: process2.instanceId,
          candidateId: candidate.candidateId,
          buildId: candidate.buildId,
          enabled: true,
          installedAt: (/* @__PURE__ */ new Date()).toISOString()
        });
        if (clearTargets) {
          this.db.prepare("DELETE FROM runtime_targets WHERE instance_id=?").run(process2.instanceId);
          continue;
        }
        if (!process2.configPath || process2.componentId === "host-executor")
          continue;
        requireThat(
          inside(this.config.runtimeRoot, process2.configPath),
          "target_conflict",
          "Bootstrap runtime configuration leaves the host runtime root."
        );
        const metadata = lstatSync2(process2.configPath);
        requireThat(
          metadata.isFile() && !metadata.isSymbolicLink() && metadata.size <= 1024 * 1024,
          "configuration_changed",
          "Bootstrap runtime configuration is not a bounded regular file."
        );
        const runtimeConfig = JSON.parse(
          readFileSync2(process2.configPath, "utf8")
        );
        validateHost("InstanceConfig", runtimeConfig);
        requireThat(
          runtimeConfig.instanceId === process2.instanceId && runtimeConfig.componentId === process2.componentId && runtimeConfig.hostId === this.config.hostId && runtimeConfig.artifactRoot === candidate.artifactRoot && runtimeConfig.buildId === candidate.buildId,
          "build_mismatch",
          "Bootstrap runtime configuration does not identify the installed candidate."
        );
        const target = {
          schemaVersion: 1,
          instanceId: process2.instanceId,
          revision: bootstrapLaunchId(plan, process2.instanceId),
          candidateId: candidate.candidateId,
          desired: "running",
          configPath: process2.configPath,
          requestedAt
        };
        validateHost("RuntimeTarget", target);
        this.db.prepare(
          "INSERT INTO runtime_targets VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value"
        ).run(process2.instanceId, canonical(target));
      }
    });
  }
  recordObservation(instanceId, value) {
    validateHost("RuntimeObservation", value);
    requireThat(
      value.instanceId === instanceId,
      "target_conflict",
      "Observation must belong to its instance."
    );
    this.instance(instanceId);
    this.db.prepare(
      "INSERT INTO observations VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value"
    ).run(instanceId, canonical(value, 65536));
  }
  observations() {
    return Object.fromEntries(
      this.db.prepare("SELECT * FROM observations ORDER BY instance_id").all().map((row) => [
        String(row["instance_id"]),
        JSON.parse(String(row["value"]))
      ])
    );
  }
  target(instanceId) {
    const row = this.db.prepare("SELECT value FROM runtime_targets WHERE instance_id=?").get(instanceId);
    return row ? normalizeTarget(JSON.parse(String(row["value"]))) : null;
  }
  restoreTarget(value) {
    validateHost("RuntimeTarget", value);
    this.instance(value.instanceId);
    requireThat(
      this.candidate(value.candidateId),
      "not_found",
      "Restored runtime target requires its installed candidate."
    );
    this.db.prepare(
      "INSERT INTO runtime_targets VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value"
    ).run(value.instanceId, canonical(value));
  }
  saveCandidate(candidate, manifest) {
    validateHost("Candidate", candidate);
    validateHost("ReleaseManifest", manifest);
    const value = manifest;
    requireThat(
      value.buildId === candidate.buildId && value.componentId === candidate.componentId && candidate.candidateId === candidate.buildId,
      "build_mismatch",
      "Candidate identity must match its release manifest."
    );
    this.transaction(() => {
      const prior = this.db.prepare(
        "SELECT candidate_json,manifest_json FROM candidates WHERE candidate_id=?"
      ).get(candidate.candidateId);
      if (prior)
        requireThat(
          prior["candidate_json"] === canonical(candidate) && prior["manifest_json"] === canonical(manifest),
          "mutation_conflict",
          "Candidate is immutable."
        );
      else
        this.db.prepare("INSERT INTO candidates VALUES (?,?,?,?,?)").run(
          candidate.candidateId,
          candidate.componentId,
          candidate.buildId,
          canonical(candidate),
          canonical(manifest)
        );
    });
  }
  candidate(candidateId) {
    const row = this.db.prepare("SELECT candidate_json FROM candidates WHERE candidate_id=?").get(candidateId);
    requireThat(row, "not_found", "Prepared host candidate was not found.");
    return normalizeCandidate(JSON.parse(String(row["candidate_json"])));
  }
  manifest(candidateId) {
    const row = this.db.prepare("SELECT manifest_json FROM candidates WHERE candidate_id=?").get(candidateId);
    requireThat(row, "not_found", "Prepared candidate manifest was not found.");
    return releaseManifest(JSON.parse(String(row["manifest_json"])));
  }
  candidateForBuild(componentId, buildId) {
    const rows = this.db.prepare(
      "SELECT candidate_json FROM candidates WHERE component_id=? AND build_id=? ORDER BY candidate_id LIMIT 2"
    ).all(componentId, buildId);
    requireThat(
      rows.length === 1,
      rows.length ? "ambiguous_candidate" : "not_found",
      "The retained build must identify one exact local candidate."
    );
    return normalizeCandidate(JSON.parse(String(rows[0]["candidate_json"])));
  }
  accept(request, sourceSnapshot) {
    validateHost("LocalRequest", request);
    const requestHash = hashJson(request);
    return this.transaction(() => {
      const previous = this.operation(request.operationId);
      if (previous) {
        requireThat(
          previous.record.requestHash === requestHash,
          "mutation_conflict",
          "Operation ID already identifies different original deployment arguments."
        );
        return previous;
      }
      const instance = this.instance(request.instanceId), installed = this.installed(request.instanceId);
      requireThat(
        !requiresBootstrapReplacement(instance) || !installed,
        "restart_required",
        "Bootstrap-owned releases use prepare followed by install-bootstrap."
      );
      requireThat(
        request.componentId === void 0 || request.componentId === instance.componentId,
        "target_conflict",
        "Source component does not match the configured instance."
      );
      const action = request.action;
      if (action === "deploy")
        requireThat(
          Boolean(request.candidateId) !== Boolean(request.source) && request.targetBuild === void 0,
          "invalid_arguments",
          "Deploy requires exactly one candidate or source."
        );
      else if (action === "rollback")
        requireThat(
          request.targetBuild && !request.candidateId && !request.source && request.componentId === void 0,
          "invalid_arguments",
          "Rollback requires one retained target build."
        );
      else
        requireThat(
          !request.targetBuild && !request.candidateId && !request.source && request.componentId === void 0 && installed,
          "invalid_arguments",
          "Lifecycle actions require one installed instance and no candidate arguments."
        );
      if (request.source) {
        requireThat(
          sourceSnapshot,
          "source_snapshot_required",
          "Capture source inputs before handing the action to the independent executor."
        );
        validateHost("SourceSnapshot", sourceSnapshot);
        requireThat(
          inside(this.config.stagingRoot, sourceSnapshot.sourceRoot) && inside(this.config.stagingRoot, sourceSnapshot.manifestPath),
          "target_conflict",
          "Source snapshot belongs to another host staging root."
        );
      } else
        requireThat(
          !sourceSnapshot,
          "invalid_arguments",
          "This action does not accept a source snapshot."
        );
      const candidate = request.candidateId ? this.candidate(request.candidateId) : request.targetBuild ? this.candidateForBuild(instance.componentId, request.targetBuild) : action !== "deploy" && installed ? this.candidate(installed.candidateId) : null;
      requireThat(
        !candidate || candidate.componentId === instance.componentId,
        "target_conflict",
        "Candidate belongs to another component."
      );
      const previousTarget = this.target(instance.instanceId), previousEnabled = installed?.enabled ?? instance.enabled;
      const retainedPreviousConfiguration = previousTarget ? acceptedInstanceConfigurationPath(
        this.config,
        previousTarget.revision
      ) : void 0;
      const previousConfigurationPath = retainedPreviousConfiguration && existsSync(retainedPreviousConfiguration) ? retainedPreviousConfiguration : void 0;
      const targetEnabled = action === "enable" ? true : action === "disable" ? false : previousEnabled;
      const configurationPath2 = saveAcceptedConfiguration(this.config), now2 = (/* @__PURE__ */ new Date()).toISOString(), deploymentId = randomUUID3();
      const entry = {
        schemaVersion: 1,
        request,
        ...sourceSnapshot ? { sourceSnapshot } : {},
        candidateId: candidate?.candidateId ?? null,
        previousCandidateId: installed?.candidateId ?? null,
        configurationPath: configurationPath2,
        activation: {
          previousTarget,
          ...previousConfigurationPath ? { previousConfigurationPath } : {},
          previousEnabled,
          targetEnabled,
          rollbackTargetRevision: null
        },
        record: {
          deploymentId,
          hostId: this.config.hostId,
          instanceId: instance.instanceId,
          componentId: instance.componentId,
          requestHash,
          previousBuild: installed?.buildId ?? null,
          targetBuild: candidate?.buildId ?? null,
          observedBuild: null,
          phase: candidate ? "prepared" : "preparing",
          createdAt: now2,
          updatedAt: now2,
          readiness: {
            state: "not_run",
            message: "Accepted by the independent host journal."
          }
        }
      };
      validateHost("JournalEntry", entry);
      this.db.prepare("INSERT INTO operations VALUES (?,?,?,?,?,?)").run(
        request.operationId,
        deploymentId,
        requestHash,
        entry.record.phase,
        now2,
        canonical(entry)
      );
      return entry;
    });
  }
  advance(deploymentId, expectedPhase, phase, change = {}) {
    return this.transaction(() => {
      const entry = this.get(deploymentId);
      requireThat(
        entry.record.phase === expectedPhase && (phase === expectedPhase ? !terminalPhases.has(phase) : transitions[expectedPhase].includes(phase)),
        "revision_conflict",
        "Deployment phase changed or transition is invalid."
      );
      if (change.candidateId !== void 0) {
        requireThat(
          entry.record.phase === "preparing",
          "revision_conflict",
          "Candidate can only be fixed during preparation."
        );
        const candidate = this.candidate(change.candidateId);
        requireThat(
          candidate.componentId === entry.record.componentId,
          "target_conflict",
          "Prepared source changed component."
        );
        entry.candidateId = candidate.candidateId;
        entry.record.targetBuild = candidate.buildId;
      }
      entry.record.phase = phase;
      entry.record.updatedAt = (/* @__PURE__ */ new Date()).toISOString();
      if (change.observedBuild !== void 0)
        entry.record.observedBuild = change.observedBuild;
      if (change.readiness) entry.record.readiness = change.readiness;
      if (change.errorCode) entry.record.errorCode = change.errorCode;
      if (change.target) {
        validateHost("RuntimeTarget", change.target);
        requireThat(
          change.target.instanceId === entry.record.instanceId && [entry.candidateId, entry.previousCandidateId].includes(
            change.target.candidateId
          ),
          "target_conflict",
          "Runtime target must belong to this deployment."
        );
        requireThat(
          inside(this.config.runtimeRoot, change.target.configPath),
          "target_conflict",
          "Runtime configuration must belong to this host."
        );
        this.db.prepare(
          "INSERT INTO runtime_targets VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value"
        ).run(change.target.instanceId, canonical(change.target));
      }
      if (change.rollbackTargetRevision !== void 0) {
        requireThat(
          phase === "rolling_back" && change.target?.revision === change.rollbackTargetRevision && change.target.candidateId === entry.previousCandidateId,
          "target_conflict",
          "Rollback revision must identify the retained previous target."
        );
        entry.activation.rollbackTargetRevision = change.rollbackTargetRevision;
      }
      if (change.installed) {
        requireThat(
          change.installed.instanceId === entry.record.instanceId && [entry.candidateId, entry.previousCandidateId].includes(
            change.installed.candidateId
          ),
          "target_conflict",
          "Installed pointer must belong to this deployment."
        );
        this.setInstalled(change.installed);
      }
      validateShared("DeploymentRecord", entry.record);
      validateHost("JournalEntry", entry);
      this.db.prepare(
        "UPDATE operations SET phase=?,entry_json=? WHERE deployment_id=?"
      ).run(phase, canonical(entry), deploymentId);
      return entry;
    });
  }
};

// packages/host-runtime/src/host-config.ts
import { mkdir, realpath as realpath2, access } from "node:fs/promises";
import { constants } from "node:fs";
import { isAbsolute as isAbsolute3, resolve as resolve2, dirname as dirname3, basename as basename2, join as join6 } from "node:path";

// packages/host-runtime/src/native-backup-paths.ts
import { isAbsolute as isAbsolute2, join as join5 } from "node:path";

// packages/host-runtime/src/codex-home.ts
import { homedir as homedir2 } from "node:os";
import { posix as posix2, win32 as win322 } from "node:path";
var usesExternalAppServer = (settings) => settings.appServer?.mode !== "owned-stdio";
function expectedServerHome(settings) {
  return settings.appServer?.mode === "external-proxy" && settings.appServer.expectedCodexHome !== void 0 ? settings.appServer.expectedCodexHome : resolveCodexHome(settings).path;
}
function resolveCodexHome(settings, options = {}) {
  const platform = options.platform ?? process.platform, environment = options.environment ?? process.env;
  const paths = platform === "win32" ? win322 : posix2;
  const identity = (path) => platform === "win32" ? paths.normalize(path).toLowerCase() : paths.normalize(path);
  requireThat(
    !(settings.codexHome && settings.nativeHome) || identity(settings.codexHome) === identity(settings.nativeHome),
    "invalid_arguments",
    "codexHome and the existing explicit nativeHome must not select different homes."
  );
  const configured = settings.codexHome ?? settings.nativeHome;
  if (configured !== void 0) {
    requireThat(configured.trim().length > 0 && paths.isAbsolute(configured), "invalid_arguments", "Explicit Codex home must be an absolute path.");
    return { path: paths.normalize(configured), source: settings.codexHome !== void 0 ? "host-configuration" : "existing-native-home" };
  }
  const inherited = environment["CODEX_HOME"]?.trim();
  if (inherited) {
    return { path: paths.resolve(options.cwd ?? process.cwd(), inherited), source: "environment" };
  }
  const userHome = options.userHome ?? environment[platform === "win32" ? "USERPROFILE" : "HOME"] ?? homedir2();
  requireThat(paths.isAbsolute(userHome), "invalid_arguments", "The executing account must have an absolute home directory.");
  return { path: paths.join(userHome, ".codex"), source: "user-home" };
}
function instanceOwnedCodexHome(settings, dataRoot, platform = process.platform) {
  if (settings.codexHome === void 0 && settings.nativeHome === void 0) return null;
  const paths = platform === "win32" ? win322 : posix2;
  const home = resolveCodexHome(settings, { platform }).path, local = paths.relative(dataRoot, home);
  return local && local !== ".." && !local.startsWith(".." + paths.sep) && !paths.isAbsolute(local) ? home : null;
}

// packages/host-runtime/src/native-backup-paths.ts
function sharedNativeHomes(config) {
  return config.instances.filter((instance) => instance.componentId === "agent-manager").flatMap((instance) => {
    const settings = instance.settings;
    if (!usesExternalAppServer(instance.settings) && instanceOwnedCodexHome(settings, servicePaths(config, instance).data)) return [];
    const home = expectedServerHome(instance.settings);
    requireThat(
      typeof home === "string" && isAbsolute2(home) && [...hostStorageAreas(config).map((area) => area.path), config.artifactRoot, config.stagingRoot].every((root) => !inside(root, home) && !inside(home, root)),
      "target_conflict",
      "Shared Codex homes must not overlap owned host, service, project or software storage. Use explicit owned-stdio for an instance-owned home."
    );
    return [{ instanceId: instance.instanceId, path: home, kind: "external-user-state" }];
  }).sort((a, b) => a.instanceId.localeCompare(b.instanceId));
}

// packages/host-runtime/src/host-config.ts
async function hostConfig(path) {
  return checkedHostConfig(await jsonFile(path), resolve2(path));
}
async function checkedHostConfig(input, configurationPath2) {
  validateHost("HostConfigInput", input);
  const config = resolveHostConfiguration(input, resolve2(configurationPath2));
  validateHost("HostConfig", config);
  baseUrl(config.publicBaseUrl);
  requireThat(!config.configPath || await resolvedFuturePath(config.configPath) === await resolvedFuturePath(configurationPath2), "target_conflict", "Central configPath must identify this actual host configuration file.");
  requireThat(new Set(config.instances.map((instance) => instance.instanceId)).size === config.instances.length && new Set(config.instances.map((instance) => instance.serviceNodeId)).size === config.instances.length, "invalid_arguments", "Host instance and Service Node identities must be unique.");
  const roots = [config.runtimeRoot, config.artifactRoot, config.stagingRoot];
  requireThat(roots.every(isAbsolute3) && roots.every((root, index) => roots.every((other, otherIndex) => index === otherIndex || !inside(root, other))), "invalid_arguments", "Host runtime, artifact and staging roots must be disjoint absolute directories.");
  requireThat(!inside(config.artifactRoot, resolve2(configurationPath2)) && !inside(config.stagingRoot, resolve2(configurationPath2)), "invalid_arguments", "Private host configuration cannot live inside artifacts or source staging.");
  const serviceDirectories = config.instances.flatMap((instance) => Object.values(servicePaths(config, instance)));
  requireThat(
    serviceDirectories.every((value, index) => isAbsolute3(value) && serviceDirectories.every((other, otherIndex) => index === otherIndex || !inside(value, other))),
    "target_conflict",
    "Service data, work and logs collide; configure explicitly separate paths for repeated service instances."
  );
  requireThat(
    serviceDirectories.every((value) => [config.artifactRoot, config.stagingRoot, ...config.servicesRoot ? [config.runtimeRoot] : []].every((root) => !inside(root, value) && !inside(value, root))) && serviceDirectories.every((value) => !inside(value, resolve2(configurationPath2))),
    "target_conflict",
    "Service storage must not overlap software, staging, host state or central configuration."
  );
  for (const root of roots) await mkdir(root, { recursive: true });
  const actualRoots = await Promise.all(roots.map((root) => realpath2(root)));
  requireThat(actualRoots.every((root, index) => actualRoots.every((other, otherIndex) => index === otherIndex || !inside(root, other))), "invalid_arguments", "Host roots overlap through filesystem links.");
  const actualServices = await Promise.all(serviceDirectories.map((directory) => resolvedFuturePath(directory)));
  requireThat(
    actualServices.every((value, index) => actualServices.every((other, otherIndex) => index === otherIndex || !inside(value, other)) && actualRoots.slice(config.servicesRoot ? 0 : 1).every((root) => !inside(root, value) && !inside(value, root))),
    "target_conflict",
    "Service storage overlaps through filesystem links."
  );
  const internal = await Promise.all([...new Set(config.instances.filter((instance) => instance.componentId === "agent-manager").map((instance) => instance.settings["internalProjectRoot"]).filter((value) => typeof value === "string"))].map(async (path) => {
    requireThat(isAbsolute3(path), "invalid_arguments", "Internal project root must be absolute.");
    return resolvedFuturePath(path);
  }));
  const shared = await Promise.all(sharedNativeHomes(config).map((home) => resolvedFuturePath(home.path)));
  const configuration = resolve2(config.configPath ?? configurationPath2);
  requireThat(
    internal.every((path, index) => [...actualRoots, ...actualServices, ...shared].every((root) => !inside(root, path) && !inside(path, root)) && internal.every((other, otherIndex) => index === otherIndex || !inside(path, other)) && !inside(path, configuration)),
    "target_conflict",
    "Internal project storage overlaps service state, software, shared Codex homes or configuration."
  );
  for (const instance of config.instances) if (instance.componentId === "agent-manager" && instance.settings["projectRoot"] !== void 0) {
    requireThat(typeof instance.settings["projectRoot"] === "string" && isAbsolute3(instance.settings["projectRoot"]), "invalid_arguments", "Normal project root must be absolute.");
    const normal = await resolvedFuturePath(instance.settings["projectRoot"]);
    requireThat(
      [...actualRoots, ...actualServices, ...shared, ...internal].every((root) => !inside(root, normal)),
      "target_conflict",
      "New normal projects must not be allocated inside private service/internal/Codex storage or executable artifacts."
    );
  }
  for (const directory of serviceDirectories) await mkdir(directory, { recursive: true });
  await Promise.all([...roots, ...serviceDirectories].map((directory) => access(directory, constants.W_OK)));
  for (const home of sharedNativeHomes(config)) {
    let ancestor = resolve2(home.path);
    const suffix = [];
    let actual;
    while (true) {
      try {
        actual = join6(await realpath2(ancestor), ...suffix.reverse());
        break;
      } catch (error) {
        if (error.code !== "ENOENT") throw error;
        const parent = dirname3(ancestor);
        requireThat(parent !== ancestor, "target_conflict", "Shared home has no existing ancestor.");
        suffix.push(basename2(ancestor));
        ancestor = parent;
      }
    }
    requireThat([...actualRoots, ...actualServices].every((root) => !inside(root, actual) && !inside(actual, root)), "target_conflict", "Shared Codex homes cannot overlap owned runtime, service, artifact or staging storage through filesystem links.");
  }
  for (const path of Object.values(config.executables)) requireThat(isAbsolute3(path), "invalid_arguments", "Host runtime executable mappings require absolute paths.");
  for (const instance of config.instances) {
    requireThat(instance.engine !== "docker" || process.platform === "linux" && instance.docker, "invalid_arguments", "Docker instances require an explicit Linux container configuration.");
    requireThat(/^[a-zA-Z0-9][a-zA-Z0-9_-]{0,127}$/.test(instance.instanceId) && /^[a-z][a-z0-9-]*$/.test(instance.componentId), "invalid_arguments", "Instance/component IDs must be safe local names.");
  }
  return config;
}

// packages/host-runtime/src/runtime-owner.ts
import { join as join9, resolve as resolve4 } from "node:path";
import { mkdir as mkdir2 } from "node:fs/promises";
import { randomUUID as randomUUID4 } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";

// packages/host-runtime/src/process.ts
import { spawn } from "node:child_process";
import { isAbsolute as isAbsolute4, resolve as resolve3, join as join7 } from "node:path";
import { realpath as realpath3, stat } from "node:fs/promises";
function runtimeEnvironment(extra = {}) {
  const allowed = /* @__PURE__ */ new Set(["systemroot", "windir", "comspec", "path", "pathext", "temp", "tmp", "tmpdir", "userprofile", "home", "codex_home", "appdata", "localappdata", "programdata", "allusersprofile", "programfiles", "programfiles(x86)", "lang", "lc_all", "timezone"]);
  const overrides = new Set(Object.keys(extra).map((key) => key.toLowerCase()));
  const env = {};
  for (const [key, value] of Object.entries(process.env)) if (value !== void 0 && allowed.has(key.toLowerCase()) && !overrides.has(key.toLowerCase())) env[key] = value;
  return { ...env, NODE_NO_WARNINGS: "1", ...extra };
}
async function resolveCommand(command, root, executables) {
  const base = await realpath3(root), cwd = await realpath3(resolve3(base, command.cwd ?? "."));
  requireThat(inside(base, cwd), "invalid_arguments", "Command working directory leaves its artifact/source root.");
  const configured = Object.hasOwn(executables, command.executable) ? executables[command.executable] : null;
  const requested = configured ?? resolve3(base, command.executable);
  const executable = await realpath3(requested);
  requireThat(configured !== null && isAbsolute4(configured) || !isAbsolute4(command.executable) && inside(base, executable), "invalid_arguments", "Command executable must be inside the artifact or explicitly mapped by host configuration.");
  requireThat((await stat(executable)).isFile() && !/\.(?:cmd|bat|ps1)$/i.test(executable), "invalid_arguments", "Commands need a direct executable; shell scripts require an explicit interpreter and argument array.");
  return { executable, cwd, args: command.args };
}
async function startProcess(command, root, executables, options = {}) {
  const secrets = (options.redact ?? []).filter(Boolean);
  requireThat(secrets.every((secret) => secret.length <= 65536), "invalid_arguments", "Redaction values exceed their bounded length.");
  const maxOutputBytes = options.maxOutputBytes ?? 65536;
  requireThat(
    Number.isSafeInteger(maxOutputBytes) && maxOutputBytes >= 65536 && maxOutputBytes <= 16 * 1024 * 1024,
    "invalid_arguments",
    "Captured command output needs an explicit bounded size."
  );
  const resolved = await resolveCommand(command, root, executables);
  const windows = process.platform === "win32";
  const useJobLauncher = windows && options.useJobLauncher !== false;
  const launcher = useJobLauncher ? await realpath3(options.jobLauncher ?? join7(root, "dist", "native", "ivy-job.exe")) : resolved.executable;
  const args = useJobLauncher ? ["--parent", String(process.pid), ...options.allowWindowsBreakaway ? ["--allow-breakaway"] : [], resolved.executable, ...resolved.args] : resolved.args;
  const cwd = options.executionCwd === void 0 ? resolved.cwd : await realpath3(options.executionCwd);
  const child = spawn(launcher, args, { cwd, env: runtimeEnvironment(options.environment), windowsHide: true, detached: !windows, stdio: ["pipe", "pipe", "pipe"] });
  let ended = false, errorCode = null, truncated = false;
  const output = { stdout: Buffer.alloc(0), stderr: Buffer.alloc(0) };
  const retained = Math.max(1, ...secrets.map((secret) => secret.length)) - 1;
  const tails = { stdout: "", stderr: "" }, decoders = { stdout: new TextDecoder(), stderr: new TextDecoder() };
  const append = (stream, value) => {
    const safe = Buffer.from(secrets.reduce((result, secret) => result.replaceAll(secret, "[redacted]"), value));
    output[stream] = Buffer.concat([output[stream], safe]);
    if (output[stream].length > maxOutputBytes) {
      output[stream] = output[stream].subarray(-maxOutputBytes);
      truncated = true;
    }
    try {
      options.onOutput?.(stream, safe.toString("utf8"));
    } catch {
    }
  };
  const received = (stream, chunk) => {
    if (ended) return;
    if (options.captureOutput === false) return;
    const text = tails[stream] + decoders[stream].decode(chunk, { stream: chunk !== void 0 });
    let boundary = chunk === void 0 ? text.length : Math.max(0, text.length - retained);
    for (const secret of secrets) {
      let at = text.indexOf(secret);
      while (at >= 0 && at < boundary) {
        if (at + secret.length > boundary) boundary = at;
        at = text.indexOf(secret, at + 1);
      }
    }
    append(stream, text.slice(0, boundary));
    tails[stream] = text.slice(boundary);
  };
  child.stdout.on("data", (bytes) => received("stdout", bytes));
  child.stderr.on("data", (bytes) => received("stderr", bytes));
  const signal = (hard) => {
    if (ended || !child.pid) return;
    try {
      if (windows) child.kill();
      else process.kill(-child.pid, hard ? "SIGKILL" : "SIGTERM");
    } catch {
    }
  };
  let finishCompletion;
  const completion = new Promise((resolve6) => {
    finishCompletion = resolve6;
  });
  const finish = (exitCode, exitSignal) => {
    if (ended) return;
    received("stdout");
    received("stderr");
    ended = true;
    finishCompletion({ exitCode, signal: exitSignal, errorCode, stdout: output.stdout.toString("utf8"), stderr: output.stderr.toString("utf8"), truncated });
  };
  child.on("error", () => {
    if (!ended) errorCode = "spawn_failed";
  });
  const exited = (exitCode, exitSignal) => {
    if (ended) return;
    if (!windows && child.pid) {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {
      }
    }
    finish(exitCode, exitSignal);
  };
  if (useJobLauncher) child.once("exit", exited);
  child.once("close", exited);
  let stopping = null;
  const stop = (graceMs = 1500) => {
    requireThat(Number.isSafeInteger(graceMs) && graceMs >= 0 && graceMs <= 6e4, "invalid_arguments", "Process stop grace must be finite and at most one minute.");
    if (stopping || ended) return stopping ?? completion;
    signal(false);
    const hard = setTimeout(() => signal(true), graceMs);
    const final = setTimeout(() => {
      signal(true);
      errorCode = "outcome_unknown";
      truncated = true;
      finish(child.exitCode, child.signalCode);
      child.stdin?.destroy();
      child.stdout?.destroy();
      child.stderr?.destroy();
      child.unref();
    }, graceMs + 2e3);
    stopping = completion.finally(() => {
      clearTimeout(hard);
      clearTimeout(final);
    });
    return stopping;
  };
  return { child, completion, stop };
}
async function runCommand(command, root, executables, options = {}) {
  requireThat(options.input === void 0 || Buffer.byteLength(options.input) <= 65536, "limit_exceeded", "Command input exceeds its bounded size.");
  const process2 = await startProcess(command, root, executables, options);
  let timedOut = false;
  const deadline = setTimeout(() => {
    timedOut = true;
    void process2.stop(250);
  }, command.timeoutMs);
  try {
    if (options.input !== void 0) {
      process2.child.stdin.on("error", () => void 0);
      process2.child.stdin.end(options.input);
    }
    const result = await process2.completion;
    if (timedOut) throw new IvyError("deadline_exceeded", "Command exceeded its finite deadline.", "unknown");
    if (result.exitCode !== 0 || result.errorCode) throw new IvyError(
      result.errorCode ?? "command_failed",
      "Command failed; bounded diagnostics record the outcome.",
      result.errorCode === "spawn_failed" && process2.child.pid === void 0 ? "not_executed" : "unknown",
      { exitCode: result.exitCode, signal: result.signal }
    );
    return result;
  } finally {
    clearTimeout(deadline);
  }
}

// packages/host-runtime/src/service-log.ts
import { appendFileSync, mkdirSync as mkdirSync3, lstatSync as lstatSync3, renameSync, unlinkSync } from "node:fs";
import { join as join8 } from "node:path";
function serviceLog(root) {
  mkdirSync3(root, { recursive: true, mode: 448 });
  const info = lstatSync3(root);
  requireThat(info.isDirectory() && !info.isSymbolicLink(), "target_conflict", "Service log directory must not be a link.");
  const current = join8(root, "process.log"), previous = join8(root, "process.previous.log");
  const metadata = (path) => {
    try {
      const value = lstatSync3(path);
      requireThat(value.isFile() && !value.isSymbolicLink(), "target_conflict", "Service log target is not a regular file.");
      return value;
    } catch (error) {
      if (error.code === "ENOENT") return null;
      throw error;
    }
  };
  metadata(current);
  metadata(previous);
  return (stream, text) => {
    const bytes = Buffer.from((/* @__PURE__ */ new Date()).toISOString() + " " + stream + " " + text).subarray(-65536);
    if ((metadata(current)?.size ?? 0) + bytes.length > 1048576) {
      if (metadata(previous)) unlinkSync(previous);
      if (metadata(current)) renameSync(current, previous);
    }
    appendFileSync(current, bytes, { mode: 384 });
  };
}

// packages/host-runtime/src/runtime-owner.ts
var now = () => (/* @__PURE__ */ new Date()).toISOString();
var safeMessage = (message) => message.slice(0, 4096);
var processCode = (result) => {
  let code = result.errorCode ?? (result.exitCode === 0 ? "process_stopped" : "process_exited");
  for (const line of result.stderr.trim().split(/\r?\n/).slice(-4)) {
    try {
      const value = JSON.parse(line);
      if (typeof value.code === "string" && /^[a-z][a-z0-9_]{0,127}$/.test(value.code))
        code = value.code;
    } catch {
    }
  }
  return code;
};
var directHealthProbe = (command) => command.executable === "node" && command.args.length === 1 && /^dist\/(?:packages\/host-runtime\/src|services\/[a-z][a-z0-9-]*\/src)\/health\.js$/.test(
  command.args[0] ?? ""
);
var selectedSecret = (settings, pointer) => {
  let value = { settings };
  for (const part of pointerParts(pointer)) {
    if (value === null || typeof value !== "object" || Array.isArray(value) || !Object.hasOwn(value, part))
      return void 0;
    value = value[part];
  }
  return value;
};
var secretStrings = (value) => {
  if (typeof value === "string") return value ? [value] : [];
  if (Array.isArray(value)) return value.flatMap(secretStrings);
  if (value && typeof value === "object")
    return Object.values(value).flatMap(secretStrings);
  return [];
};
var RuntimeOwner = class {
  constructor(host, instanceId, bootstrapRoot) {
    this.instanceId = instanceId;
    this.bootstrapRoot = bootstrapRoot;
    requireThat(
      process.platform === "win32",
      "unsupported_runtime",
      "RuntimeOwner is reserved for Windows process services; Linux uses systemd directly."
    );
    requireThat(
      host.instances.find((value) => value.instanceId === instanceId)?.engine === "process",
      "unsupported_runtime",
      "RuntimeOwner cannot own Docker instances."
    );
    this.lock = new ExecutorLock(join9(host.runtimeRoot, "owners", instanceId));
    try {
      this.journal = new HostJournal(host);
      this.journal.instance(instanceId);
      const previous = this.journal.observations()[instanceId];
      this.blocked = previous?.state === "unknown" && previous.code === "outcome_unknown" && previous.ownerPid === process.pid;
    } catch (error) {
      this.lock.close();
      throw error;
    }
  }
  instanceId;
  bootstrapRoot;
  lock;
  journal;
  bootId = randomUUID4();
  controller = new AbortController();
  task = null;
  child = null;
  childResult = null;
  active = null;
  config = null;
  manifest = null;
  restarts = 0;
  nextRestart = 0;
  startedAt = 0;
  unreadySince = 0;
  lastReady = false;
  checkAt = 0;
  restartAllowed = true;
  restartCode = "restart_backoff";
  blocked = false;
  closed = false;
  lastObservation = "";
  get host() {
    return this.journal.config;
  }
  get completion() {
    return this.task ?? Promise.resolve();
  }
  start() {
    if (!this.task) this.task = this.run();
  }
  async close() {
    if (this.closed) return;
    this.closed = true;
    this.controller.abort();
    try {
      await this.task;
      await this.stopChild();
      if (!this.blocked)
        this.observe("stopped", null, "Runtime owner stopped.");
    } finally {
      this.journal.close();
      this.lock.close();
    }
  }
  observe(state, code = null, message = "", health = null) {
    const target = this.active;
    const value = {
      schemaVersion: 1,
      instanceId: this.instanceId,
      ownerPid: process.pid,
      ownerBootId: this.bootId,
      observedAt: now(),
      targetRevision: target?.revision ?? null,
      candidateId: target?.candidateId ?? null,
      buildId: target === this.active ? this.config?.buildId ?? null : null,
      state,
      health,
      restartCount: this.restarts,
      nextRestartAt: this.nextRestart ? new Date(this.nextRestart).toISOString() : null,
      code,
      message: safeMessage(message)
    };
    const semantic = JSON.stringify({
      ...value,
      observedAt: "",
      health: health ? { ...health, observedAt: "" } : null
    });
    if (semantic === this.lastObservation) return;
    this.lastObservation = semantic;
    this.journal.recordObservation(this.instanceId, value);
  }
  async stopChild() {
    const dataRoot = this.config?.dataRoot ?? servicePaths(this.host, this.journal.instance(this.instanceId)).data;
    if (!this.child) return;
    this.observe("draining");
    const timeout = this.manifest?.shutdown?.timeoutMs ?? 15e3;
    try {
      const health = await jsonFile(join9(dataRoot, "health.json"));
      if (health.launchId && health.launchId === this.active?.revision) {
        const control = {
          schemaVersion: 1,
          instanceId: this.instanceId,
          launchId: health.launchId,
          bootId: health.bootId,
          action: "shutdown",
          requestedAt: now()
        };
        await atomicJson(join9(dataRoot, "control.json"), control);
      }
    } catch {
    }
    const result = await this.child.stop(timeout);
    this.child = null;
    this.childResult = null;
    if (result.errorCode === "outcome_unknown") {
      this.blocked = true;
      this.observe(
        "unknown",
        "outcome_unknown",
        "The owned process tree did not establish a stopped outcome; no replacement will start."
      );
      throw new IvyError(
        "outcome_unknown",
        "The owned process tree did not establish a stopped outcome.",
        "unknown"
      );
    }
    this.observe("stopped", null, "Owned process stopped.");
  }
  async select(target) {
    requireThat(
      inside(this.host.runtimeRoot, target.configPath),
      "target_conflict",
      "Instance configuration leaves host runtime data."
    );
    if (target.desired === "stopped") {
      await this.stopChild();
      this.active = target;
      this.manifest = this.journal.manifest(target.candidateId);
      this.config = await instanceConfig(target.configPath).catch(() => null);
      return;
    }
    const config = await instanceConfig(target.configPath);
    requireThat(
      config.instanceId === this.instanceId && config.hostId === this.host.hostId,
      "target_conflict",
      "Runtime target has no matching local identity."
    );
    requireThat(
      config.artifactRoot === this.journal.candidate(target.candidateId).artifactRoot && config.dataRoot === servicePaths(this.host, this.journal.instance(this.instanceId)).data,
      "build_mismatch",
      "Runtime target identity does not match its accepted candidate or service storage."
    );
    const manifest = this.journal.manifest(target.candidateId);
    requireThat(
      !manifest.connectsToHive || config.credential,
      "invalid_arguments",
      "A Hive service requires its own credential."
    );
    await this.stopChild();
    this.active = target;
    this.config = config;
    this.manifest = manifest;
    this.restarts = 0;
    this.nextRestart = 0;
    this.startedAt = 0;
    this.unreadySince = 0;
    this.lastReady = false;
    this.checkAt = 0;
    this.restartAllowed = true;
  }
  async launch() {
    const target = this.active, manifest = this.manifest;
    const selected = this.config, refreshed = await instanceConfig(target.configPath);
    requireThat(
      refreshed.instanceId === this.instanceId && refreshed.hostId === this.host.hostId && refreshed.componentId === selected.componentId && refreshed.serviceNodeId === selected.serviceNodeId && refreshed.artifactRoot === selected.artifactRoot && refreshed.buildId === selected.buildId && refreshed.dataRoot === selected.dataRoot,
      "configuration_changed",
      "Reloaded instance configuration no longer belongs to the selected release and storage owner."
    );
    this.config = refreshed;
    const config = this.config;
    this.observe(
      "starting",
      "artifact_verification",
      "Checking the selected release stamp and entrypoint."
    );
    requireRuntimeRequirements(manifest.requirements);
    await verifyLaunchCandidate(
      this.journal.candidate(target.candidateId),
      this.host,
      manifest.entrypoint?.args[0]
    );
    this.observe("starting");
    this.childResult = null;
    this.startedAt = Date.now();
    this.unreadySince = this.startedAt;
    this.checkAt = 0;
    const instance = this.journal.instance(this.instanceId), entrypoint = manifest.entrypoint;
    const workRoot = config.workRoot && entrypoint.executable === "node" && entrypoint.args[0]?.startsWith("dist/") ? config.workRoot : void 0;
    if (workRoot) await mkdir2(workRoot, { recursive: true });
    const command = workRoot ? {
      ...entrypoint,
      args: [
        resolve4(config.artifactRoot, entrypoint.args[0]),
        ...entrypoint.args.slice(1)
      ]
    } : entrypoint;
    this.child = await startProcess(
      command,
      config.artifactRoot,
      this.host.executables,
      {
        environment: {
          IVY_INSTANCE_CONFIG: target.configPath,
          IVY_LAUNCH_ID: target.revision
        },
        allowWindowsBreakaway: instance.process?.allowWindowsBreakaway ?? false,
        ...workRoot ? { executionCwd: workRoot } : {},
        ...config.logsRoot ? { onOutput: serviceLog(config.logsRoot) } : {},
        jobLauncher: join9(this.bootstrapRoot, "dist/native/ivy-job.exe"),
        redact: this.secrets()
      }
    );
    const owned = this.child;
    void owned.completion.then((result) => {
      if (this.child === owned) this.childResult = result;
    });
  }
  secrets() {
    const configured = this.host.instances.flatMap((instance) => [
      ...instance.credential ? [instance.credential] : [],
      ...(instance.secretPaths ?? []).flatMap(
        (path) => secretStrings(selectedSecret(instance.settings, path))
      )
    ]);
    if (this.config) {
      const instance = this.host.instances.find(
        (value) => value.instanceId === this.instanceId
      );
      configured.push(
        ...this.config.credential ? [this.config.credential] : []
      );
      if (instance)
        configured.push(
          ...(instance.secretPaths ?? []).flatMap(
            (path) => secretStrings(selectedSecret(this.config.settings, path))
          )
        );
    }
    return [...new Set(configured)].filter(Boolean);
  }
  backoff(code) {
    this.restartCode = code;
    this.lastReady = false;
    const policy = this.manifest?.restart;
    this.restarts++;
    this.nextRestart = Date.now() + Math.min(
      policy?.maximumDelayMs ?? 6e4,
      (policy?.minimumDelayMs ?? 1e3) * 2 ** Math.min(this.restarts - 1, 16)
    );
    this.observe(
      "failed",
      code,
      "Readiness or process exit failed; restart backoff is active."
    );
  }
  async tick() {
    const target = this.journal.target(this.instanceId);
    if (!target) {
      if (this.child) await this.stopChild();
      this.active = null;
      this.config = null;
      this.manifest = null;
      this.observe("idle");
      return;
    }
    if (target.revision !== this.active?.revision) {
      await this.select(target);
    }
    if (this.blocked) {
      this.observe(
        "unknown",
        "outcome_unknown",
        "Runtime ownership is blocked until the last process outcome is reconciled."
      );
      return;
    }
    if (target.desired === "stopped") {
      await this.stopChild();
      this.observe("stopped");
      return;
    }
    requireThat(
      this.config && this.manifest,
      "configuration_changed",
      "The running target has no accepted instance configuration."
    );
    if (this.child && this.childResult) {
      const result = this.childResult;
      this.child = null;
      this.childResult = null;
      if (result.errorCode === "outcome_unknown") {
        this.blocked = true;
        this.observe(
          "unknown",
          "outcome_unknown",
          "The owned process outcome is unknown; no second launch will be attempted."
        );
        return;
      }
      const failed = result.exitCode !== 0 || result.errorCode !== null;
      const code = processCode(result);
      if (this.manifest.restart.policy === "never" || this.manifest.restart.policy === "on-failure" && !failed) {
        this.restartAllowed = false;
        this.observe("exited", failed ? code : null);
        return;
      }
      this.backoff(code);
    }
    if (!this.child) {
      if (!this.restartAllowed) {
        this.observe("exited");
        return;
      }
      if (Date.now() < this.nextRestart) {
        this.observe("failed", this.restartCode);
        return;
      }
      try {
        await this.launch();
        this.nextRestart = 0;
      } catch (error) {
        const failure = IvyError.from(error);
        if (failure.code === "outcome_unknown") {
          this.blocked = true;
          this.observe("unknown", failure.code, failure.message);
          return;
        }
        this.backoff(failure.code);
        return;
      }
    }
    let health = null;
    try {
      const instance = this.journal.instance(this.instanceId);
      const endpoint = void 0;
      try {
        health = await checkHealth(this.config, endpoint, !this.lastReady);
      } catch (error) {
        const failure = IvyError.from(error);
        if (failure.code === "outcome_unknown") {
          if (!this.unreadySince) this.unreadySince = Date.now();
          this.observe(
            "starting",
            "service_not_ready",
            "Health dependency outcome is not yet observable."
          );
          return;
        }
        throw error;
      }
      requireThat(
        health.launchId === target.revision,
        "service_not_ready",
        "Health belongs to a previous launch."
      );
      if (!this.lastReady && Date.now() >= this.checkAt) {
        await runCommand(
          this.manifest.readiness.command,
          this.config.artifactRoot,
          this.host.executables,
          {
            environment: {
              IVY_INSTANCE_CONFIG: target.configPath,
              IVY_LAUNCH_ID: target.revision,
              ...endpoint ? { IVY_HIVE_HEALTH_URL: endpoint } : {}
            },
            jobLauncher: join9(this.bootstrapRoot, "dist/native/ivy-job.exe"),
            redact: this.secrets(),
            useJobLauncher: !directHealthProbe(
              this.manifest.readiness.command
            )
          }
        );
        this.lastReady = true;
        this.checkAt = Number.POSITIVE_INFINITY;
      }
      requireThat(
        this.lastReady,
        "service_not_ready",
        "Manifest readiness has not passed."
      );
      this.unreadySince = 0;
      if (Date.now() - this.startedAt >= 3e4) this.restarts = 0;
      this.observe("ready", null, "", health);
    } catch (error) {
      const failure = IvyError.from(error);
      if (failure.code === "outcome_unknown") {
        this.blocked = true;
        this.observe("unknown", failure.code, failure.message, health);
        return;
      }
      if (!this.unreadySince) this.unreadySince = Date.now();
      if (Date.now() - this.unreadySince > this.manifest.readiness.timeoutMs) {
        try {
          await this.stopChild();
        } catch {
          this.blocked = true;
          return;
        }
        this.backoff("readiness_deadline");
      } else
        this.observe(
          "starting",
          "service_not_ready",
          safeMessage("Readiness failed: " + failure.code + "."),
          health
        );
    }
  }
  async run() {
    while (!this.controller.signal.aborted) {
      try {
        await this.tick();
      } catch (error) {
        const failure = IvyError.from(error);
        this.observe(
          failure.code === "outcome_unknown" ? "unknown" : "failed",
          failure.code,
          safeMessage(failure.message)
        );
        if (failure.code === "outcome_unknown") this.blocked = true;
      }
      try {
        await delay(500, void 0, { signal: this.controller.signal });
      } catch {
        break;
      }
    }
  }
};

// packages/host-runtime/src/runtime-maintenance.ts
import { lstat } from "node:fs/promises";
import { dirname as dirname4, join as join10 } from "node:path";
var runtimeResetMarker = (config) => join10(config.ivyRoot ?? dirname4(config.runtimeRoot), "reset", "maintenance.json");
async function runtimeResetActive(config) {
  try {
    return (await lstat(runtimeResetMarker(config))).isFile();
  } catch (error) {
    if (error.code === "ENOENT") return false;
    throw error;
  }
}

// specs/schemas/component.schema.json
var component_schema_default = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://ivy.invalid/schemas/component.schema.json",
  title: "Ivy component build plan",
  type: "object",
  properties: {
    schemaVersion: {
      const: 1
    },
    componentId: {
      type: "string",
      pattern: "^[a-z][a-z0-9-]*$",
      maxLength: 128
    },
    kind: {
      enum: [
        "hive",
        "service",
        "service-manager",
        "app",
        "native"
      ]
    },
    version: {
      type: "string",
      minLength: 1,
      maxLength: 128
    },
    description: {
      type: "string",
      minLength: 1,
      maxLength: 2048
    },
    connectsToHive: {
      type: "boolean"
    },
    requirements: {
      type: "object",
      properties: {
        node: {
          type: "string",
          pattern: "^>=(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*) <(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$",
          maxLength: 128
        },
        os: {
          type: "array",
          items: { enum: ["win32", "linux"] },
          uniqueItems: true,
          minItems: 1,
          maxItems: 2
        },
        arch: {
          type: "array",
          items: { enum: ["x64", "arm64"] },
          uniqueItems: true,
          minItems: 1,
          maxItems: 2
        },
        hiveProtocol: {
          anyOf: [
            {
              const: 1
            },
            {
              type: "null"
            }
          ]
        },
        contracts: {
          type: "array",
          maxItems: 128,
          items: {
            type: "object",
            properties: {
              key: {
                type: "string",
                minLength: 1,
                maxLength: 192
              },
              readVersions: {
                type: "array",
                items: {
                  type: "string",
                  pattern: "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$"
                },
                uniqueItems: true,
                maxItems: 64
              },
              writeVersions: {
                type: "array",
                items: {
                  type: "string",
                  pattern: "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$"
                },
                uniqueItems: true,
                maxItems: 64
              },
              readScope: {
                type: "object",
                properties: {
                  roots: {
                    anyOf: [
                      {
                        type: "null"
                      },
                      {
                        type: "array",
                        items: {
                          type: "string",
                          minLength: 1,
                          maxLength: 256
                        },
                        uniqueItems: true,
                        maxItems: 128
                      }
                    ]
                  },
                  history: {
                    enum: [
                      "current",
                      "all"
                    ]
                  },
                  includeArchived: {
                    type: "boolean"
                  },
                  references: {
                    type: "array",
                    maxItems: 128,
                    uniqueItems: true,
                    items: {
                      type: "object",
                      properties: {
                        objectId: {
                          type: "string",
                          minLength: 1,
                          maxLength: 256
                        },
                        revision: {
                          type: "integer",
                          minimum: 1,
                          maximum: 9007199254740991
                        }
                      },
                      required: [
                        "objectId",
                        "revision"
                      ],
                      additionalProperties: false
                    }
                  }
                },
                required: [
                  "roots",
                  "history",
                  "includeArchived",
                  "references"
                ],
                additionalProperties: false
              }
            },
            required: [
              "key",
              "readVersions",
              "writeVersions"
            ],
            additionalProperties: false
          }
        }
      },
      required: [
        "node",
        "hiveProtocol",
        "contracts"
      ],
      additionalProperties: false
    },
    entrypoint: {
      type: "object",
      properties: {
        executable: {
          type: "string",
          minLength: 1,
          maxLength: 1024
        },
        args: {
          type: "array",
          items: {
            type: "string"
          },
          maxItems: 128
        },
        cwd: {
          type: "string",
          minLength: 1,
          maxLength: 1024
        },
        timeoutMs: {
          type: "integer",
          minimum: 1,
          maximum: 36e5
        }
      },
      required: [
        "executable",
        "args",
        "timeoutMs"
      ],
      additionalProperties: false
    },
    prepare: {
      type: "array",
      items: {
        type: "object",
        properties: {
          executable: {
            type: "string",
            minLength: 1,
            maxLength: 1024
          },
          args: {
            type: "array",
            items: {
              type: "string"
            },
            maxItems: 128
          },
          cwd: {
            type: "string",
            minLength: 1,
            maxLength: 1024
          },
          timeoutMs: {
            type: "integer",
            minimum: 1,
            maximum: 36e5
          }
        },
        required: [
          "executable",
          "args",
          "timeoutMs"
        ],
        additionalProperties: false
      },
      maxItems: 32
    },
    checks: {
      type: "array",
      items: {
        type: "object",
        properties: {
          executable: {
            type: "string",
            minLength: 1,
            maxLength: 1024
          },
          args: {
            type: "array",
            items: {
              type: "string"
            },
            maxItems: 128
          },
          cwd: {
            type: "string",
            minLength: 1,
            maxLength: 1024
          },
          timeoutMs: {
            type: "integer",
            minimum: 1,
            maximum: 36e5
          }
        },
        required: [
          "executable",
          "args",
          "timeoutMs"
        ],
        additionalProperties: false
      },
      maxItems: 32
    },
    readiness: {
      type: "object",
      properties: {
        timeoutMs: {
          type: "integer",
          minimum: 1,
          maximum: 6e5
        },
        command: {
          type: "object",
          properties: {
            executable: {
              type: "string",
              minLength: 1,
              maxLength: 1024
            },
            args: {
              type: "array",
              items: {
                type: "string"
              },
              maxItems: 128
            },
            cwd: {
              type: "string",
              minLength: 1,
              maxLength: 1024
            },
            timeoutMs: {
              type: "integer",
              minimum: 1,
              maximum: 36e5
            }
          },
          required: [
            "executable",
            "args",
            "timeoutMs"
          ],
          additionalProperties: false
        }
      },
      required: [
        "timeoutMs",
        "command"
      ],
      additionalProperties: false
    },
    shutdown: {
      type: "object",
      properties: {
        timeoutMs: {
          type: "integer",
          minimum: 1,
          maximum: 6e5
        }
      },
      required: [
        "timeoutMs"
      ],
      additionalProperties: false
    },
    restart: {
      type: "object",
      properties: {
        policy: {
          enum: [
            "never",
            "on-failure",
            "always"
          ]
        },
        minimumDelayMs: {
          type: "integer",
          minimum: 100,
          maximum: 6e5
        },
        maximumDelayMs: {
          type: "integer",
          minimum: 100,
          maximum: 36e5
        }
      },
      required: [
        "policy",
        "minimumDelayMs",
        "maximumDelayMs"
      ],
      additionalProperties: false
    },
    app: {
      type: "object",
      properties: {
        appId: {
          type: "string",
          pattern: "^[a-z][a-z0-9-]*$",
          maxLength: 128
        },
        dist: {
          type: "string",
          pattern: "^dist/apps/[a-z][a-z0-9-]*$",
          maxLength: 1024
        },
        entryPath: {
          type: "string",
          pattern: "^(?![\\\\/])(?!.*(?:^|[\\\\/])\\.\\.(?:[\\\\/]|$)).+\\.html$",
          maxLength: 1024
        }
      },
      required: [
        "appId",
        "dist",
        "entryPath"
      ],
      additionalProperties: false
    },
    storage: {
      type: "object",
      properties: {
        minReadableFormat: {
          type: "integer",
          minimum: 1
        },
        maxReadableFormat: {
          type: "integer",
          minimum: 1
        },
        writeFormat: {
          type: "integer",
          minimum: 1
        }
      },
      required: [
        "minReadableFormat",
        "maxReadableFormat",
        "writeFormat"
      ],
      additionalProperties: false
    },
    runtimeReset: {
      type: "object",
      properties: {
        delete: {
          type: "array",
          maxItems: 64,
          items: {
            type: "object",
            properties: {
              area: { enum: ["host", "data", "work", "logs"] },
              path: { type: "string", minLength: 1, maxLength: 256, pattern: "^(?![\\\\/])(?!.*(?:^|[\\\\/])\\.\\.(?:[\\\\/]|$)).+$" }
            },
            required: ["area", "path"],
            additionalProperties: false
          }
        },
        preserve: {
          type: "array",
          maxItems: 64,
          items: {
            type: "object",
            properties: {
              area: { enum: ["host", "data", "work", "logs"] },
              path: { type: "string", minLength: 1, maxLength: 256, pattern: "^(?![\\\\/])(?!.*(?:^|[\\\\/])\\.\\.(?:[\\\\/]|$)).+$" }
            },
            required: ["area", "path"],
            additionalProperties: false
          }
        },
        completionMarker: {
          type: "string",
          minLength: 1,
          maxLength: 256,
          pattern: "^(?![\\\\/])(?!.*(?:^|[\\\\/])\\.\\.(?:[\\\\/]|$)).+$"
        }
      },
      required: ["delete", "preserve"],
      additionalProperties: false
    }
  },
  required: [
    "schemaVersion",
    "componentId",
    "kind",
    "version",
    "description",
    "connectsToHive",
    "requirements",
    "prepare",
    "checks"
  ],
  additionalProperties: false,
  allOf: [
    {
      if: {
        properties: {
          kind: {
            const: "app"
          }
        },
        required: [
          "kind"
        ]
      },
      then: {
        required: [
          "app"
        ],
        properties: {
          connectsToHive: {
            const: false
          }
        },
        not: {
          required: [
            "entrypoint"
          ]
        }
      },
      else: {
        required: [
          "entrypoint",
          "readiness",
          "shutdown",
          "restart"
        ]
      }
    },
    {
      if: {
        properties: {
          connectsToHive: {
            const: true
          }
        },
        required: [
          "connectsToHive"
        ]
      },
      then: {
        properties: {
          requirements: {
            properties: {
              hiveProtocol: {
                const: 1
              }
            }
          }
        }
      }
    },
    {
      if: {
        properties: {
          kind: {
            const: "hive"
          }
        },
        required: [
          "kind"
        ]
      },
      then: {
        required: [
          "storage"
        ],
        properties: {
          connectsToHive: {
            const: false
          }
        }
      }
    }
  ]
};

// packages/contracts/src/component-validation.ts
import { Ajv2020 } from "ajv/dist/2020.js";
var componentSchema = component_schema_default;
var ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [component_schema_default, host_schema_default, hive_wire_schema_default]) ajv.addSchema(schema);
function validateComponent(value) {
  const validator = ajv.getSchema(String(componentSchema["$id"]));
  requireThat(validator && validator(value), "invalid_arguments", "Invalid component manifest.");
}

// packages/contracts/src/host-bundle.ts
function bundledSchema(reference, document = String(hostSchema["$id"])) {
  const sources = { Host: hostSchema, Wire: wireSchema };
  const documents = new Map(Object.entries(sources).map(([prefix, schema]) => [String(schema["$id"]), { prefix, schema }]));
  const definitions = /* @__PURE__ */ Object.create(null);
  const resolve6 = (ref, base) => {
    const url = new URL(ref, base), documentId = url.origin + url.pathname, source = documents.get(documentId)?.schema;
    requireThat(source && /^#\/\$defs\/[A-Za-z0-9]+$/.test(url.hash), "internal_error", "Unknown host schema reference.");
    const name = url.hash.slice("#/$defs/".length), value = source["$defs"][name];
    requireThat(value, "internal_error", "Missing host schema definition.");
    return { value, document: documentId, name };
  };
  const clone = (value, base) => {
    if (Array.isArray(value)) return value.map((item) => clone(item, base));
    if (value === null || typeof value !== "object") return value;
    const result = {};
    for (const [key, item] of Object.entries(value)) {
      if (key === "$id" || key === "$schema") continue;
      if (key !== "$ref") {
        result[key] = clone(item, base);
        continue;
      }
      const resolved = resolve6(String(item), base), name = documents.get(resolved.document).prefix + resolved.name;
      if (!Object.hasOwn(definitions, name)) {
        definitions[name] = true;
        definitions[name] = clone(resolved.value, resolved.document);
      }
      result[key] = "#/$defs/" + name;
    }
    return result;
  };
  let root = resolve6(reference, document);
  while (typeof root.value["$ref"] === "string") root = resolve6(root.value["$ref"], root.document);
  return { ...clone(root.value, root.document), $defs: definitions };
}

// packages/host-runtime/src/build-identity.mjs
import { createHash, randomUUID as randomUUID5 } from "node:crypto";
import { readFileSync as readFileSync3 } from "node:fs";
import { resolve as resolve5 } from "node:path";
function buildIdentity(root = process.cwd()) {
  const pkg = JSON.parse(readFileSync3(resolve5(root, "package.json"), "utf8"));
  const supplied = process.env.IVY_BUILD_ID;
  const buildId = supplied && /^sha256:[0-9a-f]{64}$/.test(supplied) ? supplied : "sha256:" + createHash("sha256").update(`${pkg.version}\0${Date.now()}\0${randomUUID5()}`).digest("hex");
  return { schemaVersion: 1, version: pkg.version, buildId };
}
export {
  HealthFile,
  HostJournal,
  RuntimeOwner,
  atomicJson,
  buildIdentity,
  bundledSchema as bundledHostSchema,
  checkHealth,
  configurationPath,
  expectedServerHome,
  freshExecutorStatus,
  hostConfig,
  inside,
  instanceConfig,
  instanceOwnedCodexHome,
  jsonFile,
  resolveCodexHome,
  runtimeResetActive,
  usesExternalAppServer,
  validateComponent,
  validateHost
};
//# sourceMappingURL=host.js.map
