import { lstat, open, readdir, realpath, rename, rm } from 'node:fs/promises';
import { basename, dirname, join, resolve } from 'node:path';
import { atomicJson, inside, jsonFile } from './config.js';
import { ExecutorLock, HostJournal } from './journal.js';
import { readReleaseManifest, verifyCandidate } from './artifact.js';
import { sourceBuildPlan, verifySnapshot } from './source.js';
import { availableStorageBytes, treeBytes } from './storage-space.js';
import type { RetentionProgress } from './storage-space.js';
import { hashJson } from '../../contracts/src/canonical.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import type { Host } from '../../contracts/src/generated.js';

const buildName = /^build-([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/;
const snapshotName = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const now = () => new Date().toISOString();
const rootOf = (host: Host.HostConfig, id: string) => join(host.stagingRoot, 'build-' + id);
const preparationId = (buildId: string): string => {
  requireThat(/^sha256:[0-9a-f]{64}$/.test(buildId), 'build_mismatch', 'Preparation needs an exact release build identity.');
  const hash = buildId.slice(7);
  return `${hash.slice(0, 8)}-${hash.slice(8, 12)}-5${hash.slice(13, 16)}-${((parseInt(hash[16]!, 16) & 3) | 8).toString(16)}${hash.slice(17, 20)}-${hash.slice(20, 32)}`;
};
const missing = (error: unknown) => (error as NodeJS.ErrnoException).code === 'ENOENT';
async function exists(path: string) { try { await lstat(path); return true; } catch (error) { if (missing(error)) return false; throw error; } }

async function ownedRoot(host: Host.HostConfig, root: string): Promise<string> {
  requireThat(buildName.test(basename(root)) && dirname(resolve(root)) === resolve(host.stagingRoot), 'target_conflict', 'Preparation must be one directly owned build directory.');
  const metadata = await lstat(root), staging = await realpath(host.stagingRoot), actual = await realpath(root);
  requireThat(metadata.isDirectory() && !metadata.isSymbolicLink() && dirname(actual) === staging && basename(actual) === basename(root), 'target_conflict', 'Preparation directory cannot traverse a link.');
  return actual;
}
async function roots(host: Host.HostConfig): Promise<string[]> {
  const names = await readdir(host.stagingRoot).catch(error => { if (missing(error)) return []; throw error; });
  const result = names.filter(name => buildName.test(name)).sort().map(name => join(host.stagingRoot, name));
  requireThat(result.length <= 10000, 'limit_exceeded', 'Preparation directory count exceeds its bounded inventory.'); return result;
}
export async function savePreparation(host: Host.HostConfig, record: Host.PreparationRecord): Promise<void> {
  validateHost('PreparationRecord', record); const root = rootOf(host, record.preparationId); await ownedRoot(host, root);
  await atomicJson(join(root, 'preparation.json'), record);
}
async function readRecord(host: Host.HostConfig, root: string): Promise<Host.PreparationRecord | null> {
  await ownedRoot(host, root);
  let record: Host.PreparationRecord;
  try { record = await jsonFile<Host.PreparationRecord>(join(root, 'preparation.json'), 32768); }
  catch (error) { if (missing(error)) return null; throw error; }
  validateHost('PreparationRecord', record);
  requireThat(rootOf(host, record.preparationId) === root && (!record.candidate || record.candidate.buildId === record.buildId && record.candidate.componentId === record.componentId), 'build_mismatch', 'Preparation record does not match its release identity.');
  return record;
}
async function publication(journal: HostJournal, record: Host.PreparationRecord, verifyRelease: boolean): Promise<{ candidate: Host.Candidate; recovered: boolean }> {
  const host = journal.config, candidate = record.candidate;
  requireThat(candidate, 'preparation_incomplete', 'Preparation has no complete publication identity.');
  const directory = join(host.artifactRoot, 'candidates', candidate.candidateId.slice(7));
  requireThat(candidate.artifactRoot === join(directory, 'artifact') && (!candidate.archivePath || candidate.archivePath === join(directory, 'package.tar.gz')) && candidate.manifestPath === join(directory, 'component.json'), 'target_conflict', 'Publication paths must identify the exact candidate directory.');
  if (verifyRelease) await verifySnapshot(record.snapshot, host);
  const manifest = verifyRelease ? await verifyCandidate(candidate, host) : await readReleaseManifest(candidate.manifestPath);
  const plan = await sourceBuildPlan(record.snapshot.sourceRoot, record.componentId);
  const expected = { schemaVersion: 1, componentId: plan.componentId, kind: plan.kind, version: plan.version, buildId: candidate.buildId,
    connectsToHive: plan.connectsToHive, requirements: plan.requirements,
    ...(plan.kind === 'app' ? { app: plan.app! } : { entrypoint: plan.entrypoint!, readiness: plan.readiness!,
      shutdown: plan.shutdown!, restart: plan.restart! }), ...(plan.storage ? { storage: plan.storage } : {}),
    ...(plan.runtimeReset ? { runtimeReset: plan.runtimeReset } : {}) };
  requireThat(hashJson(expected) === hashJson(manifest), 'build_mismatch', 'Recovered release manifest differs from its accepted source plan.');
  requireThat(plan.checks.length === 0, 'checks_required', 'Recovered publication contains unsupported diagnostic checks.');
  let recovered = false;
  try { requireThat(hashJson(journal.candidate(candidate.candidateId)) === hashJson(candidate), 'build_mismatch', 'Published candidate differs from its host journal identity.'); }
  catch (error) { if (!(error instanceof IvyError && error.code === 'not_found')) throw error; recovered = true; }
  journal.saveCandidate(candidate, manifest); return { candidate, recovered };
}
/** Caller holds this exact build's OS lock. Unresolved or failed work is never compacted. */
export async function compactPreparation(journal: HostJournal, record: Host.PreparationRecord, trustedPublication = false, measureBytes = false): Promise<Host.PreparationCompaction['entries'][number]> {
  const root = rootOf(journal.config, record.preparationId), boundary = await ownedRoot(journal.config, root);
  requireThat(['publishing', 'verified', 'compacted'].includes(record.phase) || record.phase === 'unknown' && record.candidate, 'preparation_incomplete', 'Only completed publication may be compacted.');
  requireThat(record.candidate, 'preparation_incomplete', 'Completed preparation needs its original candidate identity.');
  if (record.phase === 'compacted' && !(await Promise.all(['source', 'artifact', 'artifact.tar.gz'].map(name => exists(join(root, name))))).some(Boolean)) {
    try {
      requireThat(hashJson(journal.candidate(record.candidate.candidateId)) === hashJson(record.candidate), 'build_mismatch', 'Compacted preparation and registered candidate differ.');
      return { preparationId: record.preparationId, action: 'retained', candidateId: record.candidate.candidateId, reclaimedBytes: 0, code: 'already_compacted' };
    } catch (error) { if (!(error instanceof IvyError && error.code === 'not_found')) throw error; }
    // A lost registration still requires full original publication verification and recovery.
  }
  const { candidate, recovered } = await publication(journal, record, !trustedPublication);
  for (const path of [record.snapshot.sourceRoot, record.snapshot.manifestPath, candidate.artifactRoot, ...(candidate.archivePath ? [candidate.archivePath] : []), candidate.manifestPath])
    requireThat(!inside(boundary, await realpath(path)), 'target_conflict', 'Compaction cannot remove an authoritative source snapshot or published candidate.');
  const paths: { path: string; directory: boolean; bytes: number }[] = [];
  for (const name of ['source', 'artifact', 'artifact.tar.gz']) {
    const path = join(root, name); if (!await exists(path)) continue;
    const metadata = await lstat(path);
    requireThat(!metadata.isSymbolicLink() && (name === 'artifact.tar.gz' ? metadata.isFile() : metadata.isDirectory()), 'target_conflict', 'Compaction payload root must be a regular owned file/directory.');
    requireThat(dirname(await realpath(path)) === boundary, 'target_conflict', 'Compaction target leaves its exact owned build.');
    // Optional diagnostics must never prevent removal of an independently verified payload.
    paths.push({ path, directory: metadata.isDirectory(), bytes: measureBytes ? await treeBytes(path, boundary).catch(() => 0) : 0 });
  }
  if (record.errorCode && !await exists(join(root, 'preparation-before-recovery.json'))) await atomicJson(join(root, 'preparation-before-recovery.json'), record);
  await savePreparation(journal.config, { ...record, phase: 'verified', updatedAt: now(), errorCode: null });
  for (const value of paths) {
    await ownedRoot(journal.config, root);
    requireThat(!(await lstat(value.path)).isSymbolicLink() && dirname(await realpath(value.path)) === boundary, 'target_conflict', 'Compaction target changed before removal.');
    await rm(value.path, { recursive: value.directory, maxRetries: 3, retryDelay: 100 });
  }
  await savePreparation(journal.config, { ...record, phase: 'compacted', updatedAt: now(), errorCode: null });
  return { preparationId: record.preparationId, action: recovered || record.phase === 'unknown' || record.phase === 'publishing' ? 'recovered' : 'compacted', candidateId: candidate.candidateId, reclaimedBytes: paths.reduce((sum, value) => sum + value.bytes, 0), code: null };
}
/** Caller holds the build lock, so a stale building record cannot belong to an active builder. */
export async function recoverPreparedBuild(journal: HostJournal, buildId: string): Promise<Host.Candidate | null> {
  // A build's preparation directory is derived from its immutable build ID.
  // Recovery can therefore inspect the one relevant record instead of walking
  // every historical preparation before every deployment.
  const root = rootOf(journal.config, preparationId(buildId));
  if (!await exists(root)) return null;
  const record = await readRecord(journal.config, root);
  if (record) {
    requireThat(record.buildId === buildId, 'build_mismatch', 'Preparation directory belongs to a different release build.');
    if (record.phase === 'building') { await savePreparation(journal.config, { ...record, phase: 'unknown', updatedAt: now(), errorCode: 'preparation_interrupted' }); return null; }
    if (!record.candidate) return null;
    if (record.phase === 'unknown' || record.phase === 'failed') throw new IvyError('outcome_unknown', 'Prior candidate publication requires explicit reconciliation before another preparation.', 'unknown', { preparationId: record.preparationId });
    if (record.phase === 'compacted') {
      try { return journal.candidate(record.candidate.candidateId); } catch (error) { if (!(error instanceof IvyError && error.code === 'not_found')) throw error; }
    }
    try { await compactPreparation(journal, record); return record.candidate; }
    catch (error) {
      let registered: Host.Candidate | null = null;
      try { registered = journal.candidate(record.candidate.candidateId); await verifyCandidate(registered, journal.config); } catch { registered = null; }
      await savePreparation(journal.config, { ...record, phase: registered ? 'verified' : 'unknown', updatedAt: now(), errorCode: IvyError.from(error).code });
      if (registered) return registered;
      throw new IvyError('outcome_unknown', 'Interrupted publication could not be verified; its evidence is retained.', 'unknown', { preparationId: record.preparationId, code: IvyError.from(error).code });
    }
  }
  return null;
}

export const preparationIdForBuild = preparationId;
export async function preparationInventory(host: Host.HostConfig): Promise<Host.PreparationInventory> {
  const entries: Host.PreparationInventory['entries'] = [];
  for (const root of await roots(host)) {
    const entry: Host.PreparationInventory['entries'][number] = { preparationId: basename(root).slice(6), path: root, phase: 'unrecognized', buildId: null, candidateId: null, bytes: 0, code: null };
    try {
      const record = await readRecord(host, root);
      if (record) { entry.phase = record.phase; entry.buildId = record.buildId; entry.candidateId = record.candidate?.candidateId ?? null; entry.code = record.errorCode; }
      entry.bytes = await treeBytes(root, await ownedRoot(host, root));
    } catch (error) { entry.code = IvyError.from(error).code; }
    entries.push(entry);
  }
  const result: Host.PreparationInventory = { schemaVersion: 1, hostId: host.hostId, observedAt: now(), availableBytes: await availableStorageBytes(host.stagingRoot), entries };
  validateHost('PreparationInventory', result); return result;
}
/** Acquires the same kernel lock as builders; old failed/unrecognized state stays untouched. */
export async function compactPreparations(host: Host.HostConfig, onlyBuildId?: string, measureBytes = true): Promise<Host.PreparationCompaction> {
  const journal = new HostJournal(host), entries: Host.PreparationCompaction['entries'] = [];
  try {
    for (const root of await roots(host)) {
      const entry: Host.PreparationCompaction['entries'][number] = { preparationId: basename(root).slice(6), action: 'retained', candidateId: null, reclaimedBytes: 0, code: null };
      let lock: ExecutorLock | null = null;
      try {
        let record = await readRecord(host, root);
        if (!record) { entry.code = 'preparation_unrecognized'; entries.push(entry); continue; }
        if (onlyBuildId && record.buildId !== onlyBuildId) continue;
        entry.candidateId = record.candidate?.candidateId ?? null;
        lock = new ExecutorLock(join(host.stagingRoot, 'build-locks', record.buildId.slice(7)));
        record = await readRecord(host, root);
        requireThat(record, 'preparation_unrecognized', 'The original preparation identity must remain present after acquiring its lock.');
        if (record.phase === 'building') await savePreparation(host, { ...record, phase: 'unknown', updatedAt: now(), errorCode: 'preparation_interrupted' });
        if (!['publishing', 'verified', 'compacted'].includes(record.phase) && !(record.phase === 'unknown' && record.candidate)) { entry.code = record.errorCode ?? 'preparation_interrupted'; entries.push(entry); continue; }
        entries.push(await compactPreparation(journal, record, false, measureBytes));
      } catch (error) { entry.code = IvyError.from(error).code; if (entry.code === 'executor_already_running') entry.action = 'busy'; entries.push(entry); }
      finally { lock?.close(); }
    }
    const result: Host.PreparationCompaction = { schemaVersion: 1, hostId: host.hostId, observedAt: now(), entries }; validateHost('PreparationCompaction', result); return result;
  } finally { journal.close(); }
}

type RetiredArea = 'preparations' | 'snapshots';
async function removeRetiredDirectory(path: string, area: RetiredArea, progress?: RetentionProgress): Promise<number> {
  try {
    const parent = dirname(path), metadata = await lstat(path), actual = await realpath(path);
    requireThat(metadata.isDirectory() && !metadata.isSymbolicLink() && dirname(actual) === await realpath(parent) && basename(actual) === basename(path),
      'target_conflict', 'Retired staging directory must remain directly owned.');
    // Persist the rename before removing its contents, including when resuming after a crash.
    if (process.platform !== 'win32') { const directory = await open(parent, 'r'); try { await directory.sync(); } finally { await directory.close(); } }
    await rm(path, { recursive: true });
    progress?.removed(area); return 1;
  } catch (error) { progress?.skipped(area, IvyError.from(error).code, true); return 0; }
}
async function collectRetiredDirectories(parent: string, pattern: RegExp, area: RetiredArea, progress?: RetentionProgress): Promise<number> {
  const names = await readdir(parent).catch(error => { if (missing(error)) return []; throw error; });
  let removed = 0;
  for (const name of names) if (name.startsWith('.retiring-') && pattern.test(name.slice('.retiring-'.length)))
    removed += await removeRetiredDirectory(join(parent, name), area, progress);
  return removed;
}
/** Caller verified eligibility and ownership. The renamed directory itself records deletion intent. */
async function retireDirectory(path: string, area: RetiredArea, progress?: RetentionProgress): Promise<number> {
  const retired = join(dirname(path), '.retiring-' + basename(path));
  requireThat(!await exists(retired), 'mutation_conflict', 'An earlier staging deletion is still pending.');
  await rename(path, retired);
  return removeRetiredDirectory(retired, area, progress);
}

/** Retire only completed build evidence and unreferenced source snapshots after a grace period. */
export async function collectPreparationStorage(host: Host.HostConfig, journal: HostJournal, cutoff: number, progress?: RetentionProgress): Promise<{
  preparations: number; snapshots: number; protectedCandidates: Set<string>;
}> {
  const protectedSnapshots = new Set<string>(), protectedCandidates = new Set<string>();
  const sourceRoot = join(host.stagingRoot, 'sources');
  let preparations = await collectRetiredDirectories(host.stagingRoot, buildName, 'preparations', progress),
    snapshots = await collectRetiredDirectories(sourceRoot, snapshotName, 'snapshots', progress), unknownPreparation = false;
  const protectUnknown = (root: string) => {
    unknownPreparation = true;
    progress?.skipped('preparations', 'unrecognized_preparation', true);
    // A crash may leave only the deterministic directory name. Keep every matching
    // build (including hash-prefix collisions) and all snapshots of unknown ownership.
    for (const row of journal.db.prepare('SELECT candidate_id,build_id FROM candidates').all()) {
      if (preparationId(String(row['build_id'])) === basename(root).slice(6)) protectedCandidates.add(String(row['candidate_id']));
    }
  };
  const operationRows = journal.db.prepare("SELECT entry_json FROM operations WHERE phase NOT IN ('succeeded','rolled_back')").all();
  for (const row of operationRows) {
    const entry = JSON.parse(String(row['entry_json'])) as Host.JournalEntry;
    if (entry.sourceSnapshot) {
      validateHost('SourceSnapshot', entry.sourceSnapshot);
      protectedSnapshots.add(entry.sourceSnapshot.snapshotId);
    }
  }
  for (const root of await roots(host)) {
    let record: Host.PreparationRecord | null;
    try { record = await readRecord(host, root); }
    catch { protectUnknown(root); continue; }
    if (!record) { protectUnknown(root); continue; }
    // Retain the original references even if a later read or removal fails.
    const original = record;
    if (record.phase === 'compacted' && Date.parse(record.updatedAt) <= cutoff) {
      let lock: ExecutorLock | null = null;
      try {
        lock = new ExecutorLock(join(host.stagingRoot, 'build-locks', record.buildId.slice(7)));
        record = await readRecord(host, root);
        requireThat(record, 'preparation_unrecognized', 'Preparation changed while collecting its history.');
        if (record.phase === 'compacted' && Date.parse(record.updatedAt) <= cutoff && record.candidate &&
          hashJson(journal.candidate(record.candidate.candidateId)) === hashJson(record.candidate)) {
          const names = await readdir(root);
          requireThat(names.every(name => ['preparation.json', 'preparation-before-recovery.json'].includes(name)),
            'target_conflict', 'Compacted preparation contains unrecognized retained evidence.');
          await ownedRoot(host, root);
          preparations += await retireDirectory(root, 'preparations', progress);
          continue;
        }
      } catch (error) {
        progress?.skipped('preparations', IvyError.from(error).code, true);
        if (!['executor_already_running', 'not_found'].includes(IvyError.from(error).code)) protectUnknown(root);
      } finally { lock?.close(); }
    }
    progress?.skipped('preparations', record?.phase === 'compacted' ? 'grace_period' : 'unfinished_preparation', record?.phase !== 'compacted');
    for (const value of [original, ...(record ? [record] : [])]) {
      protectedSnapshots.add(value.snapshot.snapshotId);
      protectedCandidates.add(value.buildId);
      if (value.candidate) protectedCandidates.add(value.candidate.candidateId);
    }
  }
  if (unknownPreparation) {
    progress?.skipped('snapshots', 'unknown_preparation_ownership', true);
    return { preparations, snapshots, protectedCandidates };
  }
  const entries = await readdir(sourceRoot, { withFileTypes: true }).catch(error => { if (missing(error)) return []; throw error; });
  const boundary = entries.length ? await realpath(sourceRoot) : null;
  for (const entry of entries) {
    if (!snapshotName.test(entry.name) || !entry.isDirectory() || entry.isSymbolicLink()) continue;
    if (protectedSnapshots.has(entry.name)) { progress?.skipped('snapshots', 'referenced'); continue; }
    const path = join(sourceRoot, entry.name), metadata = await lstat(path), actual = await realpath(path);
    if (metadata.mtimeMs > cutoff) { progress?.skipped('snapshots', 'grace_period'); continue; }
    if (dirname(actual) !== boundary || basename(actual) !== entry.name) { progress?.skipped('snapshots', 'unrecognized_path', true); continue; }
    const manifest = await jsonFile<Host.SnapshotManifest>(join(path, 'snapshot.json'), 8 * 1024 * 1024).catch(() => null);
    if (!manifest) { progress?.skipped('snapshots', 'unrecognized_manifest', true); continue; }
    try { validateHost('SnapshotManifest', manifest); } catch { progress?.skipped('snapshots', 'unrecognized_manifest', true); continue; }
    if (manifest.snapshot.snapshotId !== entry.name || manifest.snapshot.manifestPath !== join(path, 'snapshot.json') ||
      manifest.snapshot.sourceRoot !== join(path, 'source')) { progress?.skipped('snapshots', 'unrecognized_path', true); continue; }
    snapshots += await retireDirectory(path, 'snapshots', progress);
  }
  return { preparations, snapshots, protectedCandidates };
}
