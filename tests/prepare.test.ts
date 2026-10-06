import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, writeFile, rm, readdir, cp, symlink, stat, statfs, unlink } from 'node:fs/promises';
import { join, relative, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { captureSource } from '../packages/host-runtime/src/source.js';
import { prepareCandidate } from '../packages/host-runtime/src/prepare.js';
import { STORAGE_FORMAT } from '../services/hive/src/storage-schema.js';
import { backupHost } from '../packages/host-runtime/src/private-backup.js';
import { verifyCandidate, verifyLaunchCandidate, verifyTransferredArchive } from '../packages/host-runtime/src/artifact.js';
import { runCommand, startProcess } from '../packages/host-runtime/src/process.js';
import { atomicJson } from '../packages/host-runtime/src/config.js';
import { HostJournal, ExecutorLock } from '../packages/host-runtime/src/journal.js';
import { compactPreparations, preparationInventory, savePreparation } from '../packages/host-runtime/src/preparations.js';
import { collectHostStorage } from '../packages/host-runtime/src/storage-retention.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { digest } from '../packages/contracts/src/canonical.js';
import type { Host } from '../packages/contracts/src/generated.js';
import { until } from './fixtures/host.js';

const executable = (name: string) => spawnSync(process.platform === 'win32' ? 'where.exe' : 'which', [name], { encoding: 'utf8', windowsHide: true, timeout: 5000 }).stdout.trim().split(/\r?\n/)[0]!;
async function writeSourcePlan(source: string, plan: Host.BuildPlan): Promise<void> {
  const directory = join(source, plan.kind === 'app' ? 'ui' : 'services', plan.componentId);
  await mkdir(directory, { recursive: true });
  await atomicJson(join(directory, 'deploy.json'), plan);
}
async function fixture(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), 'ivy-prepare-test-')), source = join(root, 'checkout');
  const config: Host.HostConfig = { schemaVersion: 1, hostId: 'prepare-fixture', runtimeRoot: join(root, 'state'), artifactRoot: join(root, 'artifacts'), stagingRoot: join(root, 'staging'), publicBaseUrl: 'http://127.0.0.1:39081/ivy',
    executables: { git: executable('git'), tar: executable('tar'), node: process.execPath }, instances: [] };
  for (const folder of ['packages', 'tools/build']) await mkdir(join(source, folder), { recursive: true });
  await atomicJson(join(source, 'package.json'), { name: 'prepare-fixture', version: '1.0.0', private: true, type: 'module', packageManager: 'npm@11.16.0' });
  await atomicJson(join(source, 'package-lock.json'), { name: 'prepare-fixture', version: '1.0.0', lockfileVersion: 3 });
  await writeFile(join(source, '.gitignore'), 'config.json\ndist/\nnode_modules/\n');
  await writeFile(join(source, 'packages/main.mjs'), 'import fs from "node:fs";console.log(JSON.stringify({value:"captured-v1",buildId:JSON.parse(fs.readFileSync("dist/build-info.json","utf8")).buildId}));\n');
  await writeFile(join(source, 'tools/build/build.mjs'), 'import fs from "node:fs/promises";await fs.mkdir("dist",{recursive:true});await fs.copyFile("packages/main.mjs","dist/main.mjs");\n');
  const plan: Host.BuildPlan = { schemaVersion: 1, componentId: 'fixture', kind: 'native', version: '1.0.0', description: 'Real local executable used to test preparation, no external effect.', connectsToHive: false,
    requirements: { node: '>=24.18.0 <25.0.0', hiveProtocol: null, contracts: [] }, entrypoint: { executable: 'node', args: ['dist/main.mjs'], timeoutMs: 5000 },
    prepare: [{ executable: 'node', args: ['tools/build/build.mjs'], timeoutMs: 5000 }], checks: [],
    readiness: { timeoutMs: 5000, command: { executable: 'node', args: ['dist/main.mjs'], timeoutMs: 2000 } }, shutdown: { timeoutMs: 2000 }, restart: { policy: 'never', minimumDelayMs: 100, maximumDelayMs: 1000 },
    runtimeReset: { delete: [{ area: 'data', path: 'journal.sqlite' }], preserve: [{ area: 'data', path: 'settings.json' }] } };
  await writeSourcePlan(source, plan);
  await runCommand({ executable: 'git', args: ['init', '--quiet'], timeoutMs: 5000 }, source, config.executables, { jobLauncher: resolve('dist/native/ivy-job.exe') });
  t.after(async () => { assert.ok(relative(tmpdir(), root).startsWith('ivy-prepare-test-')); await rm(root, { recursive: true, force: true }); });
  return { root, config, source, plan };
}

test('Hive release storage declaration matches the implemented storage format', async () => {
  const plan = JSON.parse(await readFile(resolve('services/hive/deploy.json'), 'utf8')) as Host.BuildPlan;
  assert.deepEqual(plan.storage, { minReadableFormat: STORAGE_FORMAT, maxReadableFormat: STORAGE_FORMAT, writeFormat: STORAGE_FORMAT });
});

test('preparation publishes one compact reusable package without archive inventories', { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  f.config.developmentMode = true;
  const configPath = join(f.root, 'host.json'); await atomicJson(configPath, f.config);
  const snapshot = await captureSource(f.source, f.config);
  const candidate = await prepareCandidate(snapshot, 'fixture', f.config, resolve('.'));
  assert.equal(candidate.archivePath, join(candidate.artifactRoot, '..', 'package.tar.gz')); assert.match(candidate.archiveHash!, /^sha256:[0-9a-f]{64}$/);
  assert.ok((await stat(candidate.archivePath!)).size > 0);
  const manifest = JSON.parse(await readFile(candidate.manifestPath, 'utf8'));
  assert.equal(manifest.buildId, candidate.buildId); assert.equal(manifest.componentId, candidate.componentId);
  assert.deepEqual(manifest.runtimeReset, f.plan.runtimeReset);
  assert.equal(manifest.artifactHash, undefined); assert.equal(manifest.verification, undefined);
  await assert.rejects(stat(join(candidate.artifactRoot, '../evidence')), { code: 'ENOENT' });
  await verifyCandidate(candidate, f.config);
  await verifyLaunchCandidate(candidate, f.config, 'dist/main.mjs');
  const transferred = join(f.config.artifactRoot, 'one-time-transfer.tar.gz'), transferredBytes = Buffer.from('synthetic transferred archive');
  await writeFile(transferred, transferredBytes);
  await verifyTransferredArchive(transferred, digest(transferredBytes));
  await writeFile(transferred, Buffer.from('damaged transferred archive'));
  await assert.rejects(verifyTransferredArchive(transferred, digest(transferredBytes)), { code: 'artifact_changed' });
  await unlink(transferred);
  await verifyCandidate(candidate, f.config);
  const launched = await runCommand(f.plan.entrypoint!, candidate.artifactRoot, f.config.executables, { jobLauncher: resolve('dist/native/ivy-job.exe') });
  assert.equal(JSON.parse(launched.stdout).buildId, candidate.buildId);
  assert.deepEqual(await prepareCandidate(snapshot, 'fixture', f.config, resolve('.')), candidate);
  assert.equal((await preparationInventory(f.config)).entries[0]!.phase, 'compacted');
  const backup = await backupHost(configPath, join(f.root, 'backup'), resolve('.'));
  assert.equal('candidates' in JSON.parse(await readFile(join(backup.directory, 'backup.json'), 'utf8')), false);
  const release = await prepareCandidate(snapshot, 'fixture', { ...f.config, developmentMode: false }, resolve('.'));
  assert.deepEqual(release, candidate); assert.equal(release.archivePath, candidate.archivePath);
  await verifyCandidate(release, f.config);
  await writeFile(join(candidate.artifactRoot, 'dist/main.mjs'), 'changed');
  await verifyCandidate(candidate, f.config);
  await atomicJson(join(candidate.artifactRoot, 'dist/build-info.json'), { buildId: 'sha256:' + '0'.repeat(64), version: '1.0.0' });
  await assert.rejects(verifyCandidate(candidate, f.config), { code: 'build_mismatch' });
});

test('component releases keep their own version independently of the workspace package', { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  f.plan.version = '2.3.4';
  await writeSourcePlan(f.source, f.plan);
  const candidate = await prepareCandidate(await captureSource(f.source, f.config), 'fixture', f.config, resolve('.'));
  const manifest = JSON.parse(await readFile(candidate.manifestPath, 'utf8')) as Host.ReleaseManifest;
  assert.equal(manifest.version, '2.3.4');
  assert.equal(JSON.parse(await readFile(join(f.source, 'package.json'), 'utf8')).version, '1.0.0');
});

test('ui preparation packages only the selected static bundle', { timeout: 30_000 }, async t => {
  const f = await fixture(t), componentId = 'fixture-ui';
  await writeFile(join(f.source, 'tools/build', 'build-ui.mjs'), `import { mkdir, writeFile } from 'node:fs/promises';
await mkdir('dist/apps/${componentId}',{recursive:true});
await writeFile('dist/apps/${componentId}/index.html','<!doctype html><title>fixture</title>');
await writeFile('dist/apps/${componentId}/ivy-ui.json',JSON.stringify({metadata:{uiId:'${componentId}',displayName:'Fixture',description:'Fixture ui.',iconKey:'ui'},entryPath:'index.html',requirements:{hiveProtocol:1,contracts:[],services:[]},dataContracts:[]}));
await mkdir('dist/apps/unrelated',{recursive:true});await writeFile('dist/apps/unrelated/secret.txt','must not ship');
`);
  const plan: Host.BuildPlan = { schemaVersion: 1, componentId, kind: 'app', version: '1.0.0', description: 'Compact static ui package fixture.',
    connectsToHive: false, requirements: { node: '>=24.18.0 <25.0.0', hiveProtocol: 1, contracts: [] },
    app: { appId: componentId, dist: `dist/apps/${componentId}`, entryPath: 'index.html' },
    prepare: [{ executable: 'node', args: ['tools/build/build-ui.mjs'], timeoutMs: 5000 }], checks: [] };
  await writeSourcePlan(f.source, plan);
  const candidate = await prepareCandidate(await captureSource(f.source, f.config), componentId, f.config, resolve('.'));
  const manifest = JSON.parse(await readFile(candidate.manifestPath, 'utf8')) as Host.ReleaseManifest;
  assert.equal(manifest.kind, 'app'); assert.deepEqual(manifest.app, plan.app); assert.equal(manifest.entrypoint, undefined);
  assert.equal(await readFile(join(candidate.artifactRoot, plan.app!.dist, 'index.html'), 'utf8'), '<!doctype html><title>fixture</title>');
  for (const omitted of ['node_modules', 'package.json', 'package-lock.json', 'dist/apps/unrelated'])
    await assert.rejects(stat(join(candidate.artifactRoot, omitted)), { code: 'ENOENT' });
  assert.ok((await stat(candidate.archivePath!)).size < 64 * 1024, 'static package stays compact');
  await verifyCandidate(candidate, f.config);
  const prepared = (await preparationInventory(f.config)).entries.find(entry => entry.candidateId === candidate.candidateId)!;
  assert.equal(prepared.phase, 'compacted', 'app publications compact without executable-only manifest fields');
  // Reproduce a verified app left behind by an older publisher, then exercise recovery.
  const record = JSON.parse(await readFile(join(prepared.path, 'preparation.json'), 'utf8')) as Host.PreparationRecord;
  await mkdir(join(prepared.path, 'artifact'), { recursive: true });
  await writeFile(join(prepared.path, 'artifact', 'old-bundle'), 'stale ui copy');
  const dependencyCache = join(f.config.stagingRoot, 'dependency-cache', 'fixture');
  await mkdir(dependencyCache, { recursive: true }); await writeFile(join(dependencyCache, 'keep'), 'shared dependencies');
  await mkdir(join(prepared.path, 'source'), { recursive: true });
  await symlink(dependencyCache, join(prepared.path, 'source', 'node_modules'), process.platform === 'win32' ? 'junction' : 'dir');
  await savePreparation(f.config, { ...record, phase: 'verified' });
  const recovered = await compactPreparations(f.config);
  assert.equal(recovered.entries.find(entry => entry.candidateId === candidate.candidateId)?.action, 'compacted');
  await assert.rejects(stat(join(prepared.path, 'artifact')), { code: 'ENOENT' });
  assert.equal(await readFile(join(dependencyCache, 'keep'), 'utf8'), 'shared dependencies');
  assert.deepEqual(await collectHostStorage(f.config, Date.now() + 4 * 24 * 60 * 60 * 1000),
    { candidates: 0, preparations: 1, snapshots: 1 });
});

test('independent captures of unchanged source reuse the exact prepared candidate', { timeout: 20_000 }, async t => {
  const f = await fixture(t), builds = join(f.root, 'builds.txt');
  await writeFile(join(f.source, 'tools/build/build.mjs'), (await readFile(join(f.source, 'tools/build/build.mjs'), 'utf8')) +
    'await fs.appendFile(' + JSON.stringify(builds) + ', "build\\n");\n');
  const firstSnapshot = await captureSource(f.source, f.config);
  const first = await prepareCandidate(firstSnapshot, 'fixture', f.config, resolve('.'));
  const secondSnapshot = await captureSource(f.source, f.config);
  const second = await prepareCandidate(secondSnapshot, 'fixture', f.config, resolve('.'));
  assert.notEqual(secondSnapshot.snapshotId, firstSnapshot.snapshotId);
  assert.deepEqual(second, first);
  assert.equal(await readFile(builds, 'utf8'), 'build\n');
});

test('actual preparation builds a fixed snapshot and executes the atomically published candidate', { timeout: 20_000 }, async t => {
  const f = await fixture(t), snapshot = await captureSource(f.source, f.config);
  await writeFile(join(f.source, 'packages/main.mjs'), 'this later edit must not enter the accepted build');
  const candidate = await prepareCandidate(snapshot, 'fixture', f.config, resolve('.'));
  await verifyCandidate(candidate, f.config);
  await verifyLaunchCandidate(candidate, f.config, 'dist/main.mjs');
  const manifest = JSON.parse(await readFile(candidate.manifestPath, 'utf8'));
  assert.equal(manifest.buildId, candidate.buildId); assert.equal(manifest.verification, undefined);
  await assert.rejects(stat(join(candidate.artifactRoot, '../evidence')), { code: 'ENOENT' });
  assert.equal(candidate.archivePath, join(candidate.artifactRoot, '..', 'package.tar.gz'));
  const result = await runCommand(f.plan.entrypoint!, candidate.artifactRoot, f.config.executables, { jobLauncher: resolve('dist/native/ivy-job.exe') });
  assert.deepEqual(JSON.parse(result.stdout), { value: 'captured-v1', buildId: candidate.buildId });
  assert.deepEqual(await prepareCandidate(snapshot, 'fixture', f.config, resolve('.')), candidate);
  const inventory = await preparationInventory(f.config);
  assert.equal(inventory.entries.length, 1); assert.equal(inventory.entries[0]!.phase, 'compacted');
  assert.ok(inventory.entries[0]!.bytes < 100000); assert.ok(inventory.availableBytes > 0);
  for (const name of ['source', 'artifact', 'artifact.tar.gz']) await assert.rejects(readFile(join(inventory.entries[0]!.path, name)), (error: NodeJS.ErrnoException) => error.code === 'ENOENT');
  const journal = new HostJournal(f.config);
  try { assert.equal(journal.manifest(candidate.candidateId).buildId, candidate.buildId); } finally { journal.close(); }
  await writeFile(join(candidate.artifactRoot, 'dist/main.mjs'), 'modified live candidate');
  await verifyLaunchCandidate(candidate, f.config, 'dist/main.mjs');
  await verifyCandidate(candidate, f.config);
});

test('dependency cache keeps external packages and .bin entries while relinking changed workspace packages', { timeout: 30_000 }, async t => {
  const f = await fixture(t);
  await mkdir(join(f.source, 'packages', 'ui'), { recursive: true });
  await writeFile(join(f.source, 'packages', 'ui', 'index.mjs'), 'export const label = "ui-v1";\n');
  await atomicJson(join(f.source, 'package-lock.json'), { name: 'prepare-fixture', version: '1.0.0', lockfileVersion: 3, packages: {
    '': {}, 'node_modules/@ivy/ui': { link: true, resolved: 'packages/ui' }, 'packages/ui': {}, 'node_modules/external-runtime': {},
  } });
  await writeFile(join(f.source, 'tools/build', 'run-npm.mjs'), `import { mkdir, readFile, stat, symlink, writeFile } from 'node:fs/promises';
import { dirname, relative, resolve } from 'node:path';
const external = resolve('node_modules/external-runtime'), link = resolve('node_modules/@ivy/ui');
if (process.env.IVY_DEPENDENCY_CACHE_HIT === '1') {
  if ((await readFile(resolve(external, 'index.mjs'), 'utf8')) !== 'external-v1') throw new Error('external dependency was not restored');
  await stat(resolve('node_modules/.bin/runtime-tool')); await stat(link); process.exit(0);
}
await mkdir(external, { recursive: true }); await writeFile(resolve(external, 'index.mjs'), 'external-v1');
await mkdir(resolve('node_modules/.bin'), { recursive: true });
const cli = 'console.log("external-cli-ok")';
await writeFile(resolve(external, 'cli.mjs'), cli);
if (process.platform === 'win32') await writeFile(resolve('node_modules/.bin/runtime-tool'), cli);
else await symlink('../external-runtime/cli.mjs', resolve('node_modules/.bin/runtime-tool'));
await mkdir(dirname(link), { recursive: true });
await symlink(process.platform === 'win32' ? resolve('packages/ui') : relative(dirname(link), resolve('packages/ui')), link, process.platform === 'win32' ? 'junction' : 'dir');
`);
  await writeFile(join(f.source, 'tools/build', 'build.mjs'), `import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
const tool = spawnSync(process.execPath, ['node_modules/.bin/runtime-tool'], { encoding: 'utf8' });
if (tool.status !== 0 || tool.stdout.trim() !== 'external-cli-ok') throw new Error('Restored relative CLI failed: ' + tool.stderr);
const ui = await readFile('node_modules/@ivy/ui/index.mjs', 'utf8'), external = await readFile('node_modules/external-runtime/index.mjs', 'utf8');
  await mkdir('dist', { recursive: true }); await writeFile('dist/main.mjs', 'console.log(JSON.stringify(' + JSON.stringify({ ui, external, cacheHit: process.env.IVY_DEPENDENCY_CACHE_HIT }) + '))\\n');
`);
  f.plan.prepare = [{ executable: 'node', args: ['tools/build/run-npm.mjs', 'ci'], timeoutMs: 5000 }, { executable: 'node', args: ['tools/build/build.mjs'], timeoutMs: 5000 }];
  await writeSourcePlan(f.source, f.plan);
  const first = await prepareCandidate(await captureSource(f.source, f.config), 'fixture', f.config, resolve('.'));
  const firstRun = await runCommand(f.plan.entrypoint!, first.artifactRoot, f.config.executables, { jobLauncher: resolve('dist/native/ivy-job.exe') });
  assert.deepEqual(JSON.parse(firstRun.stdout), { ui: 'export const label = "ui-v1";\n', external: 'external-v1', cacheHit: '0' });
  const initialPreparation = (await preparationInventory(f.config)).entries[0]!;
  await assert.rejects(stat(join(initialPreparation.path, 'source')), { code: 'ENOENT' });
  await writeFile(join(f.source, 'packages', 'ui', 'index.mjs'), 'export const label = "ui-v2";\n');
  const second = await prepareCandidate(await captureSource(f.source, f.config), 'fixture', f.config, resolve('.'));
  const secondRun = await runCommand(f.plan.entrypoint!, second.artifactRoot, f.config.executables, { jobLauncher: resolve('dist/native/ivy-job.exe') });
  assert.deepEqual(JSON.parse(secondRun.stdout), { ui: 'export const label = "ui-v2";\n', external: 'external-v1', cacheHit: '1' });
  assert.notEqual(second.buildId, first.buildId);
  const cacheRoots = await readdir(join(f.config.stagingRoot, 'dependency-cache'));
  for (const root of cacheRoots) await assert.rejects(stat(join(f.config.stagingRoot, 'dependency-cache', root, 'node_modules', '@ivy', 'ui')), { code: 'ENOENT' });
});

test('runtime packaging skips lockfile-only optional packages absent on this platform', { timeout: 20_000 }, async t => {
  const f = await fixture(t);
  await atomicJson(join(f.source, 'package-lock.json'), { name: 'prepare-fixture', version: '1.0.0', lockfileVersion: 3, packages: {
    '': {}, 'node_modules/runtime': { optionalDependencies: { 'other-platform': '1.0.0' } },
    'node_modules/other-platform': { optional: true, os: ['other'] },
  } });
  await writeFile(join(f.source, 'packages/main.mjs'), 'import value from "runtime";console.log(value);\n');
  await writeFile(join(f.source, 'tools/build/build.mjs'), `import fs from 'node:fs/promises';
await fs.mkdir('node_modules/runtime',{recursive:true});
await fs.writeFile('node_modules/runtime/package.json',JSON.stringify({name:'runtime',version:'1.0.0',type:'module',main:'index.js'}));
await fs.writeFile('node_modules/runtime/index.js','export default "runtime-ok";');
await fs.mkdir('dist',{recursive:true});await fs.copyFile('packages/main.mjs','dist/main.mjs');
`);
  const candidate = await prepareCandidate(await captureSource(f.source, f.config), 'fixture', f.config, resolve('.'));
  await assert.rejects(stat(join(candidate.artifactRoot, 'node_modules/other-platform')), { code: 'ENOENT' });
  const result = await runCommand(f.plan.entrypoint!, candidate.artifactRoot, f.config.executables, { jobLauncher: resolve('dist/native/ivy-job.exe') });
  assert.equal(result.stdout.trim(), 'runtime-ok');
});

test('each component preparation uses its own fixed source and publishes a distinct executable candidate', { timeout: 40_000 }, async t => {
  const f = await fixture(t), counter = join(f.root, 'executed.txt');
  await writeFile(join(f.source, 'tools/build/build.mjs'), (await readFile(join(f.source, 'tools/build/build.mjs'), 'utf8')) +
    'await fs.appendFile(' + JSON.stringify(counter) + ', "build\\n");\n');
  const peer = { ...f.plan, componentId: 'peer' }, extra = { ...f.plan, componentId: 'extra',
    prepare: [...f.plan.prepare, { executable: 'node', args: ['-e', 'require("node:fs").appendFileSync(' + JSON.stringify(counter) + ', "extra\\n")'], timeoutMs: 5000 }] };
  for (const plan of [f.plan, peer, extra]) await writeSourcePlan(f.source, plan);
  const snapshot = await captureSource(f.source, f.config);
  const original = await prepareCandidate(snapshot, 'fixture', f.config, resolve('.'));
  const variables = ['TEMP', 'TMP', 'TMPDIR'], environment = variables.map(key => process.env[key]);
  let peerCandidate: Host.Candidate;
  try {
    for (const key of variables) process.env[key] = join(f.root, 'different-caller-temporary-directory');
    peerCandidate = await prepareCandidate(snapshot, 'peer', f.config, resolve('.'));
  } finally {
    variables.forEach((key, index) => { if (environment[index] === undefined) delete process.env[key]; else process.env[key] = environment[index]; });
  }
  assert.notEqual(peerCandidate!.buildId, original.buildId); assert.notEqual(peerCandidate!.candidateId, original.candidateId);
  assert.equal(await readFile(counter, 'utf8'), 'build\nbuild\n');
  await assert.rejects(stat(join(peerCandidate!.artifactRoot, '../evidence')), { code: 'ENOENT' });
  await verifyCandidate(peerCandidate!, f.config); await verifyCandidate(original, f.config);
  const ran = await runCommand(peer.entrypoint!, peerCandidate!.artifactRoot, f.config.executables, { jobLauncher: resolve('dist/native/ivy-job.exe') });
  assert.deepEqual(JSON.parse(ran.stdout), { value: 'captured-v1', buildId: peerCandidate!.buildId });
  const different = await prepareCandidate(snapshot, 'extra', f.config, resolve('.'));
  assert.equal(await readFile(counter, 'utf8'), 'build\nbuild\nbuild\nextra\n');
  await assert.rejects(stat(join(different.artifactRoot, '../evidence')), { code: 'ENOENT' });
  assert.equal(JSON.parse(await readFile(different.manifestPath, 'utf8')).buildId, different.buildId);
});

test('a changed retained artifact does not authorize a different component build', { timeout: 25_000 }, async t => {
  const f = await fixture(t);
  await writeSourcePlan(f.source, { ...f.plan, componentId: 'peer' });
  const snapshot = await captureSource(f.source, f.config);
  const original = await prepareCandidate(snapshot, 'fixture', f.config, resolve('.'));
  await writeFile(join(original.artifactRoot, 'dist/main.mjs'), 'changed after verification');
  const peerCandidate = await prepareCandidate(snapshot, 'peer', f.config, resolve('.'));
  await verifyCandidate(peerCandidate, f.config);
  const journal = new HostJournal(f.config);
  try { assert.equal(journal.db.prepare('SELECT count(*) AS n FROM candidates WHERE component_id=?').get('peer')!['n'], 1); }
  finally { journal.close(); }
});

test('a failed preparation keeps bounded state and cannot register an executable candidate', { timeout: 20_000 }, async t => {
  const f = await fixture(t);
  f.plan.prepare[0] = { executable: 'node', args: ['-e', 'console.error("specific preparation failure");process.exit(7)'], timeoutMs: 5000 };
  await writeSourcePlan(f.source, f.plan);
  const snapshot = await captureSource(f.source, f.config);
  await assert.rejects(prepareCandidate(snapshot, 'fixture', f.config, resolve('.')), (error: unknown) => error instanceof IvyError && error.code === 'command_failed' && !(error.details as Record<string, unknown> | undefined)?.evidenceRoot);
  const journal = new HostJournal(f.config);
  try { assert.equal(journal.db.prepare('SELECT count(*) AS n FROM candidates').get()!['n'], 0); } finally { journal.close(); }
  const compacted = await compactPreparations(f.config); assert.equal(compacted.entries[0]!.action, 'retained');
  const retained = (await preparationInventory(f.config)).entries[0]!;
  assert.equal(retained.phase, 'unknown'); assert.equal(retained.code, 'command_failed');
  assert.ok(!(await readdir(retained.path)).includes('evidence'));
});

test('prepared runtime executes its own retained dependencies without a deployment check pipeline', { timeout: 20_000 }, async t => {
  const f = await fixture(t);
  await atomicJson(join(f.source, 'package-lock.json'), { lockfileVersion: 3, packages: {
    '': {}, 'node_modules/dev-check': { dev: true }, 'node_modules/runtime': {}, 'node_modules/shared': {},
    'node_modules/dev-check-extra': { devOptional: true },
  } });
  await writeFile(join(f.source, 'tools/build/build.mjs'), 'import fs from "node:fs/promises";\n' +
    'for(const [name,value] of [["dev-check","checked"],["runtime","runtime"],["shared","shared"],["dev-check-extra","optional"]]) {\n' +
    'const root="node_modules/"+name;await fs.mkdir(root,{recursive:true});\n' +
    'await fs.writeFile(root+"/package.json",JSON.stringify({name,version:"1.0.0",type:"module",main:"index.js"}));\n' +
    'await fs.writeFile(root+"/index.js","export default "+JSON.stringify(value)+";");}\n' +
    'await fs.mkdir("dist",{recursive:true});await fs.copyFile("packages/main.mjs","dist/main.mjs");\n');
  await writeFile(join(f.source, 'packages/main.mjs'), 'import runtime from "runtime";import shared from "shared";import optional from "dev-check-extra";\n' +
    'const diagnostic = `text containing from "not-installed" is not an import`;void diagnostic;\n' +
    'console.log(JSON.stringify({runtime,shared,optional}));\n');
  await writeSourcePlan(f.source, f.plan);
  const snapshot = await captureSource(f.source, f.config);
  const candidate = await prepareCandidate(snapshot, 'fixture', f.config, resolve('.'));
  await verifyCandidate(candidate, f.config);
  const result = await runCommand(f.plan.entrypoint!, candidate.artifactRoot, f.config.executables, { jobLauncher: resolve('dist/native/ivy-job.exe') });
  assert.deepEqual(JSON.parse(result.stdout), { runtime: 'runtime', shared: 'shared', optional: 'optional' });
  await assert.rejects(stat(join(candidate.artifactRoot, 'node_modules/dev-check')), (error: NodeJS.ErrnoException) => error.code === 'ENOENT');
  await assert.rejects(stat(join(candidate.artifactRoot, '../evidence')), { code: 'ENOENT' });
  assert.equal(JSON.parse(await readFile(join(candidate.artifactRoot, 'package-lock.json'), 'utf8')).packages['node_modules/dev-check'].dev, true);
});

test('an actual preparing process dies after publication and its identical candidate recovers without a second build', { timeout: 35_000 }, async t => {
  const f = await fixture(t), count = join(f.root, 'builds.txt'), ready = join(f.root, 'build-ready'), permit = join(f.root, 'continue-build'), configPath = join(f.root, 'config.json');
  await writeFile(join(f.source, 'tools/build/build.mjs'), (await readFile(join(f.source, 'tools/build/build.mjs'), 'utf8')) +
    'await fs.appendFile(' + JSON.stringify(count) + ', "build\\n");await fs.writeFile(' + JSON.stringify(ready) + ', "ready");while(!(await fs.stat(' + JSON.stringify(permit) + ').catch(() => null)))await new Promise(resolve => setTimeout(resolve, 25));\n');
  await writeSourcePlan(f.source, f.plan); await atomicJson(configPath, f.config);
  const snapshot = await captureSource(f.source, f.config), journal = new HostJournal(f.config);
  const child = await startProcess({ executable: 'node', args: ['dist/packages/cli/src/main.js', 'prepare', '--config', configPath, '--source', f.source, '--component', 'fixture', '--json'], timeoutMs: 20000 }, resolve('.'), f.config.executables);
  t.after(() => child.stop()); let locked = false, record: Host.PreparationRecord | null = null;
  try {
    await until(async () => !!await readFile(ready, 'utf8').catch(() => ''));
    journal.db.exec('BEGIN IMMEDIATE'); locked = true; await writeFile(permit, 'continue');
    await until(async () => {
      const names = (await readdir(f.config.stagingRoot)).filter(name => name.startsWith('build-') && name !== 'build-locks');
      for (const name of names) {
        const value = await readFile(join(f.config.stagingRoot, name, 'preparation.json'), 'utf8').then(text => JSON.parse(text) as Host.PreparationRecord, () => null);
        if (value?.phase === 'publishing' && value.candidate?.archivePath && value.candidate.archiveHash &&
          await readFile(value.candidate.manifestPath, 'utf8').catch(() => null) && await stat(value.candidate.archivePath).catch(() => null)) {
          record = value; return true;
        }
      }
      return false;
    });
    await child.stop(); assert.equal(journal.db.prepare('SELECT count(*) AS n FROM candidates').get()!['n'], 0);
  } finally { if (locked) journal.db.exec('ROLLBACK'); journal.close(); }
  assert.ok(record); const candidate = (record as Host.PreparationRecord).candidate!;
  const staged = join(f.config.stagingRoot, 'build-' + (record as Host.PreparationRecord).preparationId);
  for (const name of ['artifact', 'artifact.tar.gz']) await assert.rejects(stat(join(staged, name)), { code: 'ENOENT' });
  assert.deepEqual(await prepareCandidate((record as Host.PreparationRecord).snapshot, 'fixture', f.config, resolve('.')), candidate);
  assert.equal(await readFile(count, 'utf8'), 'build\n');
  const after = await preparationInventory(f.config); assert.equal(after.entries.length, 1); assert.equal(after.entries[0]!.phase, 'compacted');
  const retained = await readFile(join(after.entries[0]!.path, 'preparation.json'), 'utf8');
  const again = await compactPreparations(f.config);
  assert.deepEqual(again.entries.map(value => ({ bytes: value.reclaimedBytes, code: value.code })), [{ bytes: 0, code: 'already_compacted' }]);
  assert.equal(await readFile(join(after.entries[0]!.path, 'preparation.json'), 'utf8'), retained);
  const lost = new HostJournal(f.config); try { lost.db.prepare('DELETE FROM candidates WHERE candidate_id=?').run(candidate.candidateId); } finally { lost.close(); }
  assert.equal((await compactPreparations(f.config)).entries[0]!.action, 'recovered');
  const restored = new HostJournal(f.config); try { assert.deepEqual(restored.candidate(candidate.candidateId), candidate); } finally { restored.close(); }
  assert.equal(await readFile(count, 'utf8'), 'build\n');
});

test('actual preparation across distinct Linux filesystems preserves payloads through copy fallback', { skip: process.platform !== 'linux', timeout: 25000 }, async t => {
  const f = await fixture(t);
  const memoryFs = await stat('/dev/shm').catch((error: NodeJS.ErrnoException) => { if (error.code === 'ENOENT') return null; throw error; });
  if (!memoryFs || memoryFs.dev === (await stat(f.root)).dev) { t.skip('A distinct /dev/shm filesystem is unavailable.'); return; }
  const capacity = await statfs('/dev/shm');
  if (capacity.bavail * capacity.bsize < 512 * 1024 * 1024) { t.skip('The distinct filesystem cannot meet the normal preparation reserve.'); return; }
  const staging = await mkdtemp('/dev/shm/ivy-prepare-cross-volume-');
  t.after(async () => { assert.ok(staging.startsWith('/dev/shm/ivy-prepare-cross-volume-')); await rm(staging, { recursive: true }); });
  f.config.stagingRoot = staging;
  const snapshot = await captureSource(f.source, f.config);
  const candidate = await prepareCandidate(snapshot, 'fixture', f.config, resolve('.'));
  assert.notEqual((await stat(staging)).dev, (await stat(candidate.artifactRoot)).dev);
  await verifyCandidate(candidate, f.config);
  const result = await runCommand(f.plan.entrypoint!, candidate.artifactRoot, f.config.executables);
  assert.deepEqual(JSON.parse(result.stdout), { value: 'captured-v1', buildId: candidate.buildId });
  assert.equal((await preparationInventory(f.config)).entries[0]!.phase, 'compacted');
  assert.deepEqual(await prepareCandidate(snapshot, 'fixture', f.config, resolve('.')), candidate);
});

test('compaction retains unrecognized payloads, protects a busy recorded build, and refuses outside links', { timeout: 25_000 }, async t => {
  const f = await fixture(t), snapshot = await captureSource(f.source, f.config);
  const candidate = await prepareCandidate(snapshot, 'fixture', f.config, resolve('.'));
  const retainedId = randomUUID(), retained = join(f.config.stagingRoot, 'build-' + retainedId);
  await mkdir(retained); await cp(snapshot.sourceRoot, join(retained, 'source'), { recursive: true });
  await cp(candidate.artifactRoot, join(retained, 'artifact'), { recursive: true });
  const unrecognized = (await compactPreparations(f.config)).entries.find(value => value.preparationId === retainedId)!;
  assert.equal(unrecognized.action, 'retained'); assert.equal(unrecognized.code, 'preparation_unrecognized');
  assert.equal(unrecognized.reclaimedBytes, 0);
  await assert.rejects(readFile(join(retained, 'preparation.json')), (error: NodeJS.ErrnoException) => error.code === 'ENOENT');
  assert.ok((await readFile(join(retained, 'source/packages/main.mjs'), 'utf8')).includes('captured-v1'));
  await savePreparation(f.config, { schemaVersion: 1, preparationId: retainedId, componentId: candidate.componentId,
    buildId: candidate.buildId, snapshot, phase: 'verified', candidate, startedAt: candidate.createdAt, updatedAt: candidate.createdAt, errorCode: null });
  const lock = new ExecutorLock(join(f.config.stagingRoot, 'build-locks', candidate.buildId.slice(7)));
  try { assert.equal((await compactPreparations(f.config)).entries.find(value => value.preparationId === retainedId)!.action, 'busy'); }
  finally { lock.close(); }
  assert.ok((await readFile(join(retained, 'source/packages/main.mjs'), 'utf8')).includes('captured-v1'));
  const compacted = (await compactPreparations(f.config)).entries.find(value => value.preparationId === retainedId)!;
  assert.equal(compacted.action, 'compacted'); assert.ok(compacted.reclaimedBytes > 0); await verifyCandidate(candidate, f.config);
  const linkedId = randomUUID(), linked = join(f.config.stagingRoot, 'build-' + linkedId), outside = join(f.root, 'separate-reference');
  await mkdir(linked); await mkdir(outside); await writeFile(join(outside, 'keep.txt'), 'reference survives');
  await symlink(outside, join(linked, 'source'), process.platform === 'win32' ? 'junction' : 'dir');
  const record = JSON.parse(await readFile(join(retained, 'preparation.json'), 'utf8')) as Host.PreparationRecord;
  await savePreparation(f.config, { ...record, preparationId: linkedId, phase: 'verified' });
  const refused = (await compactPreparations(f.config)).entries.find(value => value.preparationId === linkedId)!;
  assert.equal(refused.action, 'retained'); assert.equal(refused.code, 'target_conflict');
  assert.equal(await readFile(join(outside, 'keep.txt'), 'utf8'), 'reference survives');
});

test('nested preparation tools reach build children while a fixed snapshot reuses its release identity', { timeout: 30_000 }, async t => {
  const f = await fixture(t), compiler = join(f.root, 'selected-compiler.exe');
  await writeFile(compiler, 'compiler revision one');
  f.config.executables['dotnet'] = process.execPath; f.config.executables['gcc'] = compiler;
  const verify = 'import assert from "node:assert/strict";import fs from "node:fs";' +
    'assert.equal(process.env.IVY_DOTNET,fs.realpathSync(process.execPath));' +
    'assert.ok(fs.readFileSync(process.env.IVY_EVS_CC,"utf8").startsWith("compiler revision "));' +
    'assert.equal(process.env.IVY_EVS_JOBS,undefined);';
  const previous = process.env['IVY_EVS_JOBS']; process.env['IVY_EVS_JOBS'] = '31';
  t.after(() => { if (previous === undefined) delete process.env['IVY_EVS_JOBS']; else process.env['IVY_EVS_JOBS'] = previous; });
  await writeFile(join(f.source, 'tools/check-tools.mjs'), verify);
  f.plan.prepare.unshift({ executable: 'node', args: ['tools/check-tools.mjs'], timeoutMs: 5000 });
  await writeSourcePlan(f.source, f.plan);
  const snapshot = await captureSource(f.source, f.config);
  const original = await prepareCandidate(snapshot, 'fixture', f.config, resolve('.'));
  assert.deepEqual(await prepareCandidate(snapshot, 'fixture', f.config, resolve('.')), original);
  const manifest = JSON.parse(await readFile(original.manifestPath, 'utf8'));
  assert.equal(manifest.provenance, undefined);
  await writeFile(compiler, 'compiler revision two');
  const changed = await prepareCandidate(snapshot, 'fixture', f.config, resolve('.'));
  assert.deepEqual(changed, original);
  await verifyCandidate(original, f.config); await verifyCandidate(changed, f.config);
});

test('preparation keeps the fixed snapshot contract when a selected tool changes during preparation', { timeout: 20_000 }, async t => {
  const f = await fixture(t), compiler = join(f.root, 'selected-compiler.exe');
  await writeFile(compiler, 'original selected bytes'); f.config.executables['gcc'] = compiler;
  f.plan.prepare.push({ executable: 'node', args: ['-e', 'require("node:fs").writeFileSync(process.env.IVY_EVS_CC,"changed during preparation")'], timeoutMs: 5000 });
  await writeSourcePlan(f.source, f.plan);
  const snapshot = await captureSource(f.source, f.config);
  const candidate = await prepareCandidate(snapshot, 'fixture', f.config, resolve('.'));
  await verifyCandidate(candidate, f.config);
  const inventory = await preparationInventory(f.config);
  assert.equal(inventory.entries.length, 1); assert.equal(inventory.entries[0]!.phase, 'compacted');
  const record = JSON.parse(await readFile(join(inventory.entries[0]!.path, 'preparation.json'), 'utf8'));
  assert.equal(record.candidate.candidateId, candidate.candidateId);
});

test('general deployment checks are rejected before preparation starts', { timeout: 20_000 }, async t => {
  const f = await fixture(t);
  f.plan.checks = [{ executable: 'node', args: ['-e', 'process.exit(0)'], timeoutMs: 5000 }];
  await writeSourcePlan(f.source, f.plan);
  const snapshot = await captureSource(f.source, f.config);
  await assert.rejects(prepareCandidate(snapshot, 'fixture', f.config, resolve('.')), { code: 'invalid_arguments' });
});
