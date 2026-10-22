import type { Host } from '../../contracts/src/generated.js';
/** Relocate the disposable backup index only. Hive assignments and saved plans retain their original paths. */
export declare function relocateProjects(original: Host.HostConfig, config: Host.HostConfig, captured?: NonNullable<Host.BackupManifest['storageAreas']>): Promise<void>;
