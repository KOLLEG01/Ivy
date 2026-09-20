import type { Host } from '../../contracts/src/generated.js';
declare function readiness(config: Host.HostConfig, plan: Host.BootstrapPlan, states: Host.BootstrapSnapshot, notBefore: number, timeoutMs?: number): Promise<void>;
export declare function runAutomaticBootstrapUpdate(configPath: string, requestPath: string, distribution: string, os?: import("./bootstrap-os.js").BootstrapOs, waitForReadiness?: typeof readiness): Promise<void>;
export {};
