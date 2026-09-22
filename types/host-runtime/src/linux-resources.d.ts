import type { Host } from '../../contracts/src/generated.js';
export declare const installationIdentity: (config: Host.HostConfig) => string;
export declare function linuxResourceScope(installationId: string, budget: Host.LinuxResourceBudget): Host.LinuxResourceScope;
export declare function validateLinuxResourceScope(scope: Host.LinuxResourceScope, installationId?: string): void;
export declare function linuxSlice(scope: Host.LinuxResourceScope): string;
export declare function ensureLinuxSlice(scope: Host.LinuxResourceScope): Promise<void>;
export declare function inspectLinuxSlice(scope: Host.LinuxResourceScope): Promise<void>;
export declare function assertLinuxResourceMembership(scope: Host.LinuxResourceScope, unit: string, membership: string, values: Record<string, string>): void;
export declare function verifyLinuxResourceMembership(config: Host.HostConfig, instanceId: string, scope: Host.LinuxResourceScope): Promise<void>;
