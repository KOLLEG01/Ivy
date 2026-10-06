import test from 'node:test';
import assert from 'node:assert/strict';
import { join, resolve } from 'node:path';
import { mkdir, mkdtemp, readFile, rename, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import { hostFixture, until } from './fixtures/host.js';
import { RuntimeOwner } from '../packages/host-runtime/src/runtime-owner.js';
import { HostExecutor, readinessExpired, readinessFailureStart, replacementNeedsSettledObservation, runningHiveCanBeInspected, stoppedTargetIsConclusive } from '../packages/host-runtime/src/executor.js';
import { candidateBoundSettings, materializeTarget } from '../packages/host-runtime/src/target.js';
import { fileHash } from '../packages/host-runtime/src/artifact.js';
import type { DockerAdapter } from '../packages/host-runtime/src/docker.js';
import { startProcess } from '../packages/host-runtime/src/process.js';
import { cli } from '../packages/cli/src/main.js';
import { HostJournal, terminalPhases } from '../packages/host-runtime/src/journal.js';
import { atomicJson } from '../packages/host-runtime/src/config.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { captureSource } from '../packages/host-runtime/src/source.js';
import { prepareCandidate } from '../packages/host-runtime/src/prepare.js';
import type { Host } from '../packages/contracts/src/generated.js';
import { collectHostStorage } from '../packages/host-runtime/src/storage-retention.js';

test('executor heartbeat reflects completed manual storage collection', async t => {
  const f = await hostFixture(t);
  const unresolved = join(f.config.stagingRoot, 'build-11111111-1111-4111-8111-111111111111');
  await mkdir(unresolved, { recursive: true });
  await collectHostStorage(f.config);
  assert.equal(f.journal.storageRetentionStatus()?.state, 'partial');
  const executor = new HostExecutor(f.config, f.configPath, resolve('.'));
  executor.start(); f.cleanups.push(() => executor.close());
  const observed = async () => JSON.parse(await readFile(join(f.config.runtimeRoot, 'executor.json'), 'utf8')) as Host.ExecutorStatus;
  await until(async () => (await observed().catch(() => null))?.code === 'storage_retention_attention');
  await rename(unresolved, join(f.root, 'retained-failed-preparation'));
  await collectHostStorage(f.config);
  assert.equal(f.journal.storageRetentionStatus()?.state, 'succeeded');
  await until(async () => (await observed()).code === null);
  await mkdir(unresolved);
  await collectHostStorage(f.config);
  assert.equal(f.journal.storageRetentionStatus()?.state, 'partial');
  await until(async () => (await observed()).code === 'storage_retention_attention');
});

test('Phone target binds its native hash to the selected candidate instead of stale host state', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-phone-target-')); t.after(() => import('node:fs/promises').then(fs => fs.rm(root, { recursive: true, force: true })));
  const executable = 'dist/native/phone/Ivy.PhoneRuntime.exe', actual = join(root, executable);
  await mkdir(join(root, 'dist/native/phone'), { recursive: true }); await writeFile(actual, 'selected candidate bytes');
  const input = { native: { executable, executableHash: 'sha256:' + '0'.repeat(64) }, retained: true } as Host.InstanceConfig['settings'];
  const settings = await candidateBoundSettings('phone-bridge', input, root) as Record<string, unknown>;
  assert.equal(((settings['native'] as Record<string, unknown>)['executableHash']), await fileHash(actual));
  assert.equal(((input as Record<string, unknown>)['native'] as Record<string, unknown>)['executableHash'], 'sha256:' + '0'.repeat(64));
  assert.deepEqual(await candidateBoundSettings('fixture', input, root), input);
});

test('executor readiness deadlines begin at the first failed check after a ready period', () => {
  const firstFailure = readinessFailureStart(0, 1_000);
  assert.equal(firstFailure, 1_000);
  assert.equal(readinessFailureStart(firstFailure, 2_000), firstFailure);
  assert.equal(readinessExpired(firstFailure, 500, 1_499), false);
  assert.equal(readinessExpired(firstFailure, 500, 1_501), true);
});

test('a target for the installed build can be drained without waiting for a fresh steady-state observation', () => {
  assert.equal(replacementNeedsSettledObservation('installed', 'installed'), false);
  assert.equal(replacementNeedsSettledObservation(null, 'installed'), false);
  assert.equal(replacementNeedsSettledObservation('other', 'installed'), true);
});

test('a matching stopped observation remains conclusive for offline compatibility inspection', () => {
  const target = { schemaVersion: 1, instanceId: 'hive', revision: 'stopped-revision', candidateId: 'candidate', desired: 'stopped',
    requestedAt: '2020-01-01T00:00:00.000Z', configPath: '/fixture/config.json' } satisfies Host.RuntimeTarget;
  const observed = { schemaVersion: 1, instanceId: 'hive', ownerPid: 1, ownerBootId: 'owner', observedAt: '2020-01-01T00:00:00.000Z',
    targetRevision: target.revision, candidateId: target.candidateId, buildId: 'build', state: 'stopped', health: null,
    restartCount: 0, nextRestartAt: null, code: null, message: 'stopped' } satisfies Host.RuntimeObservation;
  assert.equal(stoppedTargetIsConclusive(observed, target), true);
  assert.equal(stoppedTargetIsConclusive({ ...observed, targetRevision: 'other' }, target), false);
});

test('an uninstalled idle instance remains safe to deploy after its observation ages', () => {
  const observed = { schemaVersion: 1, instanceId: 'new-agent', ownerPid: 1, ownerBootId: 'owner', observedAt: '2020-01-01T00:00:00.000Z',
    targetRevision: null, candidateId: null, buildId: null, state: 'idle', health: null,
    restartCount: 0, nextRestartAt: null, code: null, message: '' } satisfies Host.RuntimeObservation;
  assert.equal(stoppedTargetIsConclusive(observed, null), true);
  assert.equal(stoppedTargetIsConclusive(undefined, null), false);
  assert.equal(stoppedTargetIsConclusive({ ...observed, state: 'unknown' }, null), false);
  assert.equal(stoppedTargetIsConclusive({ ...observed, candidateId: 'prior-candidate' }, null), false);
  const target = { schemaVersion: 1, instanceId: 'new-agent', revision: 'running', candidateId: 'candidate', desired: 'running',
    requestedAt: '2020-01-01T00:00:00.000Z', configPath: '/fixture/config.json' } satisfies Host.RuntimeTarget;
  assert.equal(stoppedTargetIsConclusive(observed, target), false);
});

test('an unchanged ready Hive remains eligible for verified live inspection after its observation ages', () => {
  const target = { schemaVersion: 1, instanceId: 'hive', revision: 'running-revision', candidateId: 'candidate', desired: 'running',
    requestedAt: '2020-01-01T00:00:00.000Z', configPath: '/fixture/config.json' } satisfies Host.RuntimeTarget;
  const observed = { schemaVersion: 1, instanceId: 'hive', ownerPid: 1, ownerBootId: 'owner', observedAt: '2020-01-01T00:00:00.000Z',
    targetRevision: target.revision, candidateId: target.candidateId, buildId: 'build', state: 'ready', health: null,
    restartCount: 0, nextRestartAt: null, code: null, message: 'ready' } satisfies Host.RuntimeObservation;
  assert.equal(runningHiveCanBeInspected(observed, target), true);
  assert.equal(runningHiveCanBeInspected({ ...observed, candidateId: 'other' }, target), false);
});

test('executor observes an exited Docker target without becoming a second restart owner', { timeout: 15_000 }, async t => {
  const f = await hostFixture(t), instance = f.config.instances[0]!;
  const dataRoot = join(f.root, 'docker-data'), backupRoot = join(f.root, 'docker-backup'), artifactRoot = join(f.root, 'docker-artifact');
  await mkdir(join(artifactRoot, 'dist'), { recursive: true }); await mkdir(dataRoot, { recursive: true }); await mkdir(backupRoot, { recursive: true });
  const config = { ...f.config, instances: [{ ...instance, engine: 'docker' as const, settings: {
    listenHost: '127.0.0.1', listenPort: 39991, credentials: [], backup: { directory: backupRoot, intervalHours: 24, retain: 7 },
  }, docker: { imageRepository: 'ivy/fixture', ports: [] } }, f.config.instances[1]!] };
  const journal = new HostJournal(config); f.cleanups.push(async () => { journal.close(); });
  const buildId = 'sha256:' + '1'.repeat(64), candidateId = buildId;
  await atomicJson(join(artifactRoot, 'dist', 'build-info.json'), { buildId, version: '1.0.0' });
  const manifest: Host.ReleaseManifest = {
    schemaVersion: 1, componentId: 'fixture', kind: 'native', version: '1.0.0', connectsToHive: false,
    requirements: { node: '>=24.18.0 <25.0.0', hiveProtocol: null, contracts: [] }, entrypoint: { executable: 'node', args: ['dist/main.mjs'], timeoutMs: 5000 },
    readiness: { timeoutMs: 5000, command: { executable: 'node', args: ['-e', ''], timeoutMs: 5000 } }, shutdown: { timeoutMs: 1000 },
    restart: { policy: 'always', minimumDelayMs: 100, maximumDelayMs: 500 }, buildId,
  };
  const candidate: Host.Candidate = { candidateId, componentId: 'fixture', artifactRoot,
    manifestPath: join(artifactRoot, 'component.json'), createdAt: new Date().toISOString(), buildId,
    platform: { os: process.platform as 'win32' | 'linux', arch: process.arch as 'x64' | 'arm64', node: process.version },
    dockerImage: 'sha256:' + '3'.repeat(64) };
  journal.saveCandidate(candidate, manifest);
  const { target } = await materializeTarget(journal, 'first', candidateId, true);
  const current: Host.ContainerState = { containerId: 'a'.repeat(64), name: 'fixture-container', imageId: candidate.dockerImage!, hostId: config.hostId,
    instanceId: 'first', candidateId, launchId: target.revision, state: 'exited', exitCode: 1,
    startedAt: new Date().toISOString(), cgroupParent: '' };
  let starts = 0, stops = 0;
  const adapter: DockerAdapter = { inspectContainer: async () => current, stopContainer: async () => { stops++; return current; },
    verifyContainerImage: async () => undefined, startContainer: async () => { starts++; throw new Error('passive observation attempted a start'); } };
  const executor = new HostExecutor(config, f.configPath, resolve('.'), undefined, adapter); f.cleanups.push(() => executor.close());
  await (executor as unknown as { reconcileDockerOnce: (instanceId: string, target: Host.RuntimeTarget, allowStart?: boolean) => Promise<void> })
    .reconcileDockerOnce('first', target, false);
  assert.equal(starts, 0); assert.equal(stops, 0);
  assert.equal(executor.journal.observations()['first']?.state, 'failed');
  assert.equal(executor.journal.observations()['first']?.targetRevision, target.revision);

  let managedStops = 0, disposals = 0;
  const result = { exitCode: 0, signal: null, errorCode: null, stdout: '', stderr: '', truncated: false };
  const internals = executor as unknown as {
    dockerHandles: Map<string, unknown>;
    reconcileDockerOnce: (instanceId: string, target: Host.RuntimeTarget, allowStart?: boolean) => Promise<void>;
  };
  internals.dockerHandles.set('first', { target, candidate, manifest,
    config: JSON.parse(await readFile(target.configPath, 'utf8')), result: null, startedAt: Date.now(), unreadySince: 0, readinessChecked: false,
    process: { completion: new Promise(() => {}), stop: async () => { managedStops++; return result; }, dispose: () => { disposals++; } } });
  const stoppedTarget: Host.RuntimeTarget = { ...target, revision: 'replacement-stopped-target', desired: 'stopped' };
  await internals.reconcileDockerOnce('first', stoppedTarget, false);
  assert.equal(managedStops, 1); assert.equal(disposals, 1); assert.equal(stops, 0);
  assert.equal(executor.journal.observations()['first']?.state, 'stopped');
  assert.equal(executor.journal.observations()['first']?.targetRevision, stoppedTarget.revision);
});

test('temporary bootstrap installs a disabled executor and continues serving before a running handoff', { timeout: 30_000 }, async t => {
  const f = await hostFixture(t), fixture = await f.prepare('other-instance-after-disabled-executor');
  const source = join(f.root, 'checkout');
  const plan = JSON.parse(await readFile(join(source, 'services/fixture/deploy.json'), 'utf8')) as Host.BuildPlan;
  plan.componentId = 'host-executor'; plan.storage = { minReadableFormat: 1, maxReadableFormat: 1, writeFormat: 1 };
  await mkdir(join(source, 'services/host-executor'), { recursive: true });
  await atomicJson(join(source, 'services/host-executor/deploy.json'), plan);
  f.config.instances.push({ instanceId: 'executor', serviceNodeId: 'executor', componentId: 'host-executor', enabled: false, engine: 'process', settings: {} });
  await atomicJson(f.configPath, f.config);
  const candidate = await prepareCandidate(await captureSource(source, f.config), 'host-executor', f.config, resolve('.'));
  for (const id of ['executor', 'first']) { const owner = new RuntimeOwner(f.config, id, resolve('.')); owner.start(); f.cleanups.push(() => owner.close()); }
  const executor = new HostExecutor(f.config, f.configPath, resolve('.')); executor.start(); f.cleanups.push(() => executor.close());
  const disabled = f.journal.accept({ action: 'deploy', instanceId: 'executor', candidateId: candidate.candidateId, operationId: 'install-disabled-recovery-executor' });
  await until(() => f.journal.get(disabled.record.deploymentId).record.phase === 'succeeded');
  assert.equal(f.journal.installed('executor')?.enabled, false); assert.equal(f.journal.target('executor')?.desired, 'stopped');
  await assert.rejects(readFile(join(f.config.runtimeRoot, 'instances/executor/data/launches.txt')), error => (error as NodeJS.ErrnoException).code === 'ENOENT');
  const next = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: fixture.candidateId, operationId: 'temporary-executor-still-serves' });
  await until(() => f.journal.get(next.record.deploymentId).record.phase === 'succeeded');
  assert.match(await readFile(join(f.config.runtimeRoot, 'instances/first/data/launches.txt'), 'utf8'), /^other-instance-after-disabled-executor /);
});

test('same-manager owner reconstruction stays fenced while a replacement manager recovers its retired Windows job', { timeout: 30_000 }, async t => {
  const f = await hostFixture(t), candidate = await f.prepare('fence-fixture');
  let owner = new RuntimeOwner(f.config, 'first', resolve('.')); owner.start();
  f.cleanups.push(async () => { await owner.close().catch(error => { if (!(error instanceof IvyError && error.code === 'outcome_unknown')) throw error; }); });
  const executor = new HostExecutor(f.config, f.configPath, resolve('.')); executor.start(); f.cleanups.push(() => executor.close());
  const initial = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: candidate.candidateId, operationId: 'install-before-fence' });
  await until(() => f.journal.get(initial.record.deploymentId).record.phase === 'succeeded');
  await executor.close(); await owner.close();
  const dataRoot = join(f.config.runtimeRoot, 'instances/first/data'), launchFile = join(dataRoot, 'launches.txt'), launches = await readFile(launchFile, 'utf8');
  // The unknown outcome is the one durable owner state; no parallel fence file is needed.
  f.journal.recordObservation('first', { schemaVersion: 1, instanceId: 'first', ownerPid: process.pid, ownerBootId: 'unresolved-prior-native-epoch',
    observedAt: new Date().toISOString(), targetRevision: f.journal.target('first')?.revision ?? null, candidateId: candidate.candidateId,
    buildId: candidate.buildId, state: 'unknown', health: null, restartCount: 0, nextRestartAt: null, code: 'outcome_unknown', message: 'Injected unresolved owner outcome.' });
  for (let index = 0; index < 2; index++) {
    const priorBoot = f.journal.observations()['first']!.ownerBootId;
    owner = new RuntimeOwner(f.config, 'first', resolve('.')); owner.start();
    await until(() => { const value = f.journal.observations()['first']; return value?.ownerBootId !== priorBoot && value?.state === 'unknown' && value?.code === 'outcome_unknown'; });
    assert.equal(await readFile(launchFile, 'utf8'), launches);
    await owner.close();
  }
  const unresolved = f.journal.observations()['first']!;
  f.journal.recordObservation('first', { ...unresolved, ownerPid: process.pid + 100_000, ownerBootId: 'retired-service-manager' });
  const replacement = new RuntimeOwner(f.config, 'first', resolve('.')); replacement.start(); f.cleanups.push(() => replacement.close());
  await until(() => f.journal.observations()['first']?.state === 'ready');
  assert.equal((await readFile(launchFile, 'utf8')).trim().split('\n').length, launches.trim().split('\n').length + 1);
});

test('real prepared processes deploy serially, preserve unrelated launches, and disable/re-enable explicitly', { timeout: 45_000 }, async t => {
  const f = await hostFixture(t), candidate = await f.prepare('first-build');
  try {
  for (const instance of f.config.instances) { const owner = new RuntimeOwner(f.config, instance.instanceId, resolve('.')); owner.start(); f.cleanups.push(() => owner.close()); }
  const executor = new HostExecutor(f.config, f.configPath, resolve('.')); executor.start(); f.cleanups.push(() => executor.close());
  const deploy = (instanceId: string, operationId: string) => f.journal.accept({ action: 'deploy', instanceId, operationId, candidateId: candidate.candidateId });
  const first = deploy('first', 'install-first'), unrelated = deploy('unrelated', 'install-unrelated');
  await until(() => f.journal.get(first.record.deploymentId).record.phase === 'succeeded' && f.journal.get(unrelated.record.deploymentId).record.phase === 'succeeded', 25_000);
  const before = f.journal.observations()['unrelated']!.health!.bootId;
  const disabled = f.journal.accept({ action: 'disable', instanceId: 'first', operationId: 'disable-first' });
  await until(() => f.journal.get(disabled.record.deploymentId).record.phase === 'succeeded');
  assert.equal(f.journal.installed('first')?.enabled, false); assert.equal(f.journal.observations()['first']?.state, 'stopped');
  const stopped = f.journal.observations()['first']!;
  f.journal.recordObservation('first', { ...stopped, observedAt: '2020-01-01T00:00:00.000Z' });
  const enabled = f.journal.accept({ action: 'enable', instanceId: 'first', operationId: 'enable-first' });
  await until(() => f.journal.get(enabled.record.deploymentId).record.phase === 'succeeded');
  assert.equal(f.journal.observations()['unrelated']?.health?.bootId, before);
  assert.equal((await readFile(join(f.config.runtimeRoot, 'instances/first/data/launches.txt'), 'utf8')).trim().split('\n').length, 2);
  } catch (error) {
    t.diagnostic(JSON.stringify({ observations: f.journal.observations() }));
    throw error;
  }
});

test('an executor process can die after CLI handoff without killing a running unrelated instance; a new executor reconciles the durable phase', { timeout: 50_000 }, async t => {
  const f = await hostFixture(t), candidate = await f.prepare('durable-process');
  for (const instance of f.config.instances) { const owner = new RuntimeOwner(f.config, instance.instanceId, resolve('.')); owner.start(); f.cleanups.push(() => owner.close()); }
  const start = () => startProcess({ executable: 'node', args: ['dist/packages/host-runtime/src/executor.js', '--config', f.configPath], timeoutMs: 5000 }, resolve('.'), f.config.executables);
  const executor = await start(); f.cleanups.push(() => executor.stop());
  const initial = await cli(['deploy', '--config', f.configPath, '--instance', 'unrelated', '--candidate', candidate.candidateId, '--operation-id', 'cli-gone', '--json']);
  assert.equal(initial.exitCode, 0);
  await until(() => f.journal.get(initial.output.deploymentId!).record.phase === 'succeeded', 20_000);
  const boot = f.journal.observations()['unrelated']!.health!.bootId;
  const unrelatedPid = f.journal.observations()['unrelated']!.health!.pid;
  const second = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: candidate.candidateId, operationId: 'continue-after-executor-loss' });
  await until(() => f.journal.get(second.record.deploymentId).record.phase !== 'prepared');
  await executor.stop();
  assert.doesNotThrow(() => process.kill(unrelatedPid, 0));
  await until(() => f.journal.observations()['unrelated']?.state === 'ready');
  assert.equal(f.journal.observations()['unrelated']?.health?.bootId, boot, JSON.stringify(f.journal.observations()['unrelated']));
  const replacement = await start(); f.cleanups.push(() => replacement.stop());
  await until(() => terminalPhases.has(f.journal.get(second.record.deploymentId).record.phase), 25_000);
  assert.equal(f.journal.get(second.record.deploymentId).record.phase, 'succeeded');
  assert.doesNotThrow(() => process.kill(unrelatedPid, 0));
  await until(() => f.journal.observations()['unrelated']?.state === 'ready');
  assert.equal(f.journal.observations()['unrelated']?.health?.bootId, boot, JSON.stringify(f.journal.observations()['unrelated']));
  assert.equal((await readFile(join(f.config.runtimeRoot, 'instances/first/data/launches.txt'), 'utf8')).trim().split('\n').length, 1);
  assert.equal((await readFile(join(f.config.runtimeRoot, 'instances/unrelated/data/launches.txt'), 'utf8')).trim().split('\n').length, 1);
});

test('failed readiness returns to the actual previous prepared binary and records a rollback result', { timeout: 60_000 }, async t => {
  const f = await hostFixture(t), good = await f.prepare('good'), bad = await f.prepare('bad', false);
  const owner = new RuntimeOwner(f.config, 'first', resolve('.')); owner.start(); f.cleanups.push(() => owner.close());
  const executor = new HostExecutor(f.config, f.configPath, resolve('.')); executor.start(); f.cleanups.push(() => executor.close());
  const initial = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: good.candidateId, operationId: 'good' });
  await until(() => f.journal.get(initial.record.deploymentId).record.phase === 'succeeded');
  const next = structuredClone(f.config); next.instances[0]!.settings = { changed: true };
  await atomicJson(f.configPath, next);
  const caller = new HostJournal(next);
  const update = caller.accept({ action: 'deploy', instanceId: 'first', candidateId: bad.candidateId, operationId: 'bad' }); caller.close();
  await until(() => terminalPhases.has(f.journal.get(update.record.deploymentId).record.phase), 40_000);
  assert.equal(f.journal.get(update.record.deploymentId).record.phase, 'rolled_back');
  assert.equal(f.journal.installed('first')?.buildId, good.buildId); assert.equal(f.journal.observations()['first']?.health?.buildId, good.buildId);
  assert.deepEqual(JSON.parse(await readFile(join(f.config.runtimeRoot, 'instances/first/data/launch-settings.json'), 'utf8')), {});
  assert.deepEqual(JSON.parse(await readFile(f.journal.target('first')!.configPath, 'utf8')).settings, {});
});

test('a running executor applies accepted configuration only on activation and ignores later file damage until the next activation', { timeout: 45_000 }, async t => {
  const f = await hostFixture(t), candidate = await f.prepare('configuration-change');
  for (const instance of f.config.instances) { const owner = new RuntimeOwner(f.config, instance.instanceId, resolve('.')); owner.start(); f.cleanups.push(() => owner.close()); }
  const executor = new HostExecutor(f.config, f.configPath, resolve('.')); executor.start(); f.cleanups.push(() => executor.close());
  const entries = f.config.instances.map(instance => f.journal.accept({ action: 'deploy', instanceId: instance.instanceId, candidateId: candidate.candidateId, operationId: 'initial-' + instance.instanceId }));
  await until(() => entries.every(entry => f.journal.get(entry.record.deploymentId).record.phase === 'succeeded'), 25_000);
  const other = f.journal.observations()['unrelated']!.health!, previous = f.journal.target('first')!;
  const next = structuredClone(f.config); next.instances[0]!.settings = { marker: 'accepted-settings-v2' };
  await atomicJson(f.configPath, next);
  const result = await cli(['restart', '--config', f.configPath, '--instance', 'first', '--operation-id', 'new-config', '--json']);
  assert.equal(result.exitCode, 0);
  await until(() => terminalPhases.has(f.journal.get(result.output.deploymentId!).record.phase));
  const entry = f.journal.get(result.output.deploymentId!); assert.equal(entry.record.phase, 'succeeded', JSON.stringify(entry));
  assert.deepEqual(f.journal.acceptedConfiguration(entry.configurationPath), next);
  assert.deepEqual(JSON.parse(await readFile(join(f.config.runtimeRoot, 'instances/first/data/launch-settings.json'), 'utf8')), next.instances[0]!.settings);
  assert.equal(f.journal.observations()['unrelated']?.health?.bootId, other.bootId); assert.notEqual(f.journal.target('first')?.revision, previous.revision);
  const active = f.journal.target('first')!;
  const steady = f.journal.observations()['first']!;
  await delay(1200);
  assert.deepEqual(f.journal.observations()['first'], steady, 'unchanged health pulses do not create observation journal writes');
  // An already constructed local caller binds its own accepted configuration directly;
  // later master-file edits do not rewrite that accepted operation.
  const stale = f.journal.accept({ action: 'restart', instanceId: 'first', operationId: 'stale-config' });
  await until(() => terminalPhases.has(f.journal.get(stale.record.deploymentId).record.phase));
  assert.equal(f.journal.get(stale.record.deploymentId).record.phase, 'succeeded');
  assert.deepEqual(JSON.parse(await readFile(join(f.config.runtimeRoot, 'instances/first/data/launch-settings.json'), 'utf8')), {});
  assert.notDeepEqual(f.journal.target('first'), active);
  const current = f.journal.target('first')!;
  await writeFile(current.configPath, '{ damaged instance configuration');
  await delay(1200);
  assert.equal(f.journal.currentObservations()['first']?.state, 'ready');
  const disabled = await cli(['disable', '--config', f.configPath, '--instance', 'first', '--operation-id', 'disable-damaged-config', '--json']);
  assert.equal(disabled.exitCode, 0);
  await until(() => terminalPhases.has(f.journal.get(disabled.output.deploymentId!).record.phase));
  assert.equal(f.journal.get(disabled.output.deploymentId!).record.phase, 'succeeded');
  assert.equal(f.journal.observations()['first']?.state, 'stopped');
  const reused = structuredClone(next); reused.instances[0]!.serviceNodeId = 'reused-node';
  assert.throws(() => new HostJournal(reused), (error: unknown) => (error as { code: string }).code === 'target_conflict');
});

test('a missing prepared executable is rejected before the healthy previous launch is touched', { timeout: 30_000 }, async t => {
  const f = await hostFixture(t), good = await f.prepare('good'), missing = await f.prepare('missing', true, 'missing-executable');
  const owner = new RuntimeOwner(f.config, 'first', resolve('.')); owner.start(); f.cleanups.push(() => owner.close());
  const executor = new HostExecutor(f.config, f.configPath, resolve('.')); executor.start(); f.cleanups.push(() => executor.close());
  const initial = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: good.candidateId, operationId: 'good' });
  await until(() => f.journal.get(initial.record.deploymentId).record.phase === 'succeeded');
  const boot = f.journal.observations()['first']!.health!.bootId;
  const update = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: missing.candidateId, operationId: 'missing' });
  await until(() => terminalPhases.has(f.journal.get(update.record.deploymentId).record.phase));
  assert.equal(f.journal.get(update.record.deploymentId).record.phase, 'failed');
  assert.equal(f.journal.observations()['first']?.health?.bootId, boot);
});
