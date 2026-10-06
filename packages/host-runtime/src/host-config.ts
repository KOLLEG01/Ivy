import { mkdir, realpath, access } from 'node:fs/promises';
import { constants } from 'node:fs';
import { isAbsolute, resolve, dirname, basename, join } from 'node:path';
import { jsonFile, inside } from './config.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import { requireThat } from '../../contracts/src/errors.js';
import { baseUrl } from '../../sdk/src/client.js';
import type { Host } from '../../contracts/src/generated.js';
import { sharedNativeHomes } from './native-backup-paths.js';
import { resolveHostConfiguration, servicePaths, resolvedFuturePath } from './layout.js';

export async function hostConfig(path: string): Promise<Host.HostConfig> {
  return checkedHostConfig(await jsonFile<Host.HostConfigInput>(path), resolve(path));
}

/** Validate a remotely supplied complete config against the local installation
 * before replacing the durable JSON file. */
export async function checkedHostConfig(input: Host.HostConfigInput, configurationPath: string): Promise<Host.HostConfig> {
  validateHost('HostConfigInput', input);
  const config = resolveHostConfiguration(input, resolve(configurationPath)); validateHost('HostConfig', config); baseUrl(config.publicBaseUrl);
  requireThat(!config.configPath || await resolvedFuturePath(config.configPath) === await resolvedFuturePath(configurationPath), 'target_conflict', 'Central configPath must identify this actual host configuration file.');
  requireThat(new Set(config.instances.map(instance => instance.instanceId)).size === config.instances.length && new Set(config.instances.map(instance => instance.serviceNodeId)).size === config.instances.length, 'invalid_arguments', 'Host instance and Service Node identities must be unique.');
  const roots = [config.runtimeRoot, config.artifactRoot, config.stagingRoot];
  requireThat(roots.every(isAbsolute) && roots.every((root, index) => roots.every((other, otherIndex) => index === otherIndex || !inside(root, other))), 'invalid_arguments', 'Host runtime, artifact and staging roots must be disjoint absolute directories.');
  requireThat(!inside(config.artifactRoot, resolve(configurationPath)) && !inside(config.stagingRoot, resolve(configurationPath)), 'invalid_arguments', 'Private host configuration cannot live inside artifacts or source staging.');
  const serviceDirectories = config.instances.flatMap(instance => Object.values(servicePaths(config, instance)));
  requireThat(serviceDirectories.every((value, index) => isAbsolute(value) && serviceDirectories.every((other, otherIndex) => index === otherIndex || !inside(value, other))),
    'target_conflict', 'Service data, work and logs collide; configure explicitly separate paths for repeated service instances.');
  requireThat(serviceDirectories.every(value => [config.artifactRoot, config.stagingRoot, ...(config.servicesRoot ? [config.runtimeRoot] : [])]
    .every(root => !inside(root, value) && !inside(value, root))) && serviceDirectories.every(value => !inside(value, resolve(configurationPath))),
    'target_conflict', 'Service storage must not overlap software, staging, host state or central configuration.');
  for (const root of roots) await mkdir(root, { recursive: true });
  const actualRoots = await Promise.all(roots.map(root => realpath(root)));
  requireThat(actualRoots.every((root, index) => actualRoots.every((other, otherIndex) => index === otherIndex || !inside(root, other))), 'invalid_arguments', 'Host roots overlap through filesystem links.');
  const actualServices = await Promise.all(serviceDirectories.map(directory => resolvedFuturePath(directory)));
  requireThat(actualServices.every((value, index) => actualServices.every((other, otherIndex) => index === otherIndex || !inside(value, other)) &&
    actualRoots.slice(config.servicesRoot ? 0 : 1).every(root => !inside(root, value) && !inside(value, root))),
    'target_conflict', 'Service storage overlaps through filesystem links.');
  const internal = await Promise.all([...new Set(config.instances.filter(instance => instance.componentId === 'agent-manager').map(instance => instance.settings['internalProjectRoot']).filter((value): value is string => typeof value === 'string'))]
    .map(async path => { requireThat(isAbsolute(path), 'invalid_arguments', 'Internal project root must be absolute.'); return resolvedFuturePath(path); }));
  const shared = await Promise.all(sharedNativeHomes(config).map(home => resolvedFuturePath(home.path)));
  const configuration = resolve(config.configPath ?? configurationPath);
  requireThat(internal.every((path, index) => [...actualRoots, ...actualServices, ...shared].every(root => !inside(root, path) && !inside(path, root)) &&
    internal.every((other, otherIndex) => index === otherIndex || !inside(path, other)) && !inside(path, configuration)),
    'target_conflict', 'Internal project storage overlaps service state, software, shared Codex homes or configuration.');
  for (const instance of config.instances) if (instance.componentId === 'agent-manager' && instance.settings['projectRoot'] !== undefined) {
    requireThat(typeof instance.settings['projectRoot'] === 'string' && isAbsolute(instance.settings['projectRoot']), 'invalid_arguments', 'Normal project root must be absolute.');
    const normal = await resolvedFuturePath(instance.settings['projectRoot']);
    requireThat([...actualRoots, ...actualServices, ...shared, ...internal].every(root => !inside(root, normal)),
      'target_conflict', 'New normal projects must not be allocated inside private service/internal/Codex storage or executable artifacts.');
  }
  for (const directory of serviceDirectories) await mkdir(directory, { recursive: true });
  await Promise.all([...roots, ...serviceDirectories].map(directory => access(directory, constants.W_OK)));
  for (const home of sharedNativeHomes(config)) {
    let ancestor = resolve(home.path); const suffix: string[] = [];
    let actual: string;
    while (true) {
      try { actual = join(await realpath(ancestor), ...suffix.reverse()); break; }
      catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
        const parent = dirname(ancestor); requireThat(parent !== ancestor, 'target_conflict', 'Shared home has no existing ancestor.');
        suffix.push(basename(ancestor)); ancestor = parent;
      }
    }
    requireThat([...actualRoots, ...actualServices].every(root => !inside(root, actual) && !inside(actual, root)), 'target_conflict', 'Shared Codex homes cannot overlap owned runtime, service, artifact or staging storage through filesystem links.');
  }
  for (const path of Object.values(config.executables)) requireThat(isAbsolute(path), 'invalid_arguments', 'Host runtime executable mappings require absolute paths.');
  for (const instance of config.instances) {
    requireThat(instance.engine !== 'docker' || (process.platform === 'linux' && instance.docker), 'invalid_arguments', 'Docker instances require an explicit Linux container configuration.');
    requireThat(/^[a-zA-Z0-9][a-zA-Z0-9_-]{0,127}$/.test(instance.instanceId) && /^[a-z][a-z0-9-]*$/.test(instance.componentId), 'invalid_arguments', 'Instance/component IDs must be safe local names.');
  }
  return config;
}
