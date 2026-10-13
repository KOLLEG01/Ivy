import type { Host } from '../../contracts/src/generated.js';
/** Held OS locks prevent a new runtime-owner/executor/native owner throughout offline maintenance. */
export declare function offlineOwners(config: Host.HostConfig): Promise<() => void>;
/** Bootstrap replacement stops only its OS owners; ordinary services keep running. */
export declare function offlineBootstrapOwners(config: Host.HostConfig, instanceIds: readonly string[]): Promise<() => void>;
export declare function backupHost(configPath: string, destination: string, distributionRoot: string): Promise<Host.BackupResult>;
