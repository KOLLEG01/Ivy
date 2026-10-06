import { join, resolve } from "node:path";
import { mkdir } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import { HostJournal, ExecutorLock } from "./journal.js";
import { atomicJson, inside, jsonFile, instanceConfig } from "./config.js";
import { servicePaths } from "./layout.js";
import { checkHealth } from "./health.js";
import { runCommand, startProcess } from "./process.js";
import type { ProcessResult, RunningProcess } from "./process.js";
import { IvyError, requireThat } from "../../contracts/src/errors.js";
import type { Host } from "../../contracts/src/generated.js";
import { verifyLaunchCandidate } from "./artifact.js";
import { requireRuntimeRequirements } from "./package-requirements.js";
import { serviceLog } from "./service-log.js";
import { pointerParts } from "../../contracts/src/schema.js";

const now = () => new Date().toISOString();
const safeMessage = (message: string) => message.slice(0, 4096);
const processCode = (result: ProcessResult): string => {
  let code =
    result.errorCode ??
    (result.exitCode === 0 ? "process_stopped" : "process_exited");
  for (const line of result.stderr.trim().split(/\r?\n/).slice(-4)) {
    try {
      const value = JSON.parse(line) as { code?: unknown };
      if (
        typeof value.code === "string" &&
        /^[a-z][a-z0-9_]{0,127}$/.test(value.code)
      )
        code = value.code;
    } catch {
      /* Child output is never persisted as a diagnostic. */
    }
  }
  return code;
};
const directHealthProbe = (
  command: NonNullable<Host.BuildPlan["readiness"]>["command"],
): boolean =>
  command.executable === "node" &&
  command.args.length === 1 &&
  /^dist\/(?:packages\/host-runtime\/src|services\/[a-z][a-z0-9-]*\/src)\/health\.js$/.test(
    command.args[0] ?? "",
  );

const selectedSecret = (
  settings: Host.Instance["settings"],
  pointer: string,
): unknown => {
  let value: unknown = { settings };
  for (const part of pointerParts(pointer)) {
    if (
      value === null ||
      typeof value !== "object" ||
      Array.isArray(value) ||
      !Object.hasOwn(value, part)
    )
      return undefined;
    value = (value as Record<string, unknown>)[part];
  }
  return value;
};

const secretStrings = (value: unknown): string[] => {
  if (typeof value === "string") return value ? [value] : [];
  if (Array.isArray(value)) return value.flatMap(secretStrings);
  if (value && typeof value === "object")
    return Object.values(value).flatMap(secretStrings);
  return [];
};

/**
 * The single local owner for one configured runtime target.
 *
 * Deployment accepts and records targets; this small loop turns one Windows
 * process target into one child process, checks readiness and applies the
 * declared restart policy. Docker and Linux systemd have their own owners and
 * never enter this class.
 */
export class RuntimeOwner {
  private readonly lock: ExecutorLock;
  private readonly journal: HostJournal;
  private readonly bootId = randomUUID();
  private readonly controller = new AbortController();
  private task: Promise<void> | null = null;
  private child: RunningProcess | null = null;
  private childResult: ProcessResult | null = null;
  private active: Host.RuntimeTarget | null = null;
  private config: Host.InstanceConfig | null = null;
  private manifest: ReturnType<HostJournal["manifest"]> | null = null;
  private restarts = 0;
  private nextRestart = 0;
  private startedAt = 0;
  private unreadySince = 0;
  private lastReady = false;
  private checkAt = 0;
  private restartAllowed = true;
  private restartCode = "restart_backoff";
  private blocked = false;
  private closed = false;
  private lastObservation = "";

  get host(): Host.HostConfig {
    return this.journal.config;
  }
  get completion(): Promise<void> {
    return this.task ?? Promise.resolve();
  }

  constructor(
    host: Host.HostConfig,
    readonly instanceId: string,
    readonly bootstrapRoot: string,
  ) {
    requireThat(
      process.platform === "win32",
      "unsupported_runtime",
      "RuntimeOwner is reserved for Windows process services; Linux uses systemd directly.",
    );
    requireThat(
      host.instances.find((value) => value.instanceId === instanceId)
        ?.engine === "process",
      "unsupported_runtime",
      "RuntimeOwner cannot own Docker instances.",
    );
    this.lock = new ExecutorLock(join(host.runtimeRoot, "owners", instanceId));
    try {
      this.journal = new HostJournal(host);
      this.journal.instance(instanceId);
      const previous = this.journal.observations()[instanceId];
      // Reconstructing an owner inside the same manager must retain uncertainty.
      // A replacement ServiceManager can acquire this exact OS lock only after
      // the prior parent (and its kill-on-close Windows jobs) has ended.
      this.blocked =
        previous?.state === "unknown" &&
        previous.code === "outcome_unknown" &&
        previous.ownerPid === process.pid;
    } catch (error) {
      this.lock.close();
      throw error;
    }
  }

  start(): void {
    if (!this.task) this.task = this.run();
  }

  async close(): Promise<void> {
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

  private observe(
    state: Host.RuntimeObservation["state"],
    code: string | null = null,
    message = "",
    health: Host.Health | null = null,
  ): void {
    const target = this.active;
    const value: Host.RuntimeObservation = {
      schemaVersion: 1,
      instanceId: this.instanceId,
      ownerPid: process.pid,
      ownerBootId: this.bootId,
      observedAt: now(),
      targetRevision: target?.revision ?? null,
      candidateId: target?.candidateId ?? null,
      buildId: target === this.active ? (this.config?.buildId ?? null) : null,
      state,
      health,
      restartCount: this.restarts,
      nextRestartAt: this.nextRestart
        ? new Date(this.nextRestart).toISOString()
        : null,
      code,
      message: safeMessage(message),
    };
    // Current readiness comes directly from health.json. Persist only owner state
    // changes; timestamp-only health pulses are not a journal event.
    const semantic = JSON.stringify({
      ...value,
      observedAt: "",
      health: health ? { ...health, observedAt: "" } : null,
    });
    if (semantic === this.lastObservation) return;
    this.lastObservation = semantic;
    this.journal.recordObservation(this.instanceId, value);
  }

  private async stopChild(): Promise<void> {
    const dataRoot =
      this.config?.dataRoot ??
      servicePaths(this.host, this.journal.instance(this.instanceId)).data;
    if (!this.child) return;
    this.observe("draining");
    const timeout = this.manifest?.shutdown?.timeoutMs ?? 15_000;
    try {
      const health = await jsonFile<Host.Health>(join(dataRoot, "health.json"));
      if (health.launchId && health.launchId === this.active?.revision) {
        const control: Host.RuntimeControl = {
          schemaVersion: 1,
          instanceId: this.instanceId,
          launchId: health.launchId,
          bootId: health.bootId,
          action: "shutdown",
          requestedAt: now(),
        };
        await atomicJson(join(dataRoot, "control.json"), control);
      }
    } catch {
      /* A bounded process stop remains authoritative when a child is unresponsive. */
    }
    const result = await this.child.stop(timeout);
    this.child = null;
    this.childResult = null;
    if (result.errorCode === "outcome_unknown") {
      this.blocked = true;
      this.observe(
        "unknown",
        "outcome_unknown",
        "The owned process tree did not establish a stopped outcome; no replacement will start.",
      );
      throw new IvyError(
        "outcome_unknown",
        "The owned process tree did not establish a stopped outcome.",
        "unknown",
      );
    }
    this.observe("stopped", null, "Owned process stopped.");
  }

  private async select(target: Host.RuntimeTarget): Promise<void> {
    requireThat(
      inside(this.host.runtimeRoot, target.configPath),
      "target_conflict",
      "Instance configuration leaves host runtime data.",
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
      config.instanceId === this.instanceId &&
        config.hostId === this.host.hostId,
      "target_conflict",
      "Runtime target has no matching local identity.",
    );
    requireThat(
      config.artifactRoot ===
        this.journal.candidate(target.candidateId).artifactRoot &&
        config.dataRoot ===
          servicePaths(this.host, this.journal.instance(this.instanceId)).data,
      "build_mismatch",
      "Runtime target identity does not match its accepted candidate or service storage.",
    );
    const manifest = this.journal.manifest(target.candidateId);
    requireThat(
      !manifest.connectsToHive || config.credential,
      "invalid_arguments",
      "A Hive service requires its own credential.",
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

  private async launch(): Promise<void> {
    const target = this.active!,
      manifest = this.manifest!;
    const selected = this.config!,
      refreshed = await instanceConfig(target.configPath);
    requireThat(
      refreshed.instanceId === this.instanceId &&
        refreshed.hostId === this.host.hostId &&
        refreshed.componentId === selected.componentId &&
        refreshed.serviceNodeId === selected.serviceNodeId &&
        refreshed.artifactRoot === selected.artifactRoot &&
        refreshed.buildId === selected.buildId &&
        refreshed.dataRoot === selected.dataRoot,
      "configuration_changed",
      "Reloaded instance configuration no longer belongs to the selected release and storage owner.",
    );
    this.config = refreshed;
    const config = this.config!;
    this.observe(
      "starting",
      "artifact_verification",
      "Checking the selected release stamp and entrypoint.",
    );
    requireRuntimeRequirements(manifest.requirements);
    await verifyLaunchCandidate(
      this.journal.candidate(target.candidateId),
      this.host,
      manifest.entrypoint?.args[0],
    );
    this.observe("starting");
    this.childResult = null;
    this.startedAt = Date.now();
    this.unreadySince = this.startedAt;
    this.checkAt = 0;
    const instance = this.journal.instance(this.instanceId),
      entrypoint = manifest.entrypoint!;
    const workRoot =
      config.workRoot &&
      entrypoint.executable === "node" &&
      entrypoint.args[0]?.startsWith("dist/")
        ? config.workRoot
        : undefined;
    if (workRoot) await mkdir(workRoot, { recursive: true });
    const command = workRoot
      ? {
          ...entrypoint,
          args: [
            resolve(config.artifactRoot, entrypoint.args[0]!),
            ...entrypoint.args.slice(1),
          ],
        }
      : entrypoint;
    this.child = await startProcess(
      command,
      config.artifactRoot,
      this.host.executables,
      {
        environment: {
          IVY_INSTANCE_CONFIG: target.configPath,
          IVY_LAUNCH_ID: target.revision,
        },
        allowWindowsBreakaway: instance.process?.allowWindowsBreakaway ?? false,
        ...(workRoot ? { executionCwd: workRoot } : {}),
        ...(config.logsRoot ? { onOutput: serviceLog(config.logsRoot) } : {}),
        jobLauncher: join(this.bootstrapRoot, "dist/native/ivy-job.exe"),
        redact: this.secrets(),
      },
    );
    const owned = this.child;
    void owned.completion.then((result) => {
      if (this.child === owned) this.childResult = result;
    });
  }

  private secrets(): string[] {
    const configured = this.host.instances.flatMap((instance) => [
      ...(instance.credential ? [instance.credential] : []),
      ...(instance.secretPaths ?? []).flatMap((path) =>
        secretStrings(selectedSecret(instance.settings, path)),
      ),
    ]);
    if (this.config) {
      const instance = this.host.instances.find(
        (value) => value.instanceId === this.instanceId,
      );
      configured.push(
        ...(this.config.credential ? [this.config.credential] : []),
      );
      if (instance)
        configured.push(
          ...(instance.secretPaths ?? []).flatMap((path) =>
            secretStrings(selectedSecret(this.config!.settings, path)),
          ),
        );
    }
    return [...new Set(configured)].filter(Boolean);
  }

  private backoff(code: string): void {
    this.restartCode = code;
    this.lastReady = false;
    const policy = this.manifest?.restart;
    this.restarts++;
    this.nextRestart =
      Date.now() +
      Math.min(
        policy?.maximumDelayMs ?? 60_000,
        (policy?.minimumDelayMs ?? 1000) * 2 ** Math.min(this.restarts - 1, 16),
      );
    this.observe(
      "failed",
      code,
      "Readiness or process exit failed; restart backoff is active.",
    );
  }

  private async tick(): Promise<void> {
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
        "Runtime ownership is blocked until the last process outcome is reconciled.",
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
      "The running target has no accepted instance configuration.",
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
          "The owned process outcome is unknown; no second launch will be attempted.",
        );
        return;
      }
      const failed = result.exitCode !== 0 || result.errorCode !== null;
      const code = processCode(result);
      if (
        this.manifest.restart!.policy === "never" ||
        (this.manifest.restart!.policy === "on-failure" && !failed)
      ) {
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
    let health: Host.Health | null = null;
    try {
      const instance = this.journal.instance(this.instanceId);
      const endpoint = undefined;
      try {
        health = await checkHealth(this.config, endpoint, !this.lastReady);
      } catch (error) {
        // Health is a read-only dependency check. A lost/ambiguous Hive reply
        // must not be mistaken for an ambiguous process stop; keep this owner
        // alive and retry without launching or stopping another process.
        const failure = IvyError.from(error);
        if (failure.code === "outcome_unknown") {
          if (!this.unreadySince) this.unreadySince = Date.now();
          this.observe(
            "starting",
            "service_not_ready",
            "Health dependency outcome is not yet observable.",
          );
          return;
        }
        throw error;
      }
      requireThat(
        health.launchId === target.revision,
        "service_not_ready",
        "Health belongs to a previous launch.",
      );
      if (!this.lastReady && Date.now() >= this.checkAt) {
        await runCommand(
          this.manifest.readiness!.command,
          this.config.artifactRoot,
          this.host.executables,
          {
            environment: {
              IVY_INSTANCE_CONFIG: target.configPath,
              IVY_LAUNCH_ID: target.revision,
              ...(endpoint ? { IVY_HIVE_HEALTH_URL: endpoint } : {}),
            },
            jobLauncher: join(this.bootstrapRoot, "dist/native/ivy-job.exe"),
            redact: this.secrets(),
            useJobLauncher: !directHealthProbe(
              this.manifest.readiness!.command,
            ),
          },
        );
        this.lastReady = true;
        this.checkAt = Number.POSITIVE_INFINITY;
      }
      requireThat(
        this.lastReady,
        "service_not_ready",
        "Manifest readiness has not passed.",
      );
      this.unreadySince = 0;
      if (Date.now() - this.startedAt >= 30_000) this.restarts = 0;
      this.observe("ready", null, "", health);
    } catch (error) {
      const failure = IvyError.from(error);
      if (failure.code === "outcome_unknown") {
        this.blocked = true;
        this.observe("unknown", failure.code, failure.message, health);
        return;
      }
      if (!this.unreadySince) this.unreadySince = Date.now();
      if (Date.now() - this.unreadySince > this.manifest.readiness!.timeoutMs) {
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
          health,
        );
    }
  }

  private async run(): Promise<void> {
    while (!this.controller.signal.aborted) {
      try {
        await this.tick();
      } catch (error) {
        const failure = IvyError.from(error);
        this.observe(
          failure.code === "outcome_unknown" ? "unknown" : "failed",
          failure.code,
          safeMessage(failure.message),
        );
        if (failure.code === "outcome_unknown") this.blocked = true;
      }
      try {
        await delay(500, undefined, { signal: this.controller.signal });
      } catch {
        break;
      }
    }
  }
}
