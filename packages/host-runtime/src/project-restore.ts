import { join, relative } from 'node:path';
import { atomicJson, inside, jsonFile } from './config.js';
import { exists } from './backup-files.js';
import { servicePaths } from './layout.js';
import { restoredStorageAreas } from './project-storage.js';
import type { Host } from '../../contracts/src/generated.js';

/** Relocate the disposable backup index only. Hive assignments and saved plans retain their original paths. */
export async function relocateProjects(original: Host.HostConfig, config: Host.HostConfig, captured: NonNullable<Host.BackupManifest['storageAreas']> = []): Promise<void> {
  for (const instance of config.instances.filter(value => value.componentId === 'agent-manager')) {
    const path = join(servicePaths(config, instance).data, 'project-backup-paths.json'); if (!await exists(path)) continue;
    const prior = original.instances.find(value=>value.instanceId===instance.instanceId)!;
    const before = String(prior.settings['internalProjectRoot']), after = String(instance.settings['internalProjectRoot']);
    const paths = await jsonFile<string[]>(path), areas = restoredStorageAreas(config, captured);
    await atomicJson(path, paths.map(cwd => {
      const retained = captured.find(area=>area.key.startsWith('retained-projects/'+instance.instanceId+'/')&&inside(area.path,cwd));
      if(retained)return join(areas.find(area=>area.key===retained.key)!.path,relative(retained.path,cwd));
      return inside(before,cwd)?join(after,relative(before,cwd)):cwd;
    }));
  }
}
