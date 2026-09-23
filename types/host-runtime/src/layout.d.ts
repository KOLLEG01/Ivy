import type { Host } from '../../contracts/src/generated.js';
export declare function accountHome(environment?: NodeJS.ProcessEnv, platform?: NodeJS.Platform): string;
export declare function defaultHostConfigPath(environment?: NodeJS.ProcessEnv, platform?: NodeJS.Platform): string;
export declare function hostConfigurationPath(config: Host.HostConfig): string;
export declare function resolvedFuturePath(path: string): Promise<string>;
export declare function servicePaths(config: Host.HostConfig, instance: Host.Instance | string): {
    data: string;
    work: string;
    logs: string;
};
/** Resolve once at the host boundary. Explicit old roots retain the original layout. */
export declare function resolveHostConfiguration(input: Host.HostConfigInput, sourcePath: string, environment?: NodeJS.ProcessEnv, platform?: NodeJS.Platform): Host.HostConfig;
/** Permanent storage outside runtime is captured separately, never treated as staging/cache. */
export declare function hostStorageAreas(config: Host.HostConfig): {
    key: string;
    path: string;
}[];
export declare function storagePath(config: Host.HostConfig, path: string): string;
export declare function storageFile(config: Host.HostConfig, key: string, areas?: {
    key: string;
    path: string;
}[]): string;
