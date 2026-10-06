import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readdir, readFile, rm, stat, symlink, utimes, writeFile } from 'node:fs/promises';
import filesystem from 'node:fs/promises';
import { syncBuiltinESMExports } from 'node:module';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { ExecutorLock, HostJournal } from '../packages/host-runtime/src/journal.js';
import { collectHostStorage } from '../packages/host-runtime/src/storage-retention.js';
import { compactPreparations, preparationIdForBuild, savePreparation } from '../packages/host-runtime/src/preparations.js';
import { readReleaseManifest } from '../packages/host-runtime/src/artifact.js';
import { configurationBootstrapPlan, retainBootstrapPlan, retainedBootstrapPlans } from '../packages/host-runtime/src/bootstrap.js';
import { atomicJson } from '../packages/host-runtime/src/config.js';
import { PackageUpdater } from '../packages/host-runtime/src/package-updater.js';
import { bootstrapStatus, updateBootstrap } from '../packages/host-runtime/src/bootstrap-maintenance.js';
import { automaticBootstrapRecord } from '../packages/host-runtime/src/bootstrap-automatic.js';
import type { BootstrapOs } from '../packages/host-runtime/src/bootstrap-os.js';
import { hostStorageSpace, RetentionSchedule, storageIsLow, treeBytes } from '../packages/host-runtime/src/storage-space.js';
import { hashJson } from '../packages/contracts/src/canonical.js';
import type { Host } from '../packages/contracts/src/generated.js';

async function fixture(t: TestContext, componentId = 'fixture', kind: 'native' | 'app' = 'native') {
  const root = await mkdtemp(join(tmpdir(), 'ivy-storage-retention-'));
  const config: Host.HostConfig = { schemaVersion: 1, hostId: 'retention-fixture', runtimeRoot: join(root, 'runtime'),
    artifactRoot: join(root, 'artifacts'), stagingRoot: join(root, 'staging'), publicBaseUrl: 'http://127.0.0.1:38081/ivy',
    executables: { node: process.execPath }, instances: [{ instanceId: componentId, serviceNodeId: componentId,
      componentId, enabled: true, engine: 'process', settings: {} }] };
  const journal = new HostJournal(config), old = new Date(Date.now() - 5 * 24 * 60 * 60 * 1000);
  t.after(async () => { journal.close(); await rm(root, { recursive: true, force: true }); });
  const candidates: Host.Candidate[] = [];
  for (let number = 1; number <= 6; number++) {
    const buildId = 'sha256:' + number.toString(16).repeat(64), directory = join(config.artifactRoot, 'candidates', buildId.slice(7));
    await mkdir(join(directory, 'artifact'), { recursive: true });
    const candidate: Host.Candidate = { candidateId: buildId, buildId, componentId, artifactRoot: join(directory, 'artifact'),
      manifestPath: join(directory, 'component.json'), createdAt: new Date(old.getTime() + number * 1000).toISOString(),
      platform: { os: process.platform as 'win32' | 'linux', arch: process.arch as 'x64' | 'arm64', node: process.version } };
    const manifest: Host.ReleaseManifest = { schemaVersion: 1, componentId, kind,
      version: `1.0.${Math.min(number, 5)}`, buildId, connectsToHive: false,
      requirements: { node: '>=24.18.0 <25.0.0', hiveProtocol: null, contracts: [] },
      ...(kind === 'app' ? { app: { appId: componentId, dist: `dist/apps/${componentId}`, entryPath: 'index.html' } } : {
      entrypoint: { executable: 'node', args: ['dist/main.mjs'], timeoutMs: 5000 },
      readiness: { timeoutMs: 5000, command: { executable: 'node', args: ['-e', ''], timeoutMs: 5000 } },
      shutdown: { timeoutMs: 1000 }, restart: { policy: 'always' as const, minimumDelayMs: 100, maximumDelayMs: 500 } }) };
    await writeFile(join(directory, 'component.json'), JSON.stringify(manifest));
    await utimes(directory, old, old);
    journal.saveCandidate(candidate, manifest);
    candidates.push(candidate);
  }
  return { root, config, journal, candidates, old };
}

test('host collection keeps live references and recent candidates while retiring completed build copies', async t => {
  const { root, config, journal, candidates, old } = await fixture(t);
  journal.db.prepare('INSERT INTO installed VALUES (?,?)').run('installed-fixture', JSON.stringify({ candidateId: candidates[0]!.candidateId }));
  journal.db.prepare('INSERT INTO runtime_targets VALUES (?,?)').run('target-fixture', JSON.stringify({ candidateId: candidates[2]!.candidateId }));
  assert.deepEqual(await readReleaseManifest(candidates[1]!.manifestPath), journal.manifest(candidates[1]!.candidateId));
  const snapshotId = '00000000-0000-5000-8000-000000000002', source = join(config.stagingRoot, 'sources', snapshotId),
    snapshot: Host.SourceSnapshot = { snapshotId, sourceRoot: join(source, 'source'), originalRoot: join(root, 'checkout'),
      manifestPath: join(source, 'snapshot.json'), capturedAt: old.toISOString() };
  await mkdir(snapshot.sourceRoot, { recursive: true });
  await writeFile(snapshot.manifestPath, JSON.stringify({ schemaVersion: 1, snapshot }));
  await utimes(source, old, old);
  const preparationId = '00000000-0000-5000-8000-000000000002';
  await mkdir(join(config.stagingRoot, 'build-' + preparationId), { recursive: true });
  await savePreparation(config, { schemaVersion: 1, preparationId, componentId: 'fixture', buildId: candidates[1]!.buildId,
    snapshot, phase: 'compacted', candidate: candidates[1]!, startedAt: old.toISOString(), updatedAt: old.toISOString(), errorCode: null });

  const collected = await collectHostStorage(config);
  assert.deepEqual(collected, { candidates: 1, preparations: 1, snapshots: 1 });
  const status = journal.status(null).storageRetention!;
  assert.equal(status.state, 'succeeded'); assert.ok(status.lastSuccessAt);
  assert.equal(status.removed.candidates, 1); assert.ok(status.availableBytes.artifacts > 0);
  assert.ok(status.skipped.some(entry => entry.reason === 'protected_release' && !entry.requiresAttention));
  assert.deepEqual((await readdir(join(config.artifactRoot, 'candidates'))).sort(),
    [candidates[0]!, candidates[1]!, candidates[2]!, candidates[3]!, candidates[5]!].map(value => value.candidateId.slice(7)).sort());
  await assert.rejects(stat(source), { code: 'ENOENT' });
  await assert.rejects(stat(join(config.stagingRoot, 'build-' + preparationId)), { code: 'ENOENT' });
  assert.deepEqual(await collectHostStorage(config), { candidates: 0, preparations: 0, snapshots: 0 });
});

test('verified app copies compact through cache links and then expire with their source and candidate', async t => {
  const { config, journal, candidates, old } = await fixture(t, 'fixture-ui', 'app'), candidate = candidates[1]!;
  const manifest = journal.manifest(candidate.candidateId), id = preparationIdForBuild(candidate.buildId);
  const root = join(config.stagingRoot, 'build-' + id), source = join(config.stagingRoot, 'sources', id);
  const snapshot: Host.SourceSnapshot = { snapshotId: id, sourceRoot: join(source, 'source'), originalRoot: source,
    manifestPath: join(source, 'snapshot.json'), capturedAt: old.toISOString() };
  const plan: Host.BuildPlan = { schemaVersion: 1, componentId: candidate.componentId, kind: 'app', version: manifest.version,
    description: 'Static bundle fixture.', connectsToHive: false, requirements: manifest.requirements, app: manifest.app!, prepare: [], checks: [] };
  await atomicJson(join(snapshot.sourceRoot, 'ui', candidate.componentId, 'deploy.json'), plan);
  await atomicJson(snapshot.manifestPath, { schemaVersion: 1, snapshot });
  await atomicJson(join(candidate.artifactRoot, 'dist', 'build-info.json'), { buildId: candidate.buildId });
  await mkdir(join(root, 'source'), { recursive: true });
  const cache = join(config.stagingRoot, 'dependency-cache', 'fixture');
  await mkdir(cache, { recursive: true }); await writeFile(join(cache, 'keep'), 'shared dependencies');
  await symlink(cache, join(root, 'source', 'node_modules'), process.platform === 'win32' ? 'junction' : 'dir');
  await mkdir(join(root, 'artifact')); await writeFile(join(root, 'artifact', 'bundle'), 'duplicate bundle');
  await mkdir(join(root, 'artifact', ...Array<string>(66).fill('d')), { recursive: true });
  await assert.rejects(treeBytes(join(root, 'artifact')), { code: 'limit_exceeded' });
  await savePreparation(config, { schemaVersion: 1, preparationId: id, componentId: candidate.componentId, buildId: candidate.buildId,
    snapshot, phase: 'verified', candidate, startedAt: old.toISOString(), updatedAt: old.toISOString(), errorCode: null });
  const result = await compactPreparations(config);
  assert.equal(result.entries[0]!.action, 'compacted');
  assert.equal(await readFile(join(cache, 'keep'), 'utf8'), 'shared dependencies');
  await assert.rejects(stat(join(root, 'source')), { code: 'ENOENT' });
  assert.deepEqual(await collectHostStorage(config, Date.now() + 4 * 24 * 60 * 60 * 1000),
    { candidates: 4, preparations: 1, snapshots: 1 });
  assert.throws(() => journal.candidate(candidate.candidateId), { code: 'not_found' });
  await assert.rejects(stat(source), { code: 'ENOENT' });
});

for (const area of ['preparations', 'snapshots'] as const) test(`interrupted ${area} deletion resumes without its metadata`, async t => {
  const { root, config, journal, candidates, old } = await fixture(t), candidate = candidates[1]!;
  const id = preparationIdForBuild(candidate.buildId), source = join(config.stagingRoot, 'sources', id),
    preparation = join(config.stagingRoot, 'build-' + id);
  const snapshot: Host.SourceSnapshot = { snapshotId: id, sourceRoot: join(source, 'source'), originalRoot: join(root, 'checkout'),
    manifestPath: join(source, 'snapshot.json'), capturedAt: old.toISOString() };
  await mkdir(snapshot.sourceRoot, { recursive: true });
  await writeFile(join(snapshot.sourceRoot, 'payload'), 'source copy');
  await atomicJson(snapshot.manifestPath, { schemaVersion: 1, snapshot }); await utimes(source, old, old);
  if (area === 'preparations') {
    await mkdir(preparation);
    await savePreparation(config, { schemaVersion: 1, preparationId: id, componentId: candidate.componentId, buildId: candidate.buildId,
      snapshot, phase: 'compacted', candidate, startedAt: old.toISOString(), updatedAt: old.toISOString(), errorCode: null });
    await writeFile(join(preparation, 'preparation-before-recovery.json'), 'retained detail');
  }
  const original = area === 'preparations' ? preparation : source,
    retired = area === 'preparations' ? join(config.stagingRoot, '.retiring-build-' + id) : join(config.stagingRoot, 'sources', '.retiring-' + id),
    metadata = area === 'preparations' ? 'preparation.json' : 'snapshot.json';
  const remove = filesystem.rm;
  let interrupted = false;
  const removal = t.mock.method(filesystem, 'rm', async (...args: Parameters<typeof rm>) => {
    if (!interrupted && (args[0] === original || args[0] === retired)) {
      interrupted = true;
      await remove(join(String(args[0]), metadata));
      throw Object.assign(new Error('Removal interrupted after deleting metadata'), { code: 'EBUSY' });
    }
    return remove(...args);
  });
  syncBuiltinESMExports();
  try {
    const result = await collectHostStorage(config);
    assert.ok(interrupted); assert.equal(result[area], 0);
    await assert.rejects(stat(original), { code: 'ENOENT' });
    await assert.rejects(stat(join(retired, metadata)), { code: 'ENOENT' });
    assert.ok(await stat(retired));
    const status = journal.storageRetentionStatus()!;
    assert.equal(status.state, 'partial');
    assert.ok(!status.skipped.some(entry => entry.reason === 'unknown_preparation_ownership'));
    if (area === 'preparations') assert.equal(result.snapshots, 1, 'retired build evidence does not pin source copies');
  } finally { removal.mock.restore(); syncBuiltinESMExports(); }
  assert.equal((await collectHostStorage(config))[area], 1);
  await assert.rejects(stat(retired), { code: 'ENOENT' });
  assert.equal((await collectHostStorage(config))[area], 0);
});

test('retired staging cleanup rejects linked directories and preserves fresh preparation evidence', async t => {
  const { root, config, candidates } = await fixture(t), id = preparationIdForBuild(candidates[0]!.buildId);
  const target = join(root, 'keep'), original = join(config.stagingRoot, 'build-' + id), retired = join(config.stagingRoot, '.retiring-build-' + id);
  await mkdir(target); await writeFile(join(target, 'payload'), 'shared data');
  await mkdir(original, { recursive: true }); await writeFile(join(original, 'evidence'), 'new preparation');
  await symlink(target, retired, process.platform === 'win32' ? 'junction' : 'dir');
  await collectHostStorage(config);
  assert.equal(await readFile(join(target, 'payload'), 'utf8'), 'shared data');
  assert.equal(await readFile(join(original, 'evidence'), 'utf8'), 'new preparation');
  await rm(retired); await mkdir(retired); await writeFile(join(retired, 'leftover'), 'old partial deletion');
  assert.equal((await collectHostStorage(config)).preparations, 1);
  assert.equal(await readFile(join(original, 'evidence'), 'utf8'), 'new preparation');
});

test('prepared future releases cannot displace the installed rollback version', async t => {
  const { config, journal, candidates } = await fixture(t);
  journal.db.prepare('INSERT INTO installed VALUES (?,?)').run('fixture', JSON.stringify({ candidateId: candidates[2]!.candidateId }));
  assert.equal((await collectHostStorage(config)).candidates, 2);
  assert.deepEqual(journal.candidateForBuild('fixture', candidates[1]!.buildId), candidates[1]);
  assert.throws(() => journal.candidate(candidates[0]!.candidateId), { code: 'not_found' });
});

test('rollback keeps the actual previous successful build when deployment skipped versions', async t => {
  const { config, journal, candidates, old } = await fixture(t);
  journal.db.prepare('INSERT INTO installed VALUES (?,?)').run('fixture', JSON.stringify({ instanceId: 'fixture',
    candidateId: candidates[0]!.candidateId, buildId: candidates[0]!.buildId, enabled: true, installedAt: old.toISOString() }));
  const entry = journal.accept({ action: 'deploy', instanceId: 'fixture', operationId: 'retention-skipped-versions', candidateId: candidates[3]!.candidateId });
  entry.record.phase = 'succeeded';
  journal.db.prepare('UPDATE operations SET phase=?,entry_json=? WHERE operation_id=?').run('succeeded', JSON.stringify(entry), entry.request.operationId);
  journal.db.prepare('UPDATE installed SET value=? WHERE instance_id=?').run(JSON.stringify({ candidateId: candidates[3]!.candidateId }), 'fixture');
  await collectHostStorage(config);
  assert.deepEqual(journal.candidate(candidates[0]!.candidateId), candidates[0]);
  assert.deepEqual(journal.candidate(candidates[2]!.candidateId), candidates[2]);
  assert.throws(() => journal.candidate(candidates[1]!.candidateId), { code: 'not_found' });
});

test('missing and corrupt preparation records preserve their evidence without blocking unrelated candidates', async t => {
  const { config, journal, candidates, old } = await fixture(t);
  const missing = join(config.stagingRoot, 'build-' + preparationIdForBuild(candidates[0]!.buildId));
  const corrupt = join(config.stagingRoot, 'build-' + preparationIdForBuild(candidates[1]!.buildId));
  await mkdir(missing, { recursive: true }); await mkdir(corrupt, { recursive: true });
  await writeFile(join(corrupt, 'preparation.json'), '{');
  const snapshotId = '00000000-0000-5000-8000-000000000002', source = join(config.stagingRoot, 'sources', snapshotId);
  await mkdir(join(source, 'source'), { recursive: true });
  await atomicJson(join(source, 'snapshot.json'), { schemaVersion: 1, snapshot: { snapshotId, sourceRoot: join(source, 'source'),
    manifestPath: join(source, 'snapshot.json'), originalRoot: source, capturedAt: old.toISOString() } });
  await utimes(source, old, old);
  assert.deepEqual(await collectHostStorage(config), { candidates: 2, preparations: 0, snapshots: 0 });
  const status = journal.status(null).storageRetention!;
  assert.equal(status.state, 'partial'); assert.equal(status.lastSuccessAt, null);
  assert.ok(status.skipped.some(entry => entry.area === 'snapshots' && entry.reason === 'unknown_preparation_ownership' && entry.requiresAttention));
  for (const path of [missing, corrupt, source]) assert.ok((await stat(path)).isDirectory());
  for (const candidate of candidates.slice(0, 2)) assert.deepEqual(journal.candidate(candidate.candidateId), candidate);
  assert.throws(() => journal.candidate(candidates[2]!.candidateId), { code: 'not_found' });
});

test('interrupted candidate deletion resumes after reopening and rejects republication until complete', async t => {
  const { config, journal, candidates } = await fixture(t), candidate = candidates[0]!;
  const manifest = journal.manifest(candidate.candidateId);
  assert.equal(journal.retireCandidate(candidate.candidateId), true);
  await rm(candidate.manifestPath); // A process died after only part of the directory was removed.
  const reopened = new HostJournal(config);
  try {
    assert.equal(reopened.pendingCandidateRetirements().length, 1);
    assert.throws(() => reopened.saveCandidate(candidate, manifest), { code: 'mutation_conflict' });
  } finally { reopened.close(); }
  assert.equal((await collectHostStorage(config)).candidates, 4);
  assert.deepEqual(journal.pendingCandidateRetirements(), []);
  await assert.rejects(stat(join(config.artifactRoot, 'candidates', candidate.candidateId.slice(7))), { code: 'ENOENT' });
  assert.deepEqual(await collectHostStorage(config), { candidates: 0, preparations: 0, snapshots: 0 });
});

test('busy build locks defer candidate removal', async t => {
  const { config, journal, candidates } = await fixture(t), candidate = candidates[0]!;
  const lock = new ExecutorLock(join(config.stagingRoot, 'build-locks', candidate.buildId.slice(7)));
  try {
    await collectHostStorage(config);
    assert.deepEqual(journal.candidate(candidate.candidateId), candidate);
  } finally { lock.close(); }
  assert.equal((await collectHostStorage(config)).candidates, 1);
});

test('package import shares the collector lock and retries without accepting a missing candidate', async t => {
  const { config, journal, candidates, old } = await fixture(t), buildId = 'sha256:' + '7'.repeat(64);
  const directory = join(config.artifactRoot, 'candidates', buildId.slice(7)), archiveHash = hashJson('package fixture');
  const candidate: Host.Candidate = { ...candidates[0]!, candidateId: buildId, buildId, archiveHash,
    artifactRoot: join(directory, 'artifact'), manifestPath: join(directory, 'component.json'), archivePath: join(directory, 'package.tar.gz') };
  const manifest = { ...journal.manifest(candidates[0]!.candidateId), buildId, version: '1.0.6' };
  await atomicJson(candidate.manifestPath, manifest);
  await atomicJson(join(candidate.artifactRoot, 'dist', 'build-info.json'), { buildId });
  await writeFile(join(candidate.artifactRoot, 'dist', 'main.mjs'), '');
  journal.saveCandidate(candidate, manifest);
  const catalog = { schemaVersion: 1, revision: 1, packages: [{ componentId: candidate.componentId, version: manifest.version,
    buildId, archiveHash, manifest, bytes: 1, revision: 1, publishedAt: old.toISOString(), publisherPrincipalId: 'fixture-publisher' }] };
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () => Response.json(catalog);
  const updater = new PackageUpdater(config, journal, 'fixture-token');
  try {
    const lock = new ExecutorLock(join(config.stagingRoot, 'build-locks', buildId.slice(7)));
    try { await assert.rejects(updater.sync(), { code: 'executor_already_running' }); }
    finally { lock.close(); }
    await updater.sync();
    assert.equal(journal.unfinished()[0]?.candidateId, buildId);
  } finally { globalThis.fetch = originalFetch; }
});

test('a filesystem removal failure stays journaled and is retried', { skip: process.platform !== 'win32' }, async t => {
  const { config, journal, candidates } = await fixture(t), candidate = candidates[0]!, cwd = process.cwd();
  process.chdir(candidate.artifactRoot); // Windows refuses to remove a process working directory.
  try {
    await assert.rejects(collectHostStorage(config), error => ['EBUSY', 'EPERM'].includes((error as NodeJS.ErrnoException).code ?? ''));
    assert.equal(journal.pendingCandidateRetirements()[0]?.candidateId, candidate.candidateId);
    assert.equal(journal.status(null).storageRetention?.state, 'failed');
  } finally { process.chdir(cwd); }
  assert.equal((await collectHostStorage(config)).candidates, 1);
  assert.deepEqual(journal.pendingCandidateRetirements(), []);
  assert.equal(journal.status(null).storageRetention?.state, 'succeeded');
});

async function bootstrapFixture(t: TestContext) {
  const f = await fixture(t, 'host-executor'), configPath = join(f.root, 'config.json');
  const plans = f.candidates.map(candidate => {
    const plan = configurationBootstrapPlan(f.config, configPath, candidate), owner = plan.processes[0]!;
    owner.candidateId = candidate.candidateId; owner.artifactRoot = candidate.artifactRoot;
    owner.entrypoint = { executable: 'node', args: ['dist/main.mjs'], timeoutMs: 5000 };
    owner.configPath = join(f.config.runtimeRoot, 'bootstrap', plan.installationId, 'instances', owner.instanceId + '.json');
    return plan;
  });
  for (const plan of plans) {
    await retainBootstrapPlan(plan);
    await utimes(join(f.config.runtimeRoot, 'bootstrap-plans', hashJson(plan).slice(7) + '.json'), f.old, f.old);
  }
  const select = async (index: number) => {
    const plan = plans[index]!;
    await atomicJson(join(f.config.runtimeRoot, 'bootstrap', plan.installationId + '.json'), plan);
    f.journal.db.prepare('INSERT INTO installed VALUES (?,?)').run('host-executor', JSON.stringify({ candidateId: plan.candidateId }));
  };
  return { ...f, plans, select };
}

test('historical bootstrap plans stop pinning obsolete candidates but retain current and rollback OS plans', async t => {
  const { config, plans, select, journal, candidates } = await bootstrapFixture(t);
  await select(2);
  assert.equal((await collectHostStorage(config)).candidates, 2);
  assert.deepEqual(new Set((await retainedBootstrapPlans(config)).map(plan => hashJson(plan))), new Set([plans[1]!, plans[2]!].map(plan => hashJson(plan))));
  assert.deepEqual(journal.candidate(candidates[1]!.candidateId), candidates[1]);
  assert.throws(() => journal.candidate(candidates[0]!.candidateId), { code: 'not_found' });
});

test('bootstrap installation records the actual rollback even without a maintenance operation', async t => {
  const { config, plans, select, journal, candidates } = await bootstrapFixture(t);
  await select(0);
  const next = plans[3]!;
  journal.syncBootstrap(next, true);
  journal.syncBootstrap(next, true); // A configuration refresh must not replace the rollback pointer.
  await atomicJson(join(config.runtimeRoot, 'bootstrap', next.installationId + '.json'), next);
  await collectHostStorage(config);
  assert.deepEqual(journal.candidate(candidates[0]!.candidateId), candidates[0]);
  assert.ok((await retainedBootstrapPlans(config)).some(plan => plan.candidateId === candidates[0]!.candidateId));
  assert.throws(() => journal.candidate(candidates[1]!.candidateId), { code: 'not_found' });
});

for (const phase of ['needs_attention', 'succeeded']) test(`bootstrap ${phase} retains its exact previous owners despite skipped versions`, async t => {
  const { config, plans, select, journal, candidates } = await bootstrapFixture(t);
  await select(3);
  const prior = plans[0]!, next = plans[3]!, operationId = 'retention-maintenance-fixture';
  const request: Host.BootstrapMaintenanceRequest = { schemaVersion: 1, operationId, next,
    previous: { schemaVersion: 1, hostId: config.hostId, installationId: prior.installationId, configPath: prior.configPath,
      configurationHash: prior.configHash, observedAt: new Date().toISOString(), owners: prior.processes.map(process => ({
        instanceId: process.instanceId, componentId: process.componentId, name: process.name,
        candidateId: process.candidateId ?? prior.candidateId, definitionHash: hashJson(process), enabled: true, running: false })) } };
  await atomicJson(join(config.runtimeRoot, 'bootstrap-maintenance', hashJson(operationId).slice(7), 'record.json'),
    { phase, updatedAt: new Date().toISOString(), request });
  await collectHostStorage(config);
  assert.deepEqual(journal.candidate(candidates[0]!.candidateId), candidates[0]);
  assert.ok((await retainedBootstrapPlans(config)).some(plan => hashJson(plan) === hashJson(prior)));
  assert.throws(() => journal.candidate(candidates[1]!.candidateId), { code: 'not_found' });
});

test('storage pressure advances collection while repeated attempts remain bounded', async t => {
  const gib = 1024 ** 3;
  assert.equal(storageIsLow(gib, 10 * gib), true);
  assert.equal(storageIsLow(3 * gib, 40 * gib), true);
  assert.equal(storageIsLow(8 * gib, 40 * gib), false);
  const schedule = new RetentionSchedule(0);
  assert.equal(schedule.due(0, false), false); assert.equal(schedule.due(0, true), true);
  schedule.started(0);
  assert.equal(schedule.due(3599999, true), false); assert.equal(schedule.due(3600000, true), true);
  assert.equal(schedule.due(3600000, false), false); assert.equal(schedule.due(86400000, false), true);
  schedule.failed(86400000);
  assert.equal(schedule.due(86400001, true), false); assert.equal(schedule.due(90000000, false), true);
  const { config } = await fixture(t);
  const measured = await hostStorageSpace({ ...config, stagingRoot: join(config.stagingRoot, 'not-created-yet') });
  assert.ok(measured.availableBytes.staging > 0);
});

test('old maintenance details expire beyond 1000 entries while original records preserve rollback and replay', { timeout: 180000 }, async t => {
  const { config, journal, plans, select, root } = await bootstrapFixture(t);
  await select(3);
  const configPath = plans[3]!.configPath; await atomicJson(configPath, config);
  const makeRequest = (operationId: string, next = plans[5]!): Host.BootstrapMaintenanceRequest => ({ schemaVersion: 1, operationId, next,
    previous: { schemaVersion: 1, hostId: config.hostId, installationId: next.installationId, configPath,
      configurationHash: next.configHash, observedAt: new Date().toISOString(), owners: plans[0]!.processes.map(process => ({
        instanceId: process.instanceId, componentId: process.componentId, name: process.name, candidateId: process.candidateId!,
        definitionHash: hashJson(process), enabled: true, running: false })) } });
  const requests: Host.BootstrapMaintenanceRequest[] = [];
  const old = Date.now() - 45 * 86400000;
  for (let index = 0; index < 25; index++) {
    const request = makeRequest('old-maintenance-' + index, index === 0 ? plans[3]! : plans[5]!); requests.push(request);
    const at = new Date(old + index * 1000).toISOString();
    const record: Host.BootstrapMaintenanceRecord = { schemaVersion: 1, request, operationId: request.operationId, requestHash: hashJson(request),
      createdAt: at, updatedAt: at, phase: 'succeeded', errorCode: null, definitions: [{
        instanceId: 'host-executor', name: request.next.processes[0]!.name,
        previousHash: hashJson('prior'), desiredHash: hashJson('desired'), applied: true }] };
    const manual = join(config.runtimeRoot, 'bootstrap-maintenance', hashJson(request.operationId).slice(7));
    await atomicJson(join(manual, 'record.json'), record);
    await atomicJson(join(manual, 'host-executor.prior.json'), { content: 'prior' });
    await atomicJson(join(manual, 'host-executor.desired.json'), { content: 'desired' });
    const automatic = makeRequest('old-automatic-' + index);
    const directory = join(config.runtimeRoot, 'bootstrap-automatic', hashJson(automatic.operationId).slice(7));
    await atomicJson(join(directory, 'request.json'), automatic);
    await atomicJson(join(directory, 'record.json'), { schemaVersion: 1, operationId: automatic.operationId, requestHash: hashJson(automatic),
      createdAt: at, updatedAt: at, phase: 'rolled_back', errorCode: 'readiness_failed' });
  }
  // Historical temporary files also counted toward the old fatal inventory limit.
  const parent = join(config.runtimeRoot, 'bootstrap-maintenance');
  for (let batch = 0; batch < 1001; batch += 50)
    await Promise.all(Array.from({ length: Math.min(50, 1001 - batch) }, (_, offset) => writeFile(join(parent, 'historical-' + (batch + offset) + '.tmp'), '')));
  const unresolved = makeRequest('old-unresolved');
  await atomicJson(join(parent, hashJson(unresolved.operationId).slice(7), 'record.json'), { schemaVersion: 1, operationId: unresolved.operationId,
    requestHash: hashJson(unresolved), request: unresolved, createdAt: new Date(old).toISOString(), updatedAt: new Date(old).toISOString(),
    phase: 'needs_attention', errorCode: 'fixture_failure', definitions: [] });
  const compacted = requests[1]!, directory = join(parent, hashJson(compacted.operationId).slice(7));
  const original = await readFile(join(directory, 'record.json'));
  // The first known detail can be removed, but an unexpected directory must be left alone.
  const remaining = join(directory, 'host-executor.desired.json'); await rm(remaining); await mkdir(remaining);
  await writeFile(join(directory, 'unknown-evidence'), 'keep');
  await collectHostStorage(config);
  assert.equal(journal.storageRetentionStatus()!.removed.maintenance, 7);
  assert.ok(await stat(join(parent, hashJson(requests[0]!.operationId).slice(7), 'host-executor.prior.json')), 'the current rollback details survive despite their age');
  assert.ok(await stat(join(parent, hashJson(requests[24]!.operationId).slice(7), 'host-executor.prior.json')), 'recent details remain retained');
  assert.ok(await stat(join(parent, hashJson(unresolved.operationId).slice(7))));
  await assert.rejects(stat(join(directory, 'host-executor.prior.json')), { code: 'ENOENT' });
  assert.deepEqual(await readFile(join(directory, 'record.json')), original);
  assert.ok((await stat(remaining)).isDirectory());
  await rm(remaining, { recursive: true }); await atomicJson(remaining, { content: 'desired' });
  await collectHostStorage(config);
  assert.equal(journal.storageRetentionStatus()!.removed.maintenance, 1);
  await assert.rejects(stat(remaining), { code: 'ENOENT' });
  assert.equal(await readFile(join(directory, 'unknown-evidence'), 'utf8'), 'keep');
  assert.equal((await bootstrapStatus(config, compacted.operationId)).phase, 'succeeded');
  const replay = await updateBootstrap(configPath, compacted, root, {} as BootstrapOs);
  assert.deepEqual(replay, JSON.parse(original.toString()));
  await assert.rejects(updateBootstrap(configPath, { ...compacted, next: plans[2]! }, root, {} as BootstrapOs), { code: 'operation_conflict' });
  assert.equal((await automaticBootstrapRecord(config, 'old-automatic-0'))?.phase, 'rolled_back');
  assert.equal((await automaticBootstrapRecord(config, 'old-automatic-0'))?.errorCode, 'readiness_failed');
  await collectHostStorage(config);
  assert.equal(journal.storageRetentionStatus()!.removed.maintenance, 0);
});

test('automatic candidate cleanup is independent of optional inventory limits', async t => {
  const { config, candidates } = await fixture(t), candidate = candidates[0]!;
  await mkdir(join(candidate.artifactRoot, ...Array<string>(66).fill('d')), { recursive: true });
  await assert.rejects(treeBytes(candidate.artifactRoot), { code: 'limit_exceeded' });
  assert.equal((await collectHostStorage(config)).candidates, 4);
  await assert.rejects(stat(candidate.artifactRoot), { code: 'ENOENT' });
});
