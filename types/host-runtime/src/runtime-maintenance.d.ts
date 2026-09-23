import type { Host } from '../../contracts/src/generated.js';
export declare const runtimeResetMarker: (config: Pick<Host.HostConfig, "ivyRoot" | "runtimeRoot">) => string;
export declare const hiveResetBootstrapMarker: (dataRoot: string) => string;
export declare function runtimeResetActive(config: Pick<Host.HostConfig, 'ivyRoot' | 'runtimeRoot'>): Promise<boolean>;
