import { HostJournal } from './journal.js';
import type { Host } from '../../contracts/src/generated.js';
export declare function savePreparation(host: Host.HostConfig, record: Host.PreparationRecord): Promise<void>;
/** Caller holds this exact build's OS lock. Unresolved or failed work is never compacted. */
export declare function compactPreparation(journal: HostJournal, record: Host.PreparationRecord, trustedPublication?: boolean): Promise<Host.PreparationCompaction['entries'][number]>;
/** Caller holds the build lock, so a stale building record cannot belong to an active builder. */
export declare function recoverPreparedBuild(journal: HostJournal, buildId: string): Promise<Host.Candidate | null>;
export declare const preparationIdForBuild: (buildId: string) => string;
export declare function preparationInventory(host: Host.HostConfig): Promise<Host.PreparationInventory>;
/** Acquires the same kernel lock as builders; old failed/unrecognized state stays untouched. */
export declare function compactPreparations(host: Host.HostConfig, onlyBuildId?: string): Promise<Host.PreparationCompaction>;
