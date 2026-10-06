import { randomUUID } from 'node:crypto';
import { AsyncLocalStorage } from 'node:async_hooks';
import { join, isAbsolute } from 'node:path';
import { fileURLToPath } from 'node:url';
import { statfs } from 'node:fs/promises';
import { setTimeout as delay } from 'node:timers/promises';
import { HostJournal, ExecutorLock, terminalPhases } from './journal.js';
import { hostConfig } from './host-config.js';
import { atomicJson, jsonFile, instanceConfig } from './config.js';
import { materializeTarget, stoppedTarget } from './target.js';
import { readAcceptedInstanceConfiguration } from './accepted-configuration.js';
import { defaultDockerAdapter } from './docker.js';
import type { DockerAdapter, ManagedProcess } from './docker.js';
import { defaultSystemdAdapter, renderSystemdUnit, systemdUnitName } from './systemd.js';
import type { SystemdAdapter, SystemdState } from './systemd.js';
import { verifyLaunchCandidate } from './artifact.js';
import { verifyServiceStorage } from './service-storage.js';
import { runtimeResetActive } from './runtime-maintenance.js';
import { runCommand, resolveCommand } from './process.js';
import { HealthFile, checkHealth, localHiveEndpoint } from './health.js';
import { HiveClient } from '../../sdk/src/client.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import { canonical } from '../../contracts/src/canonical.js';
import { installationIdentity, validateLinuxResourceScope } from './linux-resources.js';
import { requireRuntimeRequirements } from './package-requirements.js';
import { PackageUpdater } from './package-updater.js';
import { AutomaticBootstrapUpdater } from './bootstrap-automatic.js';
import { ConfigurationUpdater, configurationFailure } from './configuration-updater.js';
import { reconcileBootstrapConfigurationRestarts } from './bootstrap-config-restart.js';
import type { Host, Wire } from '../../contracts/src/generated.js';
import { ensureLinuxProcessIdentity, linuxProcessUser } from './linux-service-identity.js';
import { sourceBuildPlan } from './source.js';
import { collectHostStorage } from './storage-retention.js';
import { hostStorageSpace, RetentionSchedule } from './storage-space.js';

const now = () => new Date().toISOString();
const fresh = (value: Host.RuntimeObservation | undefined): value is Host.RuntimeObservation => Boolean(value && Date.now() - Date.parse(value.observedAt) < 15_000 && Date.parse(value.observedAt) <= Date.now() + 1000);
/** Start a new readiness-failure window at the first failed check, not at epoch. */
export function readinessFailureStart(previous: number, at = Date.now()): number { return previous || at; }
export function readinessExpired(start: number, timeoutMs: number, at = Date.now()): boolean { return start > 0 && at - start > timeoutMs; }
/** A target that already names the installed build can be drained directly. */
export function replacementNeedsSettledObservation(currentCandidateId: string | null, installedCandidateId: string | null): boolean {
  return currentCandidateId !== null && currentCandidateId !== installedCandidateId;
}
/** Idle without a target, or stopped at the same target, cannot launch after the observation ages. */
export function stoppedTargetIsConclusive(observed: Host.RuntimeObservation | undefined, target: Host.RuntimeTarget | null): boolean {
  return Boolean(observed && (target === null
    ? observed.state === 'idle' && observed.targetRevision === null && observed.candidateId === null && observed.buildId === null
    : target.desired === 'stopped' && observed.state === 'stopped' && observed.targetRevision === target.revision));
}
/** An unchanged ready target can be verified by the live Hive identity request even after its journal observation ages. */
export function runningHiveCanBeInspected(observed: Host.RuntimeObservation | undefined, target: Host.RuntimeTarget | null): boolean {
  return Boolean(observed && target?.desired === 'running' && observed.state === 'ready' && observed.targetRevision === target.revision &&
    observed.candidateId === target.candidateId);
}
type DockerHandle = {
  target: Host.RuntimeTarget;
  process: ManagedProcess;
  result: import('./process.js').ProcessResult | null;
  candidate: Host.Candidate;
  manifest: ReturnType<HostJournal['manifest']>;
  config: Host.InstanceConfig;
  startedAt: number;
  unreadySince: number;
  readinessChecked: boolean;
};
type SystemdHandle = {
  target: Host.RuntimeTarget;
  unit: string;
  candidate: Host.Candidate;
  manifest: ReturnType<HostJournal['manifest']>;
  config: Host.InstanceConfig;
  startedAt: number;
  unreadySince: number;
  readinessChecked: boolean;
};

/** Infrastructure replacements are queue barriers: dependants must finish first. */
export function parallelDeployments(queued: readonly Host.JournalEntry[], active: readonly Host.JournalEntry[], limit = 2,
  exclusive = (entry: Host.JournalEntry) => entry.record.phase === 'preparing' || ['hive', 'host-executor'].includes(entry.record.componentId)): Host.JournalEntry[] {
  if (active.some(exclusive)) return [];
  const selected: Host.JournalEntry[] = [], instances = new Set(active.map(entry => entry.record.instanceId));
  const running = new Set(active.map(entry => entry.record.deploymentId));
  for (const entry of queued) {
    if (running.has(entry.record.deploymentId)) continue;
    if (exclusive(entry)) {
      if (!active.length && !selected.length) selected.push(entry);
      break;
    }
    if (active.length + selected.length >= limit) break;
    if (instances.has(entry.record.instanceId)) continue;
    selected.push(entry); instances.add(entry.record.instanceId);
  }
  return selected;
}

/** Only this independently guarded process holds the activation lock. It never owns managed children. */
export class HostExecutor {
  private readonly lock: ExecutorLock;
  private readonly baseJournal: HostJournal;
  private readonly executionJournal = new AsyncLocalStorage<HostJournal>();
  get journal(): HostJournal { return this.executionJournal.getStore() ?? this.baseJournal; }
  private readonly controller = new AbortController();
  private readonly bootId = randomUUID();
  private readonly release = jsonFile<{ buildId: string }>(fileURLToPath(new URL('../../../build-info.json', import.meta.url)))
    .then(value => value.buildId).catch(() => null);
  private task: Promise<void> | null = null;
  private pulse: NodeJS.Timeout | null = null;
  private activeId: string | null = null;
  private closed = false;
  private readonly dockerHandles = new Map<string, DockerHandle>();
  private readonly systemdHandles = new Map<string, SystemdHandle>();
  private linuxResourcesLoaded = false;
  private linuxResources: Host.LinuxResourceScope | undefined;
  private readonly packageUpdater: PackageUpdater | null;
  private readonly bootstrapUpdater: AutomaticBootstrapUpdater | null;
  private packageCode: string | null = null;
  private readonly configurationUpdater: ConfigurationUpdater | null;
  private configurationCode: string | null = null;
  private configurationRevision: number | null = null;
  private configurationHash: string | null = null;
  private retentionTask: Promise<void> | null = null;
  private retentionCode: string | null = null;
  private storageCode: string | null = null;
  get config(): Host.HostConfig { return this.journal.config; }
  constructor(config: Host.HostConfig, readonly configPath: string, readonly bootstrapRoot: string, readonly ownInstanceId?: string,
    private readonly dockerAdapter: DockerAdapter = defaultDockerAdapter, private readonly systemdAdapter: SystemdAdapter = defaultSystemdAdapter,
    packageCredential?: string) {
    this.lock = new ExecutorLock(join(config.runtimeRoot, 'executor'));
    try { this.baseJournal = new HostJournal(config); }
    catch (error) { this.lock.close(); throw error; }
    const updateCredential = packageCredential ?? (ownInstanceId ? config.instances.find(value => value.instanceId === ownInstanceId)?.credential :
      config.instances.find(value => value.componentId === 'host-executor')?.credential);
    this.packageUpdater = config.packageUpdates && updateCredential ? new PackageUpdater(config, this.baseJournal, updateCredential) : null;
    this.bootstrapUpdater = this.packageUpdater ? new AutomaticBootstrapUpdater(configPath, bootstrapRoot, this.baseJournal) : null;
    // A credentialed executor must bootstrap from Hive even when its older local
    // seed configuration predates the configurationUpdates setting.
    this.configurationUpdater = updateCredential ? new ConfigurationUpdater(configPath, this.baseJournal, updateCredential) : null;
  }
  private async status(state: Host.ExecutorStatus['state'], code: string | null = null): Promise<void> {
    if (state === 'ready' && (code === null || code === 'storage_retention_attention')) {
      const retention = this.baseJournal.storageRetentionStatus();
      if (retention && retention.state !== 'running')
        code = retention.state === 'succeeded' ? null : 'storage_retention_attention';
    }
    const value: Host.ExecutorStatus = { schemaVersion: 1, hostId: this.config.hostId, pid: process.pid, bootId: this.bootId,
      observedAt: now(), state, activeDeploymentId: this.activeId, code, buildId: await this.release,
      ...(this.configurationRevision === null || this.configurationHash === null ? {} : {
        configurationRevision: this.configurationRevision, configurationHash: this.configurationHash }) };
    validateHost('ExecutorStatus', value); await atomicJson(join(this.config.runtimeRoot, 'executor.json'), value);
  }
  start(): void {
    if (this.task) return;
    let busy = false;
    this.pulse = setInterval(() => { if (busy) return; busy = true; void this.status('ready', this.configurationCode ?? this.packageCode ?? this.storageCode ?? this.retentionCode).catch(() => undefined).finally(() => { busy = false; }); }, 1000);
    this.task = this.run();
  }
  get completion(): Promise<void> { return this.task ?? Promise.resolve(); }
  async close(): Promise<void> {
    if (this.closed) return; this.closed = true; this.controller.abort(); if (this.pulse) clearInterval(this.pulse);
    try {
      await this.task;
      for (const handle of this.dockerHandles.values()) handle.process.dispose?.();
      this.dockerHandles.clear();
      this.systemdHandles.clear();
      await this.status('stopping');
    } finally { this.journal.close(); this.lock.close(); }
  }
  private advance(entry: Host.JournalEntry, phase: Wire.DeploymentRecord['phase'], changes: Parameters<HostJournal['advance']>[3] = {}): Host.JournalEntry {
    return this.journal.advance(entry.record.deploymentId, entry.record.phase, phase, changes);
  }
  private dockerObservation(instanceId: string, target: Host.RuntimeTarget | null, state: Host.RuntimeObservation['state'], code: string | null = null,
    message = '', health: Host.Health | null = null, buildId: string | null = null): void {
    this.journal.recordObservation(instanceId, { schemaVersion: 1, instanceId, ownerPid: process.pid, ownerBootId: this.bootId,
      observedAt: now(), targetRevision: target?.revision ?? null, candidateId: target?.candidateId ?? null, buildId,
      state, health, restartCount: 0, nextRestartAt: null, code, message: message.slice(0, 4096) });
  }
  private async stopDocker(instanceId: string, target: Host.RuntimeTarget | null): Promise<void> {
    const handle = this.dockerHandles.get(instanceId);
    if (handle) {
      const result = await handle.process.stop(handle.manifest.shutdown?.timeoutMs ?? 15_000);
      handle.process.dispose?.(); this.dockerHandles.delete(instanceId);
      if (result.errorCode === 'outcome_unknown') throw new IvyError('outcome_unknown', 'Docker did not establish a stopped container outcome.', 'unknown');
      return;
    }
    const manifest = target ? this.journal.manifest(target.candidateId) : null;
    const stopped = await this.dockerAdapter.stopContainer(this.config, instanceId, manifest?.shutdown?.timeoutMs ?? 15_000);
    if (stopped && !['exited', 'dead', 'created'].includes(stopped.state)) throw new IvyError('outcome_unknown', 'Docker container stop remains unresolved.', 'unknown');
  }
  /** Docker is mutated only by an explicit activation/stop operation. */
  private async reconcileDockerOnce(instanceId: string, target: Host.RuntimeTarget | null, allowStart = false): Promise<void> {
    const instance = this.journal.instance(instanceId);
    requireThat(instance.engine === 'docker', 'unsupported_runtime', 'Only Docker instances use the direct Docker owner.');
    let handle: DockerHandle | undefined = this.dockerHandles.get(instanceId);
    if (!target) {
      await this.stopDocker(instanceId, null); this.dockerObservation(instanceId, null, 'idle'); return;
    }
    // A stopped target is an explicit ownership action. It must retire an older
    // in-memory launch handle before passive revision fencing, otherwise a
    // failed activation can strand its own container and make rollback wait
    // forever for the stopped observation it is responsible for producing.
    if (target.desired === 'stopped') {
      await this.stopDocker(instanceId, target);
      this.dockerObservation(instanceId, target, 'stopped', null, 'Docker target is intentionally disabled.', null, this.journal.candidate(target.candidateId).buildId); return;
    }
    if (handle && handle.target.revision !== target.revision && allowStart) {
      await this.stopDocker(instanceId, handle.target); handle = undefined;
    }
    if (handle && handle.target.revision !== target.revision) {
      this.dockerObservation(instanceId, target, 'unknown', 'target_conflict', 'A passive Docker observation cannot replace an active launch.', null,
        this.journal.candidate(target.candidateId).buildId); return;
    }
    const config = await instanceConfig(target.configPath), candidate = this.journal.candidate(target.candidateId), manifest = this.journal.manifest(target.candidateId);
    requireThat(config.instanceId === instanceId && config.hostId === this.config.hostId && config.artifactRoot === candidate.artifactRoot,
      'configuration_changed', 'Docker target configuration no longer belongs to its selected release.');
    if (!handle && !allowStart) {
      const current = await this.dockerAdapter.inspectContainer(this.config, instanceId);
      if (!current) {
        this.dockerObservation(instanceId, target, 'failed', 'container_missing', 'Docker owner has no container for the requested target.', null, candidate.buildId);
        return;
      }
      const matches = current.candidateId === candidate.candidateId && current.launchId === target.revision && current.imageId === candidate.dockerImage;
      if (!matches) {
        this.dockerObservation(instanceId, target, 'unknown', 'target_conflict', 'Docker owner reports a different launch identity.', null, candidate.buildId);
        return;
      }
      let health: Host.Health | null = null;
      try {
        health = await checkHealth(config, localHiveEndpoint(config, instance.docker).href, false);
        this.dockerObservation(instanceId, target, current.state === 'running' || current.state === 'restarting' ? 'ready' : 'starting', null, '', health, candidate.buildId);
      } catch (error) {
        const failure = IvyError.from(error);
        this.dockerObservation(instanceId, target, ['exited', 'dead', 'created'].includes(current.state) ? 'failed' : 'starting', failure.code,
          'Docker owner state is observed without starting or stopping it.', health, candidate.buildId);
      }
      return;
    }
    if (!handle) {
      this.dockerObservation(instanceId, target, 'starting', 'artifact_verification', 'Checking the selected Docker release.', null, candidate.buildId);
      await verifyLaunchCandidate(candidate, this.config, manifest.entrypoint?.args[0]);
      const managed = await this.dockerAdapter.startContainer(this.config, instance, target, config, candidate, manifest, true);
      handle = { target, process: managed, result: null, candidate, manifest, config, startedAt: Date.now(), unreadySince: Date.now(), readinessChecked: false };
      this.dockerHandles.set(instanceId, handle);
      const owned = handle;
      void managed.completion.then(result => { if (this.dockerHandles.get(instanceId) === owned) owned.result = result; });
      this.dockerObservation(instanceId, target, 'starting', null, 'Docker container accepted by the local executor.', null, candidate.buildId);
    }
    if (handle.result) {
      const result = handle.result; handle.process.dispose?.(); this.dockerHandles.delete(instanceId);
      if (result.errorCode === 'outcome_unknown') {
        this.dockerObservation(instanceId, target, 'unknown', 'outcome_unknown', 'Docker container outcome is unknown; no replacement was started.', null, candidate.buildId);
        throw new IvyError('outcome_unknown', 'Docker container outcome is unknown; reconcile the existing container.', 'unknown');
      }
      const failed = result.exitCode !== 0 || result.errorCode !== null;
      this.dockerObservation(instanceId, target, failed ? 'failed' : 'exited', result.errorCode ?? (failed ? 'process_exited' : null),
        failed ? 'Docker container exited before readiness.' : 'Docker container exited.', null, candidate.buildId); return;
    }
    let health: Host.Health | null = null;
    try {
      const endpoint = localHiveEndpoint(config, instance.docker).href;
      health = await checkHealth(config, endpoint, false);
      requireThat(health.launchId === target.revision, 'service_not_ready', 'Docker health belongs to a previous launch.');
      await handle.process.verify?.();
      if (!handle.readinessChecked) {
        await runCommand(manifest.readiness!.command, config.artifactRoot, this.config.executables,
          { environment: { IVY_INSTANCE_CONFIG: target.configPath, IVY_LAUNCH_ID: target.revision, IVY_HIVE_HEALTH_URL: endpoint },
            jobLauncher: join(this.bootstrapRoot, 'dist/native/ivy-job.exe') });
        handle.readinessChecked = true;
      }
      this.dockerObservation(instanceId, target, 'ready', null, '', health, candidate.buildId); handle.unreadySince = 0;
    } catch (error) {
      const failure = IvyError.from(error);
      if (failure.code === 'outcome_unknown') {
        this.dockerObservation(instanceId, target, 'unknown', failure.code, failure.message, health, candidate.buildId); throw error;
      }
      // A large persisted Hive database can need more than the normal service
      // readiness window while its worker reopens storage. Keep the deadline
      // bounded, but do not turn that cold-start work into a stop/start loop.
      const readinessTimeoutMs = instance.componentId === 'hive'
        ? Math.max(manifest.readiness!.timeoutMs, 180_000)
        : manifest.readiness!.timeoutMs;
      handle.unreadySince = readinessFailureStart(handle.unreadySince);
      if (readinessExpired(handle.unreadySince, readinessTimeoutMs)) {
        await this.stopDocker(instanceId, target);
        this.dockerObservation(instanceId, target, 'failed', 'readiness_deadline', 'Docker readiness deadline expired.', health, candidate.buildId);
      } else this.dockerObservation(instanceId, target, 'starting', 'service_not_ready', 'Docker readiness is not complete.', health, candidate.buildId);
    }
  }
  private async resourceScope(): Promise<Host.LinuxResourceScope | undefined> {
    if (this.linuxResourcesLoaded) return this.linuxResources;
    this.linuxResourcesLoaded = true;
    if (process.platform !== 'linux') return undefined;
    const path = join(this.config.runtimeRoot, 'bootstrap', installationIdentity(this.config) + '.json');
    try {
      const plan = await jsonFile<Host.BootstrapPlan>(path); validateHost('BootstrapPlan', plan);
      requireThat(plan.installationId === installationIdentity(this.config) && plan.hostId === this.config.hostId && plan.runtimeRoot === this.config.runtimeRoot,
        'target_conflict', 'The retained Linux bootstrap plan belongs to another installation.');
      if (plan.linuxResources) validateLinuxResourceScope(plan.linuxResources, plan.installationId);
      this.linuxResources = plan.linuxResources;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
    return this.linuxResources;
  }
  private systemdObservation(instanceId: string, target: Host.RuntimeTarget | null, state: SystemdState, runtimeState: Host.RuntimeObservation['state'],
    code: string | null = null, message = '', health: Host.Health | null = null, buildId: string | null = null): void {
    this.journal.recordObservation(instanceId, { schemaVersion: 1, instanceId, ownerPid: state.mainPid ?? process.pid,
      ownerBootId: health?.bootId ?? target?.revision ?? this.bootId, observedAt: now(), targetRevision: target?.revision ?? null,
      candidateId: target?.candidateId ?? null, buildId, state: runtimeState, health, restartCount: 0, nextRestartAt: null,
      code, message: message.slice(0, 4096) });
  }
  private async stopSystemd(instanceId: string, target: Host.RuntimeTarget | null): Promise<SystemdState | null> {
    const unit = systemdUnitName(this.config, instanceId), definition = await this.systemdAdapter.readUnit(this.config, unit);
    this.systemdHandles.delete(instanceId);
    if (definition === null) return null;
    const current = await this.systemdAdapter.inspectUnit(this.config, unit);
    if (['inactive', 'failed'].includes(current.activeState)) return current;
    return this.systemdAdapter.stopUnit(this.config, unit, target ? this.journal.manifest(target.candidateId).shutdown?.timeoutMs ?? 15_000 : 15_000);
  }
  /** systemd owns restart policy; this path mutates a unit only for an explicit activation/stop. */
  private async reconcileSystemdOnce(instanceId: string, target: Host.RuntimeTarget | null, allowStart = false): Promise<void> {
    const instance = this.journal.instance(instanceId);
    requireThat(process.platform === 'linux' && instance.engine === 'process', 'unsupported_runtime', 'Only Linux process instances use the direct systemd owner.');
    // The host executor itself is the independently bootstrapped process. It cannot
    // replace its own systemd definition while it is executing the activation.
    requireThat(instance.componentId !== 'host-executor', 'restart_required', 'The host executor is replaced through the OS bootstrap handoff.');
    const unit = systemdUnitName(this.config, instanceId);
    if (!target) { const stopped = await this.stopSystemd(instanceId, null); this.systemdObservation(instanceId, null, stopped ?? { loadState: 'not-found', activeState: 'inactive', subState: 'dead', mainPid: null }, 'idle'); return; }
    if (target.desired === 'stopped') {
      const stopped = await this.stopSystemd(instanceId, target);
      this.systemdObservation(instanceId, target, stopped ?? { loadState: 'not-found', activeState: 'inactive', subState: 'dead', mainPid: null }, 'stopped', null,
        'Systemd target is intentionally disabled.', null, this.journal.candidate(target.candidateId).buildId);
      return;
    }
    const config = await instanceConfig(target.configPath), candidate = this.journal.candidate(target.candidateId), manifest = this.journal.manifest(target.candidateId);
    requireThat(config.instanceId === instanceId && config.hostId === this.config.hostId && config.artifactRoot === candidate.artifactRoot,
      'configuration_changed', 'Systemd target configuration no longer belongs to its selected release.');
    requireThat(manifest.entrypoint, 'invalid_arguments', 'A Linux process candidate needs an explicit entrypoint.');
    const resources = await this.resourceScope(), rendered = renderSystemdUnit(this.config, instanceId, target, candidate, manifest, resources);
    let definition = await this.systemdAdapter.readUnit(this.config, unit), handle = this.systemdHandles.get(instanceId);
    if (handle && handle.target.revision !== target.revision && !allowStart) {
      this.systemdObservation(instanceId, target, await this.systemdAdapter.inspectUnit(this.config, unit), 'unknown', 'target_conflict',
        'A passive systemd observation cannot replace an active launch.', null, candidate.buildId); return;
    }
    if (definition !== rendered.content) {
      requireThat(allowStart, 'target_changed', 'Systemd definition differs from the accepted target; explicit activation is required.');
      if (definition !== null) {
        const current = await this.systemdAdapter.inspectUnit(this.config, unit);
        if (!['inactive', 'failed'].includes(current.activeState)) await this.systemdAdapter.stopUnit(this.config, unit, manifest.shutdown?.timeoutMs ?? 15_000);
      }
      const user = linuxProcessUser(instance.componentId, 'linux');
      if (user) await ensureLinuxProcessIdentity({ componentId: instance.componentId, user, configPath: target.configPath,
        artifactRoot: candidate.artifactRoot, nodeExecutable: this.config.executables['node']! }, config);
      await this.systemdAdapter.replaceUnit(this.config, unit, rendered.content);
      await this.systemdAdapter.reload(this.config);
      await this.systemdAdapter.startUnit(this.config, unit);
      definition = rendered.content;
      handle = { target, unit, candidate, manifest, config, startedAt: Date.now(), unreadySince: Date.now(), readinessChecked: false };
      this.systemdHandles.set(instanceId, handle);
    } else if (!handle || handle.target.revision !== target.revision) {
      handle = { target, unit, candidate, manifest, config, startedAt: Date.now(), unreadySince: Date.now(), readinessChecked: false };
      this.systemdHandles.set(instanceId, handle);
    }
    let observed = await this.systemdAdapter.inspectUnit(this.config, unit);
    if (observed.activeState === 'failed' || observed.activeState === 'inactive') {
      if (!allowStart) {
        this.systemdObservation(instanceId, target, observed, observed.activeState === 'failed' ? 'failed' : 'exited', 'systemd_not_running',
          'Systemd owner state is observed without starting the unit.', null, candidate.buildId);
        return;
      }
      await this.systemdAdapter.startUnit(this.config, unit); observed = await this.systemdAdapter.inspectUnit(this.config, unit);
    }
    if (observed.activeState !== 'active' || !['running', 'listening', 'exited'].includes(observed.subState)) {
      this.systemdObservation(instanceId, target, observed, observed.activeState === 'failed' ? 'failed' : 'starting', 'systemd_starting',
        'Systemd has not reported a running process yet.', null, candidate.buildId);
      return;
    }
    let health: Host.Health | null = null;
    try {
      health = await checkHealth(config, undefined, false);
      requireThat(health.launchId === target.revision, 'service_not_ready', 'Health belongs to a previous systemd launch.');
      if (!handle.readinessChecked) {
        await runCommand(manifest.readiness!.command, config.artifactRoot, this.config.executables,
          { environment: { IVY_INSTANCE_CONFIG: target.configPath, IVY_LAUNCH_ID: target.revision }, jobLauncher: join(this.bootstrapRoot, 'dist/native/ivy-job.exe') });
        handle.readinessChecked = true;
      }
      this.systemdObservation(instanceId, target, observed, 'ready', null, '', health, candidate.buildId); handle.unreadySince = 0;
    } catch (error) {
      const failure = IvyError.from(error);
      if (failure.code === 'outcome_unknown') {
        this.systemdObservation(instanceId, target, observed, 'unknown', failure.code, failure.message, health, candidate.buildId); throw error;
      }
      handle.unreadySince = readinessFailureStart(handle.unreadySince);
      if (readinessExpired(handle.unreadySince, manifest.readiness!.timeoutMs)) {
        await this.stopSystemd(instanceId, target);
        this.systemdObservation(instanceId, target, { ...observed, activeState: 'inactive', subState: 'dead', mainPid: null }, 'failed', 'readiness_deadline',
          'Systemd readiness deadline expired.', health, candidate.buildId);
      } else this.systemdObservation(instanceId, target, observed, 'starting', 'service_not_ready', 'Systemd readiness is not complete.', health, candidate.buildId);
    }
  }
  private async waitFor(instanceId: string, target: Host.RuntimeTarget | null, predicate: (value: Host.RuntimeObservation) => boolean, timeoutMs: number, allowStart = false): Promise<Host.RuntimeObservation> {
    const end = Date.now() + timeoutMs;
    while (!this.controller.signal.aborted) {
      const instance = this.journal.instance(instanceId);
      if (instance.engine === 'docker') await this.reconcileDockerOnce(instanceId, target, allowStart);
      else if (process.platform === 'linux' && instance.componentId !== 'host-executor') await this.reconcileSystemdOnce(instanceId, target, allowStart);
      const value = this.journal.observations()[instanceId];
      // A matching stopped observation stays conclusive: the immutable target
      // still requests stopped, so its owner cannot launch a replacement after
      // that observation. Requiring timestamp freshness here strands later
      // enable/deploy operations behind a steady state that intentionally emits
      // no journal heartbeat.
      const conclusivelyStopped = stoppedTargetIsConclusive(value, target);
      if ((fresh(value) || conclusivelyStopped) && value?.targetRevision === (target?.revision ?? null) && predicate(value)) return value;
      if (fresh(value) && value.targetRevision === target?.revision && target?.desired === 'running' && ['failed', 'exited', 'unknown'].includes(value.state)) {
        throw new IvyError(value.code ?? 'process_failed', `Runtime owner reported ${value.state}: ${value.code ?? 'process_failed'}. ${value.message}`, 'unknown');
      }
      if (Date.now() >= end) throw new IvyError('observation_deadline', 'The current runtime owner did not establish the required state before its deadline.', 'unknown');
      await delay(200, undefined, { signal: this.controller.signal });
    }
    this.controller.signal.throwIfAborted(); throw new Error('Cancelled');
  }
  private serviceStorage(instanceId: string, plan: Pick<Host.BuildPlan, 'componentId' | 'storage'>): void {
    const target = this.journal.target(instanceId), observed = this.journal.observations()[instanceId];
    // The exact target/build identity remains valid while the immutable target
    // is unchanged. Observation age is a liveness signal, not a storage-format
    // signal; using it here made an exclusively opened steady-state journal
    // unreadable to deployments after fifteen seconds.
    const running = target && observed?.state === 'ready' && observed.targetRevision === target.revision &&
      observed.candidateId === target.candidateId && observed.health?.launchId === target.revision && observed.health.ready &&
      observed.health.buildId === this.journal.candidate(target.candidateId).buildId ? this.journal.manifest(target.candidateId).storage : undefined;
    verifyServiceStorage(this.config, instanceId, plan, running && running.minReadableFormat === running.writeFormat && running.maxReadableFormat === running.writeFormat ? running.writeFormat : undefined);
  }
  private async compatibility(candidateId: string, instanceId: string): Promise<void> {
    const candidate = this.journal.candidate(candidateId), manifest = this.journal.manifest(candidateId);
    requireRuntimeRequirements(manifest.requirements);
    this.serviceStorage(instanceId, manifest);
    await verifyLaunchCandidate(candidate, this.config, manifest.entrypoint?.args[0]);
    if (this.journal.instance(instanceId).engine === 'docker') await this.dockerAdapter.verifyContainerImage(this.config, this.journal.instance(instanceId), candidate, manifest);
    await resolveCommand(manifest.entrypoint!, candidate.artifactRoot, this.config.executables);
    await resolveCommand(manifest.readiness!.command, candidate.artifactRoot, this.config.executables);
    if (manifest.componentId === 'host-executor') {
      const format = Number(this.journal.db.prepare('PRAGMA user_version').get()!['user_version']);
      requireThat(manifest.storage && manifest.storage.minReadableFormat <= format && manifest.storage.maxReadableFormat >= format && manifest.storage.writeFormat === format, 'incompatible_storage', 'Executor update must preserve the bootstrap-readable host journal format.');
    }
    const disk = await statfs(this.config.runtimeRoot); requireThat(disk.bavail * disk.bsize >= 256 * 1024 * 1024, 'storage_pressure', 'Insufficient free host space for guarded activation.');
    const contracts = manifest.requirements.contracts.map(({ key, readScope }) => ({ key, ...(readScope ? { readScope } : {}) }));
    let inspection: Host.StorageInspection | null = null;
    if (manifest.kind === 'hive') {
      const instance = this.journal.instance(instanceId), settings = instance.settings as unknown as Host.HiveSettings;
      validateHost('HiveSettings', settings); requireThat(settings.credentials.length > 0, 'invalid_arguments', 'Hive requires configured credentials.');
      const observed = this.journal.observations()[instanceId], currentTarget = this.journal.target(instanceId);
      if (runningHiveCanBeInspected(observed, currentTarget)) {
        const live = await jsonFile<Host.InstanceConfig>(currentTarget!.configPath);
        requireThat(live.instanceId === instanceId && live.hostId === this.config.hostId, 'configuration_changed', 'Live Hive configuration no longer belongs to its target.');
        const endpoint = localHiveEndpoint(live, this.config.instances.find(value => value.instanceId === instanceId)?.docker);
        const client = new HiveClient(endpoint.href, { credential: (live.settings as unknown as Host.HiveSettings).credentials[0]!.token });
        const status = await client.request('system.status', {}, { timeoutMs: 5000 });
        requireThat(status.ready && status.buildId === observed!.buildId, 'service_not_ready', 'Live Hive identity does not match the runtime owner observation.');
        inspection = await client.request('system.inspectStorage', { contracts }, { timeoutMs: 5000 });
      } else {
        requireThat((fresh(observed) || stoppedTargetIsConclusive(observed, currentTarget)) && ['idle', 'stopped'].includes(observed!.state) &&
          observed!.targetRevision === (currentTarget?.revision ?? null), 'compatibility_unknown', 'Hive must be observed stopped before its own offline inspector opens storage.');
        const staged = await materializeTarget(this.journal, instanceId, candidateId, false);
        const result = await runCommand({ executable: 'node', args: ['dist/services/hive/src/inspect-storage.js', '--contracts', JSON.stringify(contracts)], timeoutMs: 10_000 }, candidate.artifactRoot, this.config.executables,
          { environment: { IVY_INSTANCE_CONFIG: staged.target.configPath }, jobLauncher: join(this.bootstrapRoot, 'dist/native/ivy-job.exe') });
        inspection = JSON.parse(result.stdout) as Host.StorageInspection; validateHost('StorageInspection', inspection);
      }
      requireThat(manifest.storage && (!inspection.exists || (inspection.format >= manifest.storage.minReadableFormat && inspection.format <= manifest.storage.maxReadableFormat)), 'incompatible_storage', 'Candidate cannot read the actual database format.');
    } else if (manifest.requirements.hiveProtocol !== null || contracts.length) {
      const instance = this.journal.instance(instanceId); requireThat(instance.credential, 'invalid_arguments', 'Hive compatibility requires the instance credential.');
      const client = new HiveClient(this.config.publicBaseUrl, { credential: instance.credential });
      const status = await client.request('system.status', {}, { timeoutMs: 5000 });
      requireThat(status.ready && status.hiveProtocol === manifest.requirements.hiveProtocol, 'incompatible_protocol', 'Current Hive protocol is unavailable or incompatible.');
      inspection = await client.request('system.inspectStorage', { contracts }, { timeoutMs: 5000 });
    }
    if (inspection) for (const usage of inspection.contracts) {
      const requirement = manifest.requirements.contracts.find(item => item.key === usage.key);
      requireThat(requirement && usage.versions.every(version => requirement.readVersions.includes(version)), 'incompatible_contract', 'Candidate cannot read every stored current/historical version in its declared domain.');
    }
  }
  private async rollback(entry: Host.JournalEntry): Promise<void> {
    requireThat(entry.previousCandidateId, 'rollback_unavailable', 'No previous candidate is retained for this deployment.');
    const previousCandidateId = entry.previousCandidateId;
    let target = this.journal.target(entry.record.instanceId);
    const started = entry.activation.rollbackTargetRevision !== null;
    if (!started) {
      if (target?.desired === 'running' && entry.record.instanceId !== this.ownInstanceId) {
        target = stoppedTarget(target);
        entry = this.advance(entry, 'rolling_back', { target });
      }
      if (entry.record.instanceId !== this.ownInstanceId) await this.waitFor(entry.record.instanceId, target, value => ['idle', 'stopped'].includes(value.state), 30_000 + (this.journal.manifest(target?.candidateId ?? entry.candidateId!).shutdown?.timeoutMs ?? 15_000));
      await this.compatibility(previousCandidateId, entry.record.instanceId);
      // Restore the configuration that actually launched the retained build. A new
      // immutable target file is materialized from it so rollback neither applies
      // the failed release's settings nor reuses an old mutable target file.
      const previous = entry.activation.previousTarget;
      requireThat(previous?.candidateId === previousCandidateId && entry.activation.previousConfigurationPath,
        'rollback_unavailable', 'The previous launch configuration is not retained for rollback.');
      const previousConfig = readAcceptedInstanceConfiguration(this.journal.config, entry.activation.previousConfigurationPath);
      target = (await materializeTarget(this.journal, entry.record.instanceId, previousCandidateId, entry.activation.previousEnabled, previousConfig)).target;
      entry = this.advance(entry, 'rolling_back', { target, rollbackTargetRevision: target.revision });
    }
    requireThat(target && target.candidateId === entry.previousCandidateId, 'outcome_unknown', 'Rollback target no longer identifies its retained previous build.');
     const observation = await this.waitFor(entry.record.instanceId, target, value => value.state === (target!.desired === 'running' ? 'ready' : 'stopped'), (this.journal.manifest(target.candidateId).readiness?.timeoutMs ?? 30_000) + 15_000, target.desired === 'running');
    this.advance(entry, 'rolled_back', { observedBuild: observation.health?.buildId ?? null, readiness: { state: 'passed', message: 'Previous compatible binary restored; persistent data was retained.' },
      installed: { instanceId: entry.record.instanceId, candidateId: target.candidateId, buildId: this.journal.candidate(target.candidateId).buildId, enabled: target.desired === 'running', installedAt: now() } });
  }
  private async execute(original: Host.JournalEntry): Promise<void> {
    let entry = original;
    const id = entry.record.deploymentId, instanceId = entry.record.instanceId;
    try {
      this.journal.useConfiguration(this.journal.acceptedConfiguration(entry.configurationPath));
      this.journal.instance(instanceId);
      if (entry.record.phase === 'preparing') {
        requireThat(entry.sourceSnapshot, 'source_snapshot_required', 'Accepted source input is missing.');
        const sourcePlan = await sourceBuildPlan(entry.sourceSnapshot.sourceRoot, entry.record.componentId);
        this.serviceStorage(instanceId, sourcePlan);
        // Source builds are an exceptional compatibility path. Keep the TypeScript
        // compiler and preparation graph out of the idle executor process.
        const { prepareCandidate } = await import('./prepare.js');
        const candidate = await prepareCandidate(entry.sourceSnapshot, entry.record.componentId, this.config, this.bootstrapRoot);
        entry = this.advance(entry, 'prepared', { candidateId: candidate.candidateId });
      }
      if (entry.record.phase === 'prepared') entry = this.advance(entry, 'checking');
      if (entry.record.phase === 'checking') {
        const installed = this.journal.installed(instanceId), instance = this.journal.instance(instanceId), current = this.journal.target(instanceId);
        requireThat((installed?.candidateId ?? null) === entry.previousCandidateId, 'target_changed', 'An earlier queued deployment changed the installed build.');
        // The target itself is the owner's durable instruction. When it already
        // identifies the installed build, change it to stopped immediately and
        // let the owner acknowledge that new revision in the draining phase.
        // Only a target/install mismatch needs prior proof that nothing runs.
        if (replacementNeedsSettledObservation(current?.candidateId ?? null, installed?.candidateId ?? null)) {
          const observeTimeout = (this.journal.manifest(current!.candidateId).readiness?.timeoutMs ?? 30_000) + 15_000;
          await this.waitFor(instanceId, current, value => value.state === 'stopped', observeTimeout);
        }
        if (entry.request.action !== 'disable') await this.compatibility(entry.candidateId!, instanceId);
        requireThat(canonical(current) === canonical(entry.activation.previousTarget), 'target_changed', 'Runtime target changed after this operation was accepted.');
        requireThat(instanceId !== this.ownInstanceId || entry.activation.targetEnabled, 'invalid_arguments', 'Use the OS bootstrap to take the host executor out of service.');
        const target = current?.desired === 'running' && instanceId !== this.ownInstanceId ? stoppedTarget(current) : undefined;
        entry = this.advance(entry, 'draining', { ...(target ? { target } : {}) });
      }
      if (entry.record.phase === 'draining') {
        const target = this.journal.target(instanceId);
        let shutdownTimeoutMs = 15_000;
        try { shutdownTimeoutMs = this.journal.manifest(target?.candidateId ?? entry.candidateId!).shutdown?.timeoutMs ?? shutdownTimeoutMs; }
        catch (error) {
          // A stopped pre-package target may retain a manifest from an older
          // schema. It still has a bounded conservative drain timeout.
          if (IvyError.from(error).code !== 'invalid_arguments') throw error;
        }
        if (instanceId !== this.ownInstanceId) await this.waitFor(instanceId, target, value => ['idle', 'stopped'].includes(value.state), 30_000 + shutdownTimeoutMs);
        // Data may have changed between live preflight and the actual drain.
        if (entry.request.action !== 'disable') await this.compatibility(entry.candidateId!, instanceId);
        const next = await materializeTarget(this.journal, instanceId, entry.candidateId!, entry.activation.targetEnabled);
        entry = this.advance(entry, 'activating', { target: next.target });
        // Initial OS installation hands the lock from its temporary bootstrap executor to the
        // first independently guarded executor after durable start intent, before readiness.
        if (!this.ownInstanceId && entry.record.componentId === 'host-executor' && next.target.desired === 'running') { this.controller.abort(); return; }
      }
      if (entry.record.phase === 'activating') {
        const target = this.journal.target(instanceId);
        requireThat(target?.candidateId === entry.candidateId, 'outcome_unknown', 'Activation target is not the accepted candidate.');
        // The runtime owner publishes starting after its bounded launch checks.
        await this.waitFor(instanceId, target, value => ['starting', 'ready', 'stopped', 'failed', 'exited'].includes(value.state), 45_000, true);
        entry = this.advance(entry, 'verifying');
      }
      if (entry.record.phase === 'verifying') {
        const target = this.journal.target(instanceId);
        requireThat(target?.candidateId === entry.candidateId, 'outcome_unknown', 'Verification target is not the accepted candidate.');
        if (target.desired === 'running') await this.waitFor(instanceId, target,
          value => !(value.state === 'starting' && value.code === 'artifact_verification'), 60_000);
        const observation = await this.waitFor(instanceId, target, value => value.state === (target.desired === 'running' ? 'ready' : 'stopped'),
          this.journal.manifest(entry.candidateId!).readiness!.timeoutMs + 15_000);
        this.advance(entry, 'succeeded', { observedBuild: observation.health?.buildId ?? null,
          readiness: { state: 'passed', message: target.desired === 'running' ? 'Actual launch, manifest readiness and current dependencies verified.' : 'Verified artifact installed; instance remains intentionally disabled.' },
          installed: { instanceId, candidateId: entry.candidateId!, buildId: this.journal.candidate(entry.candidateId!).buildId, enabled: target.desired === 'running', installedAt: now() } });
      }
      if (entry.record.phase === 'rolling_back') await this.rollback(entry);
    } catch (error) {
      if (this.controller.signal.aborted) return; // The replacement executor owns the still-durable phase.
      entry = this.journal.get(id); if (terminalPhases.has(entry.record.phase)) return;
      const failure = IvyError.from(error);
      if (['preparing', 'prepared', 'checking'].includes(entry.record.phase)) {
        this.advance(entry, 'failed', { errorCode: failure.code, readiness: { state: 'failed', message: failure.message } }); return;
      }
      if (entry.previousCandidateId && entry.record.phase !== 'rolling_back') {
        entry = this.advance(entry, 'rolling_back', { errorCode: failure.code, readiness: { state: 'failed', message: failure.message } });
        try { await this.rollback(entry); return; } catch (rollbackError) { if (this.controller.signal.aborted) return; error = rollbackError; }
      }
      entry = this.journal.get(id);
      const current = this.journal.target(instanceId), target = current?.desired === 'running' ? stoppedTarget(current) : undefined;
      this.advance(entry, 'needs_attention', { ...(target ? { target } : {}), errorCode: IvyError.from(error).code,
        readiness: { state: 'unknown', message: `Deployment failed: ${failure.code}. Final failure: ${IvyError.from(error).message} The last target is requested stopped.` } });
    }
  }
  private async run(): Promise<void> {
    await this.status('reconciling');
    const active = new Map<string, { entry: Host.JournalEntry; task: Promise<void> }>();
    const retention = new RetentionSchedule(Date.now());
    let failed: unknown, nextPackageCheck = 0, nextConfigurationCheck = 0, nextBootstrapRestartCheck = 0,
      nextStorageCheck = 0, lowSpace = false, lastDesiredHint = '';
    try {
      while (!this.controller.signal.aborted) {
        const hint = await jsonFile<{ schemaVersion: number; observedAt: string; configuration: boolean; packages: boolean }>(
          join(this.config.runtimeRoot, 'desired-state-hint.json')).catch(() => null);
        if (hint?.schemaVersion === 1) {
          const identity = JSON.stringify(hint);
          if (identity !== lastDesiredHint) {
            lastDesiredHint = identity;
            if (hint.configuration) nextConfigurationCheck = 0;
            if (hint.packages) nextPackageCheck = 0;
          }
        }
        if (this.configurationUpdater && Date.now() >= nextConfigurationCheck) {
          try {
            const result = await this.configurationUpdater.sync();
            if (result.revision !== null && result.contentHash !== null) {
              this.configurationRevision = result.revision; this.configurationHash = result.contentHash;
            }
            this.configurationCode = null;
          } catch (error) { this.configurationCode = configurationFailure(error); }
          nextConfigurationCheck = Date.now() + (this.config.configurationUpdates?.intervalSeconds ?? 60) * 1000;
        }
        if (this.configurationUpdater && Date.now() >= nextBootstrapRestartCheck) {
          nextBootstrapRestartCheck = Date.now() + 5_000;
          try {
            if (await reconcileBootstrapConfigurationRestarts(this.config, this.ownInstanceId, this.bootstrapRoot)) {
              this.controller.abort(); break;
            }
          } catch (error) { this.configurationCode = configurationFailure(error); }
        }
        if (this.packageUpdater && Date.now() >= nextPackageCheck) {
          nextPackageCheck = Date.now() + this.config.packageUpdates!.intervalSeconds * 1000;
          try {
            const result = await this.packageUpdater.sync();
            if (result.bootstrap.length) await this.bootstrapUpdater?.reconcile(result.bootstrap);
            this.packageCode = null;
          }
          catch (error) { this.packageCode = IvyError.from(error).code; }
        }
        if (Date.now() >= nextStorageCheck) {
          nextStorageCheck = Date.now() + 60_000;
          const previous = this.storageCode;
          try {
            const space = await hostStorageSpace(this.config); lowSpace = space.lowSpace;
            this.storageCode = lowSpace ? 'storage_pressure' : null;
            if (this.storageCode !== previous) process.stderr.write(JSON.stringify({ code: this.storageCode ?? 'storage_pressure_cleared', ...space }) + '\n');
          } catch { this.storageCode = 'storage_probe_failed'; }
        }
        if (!this.retentionTask && retention.due(Date.now(), lowSpace) && active.size === 0 && this.baseJournal.unfinished().length === 0) {
          retention.started(Date.now());
          this.retentionTask = collectHostStorage(this.config, Date.now(), lowSpace ? 'storage_pressure' : 'scheduled').then(result => {
            const status = this.baseJournal.storageRetentionStatus();
            if (status?.state === 'failed') retention.failed(Date.now());
            this.retentionCode = status?.state === 'partial' || status?.state === 'failed' ? 'storage_retention_attention' : null;
            process.stderr.write(JSON.stringify({ code: this.retentionCode ?? 'host_storage_collected', ...result,
              skipped: status?.skipped, availableBytes: status?.availableBytes }) + '\n');
          }).catch(error => {
            retention.failed(Date.now()); this.retentionCode = 'storage_retention_failed';
            process.stderr.write(JSON.stringify({ code: 'host_storage_retention_failed', errorCode: IvyError.from(error).code }) + '\n');
          }).finally(() => { this.retentionTask = null; });
        }
        const exclusive = (entry: Host.JournalEntry) => entry.record.phase === 'preparing' || entry.record.componentId === 'host-executor' ||
          (entry.candidateId ? this.baseJournal.manifest(entry.candidateId).kind === 'hive' : entry.record.componentId === 'hive');
        for (const entry of parallelDeployments(this.baseJournal.unfinished(), [...active.values()].map(value => value.entry), 2, exclusive)) {
          // Separate handles preserve each operation's directly accepted configuration.
          const journal = new HostJournal(this.baseJournal.acceptedConfiguration(entry.configurationPath));
          const id = entry.record.deploymentId;
          const task = this.executionJournal.run(journal, async () => { await this.execute(entry); })
            .catch(error => { failed = error; this.controller.abort(); })
            .finally(() => { journal.close(); active.delete(id); this.activeId = active.keys().next().value ?? null; });
          active.set(id, { entry, task });
        }
        // The v1 status contract exposes one representative ID. All concurrent
        // operations remain visible in the journal's unfinished deployment list.
        this.activeId = active.keys().next().value ?? null;
        try { await delay(250, undefined, { signal: this.controller.signal }); } catch { break; }
      }
    } finally { await Promise.all([...active.values()].map(value => value.task)); await this.retentionTask; }
    if (failed) throw failed;
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  let executor: HostExecutor | null = null, timer: NodeJS.Timeout | null = null;
  try {
    const args = process.argv.slice(2), position = args.indexOf('--config'), ownPosition = args.indexOf('--instance-config');
    const ownPath = ownPosition >= 0 ? args[ownPosition + 1] : process.env['IVY_INSTANCE_CONFIG'];
    const own = ownPath ? await instanceConfig(ownPath) : null;
    const { defaultHostConfigPath } = await import('./layout.js');
    const path = position >= 0 ? args[position + 1] : own?.settings['hostConfigPath'] ?? process.env['IVY_HOST_CONFIG'] ?? defaultHostConfigPath();
    requireThat(typeof path === 'string' && isAbsolute(path), 'invalid_arguments', 'Executor requires an explicit absolute host configuration.');
    const config = await hostConfig(path), root = fileURLToPath(new URL('../../../../', import.meta.url));
    requireThat(!await runtimeResetActive(config), 'maintenance_active', 'Host execution remains stopped for an incomplete runtime reset.');
    executor = new HostExecutor(config, path, root, own?.instanceId, defaultDockerAdapter, defaultSystemdAdapter, own?.credential); executor.start();
    const health = own ? new HealthFile(own) : null;
    const stop = () => { if (timer) clearInterval(timer); void executor?.close().then(() => { process.exitCode = 0; }, () => { process.exitCode = 1; }); };
    void executor.completion.then(stop, error => { process.stderr.write(JSON.stringify({ code: IvyError.from(error).code, message: 'Host execution loop stopped.' }) + '\n'); stop(); process.exitCode = 1; });
    let busy = false;
    timer = setInterval(() => { if (busy) return; busy = true; void (async () => { await health?.write(true); if (await health?.control()) stop(); })().catch(() => undefined).finally(() => { busy = false; }); }, 1000);
    process.on('SIGTERM', stop); process.on('SIGINT', stop);
  } catch (error) { process.stderr.write(JSON.stringify({ code: IvyError.from(error).code, message: 'Host executor failed to start.' }) + '\n'); if (timer) clearInterval(timer); await executor?.close(); process.exitCode = 1; }
}
