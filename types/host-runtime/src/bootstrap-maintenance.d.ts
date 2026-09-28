import type { BootstrapOs } from './bootstrap-os.js';
import type { Host } from '../../contracts/src/generated.js';
export declare function bootstrapStatus(config: Host.HostConfig, operationId: string): Promise<Host.BootstrapMaintenanceRecord>;
export declare function inspectBootstrap(configPath: string, distribution: string, os?: BootstrapOs): Promise<Host.BootstrapSnapshot>;
export declare function updateBootstrap(configPath: string, request: Host.BootstrapMaintenanceRequest, distribution: string, os?: BootstrapOs): Promise<Host.BootstrapMaintenanceRecord>;
/** Only the new working copy is fenced; historical request paths and successful records are immutable. */
export declare function holdRestoredBootstrapMaintenance(config: Host.HostConfig, at: string): Promise<number>;
