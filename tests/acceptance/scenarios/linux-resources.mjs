// Real systemd/Docker fixture. It uses a pre-existing immutable local Node image, temporary owned
// unit/container identities and synthetic launch data. It does not start any installed Ivy service.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify, parseArgs } from 'node:util';
import { randomUUID } from 'node:crypto';
import { mkdir, readFile, writeFile, unlink } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { linuxUnit } from '../../../dist/packages/host-runtime/src/bootstrap.js';
import { installationIdentity, linuxResourceScope, linuxSlice, ensureLinuxSlice, inspectLinuxSlice, verifyLinuxResourceMembership } from '../../../dist/packages/host-runtime/src/linux-resources.js';
import { containerName } from '../../../dist/packages/host-runtime/src/docker.js';
import { hashJson, digest } from '../../../dist/packages/contracts/src/canonical.js';

const exec = promisify(execFile), args = parseArgs({ options: { image: { type: 'string' }, evidence: { type: 'string' } }, strict: true }).values;
assert.equal(process.platform, 'linux'); assert.equal(process.getuid(), 0);
assert.match(args.image ?? '', /^sha256:[a-f0-9]{64}$/);
const root = resolve(args.evidence ?? ''); assert.ok(root.startsWith('/opt/ivy-next/resource-acceptance-') && root.split('/').length === 4);
const distribution = resolve('.'), id = randomUUID(), hostId = 'resource-fixture-' + id;
const call = (file, argv, timeout = 30_000) => exec(file, argv, { timeout, maxBuffer: 65536 });
const docker = argv => call('/usr/bin/docker', ['--host', 'unix:///var/run/docker.sock', ...argv]);
const systemctl = argv => call('/usr/bin/systemctl', argv);
const until = async (check, timeout = 30_000) => { const end = Date.now() + timeout; while (!(await check())) { assert.ok(Date.now() < end, 'fixture deadline'); await delay(100); } };
await mkdir(root, { mode: 0o700 });
const report = { schemaVersion: 1, kind: 'actual-systemd-docker-fixture', id, startedAt: new Date().toISOString(), ok: false,
  implementation: Object.fromEntries(await Promise.all(['linux-resources.js', 'docker.js', 'bootstrap.js'].map(async name => [name, digest(await readFile(join(distribution, 'dist/packages/host-runtime/src', name)))]))) };
const instanceId = 'hive', settings = { listenHost: '127.0.0.1', listenPort: 39099, credentials: [], backup: { directory: join(root, 'backup'), intervalHours: 24, retain: 1 } };
const instance = { instanceId, serviceNodeId: 'fixture-hive', componentId: 'hive', enabled: false, engine: 'docker', settings,
  docker: { imageRepository: 'ivy-next-resource-fixture-' + id, ports: [] } };
const host = { schemaVersion: 1, hostId, runtimeRoot: join(root, 'state'), artifactRoot: join(root, 'artifacts'), stagingRoot: join(root, 'staging'),
  publicBaseUrl: 'http://127.0.0.1:39099/ivy', executables: { node: process.execPath, docker: '/usr/bin/docker' }, instances: [instance] };
const installationId = installationIdentity(host), scope = linuxResourceScope(installationId, { memoryHighBytes: 192 * 1024 * 1024, memoryMaxBytes: 256 * 1024 * 1024, tasksMax: 128 });
const name = installationId + '-' + hashJson(instanceId).slice(7, 19), unit = name + '.service', unitPath = '/etc/systemd/system/' + unit, scopePath = '/etc/systemd/system/' + scope.slice;
const artifactRoot = join(root, 'artifact');
const candidate = { candidateId: digest('fixture-candidate-' + id), buildId: digest('fixture-build-' + id), artifactRoot, dockerImage: args.image };
const configPath = join(root, 'instance.json'), config = { schemaVersion: 1, hostId, instanceId, serviceNodeId: instance.serviceNodeId, componentId: 'hive',
  publicBaseUrl: host.publicBaseUrl, dataRoot: join(root, 'data'), artifactRoot, buildId: candidate.buildId, version: 'fixture', settings };
const target = { schemaVersion: 1, instanceId, revision: id, candidateId: candidate.candidateId, desired: 'running', configPath, configHash: hashJson(config), requestedAt: new Date().toISOString() };
const manifest = { entrypoint: { executable: 'node', args: ['-e', 'console.log("isolated-cgroup-fixture");setInterval(()=>{},1000)'] } };
const plan = { schemaVersion: 1, hostId, os: 'linux', installationId, candidateId: candidate.candidateId, artifactRoot, configPath: join(root, 'host.json'), configHash: hashJson(host),
  runtimeRoot: host.runtimeRoot, nodeExecutable: process.execPath, processes: [{ instanceId, componentId: 'hive', name }], linuxResources: scope };
const definition = linuxUnit(plan, plan.processes[0]), container = containerName(hostId, instanceId), readyPath = join(root, 'ready.json');
let createdUnit = false, createdSlice = false, oldId = null;
try {
  assert.equal((await docker(['image', 'inspect', '--format', '{{.Id}}', args.image])).stdout.trim(), args.image);
  report.dockerVersion = (await docker(['version', '--format', '{{.Client.Version}} {{.Server.Version}}'])).stdout.trim();
  assert.equal((await docker(['info', '--format', '{{.CgroupDriver}} {{.CgroupVersion}}'])).stdout.trim(), 'systemd 2');
  for (const path of [unitPath, scopePath]) await assert.rejects(readFile(path), { code: 'ENOENT' });
  await mkdir(host.runtimeRoot); await mkdir(join(artifactRoot, 'dist/packages/host-runtime/src'), { recursive: true });
  for (const [path, value] of [[plan.configPath, host], [configPath, config], [join(root, 'fixture.json'), { host, instance, target, config, candidate, manifest, scope }]])
    await writeFile(path, JSON.stringify(value), { flag: 'wx', mode: 0o600 });
  const launcher = `import {readFile,writeFile} from 'node:fs/promises';
import {setTimeout as delay} from 'node:timers/promises';
import {startContainer,inspectContainer} from ${JSON.stringify(pathToFileURL(join(distribution, 'dist/packages/host-runtime/src/docker.js')).href)};
import {verifyLinuxResourceMembership} from ${JSON.stringify(pathToFileURL(join(distribution, 'dist/packages/host-runtime/src/linux-resources.js')).href)};
const data=JSON.parse(await readFile(${JSON.stringify(join(root, 'fixture.json'))},'utf8'));
const passed=JSON.parse(process.argv[process.argv.indexOf('--linux-resources')+1]);
if(JSON.stringify(passed)!==JSON.stringify(data.scope))throw new Error('systemd altered resource argv');
let stopped=false,managed;process.on('SIGTERM',()=>{stopped=true;});process.on('SIGINT',()=>{stopped=true;});
try{await verifyLinuxResourceMembership(data.host,data.instance.instanceId,passed);
managed=await startContainer(data.host,data.instance,data.target,data.config,data.candidate,data.manifest,false,passed);
await managed.verify();await writeFile(${JSON.stringify(readyPath)},JSON.stringify({pid:process.pid,container:await inspectContainer(data.host,data.instance.instanceId)}));
while(!stopped)await delay(100);
}finally{if(managed){await managed.stop(1000);managed.dispose();}}
`;
  await writeFile(join(artifactRoot, 'dist/packages/host-runtime/src/runtime-owner.js'), launcher, { flag: 'wx', mode: 0o600 });
  await ensureLinuxSlice(scope); createdSlice = true;
  await writeFile(unitPath, definition, { flag: 'wx', mode: 0o644 }); createdUnit = true;
  await systemctl(['daemon-reload']); await inspectLinuxSlice(scope);
  // Same owned launch metadata, deliberately wrong stopped cgroup. The runtime must replace it.
  const labels = { 'dev.ivy.host': hostId, 'dev.ivy.instance': instanceId, 'dev.ivy.candidate': candidate.candidateId, 'dev.ivy.launch': id };
  oldId = (await docker(['container', 'create', '--name', container, '--pull', 'never', '--restart', 'no', '--cgroup-parent', 'system.slice',
    ...Object.entries(labels).flatMap(([key, value]) => ['--label', key + '=' + value]), '--entrypoint', '/usr/local/bin/node', args.image, '-e', 'process.exit(0)'])).stdout.trim();
  assert.match(oldId, /^[a-f0-9]{64}$/);
  await systemctl(['start', unit]);
  await until(async () => { try { await readFile(readyPath); return true; } catch (error) { if (error.code === 'ENOENT') return false; throw error; } });
  const ready = JSON.parse(await readFile(readyPath, 'utf8')); assert.notEqual(ready.container.containerId, oldId); assert.equal(ready.container.cgroupParent, scope.slice);
  const raw = JSON.parse((await docker(['container', 'inspect', '--format', '{{json .}}', ready.container.containerId])).stdout);
  assert.equal(raw.State.Running, true); assert.equal(raw.HostConfig.CgroupParent, scope.slice);
  const membership = await readFile('/proc/' + ready.pid + '/cgroup', 'utf8'), dockerMembership = await readFile('/proc/' + raw.State.Pid + '/cgroup', 'utf8');
  assert.equal(membership.trim(), '0::/' + scope.slice + '/' + unit); assert.ok(dockerMembership.trim().startsWith('0::/' + scope.slice + '/docker-'));
  await assert.rejects(verifyLinuxResourceMembership(host, instanceId, scope), error => error.code === 'resource_scope_mismatch');
  const kernel = Object.fromEntries(await Promise.all(['memory.high', 'memory.max', 'memory.swap.max', 'pids.max', 'memory.current', 'memory.peak', 'memory.events'].map(async name =>
    [name, (await readFile('/sys/fs/cgroup/' + scope.slice + '/' + name, 'utf8')).trim()])));
  assert.equal(kernel['memory.max'], String(scope.budget.memoryMaxBytes));
  Object.assign(report, { scope, unit, unitDefinitionHash: digest(definition), sliceDefinitionHash: digest(linuxSlice(scope)), originalStoppedContainer: oldId,
    activeContainer: ready.container, processMembership: membership.trim(), containerMembership: dockerMembership.trim(), kernel });
  await systemctl(['stop', unit]);
  assert.equal((await systemctl(['show', unit, '--property=ActiveState', '--value'])).stdout.trim(), 'inactive');
  const stopped = JSON.parse((await docker(['container', 'inspect', '--format', '{{json .}}', ready.container.containerId])).stdout);
  assert.equal(stopped.State.Running, false); assert.ok(['exited', 'dead'].includes(stopped.State.Status));
  report.ok = true;
} catch (error) {
  report.error = { code: error.code, message: error.message };
} finally {
  try {
    if (createdUnit) { assert.equal(await readFile(unitPath, 'utf8'), definition); await systemctl(['stop', unit]); }
    const ids = (await docker(['container', 'ls', '--all', '--no-trunc', '--filter', 'name=^/' + container + '$', '--format', '{{.ID}}'])).stdout.trim();
    if (ids) {
      assert.match(ids, /^[a-f0-9]{64}$/); const raw = JSON.parse((await docker(['container', 'inspect', '--format', '{{json .}}', ids])).stdout);
      assert.equal(raw.Config.Labels['dev.ivy.host'], hostId); assert.equal(raw.Name, '/' + container); assert.equal(raw.State.Running, false);
      await docker(['container', 'rm', ids]);
    }
    const tag = instance.docker.imageRepository + ':' + candidate.buildId.slice(7);
    const tagId = await docker(['image', 'inspect', '--format', '{{.Id}}', tag]).catch(() => null);
    if (tagId) { assert.equal(tagId.stdout.trim(), args.image); await docker(['image', 'rm', tag]); }
    if (createdUnit) await unlink(unitPath);
    if (createdSlice) { assert.equal(await readFile(scopePath, 'utf8'), linuxSlice(scope)); await systemctl(['stop', scope.slice]); await unlink(scopePath); }
    if (createdUnit || createdSlice) await systemctl(['daemon-reload']);
    report.cleanup = 'owned stopped fixture definitions/container/tag removed; evidence retained';
  } catch (error) { report.ok = false; report.cleanupError = { code: error.code, message: error.message }; }
  report.completedAt = new Date().toISOString(); await writeFile(join(root, 'report.json'), JSON.stringify(report, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
}
console.log(JSON.stringify(report)); if (!report.ok) process.exitCode = 1;
