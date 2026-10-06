import { randomUUID } from 'node:crypto';
import { realpath, stat } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';
import { atomicJson, inside } from './config.js';
import { fileHash } from './artifact.js';
import { HostJournal } from './journal.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import { requireThat } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';
import { servicePaths, agentManagerAccountSettings, hostConfigurationPath } from './layout.js';
import { saveAcceptedInstanceConfiguration } from './accepted-configuration.js';

/** Bind release-owned native bytes to the selected candidate, never to stale mutable host configuration. */
export async function candidateBoundSettings(componentId: string, value: Host.InstanceConfig['settings'], artifactRoot: string): Promise<Host.InstanceConfig['settings']> {
  if (componentId !== 'phone-bridge') return value;
  const settings = structuredClone(value) as Record<string, unknown>, native = settings['native'];
  requireThat(native !== null && typeof native === 'object' && !Array.isArray(native), 'invalid_arguments', 'Phone settings require their release-owned native executable.');
  const executable = (native as Record<string, unknown>)['executable'];
  requireThat(typeof executable === 'string' && executable.length > 0 && !isAbsolute(executable), 'invalid_arguments', 'Phone executable must be relative to its selected artifact.');
  const root = await realpath(artifactRoot), actual = await realpath(resolve(root, executable));
  requireThat(inside(root, actual) && (await stat(actual)).isFile(), 'target_conflict', 'Phone executable leaves its selected artifact.');
  settings['native'] = { ...(native as Record<string, unknown>), executableHash: await fileHash(actual) };
  return settings as Host.InstanceConfig['settings'];
}

export async function materializeTarget(journal: HostJournal, instanceId: string, candidateId: string, enabled: boolean, basis?: Host.InstanceConfig): Promise<{ target: Host.RuntimeTarget; config: Host.InstanceConfig }> {
  const host = journal.config, instance = journal.instance(instanceId), candidate = journal.candidate(candidateId), manifest = journal.manifest(candidateId);
  const revision = randomUUID();
  const paths = servicePaths(host, instance);
  if (basis) requireThat(basis.instanceId === instanceId && basis.componentId === instance.componentId && basis.hostId === host.hostId,
    'target_conflict', 'Rollback configuration does not belong to this runtime instance.');
  const configuredSettings = basis?.settings ?? (instance.componentId === 'agent-manager' ? agentManagerAccountSettings(instance.settings)
    : ['host-executor', 'service-manager'].includes(instance.componentId) ? { ...instance.settings, hostConfigPath: instance.settings['hostConfigPath'] ?? hostConfigurationPath(host) } : instance.settings);
  const settings = await candidateBoundSettings(instance.componentId, configuredSettings, candidate.artifactRoot);
  const credential = basis ? basis.credential : instance.credential;
  const config: Host.InstanceConfig = { ...(basis ?? {}), schemaVersion: 1, instanceId, serviceNodeId: basis?.serviceNodeId ?? instance.serviceNodeId, componentId: instance.componentId,
    hostId: host.hostId, publicBaseUrl: basis?.publicBaseUrl ?? host.publicBaseUrl, dataRoot: basis?.dataRoot ?? paths.data, workRoot: basis?.workRoot ?? paths.work, logsRoot: basis?.logsRoot ?? paths.logs,
    artifactRoot: candidate.artifactRoot, buildId: candidate.buildId, version: manifest.version,
    ...(credential ? { credential } : {}), settings };
  validateHost('InstanceConfig', config);
  const configPath = join(host.runtimeRoot, 'instances', instanceId, 'configurations', revision, 'config.json');
  await atomicJson(configPath, config);
  saveAcceptedInstanceConfiguration(host, configPath, revision);
  const target: Host.RuntimeTarget = { schemaVersion: 1, instanceId, revision, candidateId, desired: enabled ? 'running' : 'stopped', configPath,
    requestedAt: new Date().toISOString() };
  validateHost('RuntimeTarget', target); return { target, config };
}
export function stoppedTarget(target: Host.RuntimeTarget): Host.RuntimeTarget {
  return { ...target, revision: randomUUID(), desired: 'stopped', requestedAt: new Date().toISOString() };
}
