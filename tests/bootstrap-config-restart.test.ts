import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { configurationBootstrapPlan } from '../packages/host-runtime/src/bootstrap.js';
import { reconcileBootstrapConfigurationRestarts } from '../packages/host-runtime/src/bootstrap-config-restart.js';
import { atomicJson } from '../packages/host-runtime/src/config.js';
import { digest, hashJson } from '../packages/contracts/src/canonical.js';
import type { Host } from '../packages/contracts/src/generated.js';

test('a changed bootstrap instance configuration requests one executor restart and a new boot clears it', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-bootstrap-config-restart-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const configPath = join(root, 'config.json');
  const config: Host.HostConfig = { schemaVersion: 1, hostId: 'BOOTSTRAP-CONFIG', configPath,
    runtimeRoot: join(root, 'runtime'), artifactRoot: join(root, 'artifacts'), stagingRoot: join(root, 'staging'),
    publicBaseUrl: 'https://ivy.example.test', executables: { node: process.execPath },
    instances: [{ instanceId: 'host-executor', serviceNodeId: 'host-executor', componentId: 'host-executor', enabled: true,
      engine: 'process', settings: { hostConfigPath: configPath } }] };
  const candidate: Host.Candidate = { candidateId: digest('bootstrap-config-candidate'), componentId: 'host-executor',
    buildId: digest('bootstrap-config-candidate'), artifactRoot: join(root, 'artifact'), manifestPath: join(root, 'component.json'),
    createdAt: new Date().toISOString(), platform: { os: process.platform as 'win32' | 'linux', arch: process.arch as 'x64' | 'arm64', node: process.version } };
  const plan = configurationBootstrapPlan(config, configPath, candidate);
  const owner = plan.processes[0]!;
  owner.candidateId = candidate.candidateId; owner.artifactRoot = candidate.artifactRoot;
  owner.entrypoint = { executable: 'node', args: ['dist/packages/host-runtime/src/executor.js'], timeoutMs: 30_000 };
  owner.configPath = join(config.runtimeRoot, 'bootstrap', plan.installationId, 'instances', owner.instanceId + '.json');
  await atomicJson(join(config.runtimeRoot, 'bootstrap', plan.installationId + '.json'), plan);
  const dataRoot = join(config.runtimeRoot, 'instances', owner.instanceId, 'data');
  const health: Host.Health = { schemaVersion: 1, instanceId: owner.instanceId, componentId: owner.componentId,
    buildId: candidate.buildId, pid: process.pid, bootId: 'old-boot', startedAt: new Date(Date.now() - 60_000).toISOString(),
    observedAt: new Date().toISOString(), ready: true, generation: null, details: '' };
  await atomicJson(join(dataRoot, 'health.json'), health);
  const instanceConfig: Host.InstanceConfig = { schemaVersion: 1, instanceId: owner.instanceId, serviceNodeId: owner.instanceId,
    componentId: owner.componentId, hostId: config.hostId, publicBaseUrl: config.publicBaseUrl,
    dataRoot, workRoot: join(config.runtimeRoot, 'instances', owner.instanceId, 'work'),
    logsRoot: join(config.runtimeRoot, 'instances', owner.instanceId, 'logs'), artifactRoot: candidate.artifactRoot,
    buildId: candidate.buildId, version: '1.0.0', settings: { hostConfigPath: configPath } };
  await atomicJson(owner.configPath, instanceConfig);
  assert.equal(await reconcileBootstrapConfigurationRestarts(config, owner.instanceId), true);
  await delay(10);
  await atomicJson(join(dataRoot, 'health.json'), { ...health, bootId: 'new-boot', startedAt: new Date().toISOString() });
  assert.equal(await reconcileBootstrapConfigurationRestarts(config, owner.instanceId), false);
  await delay(10);
  config.instances[0]!.credential = 'rotated-bootstrap-credential';
  assert.equal(await reconcileBootstrapConfigurationRestarts(config, owner.instanceId), true);
  const refreshed = JSON.parse(await readFile(owner.configPath, 'utf8')) as Host.InstanceConfig;
  assert.equal(refreshed.credential, 'rotated-bootstrap-credential');
  assert.equal(refreshed.dataRoot, dataRoot);
  if (process.platform !== 'win32') return;

  await atomicJson(join(dataRoot, 'health.json'), { ...health, bootId: 'settled-boot', startedAt: new Date(Date.now() + 5000).toISOString() });
  const manager: Host.Instance = { instanceId: 'service-manager', serviceNodeId: 'service-manager', componentId: 'service-manager',
    enabled: true, engine: 'process', settings: { hostConfigPath: configPath } };
  config.instances.push(manager);
  const managerBuild = digest('bootstrap-manager-candidate'), managerRoot = join(config.runtimeRoot, 'instances', manager.instanceId);
  const managerConfigPath = join(config.runtimeRoot, 'bootstrap', plan.installationId, 'instances', manager.instanceId + '.json');
  plan.processes.push({ instanceId: manager.instanceId, componentId: manager.componentId,
    name: plan.installationId + '-' + hashJson(manager.instanceId).slice(7, 19), candidateId: managerBuild,
    artifactRoot: join(root, 'manager-artifact'), entrypoint: { executable: 'node', args: ['dist/services/service-manager/src/main.js'], timeoutMs: 30_000 },
    configPath: managerConfigPath, allowWindowsBreakaway: true });
  await atomicJson(join(config.runtimeRoot, 'bootstrap', plan.installationId + '.json'), plan);
  await atomicJson(managerConfigPath, { ...instanceConfig, instanceId: manager.instanceId, serviceNodeId: manager.serviceNodeId,
    componentId: manager.componentId, dataRoot: join(managerRoot, 'data'), workRoot: join(managerRoot, 'work'), logsRoot: join(managerRoot, 'logs'),
    artifactRoot: join(root, 'manager-artifact'), buildId: managerBuild });
  await atomicJson(join(managerRoot, 'data', 'health.json'), { ...health, instanceId: manager.instanceId,
    componentId: manager.componentId, buildId: managerBuild, bootId: 'old-manager-boot' });
  let restarts = 0;
  const restart = async (_plans: Host.BootstrapPlan[], name: string) => { assert.equal(name, plan.processes[1]!.name); restarts++; };
  await reconcileBootstrapConfigurationRestarts(config, owner.instanceId, root, restart);
  await reconcileBootstrapConfigurationRestarts(config, owner.instanceId, root, restart);
  assert.equal(restarts, 1, 'an in-flight Windows task restart is not repeated every check');
});
