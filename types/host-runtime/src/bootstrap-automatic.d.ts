import { inspectBootstrap } from './bootstrap-maintenance.js';
import { HostJournal } from './journal.js';
import type { Host } from '../../contracts/src/generated.js';
import type { BootstrapPackageComponent } from './package-updater.js';
import type { BootstrapOs } from './bootstrap-os.js';
export type AutomaticBootstrapPhase = 'launched' | 'checking' | 'updating' | 'verifying' | 'succeeded' | 'rolling_back' | 'rolled_back' | 'outcome_unknown' | 'needs_attention';
export interface AutomaticBootstrapRecord {
    schemaVersion: 1;
    operationId: string;
    requestHash: string;
    phase: AutomaticBootstrapPhase;
    errorCode: string | null;
    createdAt: string;
    updatedAt: string;
}
export declare function automaticBootstrapRoot(config: Host.HostConfig, operationId: string): string;
export declare function automaticBootstrapRecord(config: Host.HostConfig, operationId: string): Promise<AutomaticBootstrapRecord | null>;
export declare function saveAutomaticBootstrapRecord(config: Host.HostConfig, value: AutomaticBootstrapRecord): Promise<void>;
export declare class AutomaticBootstrapUpdater {
    readonly configPath: string;
    readonly distribution: string;
    readonly journal: HostJournal;
    private readonly os;
    private readonly inspect;
    constructor(configPath: string, distribution: string, journal: HostJournal, os?: BootstrapOs, inspect?: typeof inspectBootstrap);
    reconcile(selected: BootstrapPackageComponent[]): Promise<boolean>;
}
