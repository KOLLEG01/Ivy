import type { Host } from '../../contracts/src/generated.js';
/** Bounded, idempotent host collection. Unknown evidence remains for explicit review. */
export declare function collectHostStorage(config: Host.HostConfig, now?: number, trigger?: Host.StorageRetentionStatus['trigger']): Promise<{
    candidates: number;
    preparations: number;
    snapshots: number;
}>;
