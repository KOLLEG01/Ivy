import assert from 'node:assert/strict';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';

// Actual isolated installation maintenance, using a fully prepared candidate's unchanged runtime.
// Run in a separate MemoryMax=640M, MemorySwapMax=0 systemd service. The existing stopped Hive
// container is bounded separately at <=256M before it can start. No OS runtime owner is resumed here.
const { values } = parseArgs({ options: Object.fromEntries(['config', 'candidate', 'distribution', 'evidence', 'operation-id'].map(key => [key, { type: 'string' }])) });
for (const key of ['config', 'candidate', 'distribution', 'evidence', 'operation-id']) assert.ok(values[key], 'Missing --' + key);
assert.equal(process.platform, 'linux'); assert.equal(process.getuid(), 0);
assert.match(values['operation-id'], /^[a-z0-9-]{1,60}$/);
const distribution = resolve(values.distribution), module = path => import(pathToFileURL(join(distribution, 'dist', path)).href);
assert.ok(distribution.startsWith('/opt/ivy-next/acceptance-linux/releases/candidates/'));
const { hostConfig } = await module('packages/host-runtime/src/host-config.js');
const { HostJournal, terminalPhases } = await module('packages/host-runtime/src/journal.js');
const { verifyCandidate } = await module('packages/host-runtime/src/artifact.js');
const { atomicJson } = await module('packages/host-runtime/src/config.js');
const { inspectContainer } = await module('packages/host-runtime/src/docker.js');
const { HostExecutor } = await module('packages/host-runtime/src/executor.js');
const { RuntimeOwner } = await module('packages/host-runtime/src/runtime-owner.js');
const { inspectBootstrap, updateBootstrap } = await module('packages/host-runtime/src/bootstrap-maintenance.js');
const { configurationBootstrapPlan } = await module('packages/host-runtime/src/bootstrap.js');
const { hashJson } = await module('packages/contracts/src/canonical.js');
const configPath = resolve(values.config), config = await hostConfig(configPath), journal = new HostJournal(config);
assert.equal(config.hostId, 'Hetzner-acceptance'); assert.equal(config.runtimeRoot, '/opt/ivy-next/acceptance-linux/state');
assert.deepEqual(config.instances.map(instance => instance.instanceId).sort(), ['agent', 'executor', 'hive-acceptance', 'manager']);
const candidate = journal.candidate(values.candidate); assert.equal(candidate.artifactRoot, distribution); assert.equal(candidate.componentId, 'host-executor');
const root = resolve(values.evidence); assert.ok(root.startsWith('/opt/ivy-next/resource-maintenance-'));
await mkdir(root, { mode: 0o700 });
const reportPath = join(root, 'report.json'); await writeFile(reportPath, '{}\n', { flag: 'wx', mode: 0o600 });
const report = { schemaVersion: 1, hostId: config.hostId, candidateId: candidate.candidateId, buildId: candidate.buildId,
  startedAt: new Date().toISOString(), phase: 'preflight', steps: [], nativeMutationDispatched: false };
const secrets = config.instances.flatMap(instance => [instance.credential, ...(instance.componentId === 'hive' ? instance.settings.credentials.map(row => row.token) : [])]).filter(Boolean);
const save = async () => { const bytes = JSON.stringify(report); for (const secret of secrets) assert.equal(bytes.includes(secret), false); await atomicJson(reportPath, report); };
const exec = promisify(execFile);
const command = async (executable, args) => (await exec(executable, args, { timeout: 45000, maxBuffer: 131072, env: { ...process.env, PATH: '/usr/bin:/bin', LANG: 'C', XDG_RUNTIME_DIR: '/run/user/0' } })).stdout.trim();
const docker = args => command(config.executables.docker, ['--host', 'unix:///var/run/docker.sock', ...args]);
const ownCgroup = (await readFile('/proc/self/cgroup', 'utf8')).trim().match(/^0::(\/system\.slice\/ivy-next-resource-maintenance-[a-z0-9-]+\.service)$/)?.[1];
assert.ok(ownCgroup, 'Maintenance must have its own bounded systemd service.');
const ownCgroupRoot = '/sys/fs/cgroup' + ownCgroup, MiB = 1048576;
assert.equal(Number(await readFile(join(ownCgroupRoot, 'memory.max'), 'utf8')), 640 * MiB);
assert.equal(Number(await readFile(join(ownCgroupRoot, 'memory.swap.max'), 'utf8')), 0);
const memory = async () => ({ current: Number(await readFile(join(ownCgroupRoot, 'memory.current'), 'utf8')),
  peak: Number(await readFile(join(ownCgroupRoot, 'memory.peak'), 'utf8')),
  events: Object.fromEntries((await readFile(join(ownCgroupRoot, 'memory.events'), 'utf8')).trim().split('\n').map(line => { const [key, value] = line.split(' '); return [key, Number(value)]; })) });
const step = async (name, details = {}) => { report.steps.push({ name, at: new Date().toISOString(), ...details }); await save(); console.log(JSON.stringify({ name, ...details })); };
const fresh = value => value && Date.now() - Date.parse(value.observedAt) < 5000;
const until = async (condition, name, timeout = 90000) => {
  const end = Date.now() + timeout;
  while (!(await condition())) {
    assert.equal((await memory()).events.oom_kill, 0, 'Own maintenance cgroup had an OOM kill.');
    assert.ok(Date.now() < end, name); await delay(500);
  }
};
const owners = new Map(); let executor;
const closeRuntimes = async () => {
  await executor?.close(); executor = undefined;
  for (const [id, owner] of [...owners].reverse()) { await owner.close(); owners.delete(id); }
};
try {
  await verifyCandidate(candidate, config); assert.equal(journal.unfinished().length, 0);
  report.before = await inspectBootstrap(configPath, distribution);
  assert.ok(report.before.owners.every(owner => !owner.running));
  report.original = config.instances.map(instance => ({ instanceId: instance.instanceId, installed: journal.installed(instance.instanceId), target: journal.target(instance.instanceId) }));
  assert.ok(report.original.every(row => row.installed?.enabled && row.target?.desired === 'running'));
  const available = Number((await readFile('/proc/meminfo', 'utf8')).match(/^MemAvailable:\s+(\d+) kB/m)?.[1]) * 1024;
  assert.ok(available >= 1024 * MiB, 'Insufficient host headroom for the bounded maintenance pair.'); report.availableBeforeBytes = available;
  report.existingServiceBefore = await command('/usr/bin/systemctl', ['--user', 'show', 'rootgrid.service', '--property=MainPID,NRestarts,ActiveState']);
  const originalContainer = await inspectContainer(config, 'hive-acceptance'), target = journal.target('hive-acceptance');
  assert.ok(originalContainer && ['created', 'exited'].includes(originalContainer.state));
  for (const [containerKey, targetKey] of [['candidateId', 'candidateId'], ['launchId', 'revision'], ['configHash', 'configHash']]) assert.equal(originalContainer[containerKey], target[targetKey]);
  assert.equal(originalContainer.imageId, journal.candidate(target.candidateId).dockerImage);
  const previousLimits = JSON.parse(await docker(['container', 'inspect', '--format', '{"memory":{{.HostConfig.Memory}},"memorySwap":{{.HostConfig.MemorySwap}}}', originalContainer.containerId]));
  const limit = previousLimits.memory > 0 ? Math.min(previousLimits.memory, 256 * MiB) : 256 * MiB;
  report.containerBefore = { ...originalContainer, ...previousLimits }; await save();
  await docker(['container', 'update', '--memory', String(limit), '--memory-swap', String(limit), originalContainer.containerId]);
  const boundedLimits = JSON.parse(await docker(['container', 'inspect', '--format', '{"memory":{{.HostConfig.Memory}},"memorySwap":{{.HostConfig.MemorySwap}}}', originalContainer.containerId]));
  assert.deepEqual(boundedLimits, { memory: limit, memorySwap: limit });
  await step('existing_owned_hive_container_bounded', { containerId: originalContainer.containerId, ...boundedLimits });
  const start = async instanceId => {
    const owner = new RuntimeOwner(config, instanceId, distribution); owners.set(instanceId, owner); owner.start();
    const started = Date.now();
    await until(() => { const value = journal.observations()[instanceId]; return fresh(value) && value.state === 'ready'; }, instanceId + ' startup deadline');
    await step('temporary_runtime_owner_ready', { instanceId, elapsedMs: Date.now() - started, observation: journal.observations()[instanceId], memory: await memory() });
  };
  await start('hive-acceptance');
  assert.equal((await inspectContainer(config, 'hive-acceptance')).containerId, originalContainer.containerId);
  executor = new HostExecutor(config, configPath, distribution); executor.start();
  const disable = async instanceId => {
    const operationId = values['operation-id'] + '-disable-' + instanceId;
    const accepted = journal.accept({ action: 'disable', instanceId, operationId });
    await step('disable_accepted', { instanceId, operationId, deploymentId: accepted.record.deploymentId });
    await until(() => terminalPhases.has(journal.get(accepted.record.deploymentId).record.phase), instanceId + ' disable deadline', 180000);
    const result = journal.get(accepted.record.deploymentId).record; assert.equal(result.phase, 'succeeded', result.errorCode ?? 'disable failed');
    assert.equal(journal.installed(instanceId).enabled, false); assert.equal(journal.target(instanceId).desired, 'stopped');
    await until(() => { const value = journal.observations()[instanceId]; return fresh(value) && value.state === 'stopped'; }, instanceId + ' stopped deadline');
    await step('disabled', { instanceId, operationId, deploymentId: result.deploymentId, observation: journal.observations()[instanceId] });
    await owners.get(instanceId).close(); owners.delete(instanceId);
  };
  for (const instanceId of ['manager', 'agent']) { await start(instanceId); await disable(instanceId); }
  await disable('hive-acceptance'); await closeRuntimes();
  assert.equal(journal.unfinished().length, 0); assert.equal(hashJson(await hostConfig(configPath)), report.before.configurationHash);
  for (const row of report.original) {
    const installed = journal.installed(row.instanceId);
    assert.equal(installed.candidateId, row.installed.candidateId); assert.equal(installed.buildId, row.installed.buildId);
    if (row.instanceId !== 'executor') assert.equal(installed.enabled, false);
  }
  const previous = await inspectBootstrap(configPath, distribution);
  assert.deepEqual(previous.owners.map(owner => [owner.name, owner.definitionHash, owner.running]), report.before.owners.map(owner => [owner.name, owner.definitionHash, owner.running]));
  const next = configurationBootstrapPlan(config, configPath, candidate, { memoryHighBytes: 704 * MiB, memoryMaxBytes: 832 * MiB, tasksMax: 512 });
  const request = { schemaVersion: 1, operationId: values['operation-id'] + '-bootstrap', previous, next };
  await atomicJson(join(root, 'request.json'), request); await step('bootstrap_update_intent', { operationId: request.operationId, resources: next.linuxResources });
  report.maintenance = await updateBootstrap(configPath, request, distribution); assert.equal(report.maintenance.phase, 'succeeded');
  assert.deepEqual(await updateBootstrap(configPath, request, distribution), report.maintenance);
  report.after = await inspectBootstrap(configPath, distribution);
  assert.ok(report.after.owners.every(owner => !owner.running && owner.candidateId === candidate.candidateId));
  report.containerAfter = await inspectContainer(config, 'hive-acceptance'); assert.equal(report.containerAfter.state, 'exited');
  report.existingServiceAfter = await command('/usr/bin/systemctl', ['--user', 'show', 'rootgrid.service', '--property=MainPID,NRestarts,ActiveState']);
  assert.equal(report.existingServiceAfter, report.existingServiceBefore);
  report.phase = 'bootstrap_installed_paused';
} catch (error) { report.phase = 'failed'; report.failure = { code: error.code ?? error.name, message: error.message }; process.exitCode = 1; }
finally {
  try { await closeRuntimes(); } catch (error) { report.cleanupFailure = { code: error.code ?? error.name, message: error.message }; process.exitCode = 1; }
  report.memory = await memory(); report.completedAt = new Date().toISOString(); await save(); journal.close();
}
console.log(JSON.stringify({ reportPath, phase: report.phase, failure: report.failure, cleanupFailure: report.cleanupFailure, memory: report.memory }));
