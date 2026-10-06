import { stat } from 'node:fs/promises';
import { join } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { canonical } from '../../contracts/src/canonical.js';
import { atomicJson, jsonFile } from './config.js';
import { retainedBootstrapPlans } from './bootstrap.js';
import { bootstrapOs } from './bootstrap-os.js';
import { servicePaths, hostConfigurationPath } from './layout.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { runtimeEnvironment } from './process.js';
import type { Host } from '../../contracts/src/generated.js';

const exec = promisify(execFile);
const scheduler = join(globalThis.process.env['SystemRoot'] ?? 'C:\\Windows', 'System32', 'schtasks.exe');
type RestartMarker = { instanceId: string; configWrittenAt: number; priorBootId: string; requestedAt: string };

async function restartWindowsOwner(plans: Host.BootstrapPlan[], name: string, distribution: string): Promise<void> {
  const os = bootstrapOs(distribution);
  const owners = await os.inspect(plans);
  const owner = owners.find(value => value.name === name);
  requireThat(owner?.enabled, 'target_conflict', 'The selected Windows bootstrap task is not enabled.');
  const run = async (action: '/End' | '/Run') => {
    try { await exec(scheduler, [action, '/TN', name], { timeout: 30_000, maxBuffer: 65_536, windowsHide: true, env: runtimeEnvironment() }); }
    catch { throw new IvyError('bootstrap_os_unavailable', 'The selected Windows bootstrap task could not be restarted.', 'unknown'); }
  };
  if (owner.running) await run('/End');
  const deadline = Date.now() + 30_000;
  while ((await os.inspect(plans)).find(value => value.name === name)?.running) {
    requireThat(Date.now() < deadline, 'outcome_unknown', 'Windows bootstrap task did not stop before restart.');
    await new Promise(resolve => setTimeout(resolve, 200));
  }
  await run('/Run');
}

/** A changed bootstrap instance file is the durable restart request. A new process
 * has a later health.startedAt, so crashes between publication and signaling do
 * not lose the request or cause repeated restarts after recovery. */
export async function reconcileBootstrapConfigurationRestarts(config: Host.HostConfig, ownInstanceId?: string,
  distribution?: string, restartManager = restartWindowsOwner): Promise<boolean> {
  const plans = await retainedBootstrapPlans(config), plan = plans[0];
  if (!plan) return false;
  let restartOwn = false;
  for (const owner of plan.processes) {
    if (!owner.configPath || !['host-executor', 'service-manager'].includes(owner.componentId)) continue;
    const instance = config.instances.find(value => value.instanceId === owner.instanceId);
    requireThat(instance && instance.componentId === owner.componentId, 'target_conflict', 'Selected bootstrap owner is absent from the host configuration.');
    requireThat(owner.candidateId && owner.artifactRoot, 'target_conflict', 'Selected bootstrap owner has no immutable release.');
    const current = await jsonFile<Host.InstanceConfig>(owner.configPath); validateHost('InstanceConfig', current);
    const paths = servicePaths(config, instance);
    requireThat(current.instanceId === instance.instanceId && current.componentId === instance.componentId && current.hostId === config.hostId &&
      current.buildId === owner.candidateId && current.artifactRoot === owner.artifactRoot && current.dataRoot === paths.data &&
      current.workRoot === paths.work && current.logsRoot === paths.logs,
    'target_conflict', 'Bootstrap instance configuration differs from the selected installation.');
    const desired: Host.InstanceConfig = { ...current, serviceNodeId: instance.serviceNodeId, publicBaseUrl: config.publicBaseUrl,
      settings: { ...instance.settings, hostConfigPath: instance.settings['hostConfigPath'] ?? hostConfigurationPath(config) } };
    if (instance.credential) desired.credential = instance.credential;
    else delete desired.credential;
    validateHost('InstanceConfig', desired);
    if (canonical(current) !== canonical(desired)) await atomicJson(owner.configPath, desired);
    const healthPath = join(paths.data, 'health.json');
    const health = await jsonFile<Host.Health>(healthPath).catch(error => {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
      throw error;
    });
    if (!health) continue;
    validateHost('Health', health);
    requireThat(health.instanceId === instance.instanceId && health.componentId === instance.componentId,
      'target_conflict', 'Bootstrap health belongs to another instance.');
    const configWrittenAt = (await stat(owner.configPath)).mtimeMs;
    if (Date.parse(health.startedAt) >= configWrittenAt) continue;
    if (instance.componentId === 'host-executor' && instance.instanceId === ownInstanceId) {
      restartOwn = configWrittenAt > Date.now() - globalThis.process.uptime() * 1000;
    } else if (globalThis.process.platform === 'win32' && instance.componentId === 'service-manager' && distribution &&
      Date.now() - Date.parse(health.observedAt) < 15_000) {
      const markerPath = join(config.runtimeRoot, 'bootstrap', 'restart-' + instance.instanceId + '.json');
      const marker = await jsonFile<RestartMarker>(markerPath).catch(error => {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
        throw error;
      });
      if (marker?.instanceId === instance.instanceId && marker.configWrittenAt === configWrittenAt &&
        marker.priorBootId === health.bootId && Date.now() - Date.parse(marker.requestedAt) < 60_000) continue;
      await atomicJson(markerPath, { instanceId: instance.instanceId, configWrittenAt, priorBootId: health.bootId,
        requestedAt: new Date().toISOString() } satisfies RestartMarker);
      await restartManager(plans, owner.name, distribution);
    }
  }
  return restartOwn;
}
