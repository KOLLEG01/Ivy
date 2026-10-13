import type { Host } from '../../contracts/src/generated.js';
/** Codex recreates only this named per-session helper cache on native startup. */
export declare function nativeBackupExclusions(config: Host.HostConfig): NonNullable<Host.BackupManifest['ephemeralNativePaths']>;
/** References only: shared user history, credentials and sockets are never copied or relocated. */
export declare function sharedNativeHomes(config: Host.HostConfig): NonNullable<Host.BackupManifest['sharedNativeHomes']>;
