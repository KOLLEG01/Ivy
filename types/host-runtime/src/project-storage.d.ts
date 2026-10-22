import type { Host } from '../../contracts/src/generated.js';
/** Called only after the host's owners are stopped. Root edits never drop earlier internal work. */
export declare function projectStorageAreas(config: Host.HostConfig): Promise<{
    key: string;
    path: string;
}[]>;
export declare function restoredStorageAreas(config: Host.HostConfig, captured: NonNullable<Host.BackupManifest['storageAreas']>): {
    key: string;
    path: string;
}[];
