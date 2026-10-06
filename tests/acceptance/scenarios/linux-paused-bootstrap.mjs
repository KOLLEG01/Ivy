import assert from 'node:assert/strict';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs, promisify } from 'node:util';
import { execFile } from 'node:child_process';

// Five-owner acceptance installation, already durably paused. Only normal bootstrap maintenance;
// this program never starts a runtime owner/domain instance or changes the aggregate budget.
const required = ['candidate-file', 'evidence', 'operation-id', 'expected-build', 'previous-candidate', 'previous-memory-high-mib'];
const { values } = parseArgs({ options: Object.fromEntries([...required, 'memory-high-mib'].map(key => [key, { type: 'string' }])) });
for (const key of required) assert.ok(values[key], 'Missing --' + key);
for (const key of ['expected-build', 'previous-candidate']) assert.match(values[key], /^sha256:[0-9a-f]{64}$/);
const memoryHighMiB = value => {
  assert.match(value, /^(?:[1-9][0-9]*)$/);
  const amount = Number(value);
  assert.ok(Number.isSafeInteger(amount) && amount >= 128 && amount <= 832);
  return amount;
};
const previousMemoryHighMiB = memoryHighMiB(values['previous-memory-high-mib']);
const nextMemoryHighMiB = values['memory-high-mib'] === undefined ? previousMemoryHighMiB : memoryHighMiB(values['memory-high-mib']);
assert.equal(process.platform, 'linux'); assert.equal(process.getuid(), 0);
assert.match(values['operation-id'], /^[a-z0-9-]{1,60}$/);
const prepared = JSON.parse(await readFile(values['candidate-file'], 'utf8'));
assert.equal(prepared.ok, true); assert.equal(prepared.code, 'candidate_prepared');
const candidate = prepared.data; assert.equal(candidate.componentId, 'host-executor');
assert.equal(candidate.buildId, values['expected-build']);
const distribution = resolve(candidate.artifactRoot);
assert.ok(distribution.startsWith('/opt/ivy-next/acceptance-linux/releases/candidates/'));
const module = name => import(pathToFileURL(join(distribution, 'dist', name)).href);
const { hostConfig } = await module('packages/host-runtime/src/host-config.js');
const { HostJournal } = await module('packages/host-runtime/src/journal.js');
const { verifyCandidate } = await module('packages/host-runtime/src/artifact.js');
const { inspectBootstrap, updateBootstrap } = await module('packages/host-runtime/src/bootstrap-maintenance.js');
const { configurationBootstrapPlan } = await module('packages/host-runtime/src/bootstrap.js');
const { hashJson } = await module('packages/contracts/src/canonical.js');
const configPath = '/opt/ivy-next/acceptance-linux/config.json', config = await hostConfig(configPath), journal = new HostJournal(config);
const previousCandidate = values['previous-candidate'];
assert.equal(config.hostId, 'Hetzner-acceptance');
assert.equal(hashJson(config), 'sha256:2ffdbd3172d662b751bca56fb089e759684153c2f833ff26353a00176a4e3d3f');
assert.deepEqual(config.instances.map(x => x.instanceId).sort(), ['agent', 'executor', 'hive-acceptance', 'manager', 'task-board']);
const cgroup = (await readFile('/proc/self/cgroup', 'utf8')).trim().match(/^0::(\/system\.slice\/ivy-next-paused-bootstrap-[a-z0-9-]+\.service)$/)?.[1];
assert.ok(cgroup, 'Use the separate bounded maintenance unit.');
const cgroupRoot = '/sys/fs/cgroup' + cgroup, MiB = 1048576;
assert.equal(Number(await readFile(join(cgroupRoot, 'memory.max'), 'utf8')), 640 * MiB);
assert.equal(Number(await readFile(join(cgroupRoot, 'memory.swap.max'), 'utf8')), 0);
const memory = async () => ({ current: Number(await readFile(join(cgroupRoot, 'memory.current'), 'utf8')),
  peak: Number(await readFile(join(cgroupRoot, 'memory.peak'), 'utf8')),
  events: Object.fromEntries((await readFile(join(cgroupRoot, 'memory.events'), 'utf8')).trim().split('\n').map(line => { const [key, value] = line.split(' '); return [key, Number(value)]; })) });
const root = resolve(values.evidence); assert.ok(root.startsWith('/opt/ivy-next/paused-bootstrap-'));
await mkdir(root, { mode: 0o700 });
const reportPath = join(root, 'report.json'), report = { schemaVersion: 1, startedAt: new Date().toISOString(), hostId: config.hostId,
  candidateId: candidate.candidateId, buildId: candidate.buildId, operationId: values['operation-id'], phase: 'preflight', steps: [] };
const secrets = config.instances.flatMap(x => [x.credential, ...(x.componentId === 'hive' ? x.settings.credentials.map(y => y.token) : [])]).filter(Boolean);
const save = async () => { const text = JSON.stringify(report, null, 2); for (const secret of secrets) assert.ok(!text.includes(secret)); await writeFile(reportPath, text + '\n', { mode: 0o600 }); };
const exec = promisify(execFile), command = async args => (await exec('/usr/bin/systemctl', args, {
  timeout: 45000, maxBuffer: 131072, env: { ...process.env, PATH: '/usr/bin:/bin', LANG: 'C', XDG_RUNTIME_DIR: '/run/user/0' },
})).stdout.trim();
try {
  await save(); await verifyCandidate(candidate, config); assert.deepEqual(journal.candidate(candidate.candidateId), candidate);
  assert.equal(journal.unfinished().length, 0);
  report.original = config.instances.map(x => ({ instanceId: x.instanceId, installed: journal.installed(x.instanceId), target: journal.target(x.instanceId) }));
  assert.ok(config.instances.filter(x => x.componentId !== 'host-executor').every(x => !(journal.installed(x.instanceId)?.enabled ?? x.enabled)));
  const selected = JSON.parse(await readFile(join(config.runtimeRoot, 'bootstrap/ivy-next-40ae1976878d.json'), 'utf8'));
  assert.equal(selected.candidateId, previousCandidate);
  // The original 704 MiB experiment has already been superseded by the measured 800 MiB
  // installation. Require the caller's explicit observed prior budget on each upgrade;
  // never silently adopt a different installed budget or reset it to the old experiment.
  assert.deepEqual(selected.linuxResources.budget, { memoryHighBytes: previousMemoryHighMiB * MiB, memoryMaxBytes: 832 * MiB, tasksMax: 512 });
  report.existingServiceBefore = await command(['--user', 'show', 'rootgrid.service', '--property=MainPID,NRestarts,ActiveState']);
  const previous = await inspectBootstrap(configPath, distribution); report.before = previous;
  assert.equal(previous.owners.length, 5); assert.ok(previous.owners.every(x => !x.running && x.candidateId === previousCandidate));
  const memoryHighBytes = nextMemoryHighMiB * MiB;
  const budget = { ...selected.linuxResources.budget, memoryHighBytes };
  const next = configurationBootstrapPlan(config, configPath, candidate, budget);
  assert.deepEqual(next.linuxResources.budget, budget);
  // The public plan derives a distinct immutable slice identity from a different budget.
  if (memoryHighBytes === selected.linuxResources.budget.memoryHighBytes) assert.deepEqual(next.linuxResources, selected.linuxResources);
  else {
    assert.match(next.linuxResources.slice, /^ivynext40ae1976878d[0-9a-f]{12}\.slice$/);
    assert.notEqual(next.linuxResources.slice, selected.linuxResources.slice);
  }
  assert.notEqual(hashJson(next), hashJson(selected), 'Acceptance must change the checked candidate or the explicit soft limit.');
  const request = { schemaVersion: 1, operationId: values['operation-id'], previous, next };
  await writeFile(join(root, 'request.json'), JSON.stringify(request, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  report.phase = 'updating'; await save();
  report.maintenance = await updateBootstrap(configPath, request, distribution); assert.equal(report.maintenance.phase, 'succeeded');
  assert.deepEqual(await updateBootstrap(configPath, request, distribution), report.maintenance);
  report.after = await inspectBootstrap(configPath, distribution);
  assert.ok(report.after.owners.every(x => !x.running && x.candidateId === candidate.candidateId));
  assert.deepEqual(config.instances.map(x => ({ instanceId: x.instanceId, installed: journal.installed(x.instanceId), target: journal.target(x.instanceId) })), report.original);
  assert.equal(hashJson(await hostConfig(configPath)), hashJson(config)); assert.equal(journal.unfinished().length, 0);
  report.existingServiceAfter = await command(['--user', 'show', 'rootgrid.service', '--property=MainPID,NRestarts,ActiveState']);
  assert.equal(report.existingServiceAfter, report.existingServiceBefore);
  report.phase = 'bootstrap_installed_paused';
} catch (error) { report.phase = 'failed'; report.failure = { code: error.code ?? error.name, message: error.message }; process.exitCode = 1; }
finally { report.memory = await memory(); report.completedAt = new Date().toISOString(); await save(); journal.close(); }
console.log(JSON.stringify({ reportPath, phase: report.phase, failure: report.failure ?? null, memory: report.memory }));
