import type { Host } from '../../contracts/src/generated.js';
export declare const linuxAgentUser = "ivy-agent";
export declare function linuxProcessUser(componentId: string, platform: NodeJS.Platform | Host.BootstrapPlan['os']): string | undefined;
type ProcessIdentity = {
    componentId: string;
    user?: string;
    configPath: string;
    artifactRoot: string;
    nodeExecutable: string;
};
/** Keep model-reachable native execution on its own non-administrative OS identity. */
export declare function ensureLinuxProcessIdentity(processIdentity: ProcessIdentity, runtime: Host.InstanceConfig): Promise<void>;
export {};
