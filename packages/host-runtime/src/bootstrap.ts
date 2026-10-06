import { parseArgs } from "node:util";
import { fileURLToPath } from "node:url";
import { join, resolve } from "node:path";
import { readFile, writeFile, readdir, lstat, unlink } from "node:fs/promises";
import { promisify } from "node:util";
import { execFile } from "node:child_process";
import { hostConfig } from "./host-config.js";
import { bootstrapLaunchId, HostJournal, ExecutorLock } from "./journal.js";
import { verifyCandidate } from "./artifact.js";
import { atomicJson, instanceConfig, jsonFile } from "./config.js";
import { exists, privateDirectory } from "./backup-files.js";
import { servicePaths, agentManagerAccountSettings, hostConfigurationPath } from "./layout.js";
import {
  installationIdentity,
  linuxResourceScope,
  validateLinuxResourceScope,
  ensureLinuxSlice,
  inspectLinuxSlice,
} from "./linux-resources.js";
import { runtimeEnvironment } from "./process.js";
import { hashJson } from "../../contracts/src/canonical.js";
import { IvyError, requireThat } from "../../contracts/src/errors.js";
import { validateHost } from "../../contracts/src/host-validation.js";
import type { Host } from "../../contracts/src/generated.js";
import {
  ensureLinuxProcessIdentity,
  linuxProcessUser,
} from "./linux-service-identity.js";

const exec = promisify(execFile);
export type BootstrapComponent = {
  candidate: Host.Candidate;
  manifest: Host.ReleaseManifest;
};
export function configurationBootstrapPlan(
  config: Host.HostConfig,
  path: string,
  candidate: Host.Candidate,
  resources?: Host.LinuxResourceBudget,
  components: readonly BootstrapComponent[] = [],
): Host.BootstrapPlan {
  const installationId = installationIdentity(config);
  requireThat(
    !resources || candidate.platform.os === "linux",
    "unsupported_runtime",
    "A Linux resource budget requires a Linux bootstrap candidate.",
  );
  // Linux ServiceManager is an executor-owned systemd target. Windows keeps it
  // as an outer scheduled-task owner, so only Windows bootstraps both roots.
  const owned = config.instances.filter(
    (instance) =>
      instance.engine === "process" &&
      (instance.componentId === "host-executor" ||
        (candidate.platform.os === "win32" &&
          instance.componentId === "service-manager")) &&
      (instance.componentId === candidate.componentId ||
        components.some(
          (value) => value.manifest.componentId === instance.componentId,
        )),
  );
  const selected = (instance: Host.Instance): BootstrapComponent | undefined =>
    components.find(
      (value) => value.manifest.componentId === instance.componentId,
    );
  const plan: Host.BootstrapPlan = {
    schemaVersion: 1,
    os: candidate.platform.os,
    hostId: config.hostId,
    installationId,
    candidateId: candidate.candidateId,
    artifactRoot: candidate.artifactRoot,
    configPath: resolve(path),
    configHash: hashJson(config),
    runtimeRoot: config.runtimeRoot,
    nodeExecutable: config.executables["node"]!,
    processes: owned.map((instance) => {
      const value = selected(instance),
        user = linuxProcessUser(instance.componentId, candidate.platform.os);
      return {
        instanceId: instance.instanceId,
        componentId: instance.componentId,
        name: installationId + "-" + hashJson(instance.instanceId).slice(7, 19),
        ...(user ? { user } : {}),
        ...(value
          ? {
              candidateId: value.candidate.candidateId,
              artifactRoot: value.candidate.artifactRoot,
              entrypoint: value.manifest.entrypoint,
              ...(value.manifest.restart
                ? { restart: value.manifest.restart }
                : {}),
              configPath: join(
                config.runtimeRoot,
                "bootstrap",
                installationId,
                "instances",
                instance.instanceId + ".json",
              ),
            }
          : {}),
        ...(candidate.platform.os === "win32" &&
        instance.process?.allowWindowsBreakaway
          ? { allowWindowsBreakaway: true }
          : {}),
      };
    }),
    ...(resources
      ? { linuxResources: linuxResourceScope(installationId, resources) }
      : {}),
  };
  validateHost("BootstrapPlan", plan);
  return plan;
}
function candidateRows(
  journal: HostJournal,
  componentId: string,
): Host.Candidate[] {
  return journal.db
    .prepare(
      "SELECT candidate_json FROM candidates WHERE component_id=? ORDER BY candidate_id LIMIT 1000",
    )
    .all(componentId)
    .map((row) => JSON.parse(String(row["candidate_json"])) as Host.Candidate);
}
/** Select one explicit bootstrap release per process. Installed state wins; fresh hosts use the newest retained candidate. */
export function bootstrapComponents(
  config: Host.HostConfig,
  journal: HostJournal,
  hostCandidate: Host.Candidate,
  preferred: readonly BootstrapComponent[] = [],
): BootstrapComponent[] {
  const result: BootstrapComponent[] = [];
  const owned = config.instances.filter(
    (value) =>
      value.engine === "process" &&
      (value.componentId === "host-executor" ||
        (hostCandidate.platform.os === "win32" &&
          value.componentId === "service-manager")),
  );
  for (const instance of owned) {
    const installed = journal.installed(instance.instanceId);
    const selected = preferred.find(
      (value) => value.candidate.componentId === instance.componentId,
    );
    const candidate =
      instance.componentId === hostCandidate.componentId
        ? hostCandidate
        : (selected?.candidate ??
          (installed
            ? journal.candidate(installed.candidateId)
            : candidateRows(journal, instance.componentId).sort((a, b) =>
                b.createdAt.localeCompare(a.createdAt),
              )[0]));
    if (!candidate) {
      if (instance.enabled)
        requireThat(
          false,
          "not_found",
          `No retained bootstrap candidate exists for ${instance.componentId}.`,
        );
      continue;
    }
    result.push({
      candidate,
      manifest:
        selected?.candidate.candidateId === candidate.candidateId
          ? selected.manifest
          : journal.manifest(candidate.candidateId),
    });
  }
  return result;
}
export async function retainedBootstrapPlans(
  config: Host.HostConfig,
): Promise<Host.BootstrapPlan[]> {
  const selected = join(
      config.runtimeRoot,
      "bootstrap",
      installationIdentity(config) + ".json",
    ),
    root = join(config.runtimeRoot, "bootstrap-plans");
  const entries = await readdir(root, { withFileTypes: true }).catch(
    (error) => {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
      throw error;
    },
  );
  const acceptedName = /^[a-f0-9]{64}\.json$/,
    pendingName = /^[a-f0-9]{64}\.json\.[a-f0-9-]{36}\.tmp$/;
  requireThat(
    entries.length <= 1000 &&
      entries.every(
        (entry) =>
          entry.isFile() &&
          (acceptedName.test(entry.name) || pendingName.test(entry.name)),
      ),
    "storage_invalid",
    "Invalid retained bootstrap plan set.",
  );
  // A process may die before atomicJson publishes an accepted plan. Its temporary bytes have no
  // authority to describe an OS definition; retain them without interpreting them as a plan.
  const paths = [
      ...((await exists(selected)) ? [selected] : []),
      ...entries
        .filter((entry) => acceptedName.test(entry.name))
        .map((entry) => join(root, entry.name)),
    ],
    plans: Host.BootstrapPlan[] = [];
  for (const path of paths) {
    try {
      const metadata = await lstat(path);
      requireThat(
        metadata.isFile() &&
          !metadata.isSymbolicLink() &&
          metadata.size <= 65536,
        "storage_invalid",
        "Retained bootstrap plan is not a bounded regular file.",
      );
      const plan = await jsonFile<Host.BootstrapPlan>(path);
      validateHost("BootstrapPlan", plan);
      requireThat(
        path === selected ||
          path === join(root, hashJson(plan).slice(7) + ".json"),
        "storage_invalid",
        "Retained bootstrap plan hash changed.",
      );
      requireThat(
        path !== selected ||
          (plan.installationId === installationIdentity(config) &&
            plan.hostId === config.hostId &&
            plan.runtimeRoot === config.runtimeRoot),
        "storage_invalid",
        "Selected bootstrap plan belongs to a different installation.",
      );
      // Restored historical plans keep their original paths; they do not own this new installation.
      if (
        plan.installationId !== installationIdentity(config) ||
        plan.hostId !== config.hostId ||
        plan.runtimeRoot !== config.runtimeRoot
      )
        continue;
      if (plan.linuxResources)
        validateLinuxResourceScope(plan.linuxResources, plan.installationId);
      plans.push(plan);
    } catch (error) {
      // Bootstrap plans from a previous contract are not a migration source or an
      // execution authority. Ignore only those stale retained documents; the next
      // plan is reconstructed from the current host configuration and candidates.
      const failure = IvyError.from(error);
      if (
        ["invalid_arguments", "storage_invalid", "artifact_changed"].includes(
          failure.code,
        )
      )
        continue;
      throw error;
    }
  }
  // The selected plan is also retained by hash. It is one authority, not two
  // different possible owners for the same OS definition.
  return [...new Map(plans.map(plan => [hashJson(plan), plan])).values()];
}
export async function retainBootstrapPlan(
  plan: Host.BootstrapPlan,
): Promise<void> {
  validateHost("BootstrapPlan", plan);
  if (plan.linuxResources)
    validateLinuxResourceScope(plan.linuxResources, plan.installationId);
  const root = join(plan.runtimeRoot, "bootstrap-plans"),
    path = join(root, hashJson(plan).slice(7) + ".json");
  if (!(await exists(root))) await privateDirectory(root);
  if (await exists(path))
    requireThat(
      hashJson(await jsonFile(path)) === hashJson(plan),
      "storage_invalid",
      "Retained bootstrap plan changed.",
    );
  else await atomicJson(path, plan);
}
export async function bootstrapPlan(
  path: string,
  candidateId: string,
  resources?: Host.LinuxResourceBudget,
  preferred: readonly BootstrapComponent[] = [],
): Promise<Host.BootstrapPlan> {
  const config = await hostConfig(resolve(path)),
    journal = new HostJournal(config);
  try {
    const candidate = journal.candidate(candidateId),
      manifest = await verifyCandidate(candidate, config);
    const selected = join(
      config.runtimeRoot,
      "bootstrap",
      installationIdentity(config) + ".json",
    );
    // Windows does not inherit a resource scope from the old scheduler plan. On
    // Linux a retained plan may carry the administrator's existing slice budget.
    // Do not let a pre-simplification plan become part of the new contract.
    if (
      !resources &&
      process.platform === "linux" &&
      (await exists(selected))
    ) {
      const prior = (await retainedBootstrapPlans(config))[0];
      resources = prior?.linuxResources?.budget;
    }
    return configurationBootstrapPlan(
      config,
      path,
      candidate,
      resources,
      bootstrapComponents(config, journal, candidate, [
        { candidate, manifest },
        ...preferred,
      ]),
    );
  } finally {
    journal.close();
  }
}

function bootstrapComponentsFromPlan(
  journal: HostJournal,
  plan: Host.BootstrapPlan,
): BootstrapComponent[] {
  return plan.processes
    .filter(
      (process) =>
        process.componentId !== "host-executor" && process.candidateId,
    )
    .map((process) => {
      const candidate = journal.candidate(process.candidateId!);
      return { candidate, manifest: journal.manifest(candidate.candidateId) };
    });
}
const unitArg = (value: string) => {
  requireThat(
    !/[\x00-\x1f]/.test(value),
    "invalid_arguments",
    "Unit arguments cannot contain control characters.",
  );
  return (
    '"' +
    value
      .replaceAll("\\", "\\\\")
      .replaceAll('"', '\\"')
      .replaceAll("%", "%%") +
    '"'
  );
};
export function linuxUnit(
  plan: Host.BootstrapPlan,
  instance: Host.BootstrapPlan["processes"][number],
  launchId = bootstrapLaunchId(plan, instance.instanceId),
): string {
  if (plan.linuxResources)
    validateLinuxResourceScope(plan.linuxResources, plan.installationId);
  // Retained plans use canonical JSON, which reorders object keys. Preserve the original generated
  // argv spelling independently of property insertion order so loading a plan cannot change a unit.
  const resources = plan.linuxResources
    ? {
        slice: plan.linuxResources.slice,
        budget: {
          memoryHighBytes: plan.linuxResources.budget.memoryHighBytes,
          memoryMaxBytes: plan.linuxResources.budget.memoryMaxBytes,
          tasksMax: plan.linuxResources.budget.tasksMax,
        },
      }
    : undefined;
  const artifactRoot = instance.artifactRoot ?? plan.artifactRoot;
  const executable =
    instance.entrypoint?.executable === "node" || !instance.entrypoint
      ? plan.nodeExecutable
      : instance.entrypoint.executable;
  const fallback =
    instance.componentId === "host-executor"
      ? "dist/packages/host-runtime/src/executor.js"
      : `dist/services/${instance.componentId}/src/main.js`;
  const entrypoint = instance.entrypoint ?? {
    executable: "node",
    args: [fallback],
    timeoutMs: 30_000,
  };
  const configPath =
    instance.componentId === "host-executor"
      ? plan.configPath
      : (instance.configPath ?? plan.configPath);
  const ownConfig =
    instance.componentId === "host-executor" && instance.configPath
      ? ["--instance-config", instance.configPath]
      : [];
  const argv = [
    executable,
    ...entrypoint.args,
    "--config",
    configPath,
    ...ownConfig,
  ]
    .map(unitArg)
    .join(" ");
  // systemd's path directive preserves quotes literally; only ExecStart parses an argv list.
  requireThat(
    artifactRoot.startsWith("/") &&
      artifactRoot.trim() === artifactRoot &&
      !/[\x00-\x1f]/.test(artifactRoot) &&
      !artifactRoot.endsWith("\\"),
    "invalid_arguments",
    "Invalid systemd working directory.",
  );
  const directory = artifactRoot.replaceAll("%", "%%");
  const restart =
    instance.restart?.policy === "never"
      ? "no"
      : (instance.restart?.policy ?? "always");
  const restartDelay = Math.max(
    1,
    Math.ceil((instance.restart?.minimumDelayMs ?? 2000) / 1000),
  );
  return `[Unit]\nDescription=IvyNext ${instance.name}\nAfter=network.target\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\n${instance.user ? `User=${instance.user}\nGroup=${instance.user}\n` : ""}ExecStart=:${argv}\nWorkingDirectory=${directory}\nRestart=${restart}\nRestartSec=${restartDelay}\nKillMode=control-group\nTimeoutStopSec=630\nUMask=0077\nNoNewPrivileges=true\nEnvironment=NODE_NO_WARNINGS=1\nEnvironment=IVY_LAUNCH_ID=${launchId}\n${instance.configPath ? "Environment=IVY_INSTANCE_CONFIG=" + unitArg(instance.configPath) + "\n" : ""}${plan.linuxResources ? "Slice=" + plan.linuxResources.slice + "\n" : ""}\n[Install]\nWantedBy=multi-user.target\n`;
}
type BootstrapInstallOptions = { replaceLegacy?: boolean };

function legacySystemdUnit(content: string): boolean {
  return (
    content.startsWith("[Unit]\nDescription=IvyNext ") &&
    content.includes("guardian.js") &&
    content.includes("Environment=")
  );
}

function directSystemdUnit(content: string): boolean {
  return (
    content.startsWith("[Unit]\nDescription=IvyNext ") &&
    content.includes("ExecStart=:") &&
    content.includes("WorkingDirectory=") &&
    content.includes("Environment=NODE_NO_WARNINGS=1\n") &&
    !content.includes("guardian.js")
  );
}
/** Current Ivy-owned units can be replaced during every update; only adopting the removed guardian form needs explicit legacy authority. */
export function replaceableSystemdOwner(
  content: string,
  allowLegacy = false,
): boolean {
  return (
    directSystemdUnit(content) || (allowLegacy && legacySystemdUnit(content))
  );
}

async function stopSystemdOwner(name: string, path: string): Promise<void> {
  await exec("/usr/bin/systemctl", ["disable", "--now", name], {
    timeout: 30_000,
    maxBuffer: 65536,
    env: runtimeEnvironment(),
  });
  const deadline = Date.now() + 30_000;
  while (true) {
    const state = await exec(
      "/usr/bin/systemctl",
      ["show", "--no-pager", "--property=ActiveState,SubState", name],
      { timeout: 30_000, maxBuffer: 65536, env: runtimeEnvironment() },
    );
    const values = Object.fromEntries(
      state.stdout
        .trim()
        .split("\n")
        .map((line) => {
          const index = line.indexOf("=");
          return [line.slice(0, index), line.slice(index + 1)];
        }),
    );
    if (
      ["inactive", "failed"].includes(values["ActiveState"] ?? "") &&
      ["dead", "failed"].includes(values["SubState"] ?? "")
    )
      break;
    if (Date.now() >= deadline)
      throw new IvyError(
        "outcome_unknown",
        "Existing systemd owner did not establish a stopped outcome.",
        "unknown",
      );
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  await unlink(path);
}

async function replaceSystemdOwners(
  plan: Host.BootstrapPlan,
  allowLegacy: boolean,
): Promise<void> {
  const escaped = plan.installationId.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const pattern = new RegExp("^" + escaped + "-[0-9a-f]{12}\\.service$");
  const entries = await readdir("/etc/systemd/system", { withFileTypes: true });
  const desired = new Map(
    plan.processes.map((instance) => [
      instance.name + ".service",
      linuxUnit(plan, instance),
    ]),
  );
  const owned = entries.filter(
    (entry) => entry.isFile() && pattern.test(entry.name),
  );
  const current = new Map<string, { path: string; content: string }>();
  let replacementNeeded = false;
  for (const entry of owned) {
    const name = entry.name,
      path = join("/etc/systemd/system", name),
      content = await readFile(path, "utf8");
    current.set(name, { path, content });
    const expected = desired.get(name);
    if (expected !== undefined && expected === content) continue;
    requireThat(
      replaceableSystemdOwner(content, allowLegacy),
      "target_conflict",
      "Existing systemd owner is not an explicitly recognized Ivy unit.",
    );
    replacementNeeded = true;
  }
  if (!replacementNeeded) return;
  for (const [name, owner] of current) await stopSystemdOwner(name, owner.path);
}

export async function installBootstrap(
  plan: Host.BootstrapPlan,
  bootstrapRoot: string,
  options: BootstrapInstallOptions = {},
): Promise<Host.CliOutput> {
  validateHost("BootstrapPlan", plan);
  const config = await hostConfig(plan.configPath);
  const lock = new ExecutorLock(
    join(config.runtimeRoot, "bootstrap-maintenance-lock"),
  );
  try {
    return await installOwnedBootstrap(plan, config, bootstrapRoot, options);
  } finally {
    lock.close();
  }
}
async function installOwnedBootstrap(
  plan: Host.BootstrapPlan,
  config: Host.HostConfig,
  bootstrapRoot: string,
  options: BootstrapInstallOptions,
): Promise<Host.CliOutput> {
  requireThat(
    hashJson(config) === plan.configHash,
    "configuration_changed",
    "Host configuration changed after the reviewed installation plan.",
  );
  const path = join(
    plan.runtimeRoot,
    "bootstrap",
    plan.installationId + ".json",
  );
  const journal = new HostJournal(config);
  try {
    const preferred = bootstrapComponentsFromPlan(journal, plan);
    requireThat(
      hashJson(plan) ===
        hashJson(
          await bootstrapPlan(
            plan.configPath,
            plan.candidateId,
            plan.linuxResources?.budget,
            preferred,
          ),
        ),
      "target_conflict",
      "Installation plan differs from the verified candidate and host configuration.",
    );
    await retainBootstrapPlan(plan);
    await atomicJson(path, plan);
    await publishBootstrapInstanceConfigurations(plan, config, journal);
    if (process.platform === "linux")
      for (const process of plan.processes.filter(
        (value) => value.user && value.configPath,
      )) {
        const runtime = await instanceConfig(process.configPath!);
        await ensureLinuxProcessIdentity(
          {
            componentId: process.componentId,
            user: process.user!,
            configPath: process.configPath!,
            artifactRoot: process.artifactRoot ?? plan.artifactRoot,
            nodeExecutable: plan.nodeExecutable,
          },
          runtime,
        );
      }
  } finally {
    journal.close();
  }
  if (process.platform === "win32") {
    const powershell = join(
      process.env["SystemRoot"] ?? "C:\\Windows",
      "System32/WindowsPowerShell/v1.0/powershell.exe",
    );
    const result = await exec(
      powershell,
      [
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        join(
          bootstrapRoot,
          "dist/packages/host-runtime/assets/install-windows.ps1",
        ),
        "-PlanPath",
        path,
        ...(options.replaceLegacy ? ["-ReplaceLegacy"] : []),
      ],
      {
        windowsHide: true,
        env: runtimeEnvironment(),
        timeout: 60_000,
        maxBuffer: 65536,
      },
    );
    const output = JSON.parse(result.stdout) as Host.CliOutput;
    validateHost("CliOutput", output);
    const state = new HostJournal(config);
    try {
      state.syncBootstrap(plan, true);
    } finally {
      state.close();
    }
    return output;
  }
  requireThat(
    process.platform === "linux" && process.getuid?.() === 0,
    "unsupported_runtime",
    "Linux bootstrap installation needs the host systemd administrator.",
  );
  await replaceSystemdOwners(plan, Boolean(options.replaceLegacy));
  if (plan.linuxResources) await ensureLinuxSlice(plan.linuxResources);
  const names: string[] = [];
  for (const instance of plan.processes) {
    requireThat(
      instance.name.startsWith(plan.installationId + "-") &&
        /^ivy-next-[0-9a-f]{12}-[0-9a-f]{12}$/.test(instance.name),
      "target_conflict",
      "Unit name is outside this installation.",
    );
    const name = instance.name + ".service",
      path = join("/etc/systemd/system", name),
      content = linuxUnit(plan, instance);
    const old = await readFile(path, "utf8").catch((error) => {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
      throw error;
    });
    requireThat(
      old === null || old === content,
      "target_conflict",
      "Existing systemd unit differs from this immutable bootstrap.",
    );
    if (old === null)
      await writeFile(path, content, { flag: "wx", mode: 0o644 });
    names.push(name);
  }
  const state = new HostJournal(config);
  try {
    state.syncBootstrap(plan);
  } finally {
    state.close();
  }
  await exec("/usr/bin/systemctl", ["daemon-reload"], {
    timeout: 30_000,
    maxBuffer: 65536,
    env: runtimeEnvironment(),
  });
  if (plan.linuxResources) await inspectLinuxSlice(plan.linuxResources);
  for (const name of names)
    await exec("/usr/bin/systemctl", ["enable", "--now", name], {
      timeout: 30_000,
      maxBuffer: 65536,
      env: runtimeEnvironment(),
    });
  return {
    schemaVersion: 1,
    ok: true,
    code: "bootstrap_installed",
    data: { units: names },
  };
}
/** The OS definition and its protected launch JSON are one selected release.
 * Maintenance calls this only with every bootstrap owner stopped. */
export async function publishBootstrapInstanceConfigurations(
  plan: Host.BootstrapPlan,
  config: Host.HostConfig,
  journal: HostJournal,
): Promise<void> {
  for (const process of plan.processes) {
    if (!process.configPath) continue;
    const instance = journal.instance(process.instanceId),
      candidate = journal.candidate(process.candidateId ?? plan.candidateId),
      manifest = journal.manifest(candidate.candidateId);
    const paths = servicePaths(config, instance);
    const settings =
      instance.componentId === "agent-manager"
        ? agentManagerAccountSettings(instance.settings)
        : ["host-executor", "service-manager"].includes(instance.componentId)
          ? {
              ...instance.settings,
              hostConfigPath:
                instance.settings["hostConfigPath"] ??
                hostConfigurationPath(config),
            }
          : instance.settings;
    const value: Host.InstanceConfig = {
      schemaVersion: 1,
      instanceId: instance.instanceId,
      serviceNodeId: instance.serviceNodeId,
      componentId: instance.componentId,
      hostId: config.hostId,
      publicBaseUrl: config.publicBaseUrl,
      dataRoot: paths.data,
      workRoot: paths.work,
      logsRoot: paths.logs,
      artifactRoot: candidate.artifactRoot,
      buildId: candidate.buildId,
      version: manifest.version,
      ...(instance.credential ? { credential: instance.credential } : {}),
      settings,
    };
    validateHost("InstanceConfig", value);
    await atomicJson(process.configPath, value);
  }
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const args = parseArgs({
      options: {
        config: { type: "string" },
        candidate: { type: "string" },
        install: { type: "boolean" },
        "linux-resources": { type: "string" },
      },
      strict: true,
    });
    requireThat(
      args.values.config && args.values.candidate,
      "invalid_arguments",
      "Bootstrap needs --config PATH and --candidate ID.",
    );
    const resources = args.values["linux-resources"]
      ? await jsonFile<Host.LinuxResourceBudget>(args.values["linux-resources"])
      : undefined;
    const plan = await bootstrapPlan(
      args.values.config,
      args.values.candidate,
      resources,
    );
    const output: Host.CliOutput = args.values.install
      ? await installBootstrap(
          plan,
          fileURLToPath(new URL("../../../../", import.meta.url)),
        )
      : { schemaVersion: 1, ok: true, code: "bootstrap_plan", data: plan };
    process.stdout.write(JSON.stringify(output) + "\n");
  } catch (error) {
    const failure = IvyError.from(error);
    process.stdout.write(
      JSON.stringify({
        schemaVersion: 1,
        ok: false,
        code: failure.code,
        data: { message: failure.message },
      }) + "\n",
    );
    process.exitCode = 1;
  }
}
