import { closeSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { canonical, hashJson } from '../../contracts/src/canonical.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import type { Host } from '../../contracts/src/generated.js';
import { inside } from './config.js';
import { servicePaths } from './layout.js';

const limit = 1024 * 1024;

/** One private immutable file is the complete configuration boundary for an
 * accepted operation. Its random path is the reference; there is no parallel
 * content identity or periodic integrity round. */
export function saveAcceptedConfiguration(config: Host.HostConfig): string {
  validateHost('HostConfig', config);
  const directory = join(config.runtimeRoot, 'accepted-configurations');
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const path = join(directory, randomUUID() + '.json'), file = openSync(path, 'wx', 0o600);
  try { writeFileSync(file, canonical(config, limit - 1) + '\n'); fsyncSync(file); }
  finally { closeSync(file); }
  if (process.platform !== 'win32') {
    const descriptor = openSync(directory, 'r'); try { fsyncSync(descriptor); } finally { closeSync(descriptor); }
  }
  return path;
}

/** Copy the exact prior launch configuration at operation acceptance. Rollback
 * never trusts the old target path again after that boundary. */
export function acceptedInstanceConfigurationPath(config: Host.HostConfig, revision: string): string {
  const fileRevision = /^sha256:[0-9a-f]{64}$/.test(revision) ? 'bootstrap-' + revision.slice(7) : revision;
  requireThat(/^[a-zA-Z0-9_-]{1,128}$/.test(fileRevision), 'target_conflict', 'Runtime target revision cannot name an accepted configuration.');
  return join(config.runtimeRoot, 'accepted-configurations', fileRevision + '.instance.json');
}

export function saveAcceptedInstanceConfiguration(config: Host.HostConfig, source: string, revision: string): string {
  requireThat(inside(config.runtimeRoot, source), 'target_conflict', 'Previous runtime configuration leaves host runtime data.');
  const metadata = lstatSync(source);
  requireThat(metadata.isFile() && !metadata.isSymbolicLink() && metadata.size <= limit, 'configuration_changed', 'Previous runtime configuration is not a bounded direct file.');
  let value: Host.InstanceConfig;
  try { value = JSON.parse(readFileSync(source, 'utf8')) as Host.InstanceConfig; validateHost('InstanceConfig', value); }
  catch { throw new IvyError('configuration_changed', 'Previous runtime configuration is unreadable or invalid.'); }
  const directory = join(config.runtimeRoot, 'accepted-configurations');
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const path = acceptedInstanceConfigurationPath(config, revision), file = openSync(path, 'wx', 0o600);
  try { writeFileSync(file, canonical(value, limit - 1) + '\n'); fsyncSync(file); }
  finally { closeSync(file); }
  if (process.platform !== 'win32') {
    const descriptor = openSync(directory, 'r'); try { fsyncSync(descriptor); } finally { closeSync(descriptor); }
  }
  return path;
}

export function readAcceptedInstanceConfiguration(anchor: Host.HostConfig, path: string): Host.InstanceConfig {
  requireThat(inside(join(anchor.runtimeRoot, 'accepted-configurations'), path), 'target_conflict', 'Accepted instance configuration leaves host runtime data.');
  const metadata = lstatSync(path);
  requireThat(metadata.isFile() && !metadata.isSymbolicLink() && metadata.size <= limit && dirname(path) === join(anchor.runtimeRoot, 'accepted-configurations'),
    'configuration_changed', 'Accepted instance configuration is not a bounded direct file.');
  try { const value = JSON.parse(readFileSync(path, 'utf8')) as Host.InstanceConfig; validateHost('InstanceConfig', value); return value; }
  catch (error) { if (error instanceof IvyError && error.code === 'target_conflict') throw error; throw new IvyError('configuration_changed', 'Accepted instance configuration is unreadable or invalid.'); }
}

export function readAcceptedConfiguration(anchor: Host.HostConfig, path: string): Host.HostConfig {
  requireThat(inside(join(anchor.runtimeRoot, 'accepted-configurations'), path), 'target_conflict', 'Accepted configuration leaves host runtime data.');
  const metadata = lstatSync(path);
  requireThat(metadata.isFile() && !metadata.isSymbolicLink() && metadata.size <= limit && dirname(path) === join(anchor.runtimeRoot, 'accepted-configurations'),
    'configuration_changed', 'Accepted configuration is not a bounded direct file.');
  try {
    const config = JSON.parse(readFileSync(path, 'utf8')) as Host.HostConfig;
    validateHost('HostConfig', config); sameInstallation(anchor, config); return config;
  } catch (error) {
    if (error instanceof IvyError && error.code === 'target_conflict') throw error;
    throw new IvyError('configuration_changed', 'Accepted configuration is unreadable or invalid.');
  }
}

export function sameInstallation(anchor: Host.HostConfig, next: Host.HostConfig): void {
  requireThat(['hostId', 'runtimeRoot', 'artifactRoot', 'stagingRoot', 'servicesRoot', 'configPath'].every(key => anchor[key as keyof Host.HostConfig] === next[key as keyof Host.HostConfig]) && anchor.executables['node'] === next.executables['node'],
    'target_conflict', 'Host identity, operational roots and bootstrap Node executable cannot change within an installation.');
  requireThat(hashJson(anchor.restoredFrom ?? null) === hashJson(next.restoredFrom ?? null), 'target_conflict', 'An installation cannot discard or substitute its original recovery identity.');
  for (const instance of anchor.instances) {
    const other = next.instances.find(value => value.instanceId === instance.instanceId);
    requireThat(!other || (other.componentId === instance.componentId && other.serviceNodeId === instance.serviceNodeId), 'target_conflict', 'An existing instance identity cannot be reused for another component or Service Node.');
    requireThat(!other || hashJson(servicePaths(anchor, instance)) === hashJson(servicePaths(next, other)), 'target_conflict', 'Existing service storage cannot move through a configuration edit; use an explicit isolated restore.');
  }
}

export function installationRecord(config: Host.HostConfig): string {
  return canonical({ runtimeRoot: config.runtimeRoot, artifactRoot: config.artifactRoot, stagingRoot: config.stagingRoot,
    servicesRoot: config.servicesRoot ?? null, configPath: config.configPath ?? null, node: config.executables['node'], restoredFrom: config.restoredFrom ?? null });
}
