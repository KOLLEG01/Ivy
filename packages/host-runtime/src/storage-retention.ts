import { lstat, readdir, realpath, rm } from 'node:fs/promises';
import { basename, dirname, join } from 'node:path';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import { hashJson } from '../../contracts/src/canonical.js';
import type { Host } from '../../contracts/src/generated.js';
import { retainedBootstrapPlans } from './bootstrap.js';
import { readReleaseManifest } from './artifact.js';
import { jsonFile } from './config.js';
import { ExecutorLock, HostJournal } from './journal.js';
import { installationIdentity } from './linux-resources.js';
import { collectPreparationStorage, compactPreparations } from './preparations.js';
import { hostStorageSpace } from './storage-space.js';
import type { RetentionProgress } from './storage-space.js';
import { collectDependencyCaches } from './dependency-cache.js';

const buildName = /^[0-9a-f]{64}$/;
const versionName = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;
const graceMs = 3 * 24 * 60 * 60 * 1000;
const missing = (error: unknown) => (error as NodeJS.ErrnoException).code === 'ENOENT';
type MaintenanceHistory = Array<{ phase: string; updatedAt: string; request: Host.BootstrapMaintenanceRequest;
  kind: string; directory: string; record: unknown }>;

function protectPlan(ids: Set<string>, plan: Host.BootstrapPlan): void {
  validateHost('BootstrapPlan', plan);
  ids.add(plan.candidateId);
  for (const process of plan.processes) if (process.candidateId) ids.add(process.candidateId);
}

async function maintenanceHistory(config: Host.HostConfig, name: string, progress: RetentionProgress): Promise<MaintenanceHistory> {
  const result: MaintenanceHistory = [];
  const root = join(config.runtimeRoot, name);
  const entries = await readdir(root, { withFileTypes: true }).catch(error => { if (missing(error)) return []; throw error; });
  for (const entry of entries) {
    if (!entry.isDirectory() || entry.isSymbolicLink() || !buildName.test(entry.name)) {
      progress.skipped('maintenance', 'unrecognized_history_entry', true); continue;
    }
    const directory = join(root, entry.name);
    const record = await jsonFile<{ phase: string; updatedAt?: string; request?: Host.BootstrapMaintenanceRequest }>(join(directory, 'record.json')).catch(error => {
      if (missing(error)) return null;
      throw error;
    });
    const request = record?.request ?? await jsonFile<Host.BootstrapMaintenanceRequest>(join(directory, 'request.json')).catch(error => {
      if (missing(error)) return null;
      throw error;
    });
    if (!request && record && ['succeeded', 'rolled_back'].includes(record.phase)) { progress.skipped('maintenance', 'unrecognized_completed_record', true); continue; }
    requireThat(request, 'storage_invalid', 'Unrecognized bootstrap maintenance prevents candidate collection.');
    try { validateHost('BootstrapMaintenanceRequest', request); }
    catch (error) { if (record && ['succeeded', 'rolled_back'].includes(record.phase)) { progress.skipped('maintenance', 'unrecognized_completed_record', true); continue; } throw error; }
    requireThat(hashJson(request.operationId).slice(7) === entry.name, 'storage_invalid', 'Maintenance history does not identify its owned directory.');
    result.push({ phase: record?.phase ?? 'unknown', updatedAt: record?.updatedAt ?? '', request, kind: name, directory, record });
  }
  return result;
}

/** Caller holds bootstrap maintenance's lock; history alone does not pin releases. */
async function collectBootstrapPlans(config: Host.HostConfig, journal: HostJournal, ids: Set<string>, cutoff: number,
  history: MaintenanceHistory, progress: RetentionProgress): Promise<Set<string>> {
  const protectedHistory = new Set<string>();
  const plans = await retainedBootstrapPlans(config), keep = new Set<string>();
  if (!plans.length) return protectedHistory;
  const selected = await jsonFile<Host.BootstrapPlan>(join(config.runtimeRoot, 'bootstrap', installationIdentity(config) + '.json'));
  validateHost('BootstrapPlan', selected);
  requireThat(plans.some(plan => hashJson(plan) === hashJson(selected)), 'storage_invalid', 'Selected bootstrap plan must belong to this installation.');
  const retain = (plan: Host.BootstrapPlan) => { keep.add(hashJson(plan)); protectPlan(ids, plan); };
  retain(selected);
  let previousSelected = false;
  for (const entry of history) {
    const unresolved = !['succeeded', 'rolled_back'].includes(entry.phase);
    const previous = !previousSelected && entry.phase === 'succeeded' && entry.request.next.processes.length === selected.processes.length &&
      selected.processes.every(process => entry.request.next.processes.some(owner => owner.instanceId === process.instanceId &&
        (owner.candidateId ?? entry.request.next.candidateId) === (process.candidateId ?? selected.candidateId)));
    if (!unresolved && !previous) continue;
    protectedHistory.add(entry.directory);
    if (previous) previousSelected = true;
    if (unresolved) retain(entry.request.next);
    const snapshot = entry.request.previous;
    for (const owner of snapshot.owners) ids.add(owner.candidateId);
    for (const plan of plans) {
      if (plan.configHash === snapshot.configurationHash && plan.processes.length === snapshot.owners.length &&
        snapshot.owners.every(owner => plan.processes.some(process => process.instanceId === owner.instanceId &&
          process.name === owner.name && (process.candidateId ?? plan.candidateId) === owner.candidateId))) retain(plan);
    }
  }
  const inventory = await Promise.all(plans.map(async plan => {
    const path = join(config.runtimeRoot, 'bootstrap-plans', hashJson(plan).slice(7) + '.json');
    const metadata = await lstat(path).catch(error => { if (missing(error)) return null; throw error; });
    return { plan, path, metadata };
  }));
  inventory.sort((a, b) => (b.metadata?.mtimeMs ?? 0) - (a.metadata?.mtimeMs ?? 0));
  // Preserve one exact OS plan for each live/rollback release even when installs
  // predate maintenance records. Do not recursively retain every historical plan.
  for (const id of journal.retentionCandidateIds()) {
    const matches = (plan: Host.BootstrapPlan) => plan.candidateId === id || plan.processes.some(process => process.candidateId === id);
    if (plans.some(plan => keep.has(hashJson(plan)) && matches(plan))) continue;
    const prior = inventory.find(value => matches(value.plan));
    if (prior) retain(prior.plan);
  }
  for (const { plan, path, metadata } of inventory) {
    if (keep.has(hashJson(plan))) continue;
    if (!metadata || metadata.mtimeMs > cutoff) { retain(plan); continue; }
    requireThat(metadata.isFile() && !metadata.isSymbolicLink(), 'storage_invalid', 'Bootstrap history must remain a regular file.');
    await rm(path);
    progress.removed('bootstrapPlans');
  }
  return protectedHistory;
}

async function collectMaintenanceHistory(config: Host.HostConfig, history: MaintenanceHistory,
  protectedHistory: Set<string>, now: number, progress: RetentionProgress): Promise<void> {
  const activeOperations = new Set(history.filter(entry => !['succeeded', 'rolled_back'].includes(entry.phase)).flatMap(entry =>
    [entry.request.operationId, entry.request.operationId + '-rollback']));
  let completed = 0;
  for (const entry of history) {
    // Automatic operations already contain only their small request and outcome records.
    if (entry.kind !== 'bootstrap-maintenance') continue;
    if (!['succeeded', 'rolled_back'].includes(entry.phase)) { progress.skipped('maintenance', 'unfinished_operation', true); continue; }
    if (++completed <= 20 || protectedHistory.has(entry.directory) || activeOperations.has(entry.request.operationId) ||
      !(Date.parse(entry.updatedAt) < now - 30 * 86400000)) { progress.skipped('maintenance', 'retained_history'); continue; }
    try {
      const value = entry.record as Host.BootstrapMaintenanceRecord;
      validateHost('BootstrapMaintenanceRecord', value);
      requireThat(value.operationId === entry.request.operationId && value.requestHash === hashJson(entry.request),
        'storage_invalid', 'Completed bootstrap record is invalid.');
      const current = await jsonFile(join(entry.directory, 'record.json'));
      requireThat(hashJson(current) === hashJson(value),
        'storage_invalid', 'Completed maintenance history changed before collection.');
      const metadata = await lstat(entry.directory), actual = await realpath(entry.directory);
      requireThat(metadata.isDirectory() && !metadata.isSymbolicLink() && dirname(actual) === await realpath(join(config.runtimeRoot, entry.kind)),
        'target_conflict', 'Maintenance history leaves its owned root.');
      // Leave record.json unchanged for replay and restore. Missing detail files simply mean an earlier pass removed them.
      for (const definition of value.definitions) for (const suffix of ['prior', 'desired']) {
        const path = join(entry.directory, definition.instanceId + '.' + suffix + '.json');
        const file = await lstat(path).catch(error => { if (missing(error)) return null; throw error; });
        if (!file) continue;
        requireThat(file.isFile() && !file.isSymbolicLink() && dirname(await realpath(path)) === actual,
          'target_conflict', 'Maintenance detail must be an owned regular file.');
        await rm(path); progress.removed('maintenance');
      }
    } catch (error) { progress.skipped('maintenance', IvyError.from(error).code, true); }
  }
}

async function ownedCandidateDirectory(config: Host.HostConfig, id: string): Promise<{ path: string; mtimeMs: number } | null> {
  const parent = join(config.artifactRoot, 'candidates'), path = join(parent, id);
  const metadata = await lstat(path).catch(error => { if (missing(error)) return null; throw error; });
  if (!metadata || !metadata.isDirectory() || metadata.isSymbolicLink()) return null;
  const actualParent = await realpath(parent), actual = await realpath(path);
  if (dirname(actual) !== actualParent || basename(actual) !== id) return null;
  return { path, mtimeMs: metadata.mtimeMs };
}

/** Bounded, idempotent host collection. Unknown evidence remains for explicit review. */
export async function collectHostStorage(config: Host.HostConfig, now = Date.now(), trigger: Host.StorageRetentionStatus['trigger'] = 'manual'): Promise<{
  candidates: number; preparations: number; snapshots: number;
}> {
  const lock = new ExecutorLock(join(config.runtimeRoot, 'artifact-retention-lock'));
  let bootstrapLock: ExecutorLock | null = null;
  let journal: HostJournal | null = null;
  let status: Host.StorageRetentionStatus | null = null;
  const progress: RetentionProgress = {
    removed(area) { status!.removed[area]++; },
    skipped(area, reason, requiresAttention = false) {
      if (status!.skipped.length >= 63 && !status!.skipped.some(entry => entry.area === area && entry.reason === reason)) {
        area = 'other'; reason = 'additional_retained_entries';
      }
      const entry = status!.skipped.find(entry => entry.area === area && entry.reason === reason);
      if (entry) { entry.count++; entry.requiresAttention ||= requiresAttention; }
      else status!.skipped.push({ area, reason, count: 1, requiresAttention });
    },
  };
  try {
    journal = new HostJournal(config);
    status = { schemaVersion: 1, hostId: config.hostId, state: 'running', trigger, startedAt: new Date(now).toISOString(),
      completedAt: null, lastSuccessAt: journal.storageRetentionStatus()?.lastSuccessAt ?? null,
      removed: { candidates: 0, preparations: 0, payloads: 0, snapshots: 0, maintenance: 0, bootstrapPlans: 0 },
      ...await hostStorageSpace(config), skipped: [], errorCode: null };
    journal.saveStorageRetentionStatus(status);
    bootstrapLock = new ExecutorLock(join(config.runtimeRoot, 'bootstrap-maintenance-lock'));
    await collectDependencyCaches(config.stagingRoot, progress);
    const compaction = await compactPreparations(config, undefined, false);
    for (const entry of compaction.entries) {
      if (['compacted', 'recovered'].includes(entry.action)) progress.removed('payloads');
      else if (entry.code && entry.code !== 'already_compacted') progress.skipped('payloads', entry.code, true);
    }
    const cutoff = now - graceMs;
    const preparation = await collectPreparationStorage(config, journal, cutoff, progress);
    const protectedIds = new Set([...preparation.protectedCandidates, ...journal.retentionCandidateIds()]);
    const history = [...await maintenanceHistory(config, 'bootstrap-maintenance', progress), ...await maintenanceHistory(config, 'bootstrap-automatic', progress)]
      .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
    const protectedHistory = await collectBootstrapPlans(config, journal, protectedIds, cutoff, history, progress);
    await collectMaintenanceHistory(config, history, protectedHistory, now, progress);
    // Requests can also exist before the first selected bootstrap plan is written.
    for (const entry of history) {
      if (['succeeded', 'rolled_back'].includes(entry.phase)) continue;
      protectPlan(protectedIds, entry.request.next);
      for (const owner of entry.request.previous.owners) protectedIds.add(owner.candidateId);
    }
    const rows = journal.db.prepare('SELECT candidate_id,manifest_json FROM candidates ORDER BY component_id,candidate_id').all();
    requireThat(rows.length <= 10000, 'limit_exceeded', 'Host candidate inventory exceeds its retention limit.');
    const candidates = rows.map(row => journal!.candidate(String(row['candidate_id'])));
    const versions = new Map(rows.map(row => {
      const version = (JSON.parse(String(row['manifest_json'])) as { version?: unknown }).version;
      return [String(row['candidate_id']), typeof version === 'string' && versionName.test(version) ? version : null] as const;
    }));
    for (const [id, version] of versions) if (!version) protectedIds.add(id);
    let removed = 0;
    const failures: unknown[] = [];
    const finishRemoval = async (candidate: Host.Candidate) => {
      const path = join(config.artifactRoot, 'candidates', candidate.candidateId.slice(7));
      requireThat(candidate.artifactRoot === join(path, 'artifact') && candidate.manifestPath === join(path, 'component.json') &&
        (!candidate.archivePath || candidate.archivePath === join(path, 'package.tar.gz')), 'target_conflict', 'Retiring candidate paths changed.');
      const directory = await ownedCandidateDirectory(config, candidate.candidateId.slice(7));
      if (directory) await rm(directory.path, { recursive: true });
      else requireThat(!await lstat(path).catch(error => { if (missing(error)) return null; throw error; }),
        'target_conflict', 'Retiring candidate directory is no longer owned.');
      journal!.finishCandidateRetirement(candidate.candidateId);
      progress.removed('candidates');
      removed++;
    };
    for (const candidate of journal.pendingCandidateRetirements()) {
      let buildLock: ExecutorLock | null = null;
      try {
        buildLock = new ExecutorLock(join(config.stagingRoot, 'build-locks', candidate.buildId.slice(7)));
        await finishRemoval(candidate);
      } catch (error) { failures.push(error); progress.skipped('candidates', IvyError.from(error).code, true); }
      finally { buildLock?.close(); }
    }
    for (const candidate of candidates) {
      const id = candidate.candidateId;
      if (protectedIds.has(id)) { progress.skipped('candidates', 'protected_release'); continue; }
      const directory = await ownedCandidateDirectory(config, id.slice(7));
      if (directory && directory.mtimeMs > cutoff) { progress.skipped('candidates', 'grace_period'); continue; }
      if (!directory || candidate.artifactRoot !== join(directory.path, 'artifact') ||
        candidate.manifestPath !== join(directory.path, 'component.json') ||
        (candidate.archivePath && candidate.archivePath !== join(directory.path, 'package.tar.gz'))) { progress.skipped('candidates', 'unrecognized_path', true); continue; }
      const manifestMatches = await readReleaseManifest(candidate.manifestPath).then(
        manifest => hashJson(manifest) === hashJson(journal!.manifest(id)), () => false);
      if (!manifestMatches) { progress.skipped('candidates', 'unrecognized_manifest', true); continue; }
      let buildLock: ExecutorLock | null = null;
      try {
        buildLock = new ExecutorLock(join(config.stagingRoot, 'build-locks', id.slice(7)));
        if (!journal.retireCandidate(id)) { progress.skipped('candidates', 'protected_release'); continue; }
        await finishRemoval(candidate);
      } catch (error) {
        progress.skipped('candidates', IvyError.from(error).code, true);
        if (!(error instanceof IvyError && error.code === 'executor_already_running')) failures.push(error);
      } finally { buildLock?.close(); }
    }
    if (failures.length) throw failures[0];
    status.state = status.skipped.some(entry => entry.requiresAttention) ? 'partial' : 'succeeded';
    return { candidates: removed, preparations: preparation.preparations, snapshots: preparation.snapshots };
  } catch (error) {
    if (status) { status.state = 'failed'; status.errorCode = IvyError.from(error).code; }
    throw error;
  } finally {
    try {
      if (journal && status) {
        status.completedAt = new Date().toISOString();
        try { Object.assign(status, await hostStorageSpace(config)); }
        catch { status.state = 'failed'; status.errorCode ??= 'storage_probe_failed'; }
        if (status.state === 'succeeded') status.lastSuccessAt = status.completedAt;
        journal.saveStorageRetentionStatus(status);
      }
    } finally { journal?.close(); bootstrapLock?.close(); lock.close(); }
  }
}
