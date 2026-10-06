import { isAbsolute, join, relative, sep } from 'node:path';
import { inside } from './config.js';
import { requireThat } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';
import { instanceOwnedCodexHome, expectedServerHome, usesExternalAppServer } from './codex-home.js';
import type { CodexHomeSettings } from './codex-home.js';
import { servicePaths, storagePath, hostStorageAreas } from './layout.js';

/** Codex recreates only this named per-session helper cache on native startup. */
export function nativeBackupExclusions(config: Host.HostConfig): NonNullable<Host.BackupManifest['ephemeralNativePaths']> {
  return config.instances.filter(instance => instance.componentId === 'agent-manager').flatMap(instance => {
    if (usesExternalAppServer(instance.settings)) return [];
    const home = instanceOwnedCodexHome(instance.settings as CodexHomeSettings, servicePaths(config, instance).data);
    if (!home) return [];
    const path = storagePath(config, join(home, 'tmp', 'arg0'));
    return [{ instanceId: instance.instanceId, path: path.startsWith('state/') ? path.slice(6) : path, kind: 'codex-arg0-helper-cache' as const }];
  }).sort((a, b) => a.path.localeCompare(b.path));
}

/** References only: shared user history, credentials and sockets are never copied or relocated. */
export function sharedNativeHomes(config: Host.HostConfig): NonNullable<Host.BackupManifest['sharedNativeHomes']> {
  return config.instances.filter(instance => instance.componentId === 'agent-manager').flatMap(instance => {
    const settings = instance.settings as CodexHomeSettings;
    if (!usesExternalAppServer(instance.settings) && instanceOwnedCodexHome(settings, servicePaths(config, instance).data)) return [];
    const home = expectedServerHome(instance.settings);
    requireThat(typeof home === 'string' && isAbsolute(home) &&
      [...hostStorageAreas(config).map(area => area.path), config.artifactRoot, config.stagingRoot]
        .every(root => !inside(root, home) && !inside(home, root)),
      'target_conflict', 'Shared Codex homes must not overlap owned host, service, project or software storage. Use explicit owned-stdio for an instance-owned home.');
    return [{ instanceId: instance.instanceId, path: home, kind: 'external-user-state' as const }];
  }).sort((a, b) => a.instanceId.localeCompare(b.instanceId));
}
