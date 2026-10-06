import { isAbsolute, join } from 'node:path';
import { exists } from './backup-files.js';
import { inside, jsonFile } from './config.js';
import { hostStorageAreas, servicePaths } from './layout.js';
import { hashJson } from '../../contracts/src/canonical.js';
import { requireThat } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';

/** Called only after the host's owners are stopped. Root edits never drop earlier internal work. */
export async function projectStorageAreas(config: Host.HostConfig): Promise<{ key: string; path: string }[]> {
  const areas = hostStorageAreas(config);
  for (const instance of [...config.instances].sort((a, b) => a.instanceId.localeCompare(b.instanceId))) {
    if (instance.componentId !== 'agent-manager') continue;
    const path = join(servicePaths(config, instance).data, 'project-backup-paths.json'); if (!await exists(path)) continue;
    const paths = await jsonFile<string[]>(path);
    requireThat(Array.isArray(paths) && paths.length <= 256 && paths.every(value=>typeof value==='string'&&isAbsolute(value)), 'backup_invalid', 'Invalid cached internal project inventory.');
    for (const cwd of paths) {
      if (areas.some(area => inside(area.path, cwd))) continue;
      areas.push({ key: 'retained-projects/' + instance.instanceId + '/' + hashJson(cwd).slice(7), path: cwd });
    }
  }
  return areas;
}

export function restoredStorageAreas(config: Host.HostConfig, captured: NonNullable<Host.BackupManifest['storageAreas']>) {
  return [...hostStorageAreas(config), ...captured.filter(area => area.key.startsWith('retained-projects/')).map(area => {
    const [, instanceId, hash] = area.key.split('/');
    const instance = config.instances.find(value => value.instanceId === instanceId && value.componentId === 'agent-manager');
    requireThat(instance && /^[a-f0-9]{64}$/.test(hash ?? ''), 'backup_invalid', 'Invalid retained internal project identity.');
    return { key: area.key, path: join(String(instance.settings['internalProjectRoot']), 'restored-' + hash) };
  })];
}
