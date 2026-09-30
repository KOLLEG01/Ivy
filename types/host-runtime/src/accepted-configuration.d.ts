import type { Host } from '../../contracts/src/generated.js';
/** One private immutable file is the complete configuration boundary for an
 * accepted operation. Its random path is the reference; there is no parallel
 * content identity or periodic integrity round. */
export declare function saveAcceptedConfiguration(config: Host.HostConfig): string;
/** Copy the exact prior launch configuration at operation acceptance. Rollback
 * never trusts the old target path again after that boundary. */
export declare function acceptedInstanceConfigurationPath(config: Host.HostConfig, revision: string): string;
export declare function saveAcceptedInstanceConfiguration(config: Host.HostConfig, source: string, revision: string): string;
export declare function readAcceptedInstanceConfiguration(anchor: Host.HostConfig, path: string): Host.InstanceConfig;
export declare function readAcceptedConfiguration(anchor: Host.HostConfig, path: string): Host.HostConfig;
export declare function sameInstallation(anchor: Host.HostConfig, next: Host.HostConfig): void;
export declare function installationRecord(config: Host.HostConfig): string;
