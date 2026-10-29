import type { Host } from '../../contracts/src/generated.js';
export declare class HealthFile {
    readonly config: Host.InstanceConfig;
    private readonly startedAt;
    private readonly bootId;
    readonly launchId: string | undefined;
    constructor(config: Host.InstanceConfig);
    write(ready: boolean, details?: string, generation?: number | null): Promise<void>;
    control(): Promise<Host.RuntimeControl | null>;
}
export declare function localHiveEndpoint(config: Pick<Host.InstanceConfig, 'publicBaseUrl' | 'settings'>, docker?: Host.Instance['docker']): URL;
export declare function checkHealth(config: Host.InstanceConfig, hiveEndpoint?: string, verifyRegistration?: boolean): Promise<Host.Health>;
