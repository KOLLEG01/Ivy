import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, writeFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createDockerAdapter, dockerRestartPolicy } from '../packages/host-runtime/src/docker.js';
import { digest, hashJson } from '../packages/contracts/src/canonical.js';
import type { Host } from '../packages/contracts/src/generated.js';

test('the direct Docker owner creates, adopts, verifies and stops one exact container identity', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-docker-owner-test-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const artifactRoot = join(root, 'artifact'), dataRoot = join(root, 'data'), backupRoot = join(root, 'backup'), configPath = join(root, 'config.json');
  await Promise.all([mkdir(artifactRoot), mkdir(dataRoot), mkdir(backupRoot)]);
  const hostId = 'docker-owner-host', instanceId = 'hive', buildId = digest('docker-build'), candidateId = buildId;
  const host = { hostId, artifactRoot: join(root, 'artifacts'), runtimeRoot: join(root, 'runtime'), instances: [] } as unknown as Host.HostConfig;
  const instance: Host.Instance = { instanceId, serviceNodeId: instanceId, componentId: 'hive', enabled: true, engine: 'docker', settings: {}, docker: { imageRepository: 'ivy/hive', ports: [] } };
  const config: Host.InstanceConfig = { schemaVersion: 1, hostId, instanceId, serviceNodeId: instanceId, componentId: 'hive', publicBaseUrl: 'http://127.0.0.1:39999/ivy',
    dataRoot, artifactRoot, buildId, version: '0.1.0', settings: { listenHost: '0.0.0.0', listenPort: 39999, credentials: [], backup: { directory: backupRoot, intervalHours: 24, retain: 7 } } };
  await writeFile(configPath, JSON.stringify(config));
  const target: Host.RuntimeTarget = { schemaVersion: 1, instanceId, revision: digest('launch'), candidateId, desired: 'running', configPath,
    requestedAt: new Date().toISOString() };
  const candidate: Host.Candidate = { candidateId, componentId: 'hive', artifactRoot, manifestPath: join(artifactRoot, 'component.json'),
    createdAt: new Date().toISOString(), platform: { os: process.platform as 'win32' | 'linux', arch: process.arch as 'x64' | 'arm64', node: process.version },
    buildId, dockerImage: digest('image') };
  const manifest: Host.ReleaseManifest = { schemaVersion: 1, componentId: 'hive', kind: 'hive', version: '0.1.0', buildId, connectsToHive: false,
    requirements: { node: '>=24.18.0 <25.0.0', hiveProtocol: null, contracts: [] }, entrypoint: { executable: 'node', args: ['dist/main.js'], timeoutMs: 5000 },
    readiness: { timeoutMs: 5000, command: { executable: 'node', args: ['-e', ''], timeoutMs: 5000 } }, shutdown: { timeoutMs: 1000 }, restart: { policy: 'always', minimumDelayMs: 100, maximumDelayMs: 500 }, storage: { minReadableFormat: 1, writeFormat: 1, maxReadableFormat: 1 } };
  let exists = false;
  let state: Host.ContainerState['state'] = 'created';
  const containerId = 'a'.repeat(64), commands: string[][] = [];
  const adapter = createDockerAdapter(async (_host, args) => {
    commands.push(args);
    if (args[0] === 'container' && args[1] === 'ls') return { exitCode: 0, signal: null, errorCode: null, stdout: exists ? containerId + '\n' : '', stderr: '', truncated: false };
    if (args[0] === 'container' && args[1] === 'inspect') return { exitCode: 0, signal: null, errorCode: null, stdout: JSON.stringify({ Id: containerId, Name: '/' + 'ivy-next-' + hashJson({ hostId, instanceId }).slice(7, 31), Image: candidate.dockerImage, Config: { Labels: {
      'dev.ivy.host': hostId, 'dev.ivy.instance': instanceId, 'dev.ivy.candidate': candidateId, 'dev.ivy.launch': target.revision,
    } }, HostConfig: { CgroupParent: '' }, State: { Status: state, ExitCode: 0, StartedAt: state === 'created' ? '0001-01-01T00:00:00Z' : new Date().toISOString() } }), stderr: '', truncated: false };
    if (args[0] === 'image' && args[1] === 'tag') return { exitCode: 0, signal: null, errorCode: null, stdout: '', stderr: '', truncated: false };
    if (args[0] === 'container' && args[1] === 'create') { exists = true; state = 'created'; return { exitCode: 0, signal: null, errorCode: null, stdout: '', stderr: '', truncated: false }; }
    if (args[0] === 'container' && args[1] === 'start') { state = 'running'; return { exitCode: 0, signal: null, errorCode: null, stdout: '', stderr: '', truncated: false }; }
    if (args[0] === 'container' && args[1] === 'stop') { state = 'exited'; return { exitCode: 0, signal: null, errorCode: null, stdout: '', stderr: '', truncated: false }; }
    if (args[0] === 'container' && args[1] === 'logs') return { exitCode: 0, signal: null, errorCode: null, stdout: 'bounded logs', stderr: '', truncated: false };
    throw new Error('Unexpected fake Docker command: ' + args.join(' '));
  });

  assert.equal(dockerRestartPolicy(manifest), 'always');
  const managed = await adapter.startContainer(host, instance, target, config, candidate, manifest, true);
  await managed.verify?.();
  const stop = await managed.stop(1000);
  assert.equal(stop.exitCode, 0);
  await managed.completion;
  const create = commands.findIndex(args => args[0] === 'container' && args[1] === 'create');
  assert.ok(create >= 0);
  assert.equal(commands[create + 1]?.includes('--restart'), false);
  assert.equal(commands[create]![commands[create]!.indexOf('--restart') + 1], 'always');
  assert.equal(commands.filter(args => args[0] === 'container' && args[1] === 'restart').length, 0);
});
