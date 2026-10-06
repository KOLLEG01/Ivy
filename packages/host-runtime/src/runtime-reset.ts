import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import {
  access,
  lstat,
  mkdir,
  readFile,
  readdir,
  realpath,
  rm,
} from "node:fs/promises";
import { existsSync } from "node:fs";
import { constants } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { promisify } from "node:util";
import { canonical, digest, hashJson } from "../../contracts/src/canonical.js";
import { IvyError, requireThat } from "../../contracts/src/errors.js";
import {
  deriveOperationId,
  operationId,
} from "../../contracts/src/operation-id.js";
import type { Host, Operation } from "../../contracts/src/generated.js";
import type { DataContract } from "../../contracts/src/types.js";
import { validateHost } from "../../contracts/src/host-validation.js";
import { HiveStore } from "../../../services/hive/src/store.js";
import { Objects } from "../../../services/hive/src/objects.js";
import { chatContracts } from "../../../services/chat-bridge/src/schema.js";
import { taskBoardContracts } from "../../../services/task-board/src/runtime/schema.js";
import { HiveClient } from "../../sdk/src/client.js";
import type { RpcClient } from "../../sdk/src/client.js";
import { publishUi, readUiBundle } from "../../cli/src/publish-ui.js";
import { releaseManifest } from "./artifact.js";
import { atomicJson, inside, instanceConfig, jsonFile } from "./config.js";
import { sourceBuildPlan } from "./source.js";
import { startContainer, stopContainer } from "./docker.js";
import { bootstrapLaunchId, ExecutorLock, HostJournal } from "./journal.js";
import { servicePaths, resolvedFuturePath } from "./layout.js";
import { installationIdentity } from "./linux-resources.js";
import { materializeTarget } from "./target.js";
import { defaultSystemdAdapter, systemdUnitName } from "./systemd.js";
import {
  hiveResetBootstrapMarker,
  runtimeResetMarker,
} from "./runtime-maintenance.js";

const execute = promisify(execFile);
const resetDirectory = (config: Host.HostConfig) =>
  join(config.ivyRoot ?? dirname(config.runtimeRoot), "reset");
const recordPath = (config: Host.HostConfig) =>
  join(resetDirectory(config), "current.json");

type ResetPath = {
  instanceId: string;
  componentId: string;
  area: "host" | "data" | "work" | "logs";
  path: string;
  absolutePath: string;
};
type PreservedPath = ResetPath & { checksum: string | null };
type CompletionMarker = {
  instanceId: string;
  componentId: string;
  path: string;
  absolutePath: string;
};
type SavedRuntime = {
  instanceId: string;
  candidate: Host.Candidate;
  manifest: Host.ReleaseManifest;
  enabled: boolean;
};
type SavedInstruction = {
  family: { key: string; owner: string; mediaType: string; objectMode: string };
  contract: { version: string; definition: string };
  object: {
    id: string;
    parentId: string | null;
    ownerObjectId: string | null;
    name: string;
    path: string;
    position: number;
    icon: string | null;
    depth: number;
    currentRevision: number;
    createdAt: string;
    updatedAt: string;
  };
  revision: {
    contractVersion: string;
    encoding: string;
    content: string;
    contentHash: string;
    byteLength: number;
    createdAt: string;
    referencesJson: string;
  };
};
export type RuntimeResetPlan = {
  schemaVersion: 1;
  deploymentIdentity: string;
  hostId: string;
  configPath: string;
  instances: { instanceId: string; componentId: string; enabled: boolean }[];
  scopeRootIds: string[];
  delete: ResetPath[];
  preserve: PreservedPath[];
  completionMarkers: CompletionMarker[];
};
type ResetRecord = {
  schemaVersion: 1;
  deploymentIdentity: string;
  resetId: string;
  runtimeEpoch: string;
  hostId: string;
  phase: "prepared" | "cleared" | "complete" | "released";
  updatedAt: string;
  planHash: string;
  plan: RuntimeResetPlan;
  saved: SavedRuntime[];
  instructions: SavedInstruction[];
  publisherCredentialDigest: string;
  instructionsCaptured?: boolean;
  deploymentComplete?: boolean;
};
type ResetUiBundle = {
  directory: string;
  definition: Operation.UiDefinition;
  checkedBundle: Awaited<ReturnType<typeof readUiBundle>>;
};

export function resetBootstrapContracts(
  bundles: readonly ResetUiBundle[],
): DataContract[] {
  const serviceContracts = [...chatContracts(), ...taskBoardContracts()];
  const bundledContracts = bundles.flatMap(
    (bundle) => bundle.definition.dataContracts,
  );
  const available = [...bundledContracts, ...serviceContracts];
  const selected = new Map<string, DataContract>();
  for (const bundle of bundles) {
    for (const required of bundle.definition.requirements.contracts)
      for (const version of [
        ...required.readVersions,
        ...required.writeVersions,
      ]) {
        const definition = available.find(
          (value) => value.key === required.key && value.version === version,
        );
        requireThat(
          definition,
          "contract_not_found",
          `Reset ui ${bundle.definition.metadata.uiId} requires unavailable contract ${required.key}@${version}.`,
        );
        if (definition.owner.kind === "service")
          selected.set(definition.key + "@" + definition.version, definition);
      }
  }
  return [...selected.values()].sort((left, right) =>
    (left.key + "@" + left.version).localeCompare(
      right.key + "@" + right.version,
    ),
  );
}

export function retainedResetIdentity(
  previous: Pick<
    ResetRecord,
    "resetId" | "runtimeEpoch" | "phase" | "planHash" | "deploymentComplete"
  > | null,
  planHash: string,
): {
  resetId: string;
  runtimeEpoch: string;
  resumeFromReleased: boolean;
} | null {
  return previous &&
    !previous.deploymentComplete &&
    previous.planHash === planHash
    ? {
        resetId: previous.resetId,
        runtimeEpoch: previous.runtimeEpoch,
        resumeFromReleased: previous.phase === "released",
      }
    : null;
}

async function exists(path: string): Promise<boolean> {
  try {
    await lstat(path);
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return false;
    throw error;
  }
}

function collectScopeRoots(value: unknown, roots: Set<string>): void {
  if (!value || typeof value !== "object") return;
  if (Array.isArray(value)) {
    for (const item of value) collectScopeRoots(item, roots);
    return;
  }
  for (const [key, item] of Object.entries(value)) {
    if (key === "rootObjectId" && typeof item === "string") roots.add(item);
    else collectScopeRoots(item, roots);
  }
}

export function configuredScopeRootIds(
  config: Pick<Host.HostConfig, "instances">,
): string[] {
  const roots = new Set<string>();
  for (const instance of config.instances)
    collectScopeRoots(instance.settings, roots);
  return [...roots].sort();
}

export function coordinatedRuntimeResetPlans(
  plans: RuntimeResetPlan[],
): RuntimeResetPlan[] {
  const roots = [...new Set(plans.flatMap((plan) => plan.scopeRootIds))].sort();
  return plans.map((plan) => ({ ...plan, scopeRootIds: roots }));
}

export async function resetUiBundles(
  distributionRoot: string,
  uiIds: readonly string[],
): Promise<ResetUiBundle[]> {
  requireThat(
    uiIds.length > 0 &&
      uiIds.length <= 32 &&
      new Set(uiIds).size === uiIds.length,
    "invalid_arguments",
    "Runtime reset needs a bounded unique configured ui list.",
  );
  const root = join(distributionRoot, "dist", "uis"),
    entries = await readdir(root, { withFileTypes: true });
  const bundles: ResetUiBundle[] = [];
  for (const uiId of uiIds) {
    const entry = entries.find((value) => value.name === uiId);
    requireThat(
      entry?.isDirectory(),
      "not_found",
      `Configured ui bundle ${uiId} is missing.`,
    );
    const directory = join(root, uiId),
      definitionPath = join(directory, "ivy-ui.json");
    requireThat(
      await exists(definitionPath),
      "not_found",
      `Configured ui definition ${uiId} is missing.`,
    );
    const definition = JSON.parse(
      await readFile(definitionPath, "utf8"),
    ) as Operation.UiDefinition;
    requireThat(
      definition.metadata?.uiId === uiId &&
        definition.dataContracts.every((value) => value.owner.kind === "agent"),
      "invalid_arguments",
      `Configured ui bundle ${uiId} cannot be published by the reset coordinator.`,
    );
    bundles.push({
      directory,
      definition,
      checkedBundle: await readUiBundle(directory, definition),
    });
  }
  return bundles;
}

export async function republishResetUis(
  client: RpcClient,
  bundles: readonly ResetUiBundle[],
  baseMutationId: string,
): Promise<string[]> {
  const published: string[] = [];
  for (const bundle of bundles) {
    const uiId = bundle.definition.metadata.uiId;
    let current: Operation.UiInspection | null = null;
    try {
      current = await client.request("uis.inspect", { uiId });
    } catch (error) {
      if (IvyError.from(error).code !== "not_found") throw error;
    }
    if (current) {
      requireThat(
        current.currentReleaseId === bundle.checkedBundle.releaseId,
        "mutation_conflict",
        `Reset ui ${uiId} has another active release.`,
      );
      requireThat(
        current.status === "ready",
        "service_not_ready",
        `Republished ui ${uiId} is not ready.`,
      );
      published.push(uiId);
      continue;
    }
    await publishUi(client, {
      ...bundle,
      expectedReleaseId: null,
      mutationId: deriveOperationId(baseMutationId, ["ui", uiId]),
    });
    const inspection = await client.request("uis.inspect", { uiId });
    requireThat(
      inspection.status === "ready",
      "service_not_ready",
      `Republished ui ${uiId} is not ready.`,
    );
    published.push(uiId);
  }
  return published;
}

export async function releaseResetHosts<T extends { hostId: string }>(
  hosts: readonly T[],
  release: (host: T) => Promise<unknown>,
  reprepare: (host: T) => Promise<unknown>,
  afterRelease: () => Promise<unknown> = async () => undefined,
): Promise<void> {
  const possiblyReleased: T[] = [];
  try {
    for (const host of hosts) {
      possiblyReleased.push(host);
      await release(host);
    }
    await afterRelease();
  } catch (error) {
    const recovery = await Promise.allSettled(
        possiblyReleased.map((host) => reprepare(host)),
      ),
      failure = IvyError.from(error);
    throw new IvyError(failure.code, failure.message, failure.outcome, {
      cause: failure.details,
      possiblyReleasedHosts: possiblyReleased.map((host) => host.hostId),
      repreparedHosts: possiblyReleased
        .filter((_host, index) => recovery[index]?.status === "fulfilled")
        .map((host) => host.hostId),
      reprepareFailures: possiblyReleased
        .filter((_host, index) => recovery[index]?.status === "rejected")
        .map((host) => host.hostId),
    });
  }
}

export async function bootstrapResetUis<T extends { hostId: string }>(
  hosts: readonly T[],
  bootstrap: (host: T) => Promise<unknown>,
  reprepare: (host: T) => Promise<unknown>,
  restoreUis: () => Promise<unknown>,
): Promise<void> {
  const possiblyBootstrapped: T[] = [];
  try {
    for (const host of hosts) {
      possiblyBootstrapped.push(host);
      await bootstrap(host);
    }
    await restoreUis();
  } catch (error) {
    const recovery = await Promise.allSettled(
        possiblyBootstrapped.map((host) => reprepare(host)),
      ),
      failure = IvyError.from(error);
    throw new IvyError(failure.code, failure.message, failure.outcome, {
      cause: failure.details,
      possiblyBootstrappedHosts: possiblyBootstrapped.map(
        (host) => host.hostId,
      ),
      repreparedHosts: possiblyBootstrapped
        .filter((_host, index) => recovery[index]?.status === "fulfilled")
        .map((host) => host.hostId),
      reprepareFailures: possiblyBootstrapped
        .filter((_host, index) => recovery[index]?.status === "rejected")
        .map((host) => host.hostId),
    });
  }
}

const encodeScopeRoots = (ids: string[]): string =>
  Buffer.from(canonical(ids), "utf8").toString("base64url");
function decodeScopeRoots(value: string): string[] {
  let decoded: unknown;
  try {
    decoded = JSON.parse(Buffer.from(value, "base64url").toString("utf8"));
  } catch {
    throw new IvyError("invalid_arguments", "Invalid coordinated scope roots.");
  }
  requireThat(
    Array.isArray(decoded) &&
      decoded.every(
        (id) => typeof id === "string" && id.length > 0 && id.length <= 256,
      ) &&
      canonical(decoded) === canonical([...new Set(decoded)].sort()),
    "invalid_arguments",
    "Invalid coordinated scope roots.",
  );
  return decoded as string[];
}

async function treeHash(path: string): Promise<string | null> {
  if (!(await exists(path))) return null;
  const rows: { path: string; hash: string }[] = [];
  const walk = async (current: string, local: string): Promise<void> => {
    const info = await lstat(current);
    requireThat(
      !info.isSymbolicLink(),
      "target_conflict",
      "Preserved reset paths cannot traverse links.",
    );
    if (info.isFile()) {
      rows.push({ path: local, hash: digest(await readFile(current)) });
      return;
    }
    requireThat(
      info.isDirectory(),
      "target_conflict",
      "Preserved reset paths must be regular files or directories.",
    );
    for (const entry of (await readdir(current, { withFileTypes: true })).sort(
      (a, b) => a.name.localeCompare(b.name),
    )) {
      requireThat(
        !entry.isSymbolicLink(),
        "target_conflict",
        "Preserved reset paths cannot contain links.",
      );
      await walk(
        join(current, entry.name),
        local ? local + "/" + entry.name : entry.name,
      );
    }
  };
  await walk(path, "");
  return hashJson(rows);
}

function deployment(
  config: Host.HostConfig,
): NonNullable<Host.HostConfig["deployment"]> {
  const value = config.deployment;
  requireThat(
    value &&
      value.hosts.length > 0 &&
      value.hosts.some((host) => host.hostId === config.hostId),
    "invalid_arguments",
    "Runtime reset requires an explicit deployment identity and host list containing this host.",
  );
  requireThat(
    new Set(value.hosts.map((host) => host.hostId)).size === value.hosts.length,
    "invalid_arguments",
    "Runtime reset deployment hosts must be unique.",
  );
  for (const host of value.hosts)
    requireThat(
      host.hostId === config.hostId ||
        (host.ssh && host.cliPath && isAbsolute(host.cliPath)),
      "invalid_arguments",
      "Every remote reset host requires an SSH identity and absolute CLI path.",
    );
  return value;
}

function resetConfiguration(config: Host.HostConfig): {
  coordinatorHostId: string;
  uiIds: string[];
} {
  const values = config.instances
    .map((instance) => instance.settings["runtimeReset"])
    .filter((value) => value !== undefined);
  requireThat(
    values.length === 1,
    "invalid_arguments",
    "Runtime reset requires one configured coordinator and ui list.",
  );
  const value = values[0];
  requireThat(
    value && typeof value === "object" && !Array.isArray(value),
    "invalid_arguments",
    "Runtime reset configuration is invalid.",
  );
  const coordinatorHostId = value["coordinatorHostId"],
    uiIds = value["uiIds"];
  requireThat(
    typeof coordinatorHostId === "string" &&
      Array.isArray(uiIds) &&
      uiIds.every((uiId) => typeof uiId === "string"),
    "invalid_arguments",
    "Runtime reset configuration is invalid.",
  );
  return { coordinatorHostId, uiIds };
}

async function componentManifest(
  config: Host.HostConfig,
  journal: HostJournal,
  instance: Host.Instance,
  distributionRoot: string,
): Promise<Host.ReleaseManifest> {
  const installed = journal.installed(instance.instanceId);
  if (installed) return journal.manifest(installed.candidateId);
  const value = await sourceBuildPlan(distributionRoot, instance.componentId);
  return releaseManifest({
    ...value,
    buildId: digest("uninstalled:" + instance.componentId),
  });
}

function areaRoot(
  config: Host.HostConfig,
  instance: Host.Instance,
  area: ResetPath["area"],
): string {
  if (area === "host") return config.runtimeRoot;
  return servicePaths(config, instance)[area];
}

export async function localRuntimeResetPlan(
  config: Host.HostConfig,
  configPath: string,
  distributionRoot: string,
): Promise<RuntimeResetPlan> {
  const selected = deployment(config),
    journal = new HostJournal(config);
  try {
    const deleting: ResetPath[] = [],
      preserving: PreservedPath[] = [],
      completionMarkers: CompletionMarker[] = [];
    for (const instance of config.instances) {
      const manifest = await componentManifest(
          config,
          journal,
          instance,
          distributionRoot,
        ),
        layout = manifest.runtimeReset;
      requireThat(
        layout,
        "unsupported_storage",
        `Component ${instance.componentId} has no fixed runtime reset layout.`,
      );
      for (const [kind, entries] of [
        ["delete", layout.delete],
        ["preserve", layout.preserve],
      ] as const)
        for (const entry of entries) {
          const root = resolve(areaRoot(config, instance, entry.area)),
            target = resolve(root, entry.path);
          requireThat(
            target === root
              ? entry.path === "." && ["work", "logs"].includes(entry.area)
              : inside(root, target),
            "target_conflict",
            "Runtime reset target leaves its declared component area.",
          );
          const actualRoot = await resolvedFuturePath(root),
            actualTarget = await resolvedFuturePath(target);
          requireThat(
            actualTarget === actualRoot
              ? target === root
              : inside(actualRoot, actualTarget),
            "target_conflict",
            "Runtime reset target traverses a filesystem link.",
          );
          const value: ResetPath = {
            instanceId: instance.instanceId,
            componentId: instance.componentId,
            area: entry.area,
            path: entry.path,
            absolutePath: target,
          };
          if (kind === "delete") deleting.push(value);
          else preserving.push({ ...value, checksum: await treeHash(target) });
        }
      if (layout.completionMarker) {
        const root = resolve(servicePaths(config, instance).data),
          target = resolve(root, layout.completionMarker);
        requireThat(
          target !== root && inside(root, target),
          "target_conflict",
          "Runtime reset completion marker leaves its instance data.",
        );
        const actualRoot = await resolvedFuturePath(root),
          actualTarget = await resolvedFuturePath(target);
        requireThat(
          inside(actualRoot, actualTarget),
          "target_conflict",
          "Runtime reset completion marker traverses a filesystem link.",
        );
        completionMarkers.push({
          instanceId: instance.instanceId,
          componentId: instance.componentId,
          path: layout.completionMarker,
          absolutePath: target,
        });
      }
    }
    const configFile = resolve(configPath),
      protectedRoots = [
        config.artifactRoot,
        config.stagingRoot,
        configFile,
        ...config.instances
          .filter((value) => value.componentId === "agent-manager")
          .flatMap((value) => [
            value.settings["projectRoot"],
            value.settings["internalProjectRoot"],
          ])
          .filter((value): value is string => typeof value === "string")
          .map((value) => resolve(value)),
      ];
    requireThat(
      deleting.every((item) =>
        protectedRoots.every(
          (root) =>
            item.absolutePath !== root &&
            !inside(item.absolutePath, root) &&
            !inside(root, item.absolutePath),
        ),
      ),
      "target_conflict",
      "Runtime reset target overlaps configuration, software, source or project storage.",
    );
    requireThat(
      deleting.every((item) =>
        preserving.every(
          (saved) =>
            item.absolutePath !== saved.absolutePath &&
            !inside(item.absolutePath, saved.absolutePath) &&
            !inside(saved.absolutePath, item.absolutePath),
        ),
      ),
      "target_conflict",
      "A preserved component setting overlaps a deletion target.",
    );
    requireThat(
      new Set(deleting.map((value) => value.absolutePath.toLowerCase()))
        .size === deleting.length,
      "target_conflict",
      "Runtime reset deletion targets overlap between component declarations.",
    );
    requireThat(
      new Set(
        completionMarkers.map((value) => value.absolutePath.toLowerCase()),
      ).size === completionMarkers.length,
      "target_conflict",
      "Runtime reset completion markers overlap between component declarations.",
    );
    return {
      schemaVersion: 1,
      deploymentIdentity: selected.identity,
      hostId: config.hostId,
      configPath: resolve(configPath),
      instances: config.instances.map(
        ({ instanceId, componentId, enabled }) => ({
          instanceId,
          componentId,
          enabled,
        }),
      ),
      scopeRootIds: configuredScopeRootIds(config),
      delete: deleting,
      preserve: preserving,
      completionMarkers,
    };
  } finally {
    journal.close();
  }
}

async function requireAdministrator(): Promise<void> {
  if (process.platform !== "win32") {
    requireThat(
      process.getuid?.() === 0,
      "access_denied",
      "Runtime reset requires the local OS administrator.",
    );
    return;
  }
  const result = await execute(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "[Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)",
    ],
    { windowsHide: true, maxBuffer: 65536 },
  ).catch(() => null);
  requireThat(
    result?.stdout.trim().toLowerCase() === "true",
    "access_denied",
    "Runtime reset requires an elevated Windows administrator token.",
  );
}

function savedInstructions(path: string): SavedInstruction[] {
  if (!existsSync(path)) return [];
  const store = new HiveStore(path);
  try {
    return store
      .all(
        "SELECT o.*,f.owner,f.media_type,f.object_mode,r.contract_version,r.encoding,r.content,r.content_hash,r.byte_length,r.created_at AS revision_created_at,r.references_json,c.definition FROM objects o JOIN contract_families f ON f.key=o.contract_key JOIN revisions r ON r.object_id=o.id AND r.revision=o.current_revision JOIN contracts c ON c.key=r.contract_key AND c.version=r.contract_version WHERE o.contract_key='agent/instructions' AND o.effective_archive=0",
      )
      .map((row) => ({
        family: {
          key: "agent/instructions",
          owner: String(row["owner"]),
          mediaType: String(row["media_type"]),
          objectMode: String(row["object_mode"]),
        },
        contract: {
          version: String(row["contract_version"]),
          definition: String(row["definition"]),
        },
        object: {
          id: String(row["id"]),
          parentId: row["parent_id"] === null ? null : String(row["parent_id"]),
          ownerObjectId:
            row["owner_object_id"] === null
              ? null
              : String(row["owner_object_id"]),
          name: String(row["name"]),
          path: String(row["path"]),
          position: Number(row["position"]),
          icon: row["icon"] === null ? null : String(row["icon"]),
          depth: Number(row["depth"]),
          currentRevision: Number(row["current_revision"]),
          createdAt: String(row["created_at"]),
          updatedAt: String(row["updated_at"]),
        },
        revision: {
          contractVersion: String(row["contract_version"]),
          encoding: String(row["encoding"]),
          content: String(row["content"]),
          contentHash: String(row["content_hash"]),
          byteLength: Number(row["byte_length"]),
          createdAt: String(row["revision_created_at"]),
          referencesJson: String(row["references_json"]),
        },
      }));
  } finally {
    store.close();
  }
}

function restoreInstructions(
  store: HiveStore,
  values: SavedInstruction[],
): void {
  store.transaction(() => {
    for (const value of values) {
      requireThat(
        value.object.parentId === null &&
          value.object.ownerObjectId === null &&
          value.revision.referencesJson === "{}",
        "storage_invalid",
        "Instruction configuration must be a root document without runtime references.",
      );
      store.run(
        "INSERT OR IGNORE INTO contract_families VALUES(?,?,?,?)",
        value.family.key,
        value.family.owner,
        value.family.mediaType,
        value.family.objectMode,
      );
      store.run(
        "INSERT OR IGNORE INTO contracts VALUES(?,?,?)",
        value.family.key,
        value.contract.version,
        value.contract.definition,
      );
      store.run(
        "INSERT INTO objects(search_id,id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at) VALUES(NULL,?,?,?,?,?,?,?,?,?,?,NULL,0,?,?)",
        value.object.id,
        value.object.parentId,
        value.object.ownerObjectId,
        value.object.name,
        value.object.path,
        value.object.position,
        value.object.icon,
        value.object.depth,
        value.family.key,
        value.object.currentRevision,
        value.object.createdAt,
        value.object.updatedAt,
      );
      store.run(
        "INSERT INTO revisions VALUES(?,?,?,?,?,?,?,?,?,?)",
        value.object.id,
        value.object.currentRevision,
        value.family.key,
        value.revision.contractVersion,
        value.revision.encoding,
        value.revision.content,
        value.revision.contentHash,
        value.revision.byteLength,
        value.revision.createdAt,
        value.revision.referencesJson,
      );
      const search = store.get(
        "SELECT search_id FROM objects WHERE id=?",
        value.object.id,
      );
      requireThat(
        search,
        "storage_invalid",
        "Restored instruction metadata is unavailable.",
      );
      store.run(
        "INSERT INTO object_fts(rowid,object_id,name,text) VALUES(?,?,?,?)",
        Number(search["search_id"]),
        value.object.id,
        value.object.name,
        "",
      );
    }
  });
}

async function sendStop(
  config: Host.HostConfig,
  instance: Host.Instance,
): Promise<number | null> {
  const data = servicePaths(config, instance).data;
  try {
    const health = await jsonFile<Host.Health>(join(data, "health.json"));
    validateHost("Health", health);
    if (health.instanceId !== instance.instanceId) return null;
    if (health.launchId)
      await atomicJson(join(data, "control.json"), {
        schemaVersion: 1,
        instanceId: instance.instanceId,
        launchId: health.launchId,
        bootId: health.bootId,
        action: "shutdown",
        requestedAt: new Date().toISOString(),
      });
    else process.kill(health.pid, "SIGTERM");
    return health.pid;
  } catch {
    return null;
  }
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}
async function waitStopped(pids: number[]): Promise<void> {
  const end = Date.now() + 60_000;
  while (pids.some(alive) && Date.now() < end)
    await new Promise((resolve) => setTimeout(resolve, 200));
  requireThat(
    !pids.some(alive),
    "outcome_unknown",
    "Not every reset writer established a stopped process.",
  );
}

async function proveLocksReleased(config: Host.HostConfig): Promise<void> {
  const names = [
    "executor",
    ...config.instances.map((instance) => "owners/" + instance.instanceId),
  ];
  for (const name of names) {
    const root = join(config.runtimeRoot, name);
    let lock: ExecutorLock | null = null,
      end = Date.now() + 30_000;
    while (!lock && Date.now() < end) {
      try {
        lock = new ExecutorLock(root);
      } catch {
        await new Promise((resolve) => setTimeout(resolve, 200));
      }
    }
    requireThat(
      lock,
      "outcome_unknown",
      "A reset writer still owns its maintenance lock.",
    );
    lock.close();
  }
}

async function stopHost(config: Host.HostConfig): Promise<void> {
  if (process.platform === "linux") {
    for (const instance of [...config.instances].reverse()) {
      if (instance.engine === "docker")
        await stopContainer(config, instance.instanceId, 30_000);
      else
        await defaultSystemdAdapter.stopUnit(
          config,
          systemdUnitName(config, instance.instanceId),
          30_000,
        );
    }
  } else {
    const ordered = [...config.instances].sort(
      (a, b) =>
        Number(["service-manager", "host-executor"].includes(a.componentId)) -
        Number(["service-manager", "host-executor"].includes(b.componentId)),
    );
    const pids = (
      await Promise.all(ordered.map((instance) => sendStop(config, instance)))
    ).filter((value): value is number => value !== null);
    await waitStopped(pids);
  }
  await proveLocksReleased(config);
}

function savedRuntime(config: Host.HostConfig): SavedRuntime[] {
  const journal = new HostJournal(config);
  try {
    return config.instances.flatMap((instance) => {
      const installed = journal.installed(instance.instanceId);
      if (!installed) return [];
      return [
        {
          instanceId: instance.instanceId,
          candidate: journal.candidate(installed.candidateId),
          manifest: journal.manifest(installed.candidateId),
          enabled: installed.enabled,
        },
      ];
    });
  } finally {
    journal.close();
  }
}

export function restoredBootstrapTarget(
  plan: Host.BootstrapPlan,
  instanceId: string,
  candidateId: string,
  enabled: boolean,
  requestedAt: string,
): Host.RuntimeTarget | null {
  const process = plan.processes.find(
    (value) =>
      value.instanceId === instanceId &&
      value.candidateId === candidateId &&
      value.configPath,
  );
  if (!process || process.componentId === "host-executor") return null;
  const target: Host.RuntimeTarget = {
    schemaVersion: 1,
    instanceId,
    revision: bootstrapLaunchId(plan, instanceId),
    candidateId,
    desired: enabled ? "running" : "stopped",
    configPath: process.configPath!,
    requestedAt,
  };
  validateHost("RuntimeTarget", target);
  return target;
}

async function prepareLocal(
  config: Host.HostConfig,
  plan: RuntimeResetPlan,
  resetId: string,
  runtimeEpoch: string,
  publisherCredentialDigest: string,
): Promise<ResetRecord> {
  requireThat(
    /^sha256:[0-9a-f]{64}$/.test(publisherCredentialDigest),
    "invalid_arguments",
    "Runtime reset requires the checked ui publisher credential digest.",
  );
  await requireAdministrator();
  const root = resetDirectory(config);
  await mkdir(root, { recursive: true });
  const previous = await jsonFile<ResetRecord>(recordPath(config)).catch(
    () => null,
  );
  const same =
    previous &&
    previous.resetId === resetId &&
    previous.runtimeEpoch === runtimeEpoch &&
    previous.planHash === hashJson(plan) &&
    previous.publisherCredentialDigest === publisherCredentialDigest;
  if (previous && !previous.deploymentComplete)
    requireThat(
      same,
      "mutation_conflict",
      "Another incomplete runtime reset must be resumed exactly.",
    );
  const retained = same ? previous : null;
  const record: ResetRecord = retained
    ? {
        ...retained,
        instructions: retained.instructions ?? [],
        phase: ["complete", "released"].includes(retained.phase)
          ? "cleared"
          : retained.phase,
        updatedAt: new Date().toISOString(),
        deploymentComplete: false,
      }
    : {
        schemaVersion: 1,
        deploymentIdentity: plan.deploymentIdentity,
        resetId,
        runtimeEpoch,
        hostId: config.hostId,
        phase: "prepared",
        updatedAt: new Date().toISOString(),
        planHash: hashJson(plan),
        plan,
        saved: savedRuntime(config),
        instructions: [],
        publisherCredentialDigest,
        instructionsCaptured: false,
        deploymentComplete: false,
      };
  await atomicJson(recordPath(config), record);
  await atomicJson(runtimeResetMarker(config), {
    schemaVersion: 1,
    deploymentIdentity: record.deploymentIdentity,
    resetId,
    runtimeEpoch,
    hostId: config.hostId,
    createdAt: record.updatedAt,
  });
  await stopHost(config);
  return record;
}

async function clearLocal(
  config: Host.HostConfig,
  resetId: string,
  runtimeEpoch: string,
  bootstrapContracts: DataContract[],
): Promise<ResetRecord> {
  await requireAdministrator();
  let record = await jsonFile<ResetRecord>(recordPath(config));
  requireThat(
    record.resetId === resetId &&
      record.runtimeEpoch === runtimeEpoch &&
      ["prepared", "cleared"].includes(record.phase) &&
      (await exists(runtimeResetMarker(config))),
    "mutation_conflict",
    "Runtime reset host is not prepared for this identity.",
  );
  if (record.phase === "cleared") return record;
  const hiveInstance = config.instances.find(
    (value) => value.componentId === "hive",
  );
  if (hiveInstance && !record.instructionsCaptured) {
    record = {
      ...record,
      instructions: savedInstructions(
        join(servicePaths(config, hiveInstance).data, "hive.sqlite"),
      ),
      instructionsCaptured: true,
      updatedAt: new Date().toISOString(),
    };
    await atomicJson(recordPath(config), record);
  }
  for (const target of record.plan.delete) {
    const actual = await resolvedFuturePath(target.absolutePath),
      root = await resolvedFuturePath(
        areaRoot(
          config,
          config.instances.find(
            (value) => value.instanceId === target.instanceId,
          )!,
          target.area,
        ),
      );
    requireThat(
      actual === root
        ? target.path === "." && ["work", "logs"].includes(target.area)
        : inside(root, actual),
      "target_conflict",
      "Runtime reset target changed after preview.",
    );
    await rm(target.absolutePath, { recursive: true, force: true });
    if (target.path === ".")
      await mkdir(target.absolutePath, { recursive: true });
  }
  for (const marker of record.plan.completionMarkers)
    await atomicJson(marker.absolutePath, {
      schemaVersion: 1,
      resetId: record.resetId,
      runtimeEpoch: record.runtimeEpoch,
      completedAt: new Date().toISOString(),
    });
  const hive = config.instances.find((value) => value.componentId === "hive");
  if (hive) {
    const path = join(servicePaths(config, hive).data, "hive.sqlite"),
      store = new HiveStore(path);
    try {
      store.setMetadata("runtime_epoch", record.runtimeEpoch);
      restoreInstructions(store, record.instructions);
      const objects = new Objects(store);
      objects.bootstrapScopeRoots(record.plan.scopeRootIds);
      objects.bootstrapServiceContracts(bootstrapContracts);
    } finally {
      store.close();
    }
  }
  const bootstrap =
    process.platform === "linux"
      ? await jsonFile<Host.BootstrapPlan>(
          join(
            config.runtimeRoot,
            "bootstrap",
            installationIdentity(config) + ".json",
          ),
        )
      : null;
  if (bootstrap) {
    validateHost("BootstrapPlan", bootstrap);
    requireThat(
      bootstrap.os === "linux" &&
        bootstrap.hostId === config.hostId &&
        bootstrap.runtimeRoot === config.runtimeRoot,
      "target_conflict",
      "Retained Linux bootstrap belongs to another host runtime.",
    );
  }
  const journal = new HostJournal(config);
  try {
    for (const saved of record.saved) {
      const requestedAt = new Date().toISOString();
      journal.saveCandidate(saved.candidate, saved.manifest);
      journal.setInstalled({
        instanceId: saved.instanceId,
        candidateId: saved.candidate.candidateId,
        buildId: saved.candidate.buildId,
        enabled: saved.enabled,
        installedAt: requestedAt,
      });
      const retained =
        bootstrap &&
        restoredBootstrapTarget(
          bootstrap,
          saved.instanceId,
          saved.candidate.candidateId,
          saved.enabled,
          requestedAt,
        );
      if (retained) journal.restoreTarget(retained);
      else {
        const materialized = await materializeTarget(
          journal,
          saved.instanceId,
          saved.candidate.candidateId,
          saved.enabled,
        );
        journal.restoreTarget(materialized.target);
      }
    }
  } finally {
    journal.close();
  }
  for (const saved of record.plan.preserve)
    requireThat(
      (await treeHash(saved.absolutePath)) === saved.checksum,
      "configuration_changed",
      "Preserved reset configuration changed.",
    );
  const next = {
    ...record,
    phase: "cleared" as const,
    updatedAt: new Date().toISOString(),
  };
  await atomicJson(recordPath(config), next);
  return next;
}

async function completeLocal(
  config: Host.HostConfig,
  resetId: string,
  runtimeEpoch: string,
): Promise<ResetRecord> {
  await requireAdministrator();
  const record = await jsonFile<ResetRecord>(recordPath(config));
  requireThat(
    record.resetId === resetId &&
      record.runtimeEpoch === runtimeEpoch &&
      ["cleared", "complete"].includes(record.phase),
    "mutation_conflict",
    "Runtime reset host is not cleared for this identity.",
  );
  if (record.phase === "complete") return record;
  const next = {
    ...record,
    phase: "complete" as const,
    deploymentComplete: false,
    updatedAt: new Date().toISOString(),
  };
  await atomicJson(recordPath(config), next);
  return next;
}

async function bootstrapLocal(
  config: Host.HostConfig,
  resetId: string,
  runtimeEpoch: string,
): Promise<ResetRecord> {
  await requireAdministrator();
  const record = await jsonFile<ResetRecord>(recordPath(config));
  requireThat(
    record.resetId === resetId &&
      record.runtimeEpoch === runtimeEpoch &&
      record.phase === "complete" &&
      (await exists(runtimeResetMarker(config))),
    "mutation_conflict",
    "Runtime reset host is not held for bootstrap.",
  );
  const enabled = new Set(
    record.saved
      .filter((value) => value.enabled)
      .map((value) => value.instanceId),
  );
  const instance = config.instances.find(
    (value) =>
      value.componentId === "hive" &&
      value.engine === "docker" &&
      enabled.has(value.instanceId),
  );
  if (!instance) return record;
  const journal = new HostJournal(config);
  try {
    const target = journal.target(instance.instanceId);
    requireThat(
      target?.desired === "running",
      "storage_invalid",
      "Enabled Hive reset target was not restored.",
    );
    const candidate = journal.candidate(target.candidateId),
      manifest = journal.manifest(target.candidateId),
      runtime = await instanceConfig(target.configPath);
    await stopContainer(
      config,
      instance.instanceId,
      manifest.shutdown!.timeoutMs,
    );
    await atomicJson(hiveResetBootstrapMarker(runtime.dataRoot), {
      schemaVersion: 1,
      resetId,
      runtimeEpoch,
      phase: "bootstrap",
      publisherCredentialDigest: record.publisherCredentialDigest,
    });
    const managed = await startContainer(
      config,
      instance,
      target,
      runtime,
      candidate,
      manifest,
      true,
    );
    await managed.verify?.();
    managed.dispose?.();
    return record;
  } finally {
    journal.close();
  }
}

async function releaseLocal(
  config: Host.HostConfig,
  resetId: string,
  runtimeEpoch: string,
): Promise<ResetRecord> {
  await requireAdministrator();
  const record = await jsonFile<ResetRecord>(recordPath(config));
  requireThat(
    record.resetId === resetId &&
      record.runtimeEpoch === runtimeEpoch &&
      ["complete", "released"].includes(record.phase),
    "mutation_conflict",
    "Runtime reset host is not jointly complete for this identity.",
  );
  if (record.phase === "released") return record;
  const enabled = new Set(
      record.saved
        .filter((value) => value.enabled)
        .map((value) => value.instanceId),
    ),
    journal = new HostJournal(config);
  let journalClosed = false,
    hiveBootstrapPath: string | null = null;
  try {
    const hive = config.instances.find(
      (value) =>
        value.componentId === "hive" &&
        value.engine === "docker" &&
        enabled.has(value.instanceId),
    );
    if (hive) {
      const target = journal.target(hive.instanceId);
      requireThat(
        target?.desired === "running",
        "storage_invalid",
        "Enabled Hive reset target was not restored.",
      );
      const manifest = journal.manifest(target.candidateId),
        runtime = await instanceConfig(target.configPath);
      hiveBootstrapPath = hiveResetBootstrapMarker(runtime.dataRoot);
      await stopContainer(
        config,
        hive.instanceId,
        manifest.shutdown!.timeoutMs,
      );
      await atomicJson(hiveBootstrapPath, {
        schemaVersion: 1,
        resetId,
        runtimeEpoch,
        phase: "released",
      });
    }
    await rm(runtimeResetMarker(config), { force: true });
    for (const instance of config.instances.filter(
      (value) => value.engine === "docker" && enabled.has(value.instanceId),
    )) {
      const target = journal.target(instance.instanceId);
      requireThat(
        target?.desired === "running",
        "storage_invalid",
        "Enabled Docker reset target was not restored.",
      );
      const candidate = journal.candidate(target.candidateId),
        manifest = journal.manifest(target.candidateId),
        runtime = await instanceConfig(target.configPath);
      const managed = await startContainer(
        config,
        instance,
        target,
        runtime,
        candidate,
        manifest,
        true,
      );
      await managed.verify?.();
      managed.dispose?.();
    }
    const processes = config.instances.filter(
      (value) => value.engine === "process" && enabled.has(value.instanceId),
    );
    if (process.platform === "linux") {
      for (const instance of processes)
        await defaultSystemdAdapter.startUnit(
          config,
          systemdUnitName(config, instance.instanceId),
        );
    } else {
      for (const instance of processes.filter((value) =>
        ["host-executor", "service-manager"].includes(value.componentId),
      )) {
        const name =
          installationIdentity(config) +
          "-" +
          hashJson(instance.instanceId).slice(7, 19);
        await execute("schtasks.exe", ["/run", "/tn", name], {
          windowsHide: true,
          maxBuffer: 65536,
          timeout: 30_000,
        });
      }
    }
    const next = {
      ...record,
      phase: "released" as const,
      deploymentComplete: false,
      updatedAt: new Date().toISOString(),
    };
    await atomicJson(recordPath(config), next);
    if (hiveBootstrapPath) await rm(hiveBootstrapPath, { force: true });
    return next;
  } catch (error) {
    journal.close();
    journalClosed = true;
    try {
      await atomicJson(runtimeResetMarker(config), {
        schemaVersion: 1,
        deploymentIdentity: record.deploymentIdentity,
        resetId: record.resetId,
        runtimeEpoch: record.runtimeEpoch,
        hostId: record.hostId,
        createdAt: new Date().toISOString(),
      });
      if (hiveBootstrapPath)
        await atomicJson(hiveBootstrapPath, {
          schemaVersion: 1,
          resetId,
          runtimeEpoch,
          phase: "bootstrap",
          publisherCredentialDigest: record.publisherCredentialDigest,
        });
    } finally {
      await stopHost(config).catch(() => undefined);
      const held = {
        ...record,
        phase: "complete" as const,
        deploymentComplete: false,
        updatedAt: new Date().toISOString(),
      };
      await atomicJson(recordPath(config), held);
    }
    throw error;
  } finally {
    if (!journalClosed) journal.close();
  }
}

async function confirmLocal(
  config: Host.HostConfig,
  resetId: string,
  runtimeEpoch: string,
): Promise<ResetRecord> {
  await requireAdministrator();
  const record = await jsonFile<ResetRecord>(recordPath(config));
  requireThat(
    record.resetId === resetId &&
      record.runtimeEpoch === runtimeEpoch &&
      record.phase === "released",
    "mutation_conflict",
    "Runtime reset host is not released for this identity.",
  );
  if (record.deploymentComplete) return record;
  const next = {
    ...record,
    deploymentComplete: true,
    updatedAt: new Date().toISOString(),
  };
  await atomicJson(recordPath(config), next);
  return next;
}

async function remote(
  config: Host.HostConfig,
  host: NonNullable<Host.HostConfig["deployment"]>["hosts"][number],
  args: string[],
): Promise<unknown> {
  const ssh = config.executables["ssh"] ?? "ssh";
  const result = await execute(
    ssh,
    [
      host.ssh!,
      host.cliPath!,
      "runtime-reset-host",
      "--config",
      host.configPath,
      ...args,
      "--json",
    ],
    { windowsHide: true, maxBuffer: 4 * 1024 * 1024, timeout: 180_000 },
  );
  const value = JSON.parse(result.stdout);
  requireThat(
    value?.ok,
    value?.code ?? "remote_reset_failed",
    "Remote runtime reset phase failed.",
  );
  return value.data;
}

export async function runtimeResetHost(
  config: Host.HostConfig,
  configPath: string,
  distributionRoot: string,
  phase:
    | "preview"
    | "prepare"
    | "clear"
    | "complete"
    | "bootstrap"
    | "release"
    | "confirm",
  resetId?: string,
  runtimeEpoch?: string,
  scopeRoots?: string,
  publisherCredentialDigest?: string,
): Promise<unknown> {
  const bundles =
    phase === "preview" || phase === "clear"
      ? await resetUiBundles(distributionRoot, resetConfiguration(config).uiIds)
      : [];
  const bootstrapContracts = resetBootstrapContracts(bundles);
  let plan = await localRuntimeResetPlan(config, configPath, distributionRoot);
  if (phase === "preview") return plan;
  requireThat(
    resetId && runtimeEpoch,
    "invalid_arguments",
    "Mutating runtime reset phases require reset and epoch identities.",
  );
  if (phase === "prepare") {
    requireThat(
      scopeRoots && publisherCredentialDigest,
      "invalid_arguments",
      "Reset prepare requires coordinated scope roots and ui publisher identity.",
    );
    plan = { ...plan, scopeRootIds: decodeScopeRoots(scopeRoots) };
  }
  if (phase === "prepare")
    return prepareLocal(
      config,
      plan,
      resetId,
      runtimeEpoch,
      publisherCredentialDigest!,
    );
  if (phase === "clear")
    return clearLocal(config, resetId, runtimeEpoch, bootstrapContracts);
  if (phase === "complete") return completeLocal(config, resetId, runtimeEpoch);
  if (phase === "bootstrap")
    return bootstrapLocal(config, resetId, runtimeEpoch);
  if (phase === "release") return releaseLocal(config, resetId, runtimeEpoch);
  return confirmLocal(config, resetId, runtimeEpoch);
}

export async function runtimeReset(
  config: Host.HostConfig,
  configPath: string,
  distributionRoot: string,
  apply: boolean,
  confirmation?: string,
): Promise<unknown> {
  const selected = deployment(config),
    reset = resetConfiguration(config),
    local = selected.hosts.find((host) => host.hostId === config.hostId)!;
  requireThat(
    reset.coordinatorHostId === config.hostId,
    "access_denied",
    "Complete runtime reset must run on the configured coordinator host.",
  );
  const bundles = await resetUiBundles(distributionRoot, reset.uiIds),
    credential = config.instances.find(
      (instance) => instance.credential,
    )?.credential;
  resetBootstrapContracts(bundles);
  requireThat(
    credential,
    "unauthenticated",
    "Runtime reset needs one configured Hive credential to restore uis.",
  );
  const publisherCredentialDigest = digest(credential);
  const previews = [];
  for (const host of selected.hosts)
    previews.push(
      host.hostId === config.hostId
        ? await localRuntimeResetPlan(config, configPath, distributionRoot)
        : await remote(config, host, ["--phase", "preview"]),
    );
  const plans = coordinatedRuntimeResetPlans(previews as RuntimeResetPlan[]),
    scopeRoots = encodeScopeRoots(plans[0]?.scopeRootIds ?? []);
  const uis = bundles.map((bundle) => ({
    uiId: bundle.definition.metadata.uiId,
    releaseId: bundle.checkedBundle.releaseId,
  }));
  if (!apply)
    return {
      schemaVersion: 1,
      mode: "preview",
      deploymentIdentity: selected.identity,
      hosts: plans,
      uis,
    };
  requireThat(
    confirmation === selected.identity,
    "invalid_arguments",
    "--confirm must exactly match the deployment identity.",
  );
  const localPlanHash = hashJson(
      plans.find((value: any) => value.hostId === config.hostId),
    ),
    previous = await jsonFile<ResetRecord>(recordPath(config)).catch(
      () => null,
    ),
    retained = retainedResetIdentity(previous, localPlanHash);
  const resetId = retained?.resetId ?? randomUUID(),
    runtimeEpoch = retained?.runtimeEpoch ?? randomUUID(),
    completed: string[] = [];
  const invoke = async (
    host: typeof local,
    phase:
      "prepare" | "clear" | "complete" | "bootstrap" | "release" | "confirm",
  ) =>
    host.hostId === config.hostId
      ? runtimeResetHost(
          config,
          configPath,
          distributionRoot,
          phase,
          resetId,
          runtimeEpoch,
          phase === "prepare" ? scopeRoots : undefined,
          phase === "prepare" ? publisherCredentialDigest : undefined,
        )
      : remote(config, host, [
          "--phase",
          phase,
          "--reset-id",
          resetId,
          "--runtime-epoch",
          runtimeEpoch,
          ...(phase === "prepare"
            ? [
                "--scope-roots",
                scopeRoots,
                "--publisher-digest",
                publisherCredentialDigest,
              ]
            : []),
        ]);
  const ordered = [
    local,
    ...selected.hosts.filter((host) => host.hostId !== config.hostId),
  ];
  const restoreUis = async () => {
    const client = new HiveClient(config.publicBaseUrl, { credential }),
      mutationId = operationId(
        runtimeEpoch,
        Date.now(),
        "runtime-reset-uis-" + resetId,
      ),
      deadline = Date.now() + 90_000;
    let failure: unknown;
    do {
      try {
        await republishResetUis(client, bundles, mutationId);
        return;
      } catch (error) {
        failure = error;
        const code = IvyError.from(error).code;
        if (
          ![
            "outcome_unknown",
            "service_unavailable",
            "service_not_ready",
            "contract_not_found",
          ].includes(code)
        )
          throw error;
        await new Promise((resolve) => setTimeout(resolve, 500));
      }
    } while (Date.now() < deadline);
    throw failure;
  };
  if (retained?.resumeFromReleased) {
    for (const host of ordered) await invoke(host, "prepare");
    for (const host of selected.hosts) await invoke(host, "complete");
  }
  if (!retained?.resumeFromReleased) {
    for (const host of ordered) await invoke(host, "prepare");
    for (const host of selected.hosts) {
      await invoke(host, "clear");
      completed.push(host.hostId);
    }
    for (const host of selected.hosts) await invoke(host, "complete");
  } else {
    completed.push(...selected.hosts.map((host) => host.hostId));
  }
  await bootstrapResetUis(
    selected.hosts,
    (host) => invoke(host, "bootstrap"),
    (host) => invoke(host, "prepare"),
    restoreUis,
  );
  const releaseOrder = [
    ...selected.hosts.filter((host) => host.hostId !== config.hostId),
    local,
  ];
  await releaseResetHosts(
    releaseOrder,
    (host) => invoke(host, "release"),
    (host) => invoke(host, "prepare"),
  );
  for (const host of selected.hosts.filter(
    (host) => host.hostId !== config.hostId,
  ))
    await invoke(host, "confirm");
  await invoke(local, "confirm");
  return {
    schemaVersion: 1,
    mode: "applied",
    deploymentIdentity: selected.identity,
    resetId,
    runtimeEpoch,
    completedHosts: completed,
  };
}
