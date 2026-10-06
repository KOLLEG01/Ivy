import { join, resolve } from 'node:path';
import { atomicJson, jsonFile } from './config.js';
import { exists, privateDirectory } from './backup-files.js';
import { configurationBootstrapPlan, bootstrapComponents, retainedBootstrapPlans } from './bootstrap.js';
import { inspectBootstrap } from './bootstrap-maintenance.js';
import { bootstrapOs } from './bootstrap-os.js';
import { HostJournal } from './journal.js';
import { hashJson } from '../../contracts/src/canonical.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import type { Host } from '../../contracts/src/generated.js';
import type { BootstrapPackageComponent } from './package-updater.js';
import type { BootstrapOs } from './bootstrap-os.js';

export type AutomaticBootstrapPhase = 'launched' | 'checking' | 'updating' | 'verifying' | 'succeeded' | 'rolling_back' | 'rolled_back' | 'outcome_unknown' | 'needs_attention';
export interface AutomaticBootstrapRecord {
  schemaVersion: 1;
  operationId: string;
  requestHash: string;
  phase: AutomaticBootstrapPhase;
  errorCode: string | null;
  createdAt: string;
  updatedAt: string;
}

export function automaticBootstrapRoot(config: Host.HostConfig, operationId: string): string {
  requireThat(operationId.length > 0 && operationId.length <= 128, 'invalid_arguments', 'Automatic bootstrap operation identity is invalid.');
  return join(config.runtimeRoot, 'bootstrap-automatic', hashJson(operationId).slice(7));
}

export async function automaticBootstrapRecord(config: Host.HostConfig, operationId: string): Promise<AutomaticBootstrapRecord | null> {
  const path = join(automaticBootstrapRoot(config, operationId), 'record.json');
  if (!await exists(path)) return null;
  const value = await jsonFile<AutomaticBootstrapRecord>(path);
  requireThat(value.schemaVersion === 1 && value.operationId === operationId && value.requestHash.startsWith('sha256:') &&
    typeof value.phase === 'string' && typeof value.createdAt === 'string' && typeof value.updatedAt === 'string',
  'storage_invalid', 'Automatic bootstrap record is invalid.');
  return value;
}

export async function saveAutomaticBootstrapRecord(config: Host.HostConfig, value: AutomaticBootstrapRecord): Promise<void> {
  value.updatedAt = new Date().toISOString();
  await atomicJson(join(automaticBootstrapRoot(config, value.operationId), 'record.json'), value);
}

export class AutomaticBootstrapUpdater {
  constructor(readonly configPath: string, readonly distribution: string, readonly journal: HostJournal,
    private readonly os: BootstrapOs = bootstrapOs(distribution),
    private readonly inspect: typeof inspectBootstrap = inspectBootstrap) {}

  async reconcile(selected: BootstrapPackageComponent[]): Promise<boolean> {
    if (!selected.length || this.journal.unfinished().length) return false;
    const config = this.journal.config, path = resolve(this.configPath), retained = await retainedBootstrapPlans(config), current = retained[0];
    requireThat(current, 'not_found', 'Automatic bootstrap update needs the retained current OS plan.');
    const executor = config.instances.find(value => value.componentId === 'host-executor');
    requireThat(executor, 'not_found', 'Automatic bootstrap update needs the configured HostExecutor.');
    const requestedExecutor = selected.find(value => value.instanceId === executor.instanceId);
    const installedExecutor = this.journal.installed(executor.instanceId);
    requireThat(requestedExecutor || installedExecutor, 'not_found', 'Automatic bootstrap update needs the installed HostExecutor release.');
    const hostCandidate = requestedExecutor?.candidate ?? this.journal.candidate(installedExecutor!.candidateId);
    const preferred = selected.map(value => ({ candidate: value.candidate, manifest: value.manifest }));
    if (!preferred.some(value => value.candidate.candidateId === hostCandidate.candidateId))
      preferred.unshift({ candidate: hostCandidate, manifest: this.journal.manifest(hostCandidate.candidateId) });
    const next = configurationBootstrapPlan(config, path, hostCandidate, current.linuxResources?.budget,
      bootstrapComponents(config, this.journal, hostCandidate, preferred));
    const desired = new Map(next.processes.map(value => [value.instanceId, value.candidateId ?? next.candidateId]));
    if (current.processes.every(value => (value.candidateId ?? current.candidateId) === desired.get(value.instanceId))) return false;
    const operationId = 'bootstrap-package-' + hashJson({ hostId: config.hostId, config: next.configHash,
      releases: [...desired].sort(([a], [b]) => a.localeCompare(b)) }).slice(7, 47);
    const root = automaticBootstrapRoot(config, operationId), requestPath = join(root, 'request.json');
    const existing = await automaticBootstrapRecord(config, operationId);
    if (existing && ['succeeded', 'rolled_back', 'needs_attention'].includes(existing.phase)) return false;
    const parent = join(config.runtimeRoot, 'bootstrap-automatic');
    if (!await exists(parent)) await privateDirectory(parent);
    if (!await exists(root)) await privateDirectory(root);
    let request: Host.BootstrapMaintenanceRequest;
    if (await exists(requestPath)) {
      request = await jsonFile<Host.BootstrapMaintenanceRequest>(requestPath); validateHost('BootstrapMaintenanceRequest', request);
      requireThat(request.operationId === operationId && hashJson(request.next) === hashJson(next), 'operation_conflict',
        'Automatic bootstrap operation belongs to another exact target.');
    } else {
      requireThat(!existing, 'storage_invalid', 'Automatic bootstrap record has no retained exact request.');
      const previous = await this.inspect(path, this.distribution, this.os);
      request = { schemaVersion: 1, operationId, previous, next }; validateHost('BootstrapMaintenanceRequest', request);
      await atomicJson(requestPath, request);
    }
    const requestHash = hashJson(request);
    requireThat(!existing || existing.requestHash === requestHash, 'operation_conflict', 'Automatic bootstrap operation belongs to another exact request.');
    const now = new Date().toISOString(), record: AutomaticBootstrapRecord = existing ?? { schemaVersion: 1, operationId, requestHash,
      phase: 'launched', errorCode: null, createdAt: now, updatedAt: now };
    if (!existing) await saveAutomaticBootstrapRecord(config, record);
    requireThat(this.os.launchAutomatic, 'unsupported_runtime', 'This host has no independent automatic bootstrap handoff.');
    try { await this.os.launchAutomatic(current, requestPath, operationId); return true; }
    catch (error) {
      if (!existing) { record.phase = 'outcome_unknown'; record.errorCode = IvyError.from(error).code; await saveAutomaticBootstrapRecord(config, record); }
      throw error;
    }
  }
}
