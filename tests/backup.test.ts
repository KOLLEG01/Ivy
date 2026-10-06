import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile, symlink, unlink } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { hostFixture, until } from './fixtures/host.js';
import { backupHost } from '../packages/host-runtime/src/private-backup.js';
import { restoreHost, verifyBackup } from '../packages/host-runtime/src/restore.js';
import { HostJournal, ExecutorLock } from '../packages/host-runtime/src/journal.js';
import { RuntimeOwner } from '../packages/host-runtime/src/runtime-owner.js';
import { HostExecutor } from '../packages/host-runtime/src/executor.js';
import { containerName } from '../packages/host-runtime/src/docker.js';
import { atomicJson, jsonFile } from '../packages/host-runtime/src/config.js';
import { fileHash } from '../packages/host-runtime/src/artifact.js';
import { exists } from '../packages/host-runtime/src/backup-files.js';
import { HiveStore } from '../services/hive/src/store.js';
import { Objects } from '../services/hive/src/objects.js';
import { NativeJournal } from '../services/agent-manager/src/journal.js';
import { cli } from '../packages/cli/src/main.js';
import { canonical, digest, hashJson } from '../packages/contracts/src/canonical.js';
import { operationId } from '../packages/contracts/src/operation-id.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Host, Agent } from '../packages/contracts/src/generated.js';
import { servicePaths, hostStorageAreas, resolveHostConfiguration } from '../packages/host-runtime/src/layout.js';
import { collectHostStorage } from '../packages/host-runtime/src/storage-retention.js';

const code = (value: string) => (error: unknown) => error instanceof IvyError && error.code === value;
const distribution = resolve('.');
const projectRoots = (root: string) => ({ projectRoot: join(root, 'normal-projects'), internalProjectRoot: join(root, 'internal-projects') });
// The backup scenario admits an uncertain operation plus the required full-outcome cleanup reserve.
const limits: Agent.Settings['limits'] = { maxOperations: 100, maxJournalBytes: 64 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 };
const mutation = (store: HiveStore, nonce: string) => operationId(store.runtimeEpoch, Date.now(), nonce);

test('backup verification retains the configured daemon home across verifier environments', { timeout: 30000 }, async t => {
  const f = await hostFixture(t), home = join(f.root, 'environment-codex'); await mkdir(home);
  await writeFile(join(home, 'user-marker'), 'Never part of the host backup');
  f.config.instances.push({ instanceId: 'native', serviceNodeId: 'environment-native', componentId: 'agent-manager', enabled: false, engine: 'process',
    settings: { ...projectRoots(f.root), nativeExecutable: process.execPath } });
  await atomicJson(f.configPath, f.config);
  const inherited = process.env['CODEX_HOME']; let backup: Host.BackupResult;
  try { process.env['CODEX_HOME'] = home; backup = await backupHost(f.configPath, join(f.root, 'environment-backup'), distribution); }
  finally { if (inherited === undefined) delete process.env['CODEX_HOME']; else process.env['CODEX_HOME'] = inherited; }
  const manifest = await verifyBackup(backup!.directory, backup!.manifestHash);
  const configuredHome = resolveHostConfiguration(f.config, f.configPath).instances.find(instance => instance.instanceId === 'native')!.settings['codexHome'];
  assert.deepEqual(manifest.sharedNativeHomes, [{ instanceId: 'native', path: configuredHome, kind: 'external-user-state' }]);
  assert.equal(manifest.files.some(file => file.path.endsWith('user-marker')), false);
  assert.equal(await readFile(join(home, 'user-marker'), 'utf8'), 'Never part of the host backup');
});

test('flat services and the configured internal directory restore together while normal projects and shared Codex stay external', { timeout: 30000 }, async t => {
  const f = await hostFixture(t), installation = join(f.root, 'modern'); await mkdir(installation);
  const host: Host.HostConfig = { ...f.config, ivyRoot: installation, servicesRoot: join(installation, 'services'), configPath: join(installation, 'config.json'),
    runtimeRoot: join(installation, 'runtime'), artifactRoot: join(installation, 'artifacts'), stagingRoot: join(installation, 'staging'),
    instances: [{ instanceId: 'native', componentId: 'agent-manager', serviceNodeId: 'flat-native', engine: 'process', enabled: false,
      settings: { ...projectRoots(f.root), codexHome: join(f.root, 'user-codex'), nativeExecutable: process.execPath } }] };
  const paths = servicePaths(host, 'native'); for (const path of Object.values(paths)) await mkdir(path, { recursive: true });
  await writeFile(join(paths.work, 'permanent.txt'), 'Work is permanent'); await writeFile(join(paths.logs, 'process.log'), 'Diagnostic');
  const location = { cwd: String(host.instances[0]!.settings['internalProjectRoot']) }, normal = { cwd: join(f.root, 'normal-project') };
  await mkdir(location.cwd, { recursive: true }); await mkdir(normal.cwd);
  await writeFile(join(location.cwd, 'result.md'), 'Saved internal result');
  await writeFile(join(normal.cwd, 'private.txt'), 'External user project');
  await atomicJson(host.configPath!, host); const backup = await backupHost(host.configPath!, join(f.root, 'modern-backup'), distribution), manifest = await verifyBackup(backup.directory, backup.manifestHash);
  assert.ok(manifest.files.some(file => file.path === 'services/native/work/permanent.txt'));
  assert.ok(manifest.files.some(file => file.path.startsWith('projects/native/') && file.path.endsWith('/result.md')));
  assert.equal(manifest.files.some(file => file.path.endsWith('/private.txt')), false);
  const targetRoot = join(f.root, 'restored-modern'); await mkdir(targetRoot);
  const target = structuredClone(host); target.ivyRoot = targetRoot; target.servicesRoot = join(targetRoot, 'services'); target.configPath = join(targetRoot, 'config.json');
  target.runtimeRoot = join(targetRoot, 'runtime'); target.artifactRoot = join(targetRoot, 'artifacts'); target.stagingRoot = join(targetRoot, 'staging'); target.publicBaseUrl = 'http://127.0.0.1:39852/ivy';
  target.instances[0]!.settings['internalProjectRoot'] = join(targetRoot, 'codex-projects');
  const template = join(targetRoot, 'template.json'); await atomicJson(template, target);
  const restored = await restoreHost(backup.directory, backup.manifestHash, template, distribution); assert.equal(restored.configPath, target.configPath);
  assert.equal(await readFile(join(servicePaths(target, 'native').work, 'permanent.txt'), 'utf8'), 'Work is permanent');
  assert.equal(await readFile(join(String(target.instances[0]!.settings['internalProjectRoot']), 'result.md'), 'utf8'), 'Saved internal result');
  assert.equal(await readFile(join(location.cwd, 'result.md'), 'utf8'), 'Saved internal result');
  assert.equal(await readFile(join(normal.cwd, 'private.txt'), 'utf8'), 'External user project');
  assert.ok(hostStorageAreas(target).every(area => !area.path.includes('services/agent-manager/main')));
});

test('shared Codex user history stays outside backup and disabled restore preserves its current bytes and binding', { timeout: 30000 }, async t => {
  const f = await hostFixture(t), home = join(f.root, 'shared-user-home');
  await mkdir(join(home, 'sessions'), { recursive: true });
  const session = join(home, 'sessions/user-session.jsonl'); await writeFile(session, 'Before backup');
  f.config.instances.push({ instanceId: 'native', serviceNodeId: 'shared-native', componentId: 'agent-manager', engine: 'process', enabled: false,
    settings: { ...projectRoots(f.root), codexHome: home, nativeExecutable: process.execPath } });
  await atomicJson(f.configPath, f.config);
  const backup = await backupHost(f.configPath, join(f.root, 'backup-shared'), distribution);
  const manifest = await verifyBackup(backup.directory, backup.manifestHash);
  assert.deepEqual(manifest.sharedNativeHomes, [{ instanceId: 'native', path: home, kind: 'external-user-state' }]);
  assert.equal(manifest.files.some(file => file.path.includes('user-session')), false);
  await writeFile(session, 'User work created after backup');
  const target = structuredClone(f.config);
  target.runtimeRoot = join(f.root, 'new-runtime'); target.artifactRoot = join(f.root, 'new-artifacts'); target.stagingRoot = join(f.root, 'new-staging');
  target.publicBaseUrl = 'http://127.0.0.1:39082/ivy'; for (const instance of target.instances) instance.enabled = false;
  target.instances.find(instance => instance.instanceId === 'native')!.settings['internalProjectRoot'] = join(f.root, 'restored-internal-projects');
  const template = join(f.root, 'shared-template.json'), invalid = structuredClone(target);
  invalid.instances.find(instance => instance.instanceId === 'native')!.settings['codexHome'] = join(f.root, 'different-home');
  await atomicJson(template, invalid); await assert.rejects(restoreHost(backup.directory, backup.manifestHash, template, distribution), code('target_conflict'));
  await atomicJson(template, target); const restored = await restoreHost(backup.directory, backup.manifestHash, template, distribution);
  const config = await jsonFile<Host.HostConfig>(restored.configPath);
  assert.equal(restored.state, 'disabled'); assert.ok(config.restoredFrom);
  assert.equal(config.instances.find(instance => instance.instanceId === 'native')!.settings['codexHome'], home);
  assert.equal(await readFile(session, 'utf8'), 'User work created after backup');
  assert.equal(await exists(join(config.runtimeRoot, 'instances/native/data/shared-user-home')), false);
});

test('private complete backup restores exact history disabled and never replays uncertain effects', { timeout: 90_000 }, async t => {
  const f = await hostFixture(t), candidate = await f.prepare('restored-real-process');
  const retiring = await f.prepare('retained-other-release');
  await collectHostStorage(f.config);
  assert.ok(f.journal.storageRetentionStatus());
  assert.equal(f.journal.retireCandidate(retiring.candidateId), true);
  f.journal.db.prepare('INSERT INTO meta VALUES (?,?)').run('bootstrap-rollback:first',
    canonical({ candidateId: candidate.candidateId, selectedCandidateId: retiring.candidateId }));
  const hiveData = join(f.config.runtimeRoot, 'instances/hive/data'), nativeData = join(f.config.runtimeRoot, 'instances/native/data');
  const secret = 'synthetic-backup-credential-only';
  f.config.instances.push({ instanceId: 'hive', componentId: 'hive', serviceNodeId: 'backup.hive', engine: 'process', enabled: false,
    settings: { listenHost: '127.0.0.1', listenPort: 39831, credentials: [{ principalId: 'backup-user', token: secret }],
      backup: { directory: join(f.root, 'old-hive-backups'), intervalHours: 24, retain: 7 } } },
  { instanceId: 'native', componentId: 'agent-manager', serviceNodeId: 'backup.native', engine: 'process', enabled: false,
    settings: { ...projectRoots(f.root), appServer: { mode: 'owned-stdio' }, nativeExecutable: process.execPath, nativeHome: join(nativeData, 'native-home') } });
  await atomicJson(f.configPath, f.config);
  const source = new HiveStore(join(hiveData, 'hive.sqlite')); source.configureCredentials([{ principalId: 'backup-user', digest: digest(secret) }]);
  const objects = new Objects(source), context = { credentialDigest: digest(secret), principalId: 'backup-user' };
  objects.registerAgent(context, { mutationId: mutation(source, 'backup-contract'), definition: { key: 'backup/text', version: '1.0.0', owner: { kind: 'agent' }, mediaType: 'text/markdown', retention: { objects: { mode: 'retain' }, revisions: { mode: 'all' } }, specMarkdown: 'Backup test content.' } });
  const write = { mutationId: mutation(source, 'once-before-snapshot'), contractVersion: '1.0.0', references: {}, create: { contractKey: 'backup/text', name: 'retained', parentId: null, ownerObjectId: null }, content: { encoding: 'text' as const, value: 'original private text' } };
  const saved = objects.write(context, write);
  objects.write(context, { mutationId: mutation(source, 'later-edit'), objectId: saved.object.id, expectedRevision: 1, contractVersion: '1.0.0', references: {}, content: { encoding: 'text', value: 'second version' } });
  const expected = objects.read({ objectId: saved.object.id });
  source.run('INSERT INTO service_nodes(id,principal_id,host_id,service_name,generation,status_json) VALUES (?,?,?,?,1,?)', 'backup.native', 'backup-user', f.config.hostId, 'agent-manager', canonical({ connected: true, synced: true, ready: true })); source.close();
  await mkdir(join(nativeData, 'native-home/sessions'), { recursive: true });
  const history = '{"type":"retained-native-history","id":"original-thread","value":"not interpreted or rewritten"}\n';
  await writeFile(join(nativeData, 'native-home/sessions/thread.jsonl'), history);
  await writeFile(join(nativeData, 'native-home/sessions/executor-lock.sqlite'), 'A user file sharing an OS lock basename must survive.');
  await mkdir(join(nativeData, 'native-home/tmp/arg0/codex-arg0-fixture'), { recursive: true });
  await writeFile(join(nativeData, 'native-home/tmp/arg0/codex-arg0-fixture/apply_patch.bat'), 'Derived helper must be recreated.');
  await writeFile(join(nativeData, 'native-home/tmp/retained.txt'), 'Other native temporary data remains in the snapshot.');
  const owner = { hostId: f.config.hostId, serviceNodeId: 'backup.native', nativeVersion: '0.154.0', nativeExecutableHash: digest('synthetic native identity') };
  const native = new NativeJournal(owner, limits), operation = { callerPrincipalId: 'backup-user', operationId: 'native-uncertain' };
  native.beginEpoch('original-epoch'); native.accept(operation, 'turn/start', { threadId: 'original-thread' }); native.dispatch(operation, 'original-epoch', 1); native.close();
  f.journal.setInstalled({ instanceId: 'first', candidateId: candidate.candidateId, buildId: candidate.buildId, enabled: true, installedAt: new Date().toISOString() });
  const terminal = f.journal.accept({ action: 'deploy', instanceId: 'first', operationId: 'past-definite-failure', candidateId: candidate.candidateId });
  const failed = f.journal.advance(terminal.record.deploymentId, 'prepared', 'failed', { readiness: { state: 'failed', message: 'Original fixture preflight failed before any action.' } });
  const uncertain = f.journal.accept({ action: 'deploy', instanceId: 'first', operationId: 'accepted-before-snapshot', candidateId: candidate.candidateId });
  const originalOperations = hashJson(f.journal.list()), originalConfig = await readFile(f.configPath, 'utf8');
  const invocation = await cli(['backup', '--config', f.configPath, '--destination', join(f.root, 'private-backup'), '--json']);
  assert.equal(invocation.exitCode, 0, JSON.stringify(invocation.output)); assert.equal(JSON.stringify(invocation.output).includes(secret), false);
  const backup = invocation.output.data as Host.BackupResult, manifest = await verifyBackup(backup.directory, backup.manifestHash);
  assert.equal(manifest.hiveSnapshots.length, 1); assert.ok(manifest.files.some(file => file.path.endsWith('thread.jsonl')));
  assert.deepEqual(manifest.ephemeralNativePaths, [{ instanceId: 'native', path: 'instances/native/data/native-home/tmp/arg0', kind: 'codex-arg0-helper-cache' }]);
  assert.equal(hashJson(f.journal.list()), originalOperations); assert.equal(await readFile(f.configPath, 'utf8'), originalConfig);
  const target: Host.HostConfig = { ...f.config, runtimeRoot: join(f.root, 'restored-state'), artifactRoot: join(f.root, 'restored-artifacts'), stagingRoot: join(f.root, 'restored-staging'), publicBaseUrl: 'http://127.0.0.1:39832/isolated',
    instances: f.config.instances.map(instance => ({ ...instance, enabled: false, settings: { ...instance.settings } })) };
  target.instances.find(value => value.instanceId === 'hive')!.settings['backup'] = { directory: join(target.runtimeRoot, 'hive-backups'), intervalHours: 24, retain: 7 };
  target.instances.find(value => value.instanceId === 'hive')!.settings['listenPort'] = 39832;
  target.instances.find(value => value.instanceId === 'native')!.settings['nativeHome'] = join(target.runtimeRoot, 'instances/native/data/native-home');
  target.instances.find(value => value.instanceId === 'native')!.settings['internalProjectRoot'] = join(f.root, 'restored-internal-projects');
  const template = join(f.root, 'restore-template.json'); await atomicJson(template, target);
  const invalidListener = structuredClone(target); invalidListener.instances.find(value => value.instanceId === 'hive')!.settings['listenPort'] = 39831;
  await atomicJson(template, invalidListener); await assert.rejects(restoreHost(backup.directory, backup.manifestHash, template, distribution), code('target_conflict')); await atomicJson(template, target);
  const restored = await cli(['restore', '--config', template, '--backup', backup.directory, '--backup-hash', backup.manifestHash, '--json']);
  assert.equal(restored.exitCode, 0, JSON.stringify(restored.output)); const result = restored.output.data as Host.RestoreResult;
  const config = await jsonFile<Host.HostConfig>(result.configPath), journal = new HostJournal(config); f.cleanups.push(async () => journal.close());
  assert.equal(result.state, 'disabled'); assert.ok(config.restoredFrom); assert.ok(config.instances.every(instance => !instance.enabled));
  assert.deepEqual(journal.observations(), {}); assert.equal(journal.target('first'), null); assert.equal(journal.installed('first'), null);
  assert.deepEqual(journal.pendingCandidateRetirements(), []);
  assert.equal(journal.storageRetentionStatus(), null);
  assert.equal(journal.db.prepare("SELECT 1 FROM meta WHERE key LIKE 'bootstrap-rollback:%'").get(), undefined);
  assert.deepEqual(f.journal.pendingCandidateRetirements(), [retiring]);
  assert.ok(f.journal.storageRetentionStatus());
  assert.equal(await fileHash(join(config.runtimeRoot, 'recovery-original/deployments.sqlite')),
    await fileHash(join(backup.directory, 'state/deployments.sqlite')));
  const { restoredFrom: _originalRecovery, ...bypass } = config;
  assert.throws(() => new HostJournal(bypass), code('target_conflict'));
  const restoredFailed = journal.get(failed.record.deploymentId);
  assert.deepEqual({ ...restoredFailed, configurationPath: '' }, { ...failed, configurationPath: '' });
  assert.deepEqual(journal.accept(failed.request), restoredFailed);
  const held = journal.get(uncertain.record.deploymentId); assert.equal(held.record.phase, 'needs_attention'); assert.equal(held.record.requestHash, uncertain.record.requestHash); assert.deepEqual(held.request, uncertain.request);
  assert.equal(journal.unfinished().length, 0);
  assert.equal(await exists(join(config.runtimeRoot, 'restore.json')), false);
  assert.equal(await readFile(join(target.runtimeRoot, 'instances/native/data/native-home/sessions/thread.jsonl'), 'utf8'), history);
  assert.equal(await readFile(join(target.runtimeRoot, 'instances/native/data/native-home/sessions/executor-lock.sqlite'), 'utf8'), 'A user file sharing an OS lock basename must survive.');
  assert.equal(await exists(join(target.runtimeRoot, 'instances/native/data/native-home/tmp/arg0')), false);
  assert.equal(await readFile(join(target.runtimeRoot, 'instances/native/data/native-home/tmp/retained.txt'), 'utf8'), 'Other native temporary data remains in the snapshot.');
  const nativeRestored = new NativeJournal(owner, limits);
  nativeRestored.beginEpoch('new-epoch'); assert.equal(nativeRestored.get(operation), null); nativeRestored.close();
  const hiveRestored = new HiveStore(join(target.runtimeRoot, 'instances/hive/data/hive.sqlite'));
  try {
    const restoredObjects = new Objects(hiveRestored); assert.deepEqual(restoredObjects.read({ objectId: saved.object.id }), expected);
    assert.deepEqual(restoredObjects.write(context, write), saved);
    assert.equal(JSON.parse(String(hiveRestored.get('SELECT status_json FROM service_nodes')!['status_json'])).connected, false);
    assert.equal(hiveRestored.all('PRAGMA table_info(service_nodes)').some(column => column['name'] === 'generation'), true);
    assert.equal(hiveRestored.get('SELECT generation FROM service_nodes')!['generation'], 1);
    assert.equal(hiveRestored.get('SELECT count(*) AS n FROM events')!['n'], 2);
    const searchId = hiveRestored.get('SELECT search_id FROM objects WHERE id=?', saved.object.id)!['search_id'];
    assert.deepEqual(restoredObjects.search({ text: 'second' }).items.map(item => item.id), [saved.object.id]);
    hiveRestored.db.exec('VACUUM');
    restoredObjects.write(context, { mutationId: mutation(hiveRestored, 'after-restore-search'), objectId: saved.object.id, expectedRevision: 2, contractVersion: '1.0.0', references: {}, content: { encoding: 'text', value: 'restoredneedle' } });
    assert.deepEqual(restoredObjects.search({ text: 'second' }).items, []);
    assert.deepEqual(restoredObjects.search({ text: 'restoredneedle' }).items.map(item => item.id), [saved.object.id]);
    assert.equal(hiveRestored.get('SELECT search_id FROM objects WHERE id=?', saved.object.id)!['search_id'], searchId);
  } finally { hiveRestored.close(); }
  assert.throws(() => journal.candidate(candidate.candidateId), code('not_found'));
  assert.notEqual(containerName(config.hostId, 'hive', { runtimeRoot: config.runtimeRoot, backupId: backup.backupId }), containerName(f.config.hostId, 'hive'));
  const runtimeOwner = new RuntimeOwner(config, 'first', distribution), executor = new HostExecutor(config, result.configPath, distribution);
  runtimeOwner.start(); executor.start(); f.cleanups.push(async () => runtimeOwner.close()); f.cleanups.push(async () => executor.close());
  const redeploy = await cli(['deploy', '--config', result.configPath, '--instance', 'first', '--source', f.source, '--operation-id', 'explicit-redeploy-after-restore', '--json']);
  assert.equal(redeploy.exitCode, 0); await until(() => journal.get(redeploy.output.deploymentId!).record.phase === 'succeeded', 30_000);
  assert.equal(journal.installed('first')!.candidateId, retiring.candidateId);
  const enabled = journal.accept({ action: 'enable', instanceId: 'first', operationId: 'explicit-enable-after-reconciliation' });
  await until(() => journal.get(enabled.record.deploymentId).record.phase === 'succeeded', 15_000);
  assert.match(await readFile(join(config.runtimeRoot, 'instances/first/data/launches.txt'), 'utf8'), /^retained-other-release /);
  assert.equal(journal.list().filter(entry => entry.request.operationId === uncertain.request.operationId).length, 1);
  assert.equal(hashJson(f.journal.list()), originalOperations); assert.equal(await readFile(f.configPath, 'utf8'), originalConfig);
  await verifyBackup(backup.directory, backup.manifestHash);
});

test('data backup excludes local releases and disabled restore requires an explicit redeploy', { timeout: 45_000 }, async t => {
  const f = await hostFixture(t), first = await f.prepare('parallel-first'); await f.prepare('parallel-second');
  const backup = await backupHost(f.configPath, join(f.root, 'parallel-backup'), distribution);
  const manifest = await jsonFile<Host.BackupManifest>(join(backup.directory, 'backup.json'));
  assert.equal('candidates' in manifest, false);
  assert.equal(manifest.files.some(file => file.path.startsWith('releases/')), false);
  const originalJournal = await fileHash(join(backup.directory, 'state/deployments.sqlite'));
  const target: Host.HostConfig = { ...f.config, runtimeRoot: join(f.root, 'parallel-state'), artifactRoot: join(f.root, 'parallel-artifacts'),
    stagingRoot: join(f.root, 'parallel-staging'), publicBaseUrl: 'http://127.0.0.1:39848/ivy', instances: f.config.instances.map(instance => ({ ...instance, enabled: false })) };
  const template = join(f.root, 'parallel-target.json'); await atomicJson(template, target);
  await restoreHost(backup.directory, backup.manifestHash, template, distribution);
  const config = await jsonFile<Host.HostConfig>(join(target.runtimeRoot, 'host.json')), journal = new HostJournal(config);
  f.cleanups.push(async () => journal.close());
  assert.ok(config.restoredFrom); assert.ok(config.instances.every(instance => !instance.enabled));
  assert.notEqual(await fileHash(join(target.runtimeRoot, 'deployments.sqlite')), originalJournal);
  assert.equal(await fileHash(join(target.runtimeRoot, 'recovery-original/deployments.sqlite')), originalJournal);
  assert.throws(() => journal.candidate(first.candidateId), code('not_found'));
  assert.equal(journal.installed('first'), null);
  assert.equal(await exists(join(target.runtimeRoot, 'instances/first/data/launches.txt')), false);
  await verifyBackup(backup.directory, backup.manifestHash);
});

test('restore retains nested prior recovery evidence without overwriting it', { timeout: 45_000 }, async t => {
  const f = await hostFixture(t); await f.prepare('nested-recovery');
  for (const [path, value] of [['recovery-original/configuration.json', 'first original'], ['previous-recovery/recovery-original/configuration.json', 'older original'], ['native-recovery.json', 'prior native report']]) {
    const target = join(f.config.runtimeRoot, path!); await mkdir(resolve(target, '..'), { recursive: true }); await writeFile(target, value!);
  }
  const backup = await backupHost(f.configPath, join(f.root, 'nested-backup'), distribution);
  const template = join(f.root, 'nested-target.json'), target: Host.HostConfig = { ...f.config, runtimeRoot: join(f.root, 'nested-state'), artifactRoot: join(f.root, 'nested-artifacts'),
    stagingRoot: join(f.root, 'nested-staging'), publicBaseUrl: 'http://127.0.0.1:39847/ivy', instances: f.config.instances.map(instance => ({ ...instance, enabled: false })) };
  await atomicJson(template, target); await restoreHost(backup.directory, backup.manifestHash, template, distribution);
  assert.equal(await readFile(join(target.runtimeRoot, 'previous-recovery/recovery-original/configuration.json'), 'utf8'), 'first original');
  assert.equal(await readFile(join(target.runtimeRoot, 'previous-recovery/previous-recovery/recovery-original/configuration.json'), 'utf8'), 'older original');
  assert.equal(await readFile(join(target.runtimeRoot, 'previous-recovery/native-recovery.json'), 'utf8'), 'prior native report');
  assert.deepEqual(await jsonFile(join(target.runtimeRoot, 'recovery-original/configuration.json')), f.config);
  await verifyBackup(backup.directory, backup.manifestHash);
});

test('unsupported native recovery leaves no published restored configuration', { timeout: 45_000 }, async t => {
  const f = await hostFixture(t); await f.prepare('incomplete-native-recovery');
  const home = join(f.config.runtimeRoot, 'instances/native/data/native-home'); await mkdir(home, { recursive: true });
  await writeFile(join(home, 'state_6.sqlite'), 'unsupported future native storage');
  f.config.instances.push({ instanceId: 'native', componentId: 'agent-manager', serviceNodeId: 'backup.native', enabled: false, engine: 'process',
    settings: { ...projectRoots(f.root), appServer: { mode: 'owned-stdio' }, nativeHome: home, nativeExecutable: process.execPath, nativeVersion: '0.154.0' } });
  await atomicJson(f.configPath, f.config);
  const backup = await backupHost(f.configPath, join(f.root, 'incomplete-backup'), distribution);
  const target: Host.HostConfig = { ...f.config, runtimeRoot: join(f.root, 'incomplete-state'), artifactRoot: join(f.root, 'incomplete-artifacts'), stagingRoot: join(f.root, 'incomplete-staging'),
    publicBaseUrl: 'http://127.0.0.1:39848/ivy', instances: f.config.instances.map(instance => ({ ...instance, enabled: false, settings: { ...instance.settings } })) };
  target.instances.find(instance => instance.instanceId === 'native')!.settings['nativeHome'] = join(target.runtimeRoot, 'instances/native/data/native-home');
  target.instances.find(instance => instance.instanceId === 'native')!.settings['internalProjectRoot'] = join(f.root, 'restored-internal-projects');
  const template = join(f.root, 'incomplete-template.json'); await atomicJson(template, target);
  await assert.rejects(restoreHost(backup.directory, backup.manifestHash, template, distribution), code('unsupported_storage'));
  assert.equal(await exists(join(target.runtimeRoot, 'host.json')), false);
  assert.equal(await exists(join(target.runtimeRoot, 'native-recovery.json')), false);
  await verifyBackup(backup.directory, backup.manifestHash);
});

test('complete backup refuses live owners, unresolved process fences and outside links before publication', { timeout: 30_000 }, async t => {
  const f = await hostFixture(t);
  const lock = new ExecutorLock(join(f.config.runtimeRoot, 'owners/first'));
  try { await assert.rejects(backupHost(f.configPath, join(f.root, 'busy'), distribution), code('backup_busy')); }
  finally { lock.close(); }
  assert.equal(await exists(join(f.root, 'busy')), false);
  const data = join(f.config.runtimeRoot, 'instances/first/data'); await mkdir(data, { recursive: true });
  await atomicJson(join(data, 'process-stop-fence.json'), { schemaVersion: 1, owner: 'runtime-owner', ownerId: 'unresolved-owner', processId: null,
    observedAt: new Date().toISOString(), code: 'outcome_unknown' });
  await assert.rejects(backupHost(f.configPath, join(f.root, 'fenced'), distribution), code('outcome_unknown')); assert.equal(await exists(join(f.root, 'fenced')), false);
  await unlink(join(data, 'process-stop-fence.json'));
  const outside = join(f.root, 'outside'); await mkdir(outside); await writeFile(join(outside, 'private.txt'), 'untouched');
  await symlink(outside, join(data, 'linked'), process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(backupHost(f.configPath, join(f.root, 'linked-backup'), distribution), code('target_conflict'));
  assert.equal(await readFile(join(outside, 'private.txt'), 'utf8'), 'untouched'); await unlink(join(data, 'linked'));
});

test('backup and disabled restore durations stay valid when the wall clock moves backwards', { timeout: 30_000 }, async t => {
  const f = await hostFixture(t), realNow = Date.now;
  let offset = 0; const clock = t.mock.method(Date, 'now', () => realNow() + offset);
  const backupWork = backupHost(f.configPath, join(f.root, 'clock-backup'), distribution); offset = -60000;
  const backup = await backupWork;
  assert.ok(Number.isSafeInteger(backup.elapsedMs) && backup.elapsedMs >= 0 && backup.elapsedMs < 30000);
  const target: Host.HostConfig = { ...f.config, runtimeRoot: join(f.root, 'clock-state'), artifactRoot: join(f.root, 'clock-artifacts'), stagingRoot: join(f.root, 'clock-staging'),
    publicBaseUrl: 'http://127.0.0.1:39849/ivy', instances: f.config.instances.map(instance => ({ ...instance, enabled: false })) };
  const template = join(f.root, 'clock-template.json'); await atomicJson(template, target);
  offset = 0; const restoreWork = restoreHost(backup.directory, backup.manifestHash, template, distribution); offset = -60000;
  const restored = await restoreWork; clock.mock.restore();
  assert.equal(restored.state, 'disabled'); assert.ok(Number.isSafeInteger(restored.elapsedMs) && restored.elapsedMs >= 0 && restored.elapsedMs < 30000);
  await verifyBackup(backup.directory, backup.manifestHash);
});

test('corrupt backup payload, reused roots and changed executable prerequisites cannot create a restored owner', { timeout: 30_000 }, async t => {
  const f = await hostFixture(t); const backup = await backupHost(f.configPath, join(f.root, 'backup'), distribution);
  const template: Host.HostConfig = { ...f.config, instances: f.config.instances.map(instance => ({ ...instance, enabled: false })),
    runtimeRoot: join(f.root, 'new-state'), artifactRoot: join(f.root, 'new-artifacts'), stagingRoot: join(f.root, 'new-staging'), publicBaseUrl: 'http://127.0.0.1:39839/isolated' };
  const path = join(f.root, 'target.json'); await atomicJson(path, template);
  await assert.rejects(restoreHost(backup.directory, digest('incorrect manifest'), path, distribution), code('backup_changed'));
  await atomicJson(path, { ...template, runtimeRoot: f.config.runtimeRoot });
  await assert.rejects(restoreHost(backup.directory, backup.manifestHash, path, distribution), code('target_conflict')); await atomicJson(path, template);
  const manifestPath = join(backup.directory, 'backup.json'), originalManifest = await readFile(manifestPath), changed = await jsonFile<Host.BackupManifest>(manifestPath);
  changed.externalFiles[0]!.hash = digest('different runtime bytes'); await atomicJson(manifestPath, changed);
  await assert.rejects(restoreHost(backup.directory, await fileHash(manifestPath), path, distribution), code('runtime_changed')); await writeFile(manifestPath, originalManifest);
  const original = await readFile(join(backup.directory, 'configuration.json')); await writeFile(join(backup.directory, 'configuration.json'), Buffer.concat([original, Buffer.from(' ')]));
  await assert.rejects(restoreHost(backup.directory, backup.manifestHash, path, distribution), code('backup_changed'));
  assert.equal(await exists(template.runtimeRoot), false);
  assert.equal((await cli(['backup', '--config', f.configPath, '--destination', backup.directory, '--json'])).output.code, 'target_conflict');
});

test('direct Hive database owners prevent complete backup even without a runtime owner', { timeout: 30_000 }, async t => {
  const f = await hostFixture(t), nativeData = join(f.config.runtimeRoot, 'instances/native/data');
  f.config.instances.push({ instanceId: 'native', serviceNodeId: 'native-test', componentId: 'agent-manager', engine: 'process', enabled: false,
    settings: { ...projectRoots(f.root), appServer: { mode: 'owned-stdio' }, nativeExecutable: process.execPath, nativeHome: join(nativeData, 'native-home') } },
  { instanceId: 'hive', serviceNodeId: 'hive-test', componentId: 'hive', engine: 'process', enabled: false,
    settings: { listenHost: '127.0.0.1', listenPort: 39835, credentials: [{ principalId: 'test', token: 'synthetic-only' }],
      backup: { directory: join(f.root, 'daily'), intervalHours: 24, retain: 7 } } });
  await atomicJson(f.configPath, f.config);
  const hive = new HiveStore(join(f.config.runtimeRoot, 'instances/hive/data/hive.sqlite'));
  const failed = join(f.root, 'hive-live');
  try { await assert.rejects(backupHost(f.configPath, failed, distribution), code('command_failed')); }
  finally { hive.close(); }
  assert.equal(await exists(join(failed, 'backup.json')), false);
  const evidence = await fileHash(join(failed, 'backup-failure.json'));
  const completed = await backupHost(f.configPath, join(f.root, 'owners-stopped'), distribution);
  assert.equal((await verifyBackup(completed.directory, completed.manifestHash)).hiveSnapshots.length, 1);
  assert.equal(await fileHash(join(failed, 'backup-failure.json')), evidence);
});

test('native helper aliases are excluded only inside the exact owned Codex cache', { timeout: 30_000 }, async t => {
  const f = await hostFixture(t), home = join(f.config.runtimeRoot, 'instances/native/data/native-home');
  f.config.instances.push({ instanceId: 'native', serviceNodeId: 'native-test', componentId: 'agent-manager', engine: 'process', enabled: false,
    settings: { ...projectRoots(f.root), appServer: { mode: 'owned-stdio' }, nativeExecutable: process.execPath, nativeHome: home } });
  await atomicJson(f.configPath, f.config);
  const cache = join(home, 'tmp/arg0'), session = join(cache, 'codex-arg0-fixture'), outside = join(f.root, 'outside');
  await mkdir(session, { recursive: true }); await mkdir(outside); await writeFile(join(outside, 'original.txt'), 'Untouched outside data.');
  const alias = join(session, 'apply_patch');
  await symlink(process.platform === 'win32' ? outside : process.execPath, alias, process.platform === 'win32' ? 'junction' : 'file');
  await writeFile(join(home, 'tmp/arg0-history.txt'), 'Similar names must survive.');
  const result = await backupHost(f.configPath, join(f.root, 'backup'), distribution), manifest = await verifyBackup(result.directory, result.manifestHash);
  assert.equal(manifest.files.some(file => file.path.includes('/tmp/arg0/')), false);
  assert.ok(manifest.files.some(file => file.path.endsWith('/tmp/arg0-history.txt')));
  assert.equal(await exists(alias), true); assert.equal(await readFile(join(outside, 'original.txt'), 'utf8'), 'Untouched outside data.');
  const manifestPath = join(result.directory, 'backup.json'), original = await readFile(manifestPath);
  manifest.ephemeralNativePaths![0]!.path = 'instances/native/data/native-home/sessions'; await atomicJson(manifestPath, manifest);
  await assert.rejects(verifyBackup(result.directory, await fileHash(manifestPath)), code('backup_invalid')); await writeFile(manifestPath, original);
  const legacy = JSON.parse(original.toString()) as Host.BackupManifest; delete legacy.ephemeralNativePaths;
  await atomicJson(manifestPath, legacy); await verifyBackup(result.directory, await fileHash(manifestPath)); await writeFile(manifestPath, original);
  await symlink(outside, join(home, 'tmp/other-linked'), process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(backupHost(f.configPath, join(f.root, 'other-link'), distribution), code('target_conflict')); await unlink(join(home, 'tmp/other-linked'));
  // The old cache becomes ordinary runtime data once the configured home changes.
  await unlink(alias);
  await atomicJson(f.configPath, { ...f.config, instances: f.config.instances.map(instance => instance.instanceId === 'native' ? { ...instance, settings: { ...instance.settings, nativeHome: outside } } : instance) });
  const sharedBackup = await backupHost(f.configPath, join(f.root, 'outside-home'), distribution);
  const sharedManifest = await verifyBackup(sharedBackup.directory, sharedBackup.manifestHash);
  assert.deepEqual(sharedManifest.sharedNativeHomes, [{ instanceId: 'native', path: outside, kind: 'external-user-state' }]);
  assert.equal(sharedManifest.files.some(file => file.path.endsWith('original.txt')), false);
  assert.equal(await readFile(join(outside, 'original.txt'), 'utf8'), 'Untouched outside data.');
  await atomicJson(f.configPath, f.config);
  const otherHome = join(f.config.runtimeRoot, 'instances/native/data/linked-cache-home'); await mkdir(join(otherHome, 'tmp'), { recursive: true });
  await symlink(outside, join(otherHome, 'tmp/arg0'), process.platform === 'win32' ? 'junction' : 'dir');
  f.config.instances.find(instance => instance.instanceId === 'native')!.settings['nativeHome'] = otherHome; await atomicJson(f.configPath, f.config);
  await assert.rejects(backupHost(f.configPath, join(f.root, 'linked-cache'), distribution), code('target_conflict'));
});
