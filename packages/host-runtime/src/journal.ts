import { DatabaseSync } from "node:sqlite";
import { existsSync, lstatSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { canonical, compareVersions, hashJson } from "../../contracts/src/canonical.js";
import { IvyError, requireThat } from "../../contracts/src/errors.js";
import { validateHost } from "../../contracts/src/host-validation.js";
import { validateShared } from "../../contracts/src/core-validation.js";
import type { Host, Wire } from "../../contracts/src/generated.js";
import { inside } from "./config.js";
import {
  acceptedInstanceConfigurationPath,
  installationRecord,
  readAcceptedConfiguration,
  sameInstallation,
  saveAcceptedConfiguration,
} from "./accepted-configuration.js";
import { servicePaths } from "./layout.js";
import { releaseManifest } from "./artifact.js";

export const terminalPhases = new Set<Wire.DeploymentRecord["phase"]>([
  "succeeded",
  "rolled_back",
  "failed",
  "needs_attention",
]);
/** Windows bootstrap owns these roots directly; normal target deployment cannot observe their drain. */
export const requiresBootstrapReplacement = (
  instance: Pick<Host.Instance, "componentId">,
): boolean =>
  instance.componentId === "host-executor" ||
  (process.platform === "win32" && instance.componentId === "service-manager");
const transitions: Record<
  Wire.DeploymentRecord["phase"],
  Wire.DeploymentRecord["phase"][]
> = {
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
  needs_attention: [],
};
export interface InstalledInstance {
  instanceId: string;
  candidateId: string;
  buildId: string;
  enabled: boolean;
  installedAt: string;
}
export interface ConfigurationAdoption {
  configuration: Host.HostConfig;
  revision: number;
  contentHash: string;
  actions: Array<{
    instanceId: string;
    action: "restart" | "enable" | "disable";
    operationId: string;
  }>;
  bootstrapRequired: string[];
}

type RetainedEntry = Host.JournalEntry & {
  configurationHash?: string;
  steps?: unknown;
};
type RetainedCandidate = Host.Candidate & {
  sourceRoot?: string;
  fileManifestHash?: string;
};
type RetainedTarget = Host.RuntimeTarget & {
  configHash?: string;
  hostConfigHash?: string;
  configMode?: string;
};

function legacyConfigurationPath(runtimeRoot: string, hash: string): string {
  requireThat(
    /^sha256:[0-9a-f]{64}$/.test(hash),
    "configuration_changed",
    "Retained configuration identity is invalid.",
  );
  return join(runtimeRoot, "host-configurations", hash.slice(7) + ".json");
}

function normalizeCandidate(input: unknown): Host.Candidate {
  const value = input as RetainedCandidate;
  const candidate: Host.Candidate = {
    candidateId: value.candidateId,
    componentId: value.componentId,
    artifactRoot: value.artifactRoot,
    manifestPath: value.manifestPath,
    createdAt: value.createdAt,
    platform: value.platform,
    buildId: value.buildId,
    ...(value.archivePath === undefined
      ? {}
      : { archivePath: value.archivePath }),
    ...(value.archiveHash === undefined
      ? {}
      : { archiveHash: value.archiveHash }),
    ...(value.dockerImage === undefined
      ? {}
      : { dockerImage: value.dockerImage }),
  };
  validateHost("Candidate", candidate);
  return candidate;
}

function normalizeTarget(input: unknown): Host.RuntimeTarget {
  const value = input as RetainedTarget;
  const target: Host.RuntimeTarget = {
    schemaVersion: value.schemaVersion,
    instanceId: value.instanceId,
    revision: value.revision,
    candidateId: value.candidateId,
    desired: value.desired,
    configPath: value.configPath,
    requestedAt: value.requestedAt,
  };
  validateHost("RuntimeTarget", target);
  return target;
}

const freshTimestamp = (value: string): boolean => {
  const at = Date.parse(value);
  return (
    Number.isFinite(at) && Date.now() - at < 15_000 && at <= Date.now() + 1000
  );
};
export function freshExecutorStatus(value: Host.ExecutorStatus): boolean {
  return freshTimestamp(value.observedAt);
}

/** Stable launch identity used to bind a direct Linux bootstrap unit to its target. */
export function bootstrapLaunchId(
  plan: Host.BootstrapPlan,
  instanceId: string,
): string {
  return hashJson({ planHash: hashJson(plan), instanceId });
}

/** One independent OS file lock. Kernel process death releases it; stale PID files never grant a lock. */
export class ExecutorLock {
  private readonly db: DatabaseSync;
  constructor(root: string) {
    mkdirSync(root, { recursive: true });
    const db = new DatabaseSync(join(root, "executor-lock.sqlite"));
    try {
      db.exec(
        "PRAGMA busy_timeout=100; PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; CREATE TABLE IF NOT EXISTS owner(pid INTEGER); DELETE FROM owner;",
      );
      db.prepare("INSERT INTO owner VALUES (?)").run(process.pid);
      db.exec("COMMIT");
    } catch {
      db.close();
      throw new IvyError(
        "executor_already_running",
        "Another executor holds the host lock.",
      );
    }
    this.db = db;
  }
  close(): void {
    this.db.close();
  }
}

/** Independent host-local authority. CLI accepts intents; the OS executor alone advances them. */
export class HostJournal {
  readonly db: DatabaseSync;
  private activeConfig: Host.HostConfig;
  get config(): Host.HostConfig {
    return this.activeConfig;
  }
  constructor(config: Host.HostConfig) {
    validateHost("HostConfig", config);
    this.activeConfig = config;
    mkdirSync(config.runtimeRoot, { recursive: true });
    const db = new DatabaseSync(join(config.runtimeRoot, "deployments.sqlite"));
    this.db = db;
    try {
      db.exec(
        "PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
      );
      const format = Number(
        db.prepare("PRAGMA user_version").get()!["user_version"],
      );
      requireThat(
        format === 0 || format === 1,
        "unsupported_storage",
        "Host journal format is not supported by this executor.",
      );
      db.exec(`BEGIN IMMEDIATE;
        CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS operations(operation_id TEXT PRIMARY KEY, deployment_id TEXT NOT NULL UNIQUE, request_hash TEXT NOT NULL, phase TEXT NOT NULL, created_at TEXT NOT NULL, entry_json TEXT NOT NULL) STRICT;
        CREATE INDEX IF NOT EXISTS operations_by_phase ON operations(phase,created_at,deployment_id);
        CREATE TABLE IF NOT EXISTS candidates(candidate_id TEXT PRIMARY KEY, component_id TEXT NOT NULL, build_id TEXT NOT NULL, candidate_json TEXT NOT NULL, manifest_json TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS candidate_retirements(candidate_id TEXT PRIMARY KEY, candidate_json TEXT NOT NULL) STRICT;
        CREATE INDEX IF NOT EXISTS candidates_by_build ON candidates(component_id,build_id);
        CREATE TABLE IF NOT EXISTS installed(instance_id TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS observations(instance_id TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS runtime_targets(instance_id TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
        PRAGMA user_version=1; COMMIT;`);
      this.transaction(() => {
        const prior = db
          .prepare("SELECT value FROM meta WHERE key=?")
          .get("hostId");
        requireThat(
          !prior || prior["value"] === config.hostId,
          "target_conflict",
          "A host journal cannot be reused for another host identity.",
        );
        db.prepare("INSERT OR IGNORE INTO meta VALUES (?,?)").run(
          "hostId",
          config.hostId,
        );
        const installation = installationRecord(config);
        const anchor = db
          .prepare("SELECT value FROM meta WHERE key=?")
          .get("installation");
        if (!anchor) {
          const legacy = db
            .prepare("SELECT value FROM meta WHERE key=?")
            .get("configurationAnchor");
          if (legacy) {
            const hash = String(legacy["value"]),
              path = legacyConfigurationPath(config.runtimeRoot, hash);
            try {
              const metadata = lstatSync(path);
              requireThat(
                metadata.isFile() &&
                  !metadata.isSymbolicLink() &&
                  metadata.size <= 1024 * 1024,
                "configuration_changed",
                "Retained installation configuration is not a bounded regular file.",
              );
              const prior = JSON.parse(
                readFileSync(path, "utf8"),
              ) as Host.HostConfig;
              validateHost("HostConfig", prior);
              requireThat(
                hashJson(prior) === hash,
                "configuration_changed",
                "Retained installation configuration changed.",
              );
              sameInstallation(prior, config);
            } catch (error) {
              if (error instanceof IvyError) throw error;
              throw new IvyError(
                "configuration_changed",
                "Retained installation configuration is missing or invalid.",
              );
            }
          }
        }
        requireThat(
          !anchor || anchor["value"] === installation,
          "target_conflict",
          "Host operational roots and recovery identity cannot change within an installation.",
        );
        db.prepare("INSERT OR IGNORE INTO meta VALUES (?,?)").run(
          "installation",
          installation,
        );
        for (const instance of config.instances) {
          const key = "instanceIdentity:" + instance.instanceId,
            identity = canonical({
              componentId: instance.componentId,
              serviceNodeId: instance.serviceNodeId,
            });
          const prior = db
            .prepare("SELECT value FROM meta WHERE key=?")
            .get(key);
          requireThat(
            !prior || prior["value"] === identity,
            "target_conflict",
            "A previously configured instance identity cannot be reused.",
          );
          db.prepare("INSERT OR IGNORE INTO meta VALUES (?,?)").run(
            key,
            identity,
          );
        }
      });
    } catch (error) {
      db.close();
      throw error;
    }
  }
  close(): void {
    this.db.close();
  }
  acceptedConfiguration(path: string): Host.HostConfig {
    return readAcceptedConfiguration(this.config, path);
  }
  useConfiguration(config: Host.HostConfig): void {
    sameInstallation(this.config, config);
    this.transaction(() => {
      for (const instance of config.instances) {
        const key = "instanceIdentity:" + instance.instanceId,
          identity = canonical({
            componentId: instance.componentId,
            serviceNodeId: instance.serviceNodeId,
          });
        const prior = this.db
          .prepare("SELECT value FROM meta WHERE key=?")
          .get(key);
        requireThat(
          !prior || prior["value"] === identity,
          "target_conflict",
          "A previously configured instance identity cannot be reused.",
        );
        this.db
          .prepare("INSERT OR IGNORE INTO meta VALUES (?,?)")
          .run(key, identity);
      }
    });
    this.activeConfig = config;
  }
  configurationAdoption(): ConfigurationAdoption | null {
    const row = this.db
      .prepare("SELECT value FROM meta WHERE key=?")
      .get("configurationAdoption");
    if (!row) return null;
    const value = JSON.parse(String(row["value"])) as ConfigurationAdoption;
    validateHost("HostConfig", value.configuration);
    sameInstallation(this.config, value.configuration);
    requireThat(
      Number.isSafeInteger(value.revision) &&
        value.revision >= 1 &&
        /^sha256:[0-9a-f]{64}$/.test(value.contentHash) &&
        Array.isArray(value.actions) &&
        value.actions.every(
          (action) =>
            ["restart", "enable", "disable"].includes(action.action) &&
            typeof action.instanceId === "string" &&
            typeof action.operationId === "string",
        ) &&
        Array.isArray(value.bootstrapRequired) &&
        value.bootstrapRequired.every(
          (instanceId) => typeof instanceId === "string",
        ),
      "configuration_changed",
      "Retained configuration adoption is invalid.",
    );
    return value;
  }
  stageConfigurationAdoption(value: ConfigurationAdoption): void {
    validateHost("HostConfig", value.configuration);
    sameInstallation(this.config, value.configuration);
    requireThat(
      hashJson(value.configuration) === value.contentHash,
      "configuration_changed",
      "Configuration adoption hash does not match its exact input.",
    );
    this.transaction(() => {
      const prior = this.db
          .prepare("SELECT value FROM meta WHERE key=?")
          .get("configurationAdoption"),
        encoded = canonical(value);
      requireThat(
        !prior || prior["value"] === encoded,
        "service_busy",
        "Another configuration adoption is still pending.",
      );
      this.db
        .prepare(
          "INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        )
        .run("configurationAdoption", encoded);
    });
  }
  finishConfigurationAdoption(value: ConfigurationAdoption): void {
    this.transaction(() => {
      const prior = this.db
        .prepare("SELECT value FROM meta WHERE key=?")
        .get("configurationAdoption");
      requireThat(
        prior && prior["value"] === canonical(value),
        "revision_conflict",
        "Configuration adoption changed before completion.",
      );
      this.db
        .prepare("DELETE FROM meta WHERE key=?")
        .run("configurationAdoption");
    });
  }
  status(executor: Host.ExecutorStatus | null): Host.HostStatus {
    const storageRetention = this.storageRetentionStatus();
    const currentExecutor =
      executor && freshExecutorStatus(executor) ? executor : null;
    const observed = this.currentObservations(currentExecutor);
    const value: Host.HostStatus = {
      schemaVersion: 1,
      hostId: this.config.hostId,
      observedAt: new Date().toISOString(),
      executor: currentExecutor,
      ...(storageRetention ? { storageRetention } : {}),
      instances: this.config.instances.map((instance) => {
        const installed = this.installed(instance.instanceId),
          actual = observed[instance.instanceId];
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
          message: actual?.message.slice(0, 512) ?? "",
        };
      }),
      unfinished: this.pendingEntries().map((value) => value.record),
    };
    validateHost("HostStatus", value);
    return value;
  }
  storageRetentionStatus(): Host.StorageRetentionStatus | null {
    const row = this.db.prepare('SELECT value FROM meta WHERE key=?').get('storage-retention');
    if (!row) return null;
    const value = JSON.parse(String(row['value'])) as Host.StorageRetentionStatus;
    validateHost('StorageRetentionStatus', value); return value;
  }
  saveStorageRetentionStatus(value: Host.StorageRetentionStatus): void {
    validateHost('StorageRetentionStatus', value);
    this.db.prepare('INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value').run('storage-retention', canonical(value));
  }
  /** Read the current owner files for a status projection without changing the journal. */
  currentObservations(
    executor: Host.ExecutorStatus | null = null,
  ): Record<string, Host.RuntimeObservation> {
    const prior = this.observations(),
      result: Record<string, Host.RuntimeObservation> = {};
    for (const instance of this.config.instances) {
      const installed = this.installed(instance.instanceId),
        target = this.target(instance.instanceId),
        previous = prior[instance.instanceId];
      if (instance.componentId === "host-executor") {
        if (!executor || !freshExecutorStatus(executor)) {
          result[instance.instanceId] = {
            schemaVersion: 1,
            instanceId: instance.instanceId,
            ownerPid: previous?.ownerPid ?? process.pid,
            ownerBootId:
              previous?.ownerBootId ?? "00000000-0000-4000-8000-000000000000",
            observedAt: previous?.observedAt ?? new Date(0).toISOString(),
            targetRevision: target?.revision ?? null,
            candidateId: installed?.candidateId ?? target?.candidateId ?? null,
            buildId: null,
            state: "unknown",
            health: null,
            restartCount: previous?.restartCount ?? 0,
            nextRestartAt: previous?.nextRestartAt ?? null,
            code: "health_stale",
            message:
              "Host executor status is missing or stale; readiness is not current.",
          };
          continue;
        }
        const buildMatches = Boolean(
          executor.buildId &&
          (!installed || executor.buildId === installed.buildId),
        );
        const state: Host.RuntimeObservation["state"] = !buildMatches
          ? "unknown"
          : executor.state === "ready"
            ? "ready"
            : executor.state === "stopping"
              ? "stopped"
              : executor.state === "failed"
                ? "failed"
                : "starting";
        result[instance.instanceId] = {
          schemaVersion: 1,
          instanceId: instance.instanceId,
          ownerPid: executor.pid,
          ownerBootId: executor.bootId,
          observedAt: executor.observedAt,
          targetRevision: target?.revision ?? null,
          candidateId: buildMatches
            ? (installed?.candidateId ?? target?.candidateId ?? null)
            : null,
          buildId: executor.buildId,
          state,
          health: null,
          restartCount: 0,
          nextRestartAt: null,
          code: buildMatches ? executor.code : "build_mismatch",
          message: !buildMatches
            ? "Running executor release does not match the installed release."
            : executor.code
              ? "Host executor reported " + executor.code + "."
              : "",
        };
        continue;
      }
      try {
        const health = JSON.parse(
          readFileSync(
            join(servicePaths(this.config, instance).data, "health.json"),
            "utf8",
          ),
        ) as Host.Health;
        validateHost("Health", health);
        requireThat(
          health.instanceId === instance.instanceId &&
            health.componentId === instance.componentId,
          "target_conflict",
          "Health file belongs to another configured instance.",
        );
        const expectedBuild = target
          ? this.candidate(target.candidateId).buildId
          : installed?.buildId;
        const launchMatches =
          !health.launchId || !target || health.launchId === target.revision;
        const runningTarget = target?.desired !== "stopped";
        const currentReady =
          freshTimestamp(health.observedAt) &&
          health.ready &&
          runningTarget &&
          launchMatches &&
          (!expectedBuild || health.buildId === expectedBuild);
        const state: Host.RuntimeObservation["state"] = currentReady
          ? "ready"
          : !freshTimestamp(health.observedAt)
            ? "unknown"
            : !runningTarget
              ? "stopped"
              : health.ready
                ? "unknown"
                : "starting";
        const code = currentReady
          ? null
          : !freshTimestamp(health.observedAt)
            ? "health_stale"
            : !launchMatches ||
                (expectedBuild && health.buildId !== expectedBuild)
              ? "build_mismatch"
              : !runningTarget
                ? null
                : "service_not_ready";
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
          message: currentReady
            ? health.details
            : code === "health_stale"
              ? "Owner health is stale; readiness is not current."
              : health.details || "Owner health is not ready.",
        };
      } catch {
        if (previous)
          result[instance.instanceId] = {
            ...previous,
            state: target?.desired === "stopped" ? "stopped" : "unknown",
            code: "health_stale",
            message:
              "Current owner health is missing or invalid; the last ready observation is not current.",
          };
      }
    }
    return result;
  }
  managementSnapshot(
    executor: Host.ExecutorStatus | null,
    minimumSequence = 0,
    status = this.status(executor),
  ): Host.ManagementSnapshot {
    requireThat(
      Number.isSafeInteger(minimumSequence) && minimumSequence >= 0,
      "invalid_arguments",
      "Invalid host observation sequence.",
    );
    requireThat(
      status.hostId === this.config.hostId,
      "target_conflict",
      "Management status belongs to another host.",
    );
    return this.transaction(() => {
      const prior = this.db
        .prepare("SELECT value FROM meta WHERE key=?")
        .get("hostReportSequence");
      const sequence =
        Math.max(Number(prior?.["value"] ?? 0), minimumSequence) + 1;
      requireThat(
        Number.isSafeInteger(sequence),
        "limit_exceeded",
        "Host observation sequence is exhausted.",
      );
      const snapshot = { sequence, status };
      validateHost("ManagementSnapshot", snapshot);
      this.db
        .prepare(
          "INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        )
        .run("hostReportSequence", String(sequence));
      return snapshot;
    });
  }
  private transaction<T>(work: () => T): T {
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
  instance(instanceId: string): Host.Instance {
    const instance = this.config.instances.find(
      (instance) => instance.instanceId === instanceId,
    );
    requireThat(
      instance,
      "not_found",
      "Instance is not in this explicit host configuration.",
    );
    return instance;
  }
  private normalizeEntry(input: unknown): Host.JournalEntry {
    const value = input as RetainedEntry;
    if (!value.configurationHash) {
      validateHost("JournalEntry", value);
      return value;
    }
    validateHost("LocalRequest", value.request);
    validateShared("DeploymentRecord", value.record);
    const unresolved = !terminalPhases.has(value.record.phase);
    const record: Wire.DeploymentRecord = unresolved
      ? {
          ...value.record,
          phase: "needs_attention",
          errorCode: "legacy_activation_requires_reconciliation",
        }
      : value.record;
    const entry: Host.JournalEntry = {
      schemaVersion: 1,
      request: value.request,
      record,
      candidateId: value.candidateId,
      previousCandidateId: value.previousCandidateId,
      configurationPath: legacyConfigurationPath(
        this.config.runtimeRoot,
        value.configurationHash,
      ),
      ...(value.sourceSnapshot === undefined
        ? {}
        : { sourceSnapshot: value.sourceSnapshot }),
      activation: {
        previousTarget: null,
        previousEnabled: true,
        targetEnabled: value.request.action !== "disable",
        rollbackTargetRevision: null,
      },
    };
    validateHost("JournalEntry", entry);
    return entry;
  }
  get(deploymentId: string): Host.JournalEntry {
    const row = this.db
      .prepare("SELECT entry_json FROM operations WHERE deployment_id=?")
      .get(deploymentId);
    requireThat(row, "not_found", "Host-local deployment was not found.");
    return this.normalizeEntry(JSON.parse(String(row["entry_json"])));
  }
  operation(operationId: string): Host.JournalEntry | null {
    const row = this.db
      .prepare("SELECT entry_json FROM operations WHERE operation_id=?")
      .get(operationId);
    return row
      ? this.normalizeEntry(JSON.parse(String(row["entry_json"])))
      : null;
  }
  list(limit = 200): Host.JournalEntry[] {
    return this.db
      .prepare(
        "SELECT entry_json FROM operations ORDER BY created_at DESC,deployment_id DESC LIMIT ?",
      )
      .all(Math.max(1, Math.min(200, limit)))
      .map((row) => this.normalizeEntry(JSON.parse(String(row["entry_json"]))));
  }
  private pendingEntries(): Host.JournalEntry[] {
    return this.db
      .prepare(
        "SELECT entry_json FROM operations WHERE phase NOT IN ('succeeded','rolled_back','failed','needs_attention') ORDER BY created_at,deployment_id LIMIT 200",
      )
      .all()
      .map((row) => this.normalizeEntry(JSON.parse(String(row["entry_json"]))));
  }
  unfinished(): Host.JournalEntry[] {
    return this.db
      .prepare(
        "SELECT entry_json FROM operations WHERE phase NOT IN ('succeeded','rolled_back','failed','needs_attention') AND json_extract(entry_json,'$.configurationHash') IS NULL ORDER BY created_at,deployment_id LIMIT 200",
      )
      .all()
      .map((row) => this.normalizeEntry(JSON.parse(String(row["entry_json"]))));
  }
  installed(instanceId: string): InstalledInstance | null {
    const row = this.db
      .prepare("SELECT value FROM installed WHERE instance_id=?")
      .get(instanceId);
    return row ? (JSON.parse(String(row["value"])) as InstalledInstance) : null;
  }
  setInstalled(value: InstalledInstance): void {
    this.instance(value.instanceId);
    const candidate = this.candidate(value.candidateId);
    requireThat(
      candidate.buildId === value.buildId,
      "build_mismatch",
      "Installed pointer must identify its exact candidate build.",
    );
    this.db
      .prepare(
        "INSERT INTO installed VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value",
      )
      .run(value.instanceId, canonical(value));
  }
  /** Record the candidates that the reviewed OS bootstrap actually installed. */
  syncBootstrap(plan: Host.BootstrapPlan, clearTargets = false): void {
    validateHost("BootstrapPlan", plan);
    this.transaction(() => {
      const requestedAt = new Date().toISOString();
      for (const process of plan.processes) {
        if (!process.candidateId) continue;
        const instance = this.instance(process.instanceId);
        requireThat(
          instance.componentId === process.componentId,
          "target_conflict",
          "Bootstrap process belongs to another configured component.",
        );
        const candidate = this.candidate(process.candidateId);
        const previous = this.installed(process.instanceId);
        if (previous && previous.candidateId !== candidate.candidateId) {
          this.db.prepare('INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value')
            .run('bootstrap-rollback:' + process.instanceId, canonical({ candidateId: previous.candidateId, selectedCandidateId: candidate.candidateId }));
        }
        this.setInstalled({
          instanceId: process.instanceId,
          candidateId: candidate.candidateId,
          buildId: candidate.buildId,
          enabled: true,
          installedAt: new Date().toISOString(),
        });
        if (clearTargets) {
          this.db
            .prepare("DELETE FROM runtime_targets WHERE instance_id=?")
            .run(process.instanceId);
          continue;
        }
        if (!process.configPath || process.componentId === "host-executor")
          continue;
        requireThat(
          inside(this.config.runtimeRoot, process.configPath),
          "target_conflict",
          "Bootstrap runtime configuration leaves the host runtime root.",
        );
        const metadata = lstatSync(process.configPath);
        requireThat(
          metadata.isFile() &&
            !metadata.isSymbolicLink() &&
            metadata.size <= 1024 * 1024,
          "configuration_changed",
          "Bootstrap runtime configuration is not a bounded regular file.",
        );
        const runtimeConfig = JSON.parse(
          readFileSync(process.configPath, "utf8"),
        ) as Host.InstanceConfig;
        validateHost("InstanceConfig", runtimeConfig);
        requireThat(
          runtimeConfig.instanceId === process.instanceId &&
            runtimeConfig.componentId === process.componentId &&
            runtimeConfig.hostId === this.config.hostId &&
            runtimeConfig.artifactRoot === candidate.artifactRoot &&
            runtimeConfig.buildId === candidate.buildId,
          "build_mismatch",
          "Bootstrap runtime configuration does not identify the installed candidate.",
        );
        const target: Host.RuntimeTarget = {
          schemaVersion: 1,
          instanceId: process.instanceId,
          revision: bootstrapLaunchId(plan, process.instanceId),
          candidateId: candidate.candidateId,
          desired: "running",
          configPath: process.configPath,
          requestedAt,
        };
        validateHost("RuntimeTarget", target);
        this.db
          .prepare(
            "INSERT INTO runtime_targets VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value",
          )
          .run(process.instanceId, canonical(target));
      }
    });
  }
  recordObservation(instanceId: string, value: Host.RuntimeObservation): void {
    validateHost("RuntimeObservation", value);
    requireThat(
      value.instanceId === instanceId,
      "target_conflict",
      "Observation must belong to its instance.",
    );
    this.instance(instanceId);
    this.db
      .prepare(
        "INSERT INTO observations VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value",
      )
      .run(instanceId, canonical(value, 65536));
  }
  observations(): Record<string, Host.RuntimeObservation> {
    return Object.fromEntries(
      this.db
        .prepare("SELECT * FROM observations ORDER BY instance_id")
        .all()
        .map((row) => [
          String(row["instance_id"]),
          JSON.parse(String(row["value"])),
        ]),
    );
  }
  target(instanceId: string): Host.RuntimeTarget | null {
    const row = this.db
      .prepare("SELECT value FROM runtime_targets WHERE instance_id=?")
      .get(instanceId);
    return row ? normalizeTarget(JSON.parse(String(row["value"]))) : null;
  }
  restoreTarget(value: Host.RuntimeTarget): void {
    validateHost("RuntimeTarget", value);
    this.instance(value.instanceId);
    requireThat(
      this.candidate(value.candidateId),
      "not_found",
      "Restored runtime target requires its installed candidate.",
    );
    this.db
      .prepare(
        "INSERT INTO runtime_targets VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value",
      )
      .run(value.instanceId, canonical(value));
  }
  saveCandidate(candidate: Host.Candidate, manifest: unknown): void {
    validateHost("Candidate", candidate);
    validateHost("ReleaseManifest", manifest);
    const value = manifest as Host.ReleaseManifest;
    requireThat(
      value.buildId === candidate.buildId &&
        value.componentId === candidate.componentId &&
        candidate.candidateId === candidate.buildId,
      "build_mismatch",
      "Candidate identity must match its release manifest.",
    );
    this.transaction(() => {
      requireThat(!this.db.prepare('SELECT 1 FROM candidate_retirements WHERE candidate_id=?').get(candidate.candidateId),
        'mutation_conflict', 'Candidate deletion is pending; retry publication after storage collection.');
      const prior = this.db
        .prepare(
          "SELECT candidate_json,manifest_json FROM candidates WHERE candidate_id=?",
        )
        .get(candidate.candidateId);
      if (prior)
        requireThat(
          prior["candidate_json"] === canonical(candidate) &&
            prior["manifest_json"] === canonical(manifest),
          "mutation_conflict",
          "Candidate is immutable.",
        );
      else
        this.db
          .prepare("INSERT INTO candidates VALUES (?,?,?,?,?)")
          .run(
            candidate.candidateId,
            candidate.componentId,
            candidate.buildId,
            canonical(candidate),
            canonical(manifest),
          );
    });
  }
  candidate(candidateId: string): Host.Candidate {
    const row = this.db
      .prepare("SELECT candidate_json FROM candidates WHERE candidate_id=?")
      .get(candidateId);
    requireThat(row, "not_found", "Prepared host candidate was not found.");
    return normalizeCandidate(JSON.parse(String(row["candidate_json"])));
  }
  manifest(candidateId: string): Host.ReleaseManifest {
    const row = this.db
      .prepare("SELECT manifest_json FROM candidates WHERE candidate_id=?")
      .get(candidateId);
    requireThat(row, "not_found", "Prepared candidate manifest was not found.");
    return releaseManifest(JSON.parse(String(row["manifest_json"])));
  }
  candidateForBuild(componentId: string, buildId: string): Host.Candidate {
    const rows = this.db
      .prepare(
        "SELECT candidate_json FROM candidates WHERE component_id=? AND build_id=? ORDER BY candidate_id LIMIT 2",
      )
      .all(componentId, buildId);
    requireThat(
      rows.length === 1,
      rows.length ? "ambiguous_candidate" : "not_found",
      "The retained build must identify one exact local candidate.",
    );
    return normalizeCandidate(JSON.parse(String(rows[0]!["candidate_json"])));
  }
  /** Live selections, unresolved operations and one rollback per live selection. */
  retentionCandidateIds(): Set<string> {
    const live = new Set<string>(), protectedIds = new Set<string>();
    const installed = new Map<string, string>();
    for (const table of ['installed', 'runtime_targets', 'observations']) {
      for (const row of this.db.prepare(`SELECT instance_id,value FROM ${table}`).all()) {
        const id = (JSON.parse(String(row['value'])) as { candidateId?: string }).candidateId;
        if (id) { live.add(id); protectedIds.add(id); if (table === 'installed') installed.set(String(row['instance_id']), id); }
      }
    }
    for (const [instanceId, selectedCandidateId] of installed) {
      const row = this.db.prepare('SELECT value FROM meta WHERE key=?').get('bootstrap-rollback:' + instanceId);
      if (!row) continue;
      const previous = JSON.parse(String(row['value'])) as { candidateId: string; selectedCandidateId: string };
      if (previous.selectedCandidateId === selectedCandidateId) protectedIds.add(previous.candidateId);
    }
    const rollbackFound = new Set<string>();
    for (const row of this.db.prepare('SELECT entry_json FROM operations ORDER BY created_at DESC,operation_id DESC').all()) {
      const entry = JSON.parse(String(row['entry_json'])) as Host.JournalEntry;
      if (!['succeeded', 'rolled_back'].includes(entry.record.phase)) {
        const value = this.normalizeEntry(entry);
        for (const id of [value.candidateId, value.previousCandidateId, value.request.candidateId,
          value.activation.previousTarget?.candidateId]) if (id) protectedIds.add(id);
      } else if (entry.record.phase === 'succeeded' && installed.get(entry.record.instanceId) === entry.candidateId &&
        entry.previousCandidateId && entry.previousCandidateId !== entry.candidateId && !rollbackFound.has(entry.record.instanceId)) {
        protectedIds.add(entry.previousCandidateId);
        rollbackFound.add(entry.record.instanceId);
      }
    }
    // Bootstrap installs and restored journals may have no deployment operation.
    // Keep the nearest older version as well as the actual prior successful build.
    const releases = this.db.prepare('SELECT candidate_id,component_id,manifest_json FROM candidates').all().map(row => ({
      id: String(row['candidate_id']), component: String(row['component_id']),
      version: (JSON.parse(String(row['manifest_json'])) as Host.ReleaseManifest).version,
    })).filter(value => /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(value.version));
    for (const id of live) {
      const current = releases.find(value => value.id === id);
      if (!current) continue;
      const previous = releases.filter(value => value.component === current.component && compareVersions(value.version, current.version) < 0)
        .sort((a, b) => compareVersions(b.version, a.version) || b.id.localeCompare(a.id))[0];
      if (previous) protectedIds.add(previous.id);
    }
    return protectedIds;
  }
  pendingCandidateRetirements(): Host.Candidate[] {
    return this.db.prepare('SELECT candidate_json FROM candidate_retirements ORDER BY candidate_id').all()
      .map(row => normalizeCandidate(JSON.parse(String(row['candidate_json']))));
  }
  finishCandidateRetirement(candidateId: string): void {
    this.transaction(() => {
      requireThat(!this.db.prepare('SELECT 1 FROM candidates WHERE candidate_id=?').get(candidateId),
        'mutation_conflict', 'A retiring candidate cannot be registered.');
      this.db.prepare('DELETE FROM candidate_retirements WHERE candidate_id=?').run(candidateId);
    });
  }
  /** Persist deletion intent with deregistration. Publication stays blocked until removal completes. */
  retireCandidate(candidateId: string): boolean {
    return this.transaction(() => {
      const row = this.db.prepare('SELECT candidate_json FROM candidates WHERE candidate_id=?').get(candidateId);
      if (!row || this.retentionCandidateIds().has(candidateId)) return false;
      this.db.prepare('INSERT INTO candidate_retirements VALUES (?,?)').run(candidateId, String(row['candidate_json']));
      this.db.prepare('DELETE FROM candidates WHERE candidate_id=?').run(candidateId);
      return true;
    });
  }
  accept(
    request: Host.LocalRequest,
    sourceSnapshot?: Host.SourceSnapshot,
  ): Host.JournalEntry {
    validateHost("LocalRequest", request);
    const requestHash = hashJson(request);
    return this.transaction(() => {
      const previous = this.operation(request.operationId);
      if (previous) {
        requireThat(
          previous.record.requestHash === requestHash,
          "mutation_conflict",
          "Operation ID already identifies different original deployment arguments.",
        );
        return previous;
      }
      const instance = this.instance(request.instanceId),
        installed = this.installed(request.instanceId);
      requireThat(
        !requiresBootstrapReplacement(instance) || !installed,
        "restart_required",
        "Bootstrap-owned releases use prepare followed by install-bootstrap.",
      );
      requireThat(
        request.componentId === undefined ||
          request.componentId === instance.componentId,
        "target_conflict",
        "Source component does not match the configured instance.",
      );
      const action = request.action;
      if (action === "deploy")
        requireThat(
          Boolean(request.candidateId) !== Boolean(request.source) &&
            request.targetBuild === undefined,
          "invalid_arguments",
          "Deploy requires exactly one candidate or source.",
        );
      else if (action === "rollback")
        requireThat(
          request.targetBuild &&
            !request.candidateId &&
            !request.source &&
            request.componentId === undefined,
          "invalid_arguments",
          "Rollback requires one retained target build.",
        );
      else
        requireThat(
          !request.targetBuild &&
            !request.candidateId &&
            !request.source &&
            request.componentId === undefined &&
            installed,
          "invalid_arguments",
          "Lifecycle actions require one installed instance and no candidate arguments.",
        );
      if (request.source) {
        requireThat(
          sourceSnapshot,
          "source_snapshot_required",
          "Capture source inputs before handing the action to the independent executor.",
        );
        validateHost("SourceSnapshot", sourceSnapshot);
        requireThat(
          inside(this.config.stagingRoot, sourceSnapshot.sourceRoot) &&
            inside(this.config.stagingRoot, sourceSnapshot.manifestPath),
          "target_conflict",
          "Source snapshot belongs to another host staging root.",
        );
      } else
        requireThat(
          !sourceSnapshot,
          "invalid_arguments",
          "This action does not accept a source snapshot.",
        );
      const candidate = request.candidateId
        ? this.candidate(request.candidateId)
        : request.targetBuild
          ? this.candidateForBuild(instance.componentId, request.targetBuild)
          : action !== "deploy" && installed
            ? this.candidate(installed.candidateId)
            : null;
      requireThat(
        !candidate || candidate.componentId === instance.componentId,
        "target_conflict",
        "Candidate belongs to another component.",
      );
      const previousTarget = this.target(instance.instanceId),
        previousEnabled = installed?.enabled ?? instance.enabled;
      const retainedPreviousConfiguration = previousTarget
        ? acceptedInstanceConfigurationPath(
            this.config,
            previousTarget.revision,
          )
        : undefined;
      const previousConfigurationPath =
        retainedPreviousConfiguration &&
        existsSync(retainedPreviousConfiguration)
          ? retainedPreviousConfiguration
          : undefined;
      const targetEnabled =
        action === "enable"
          ? true
          : action === "disable"
            ? false
            : previousEnabled;
      const configurationPath = saveAcceptedConfiguration(this.config),
        now = new Date().toISOString(),
        deploymentId = randomUUID();
      const entry: Host.JournalEntry = {
        schemaVersion: 1,
        request,
        ...(sourceSnapshot ? { sourceSnapshot } : {}),
        candidateId: candidate?.candidateId ?? null,
        previousCandidateId: installed?.candidateId ?? null,
        configurationPath,
        activation: {
          previousTarget,
          ...(previousConfigurationPath ? { previousConfigurationPath } : {}),
          previousEnabled,
          targetEnabled,
          rollbackTargetRevision: null,
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
          createdAt: now,
          updatedAt: now,
          readiness: {
            state: "not_run",
            message: "Accepted by the independent host journal.",
          },
        },
      };
      validateHost("JournalEntry", entry);
      this.db
        .prepare("INSERT INTO operations VALUES (?,?,?,?,?,?)")
        .run(
          request.operationId,
          deploymentId,
          requestHash,
          entry.record.phase,
          now,
          canonical(entry),
        );
      return entry;
    });
  }
  advance(
    deploymentId: string,
    expectedPhase: Wire.DeploymentRecord["phase"],
    phase: Wire.DeploymentRecord["phase"],
    change: {
      candidateId?: string;
      observedBuild?: string | null;
      readiness?: Wire.DeploymentRecord["readiness"];
      errorCode?: string;
      rollbackTargetRevision?: string;
      target?: Host.RuntimeTarget;
      installed?: InstalledInstance;
    } = {},
  ): Host.JournalEntry {
    return this.transaction(() => {
      const entry = this.get(deploymentId);
      requireThat(
        entry.record.phase === expectedPhase &&
          (phase === expectedPhase
            ? !terminalPhases.has(phase)
            : transitions[expectedPhase].includes(phase)),
        "revision_conflict",
        "Deployment phase changed or transition is invalid.",
      );
      if (change.candidateId !== undefined) {
        requireThat(
          entry.record.phase === "preparing",
          "revision_conflict",
          "Candidate can only be fixed during preparation.",
        );
        const candidate = this.candidate(change.candidateId);
        requireThat(
          candidate.componentId === entry.record.componentId,
          "target_conflict",
          "Prepared source changed component.",
        );
        entry.candidateId = candidate.candidateId;
        entry.record.targetBuild = candidate.buildId;
      }
      entry.record.phase = phase;
      entry.record.updatedAt = new Date().toISOString();
      if (change.observedBuild !== undefined)
        entry.record.observedBuild = change.observedBuild;
      if (change.readiness) entry.record.readiness = change.readiness;
      if (change.errorCode) entry.record.errorCode = change.errorCode;
      if (change.target) {
        validateHost("RuntimeTarget", change.target);
        requireThat(
          change.target.instanceId === entry.record.instanceId &&
            [entry.candidateId, entry.previousCandidateId].includes(
              change.target.candidateId,
            ),
          "target_conflict",
          "Runtime target must belong to this deployment.",
        );
        requireThat(
          inside(this.config.runtimeRoot, change.target.configPath),
          "target_conflict",
          "Runtime configuration must belong to this host.",
        );
        this.db
          .prepare(
            "INSERT INTO runtime_targets VALUES (?,?) ON CONFLICT(instance_id) DO UPDATE SET value=excluded.value",
          )
          .run(change.target.instanceId, canonical(change.target));
      }
      if (change.rollbackTargetRevision !== undefined) {
        requireThat(
          phase === "rolling_back" &&
            change.target?.revision === change.rollbackTargetRevision &&
            change.target.candidateId === entry.previousCandidateId,
          "target_conflict",
          "Rollback revision must identify the retained previous target.",
        );
        entry.activation.rollbackTargetRevision = change.rollbackTargetRevision;
      }
      if (change.installed) {
        requireThat(
          change.installed.instanceId === entry.record.instanceId &&
            [entry.candidateId, entry.previousCandidateId].includes(
              change.installed.candidateId,
            ),
          "target_conflict",
          "Installed pointer must belong to this deployment.",
        );
        this.setInstalled(change.installed);
      }
      validateShared("DeploymentRecord", entry.record);
      validateHost("JournalEntry", entry);
      this.db
        .prepare(
          "UPDATE operations SET phase=?,entry_json=? WHERE deployment_id=?",
        )
        .run(phase, canonical(entry), deploymentId);
      return entry;
    });
  }
}
