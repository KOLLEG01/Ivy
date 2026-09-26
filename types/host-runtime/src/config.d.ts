import type { Host } from '../../contracts/src/generated.js';
export declare function jsonFile<T>(path: string, limit?: number): Promise<T>;
export declare function atomicJson(path: string, value: unknown): Promise<void>;
export declare function inside(root: string, path: string): boolean;
export declare function instanceConfig(path: string): Promise<Host.InstanceConfig>;
export declare function configurationPath(): string;
