import type { Host } from '../../contracts/src/generated.js';
export declare function verifyServiceStorage(config: Host.HostConfig, instanceId: string, plan: Pick<Host.BuildPlan, 'componentId' | 'storage'>, verifiedRunningFormat?: number): void;
