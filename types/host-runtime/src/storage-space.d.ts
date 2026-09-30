import type { Host } from '../../contracts/src/generated.js';
export interface RetentionProgress {
    removed(area: keyof Host.StorageRetentionStatus['removed']): void;
    skipped(area: string, reason: string, requiresAttention?: boolean): void;
}
/** Count logical file bytes without following links into shared caches. */
export declare function treeBytes(path: string, boundary?: string): Promise<number>;
export declare function storageIsLow(availableBytes: number, totalBytes: number): boolean;
export declare function hostStorageSpace(config: Host.HostConfig): Promise<{
    availableBytes: Host.StorageRetentionStatus['availableBytes'];
    lowSpace: boolean;
}>;
/** Pressure retries remain throttled even if nothing safe can be deleted. */
export declare class RetentionSchedule {
    private nextScheduled;
    private nextPressure;
    constructor(now: number);
    due(now: number, lowSpace: boolean): boolean;
    started(now: number): void;
    failed(now: number): void;
}
export declare function availableStorageBytes(path: string): Promise<number>;
export declare function requireStorageSpace(path: string, requiredBytes: number): Promise<void>;
