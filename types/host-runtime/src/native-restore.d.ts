import type { Host } from '../../contracts/src/generated.js';
/** Called only on the fresh, activation-held working copy, never the source or backup SQLite. */
export declare function relocateNativeStorage(original: Host.HostConfig, config: Host.HostConfig, manifest: Host.BackupManifest, distributionRoot: string): Promise<Host.NativeRecoveryReport>;
