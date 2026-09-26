import { HostJournal } from './journal.js';
import type { DockerAdapter } from './docker.js';
import type { SystemdAdapter } from './systemd.js';
import type { Host } from '../../contracts/src/generated.js';
/** Start a new readiness-failure window at the first failed check, not at epoch. */
export declare function readinessFailureStart(previous: number, at?: number): number;
export declare function readinessExpired(start: number, timeoutMs: number, at?: number): boolean;
/** A target that already names the installed build can be drained directly. */
export declare function replacementNeedsSettledObservation(currentCandidateId: string | null, installedCandidateId: string | null): boolean;
/** A matching stopped observation remains authoritative while its immutable target still requests stopped. */
export declare function stoppedTargetIsConclusive(observed: Host.RuntimeObservation | undefined, target: Host.RuntimeTarget | null): boolean;
/** An unchanged ready target can be verified by the live Hive identity request even after its journal observation ages. */
export declare function runningHiveCanBeInspected(observed: Host.RuntimeObservation | undefined, target: Host.RuntimeTarget | null): boolean;
/** Infrastructure replacements are queue barriers: dependants must finish first. */
export declare function parallelDeployments(queued: readonly Host.JournalEntry[], active: readonly Host.JournalEntry[], limit?: number, exclusive?: (entry: Host.JournalEntry) => boolean): Host.JournalEntry[];
/** Only this independently guarded process holds the activation lock. It never owns managed children. */
export declare class HostExecutor {
    readonly configPath: string;
    readonly bootstrapRoot: string;
    readonly ownInstanceId?: string | undefined;
    private readonly dockerAdapter;
    private readonly systemdAdapter;
    private readonly lock;
    private readonly baseJournal;
    private readonly executionJournal;
    get journal(): HostJournal;
    private readonly controller;
    private readonly bootId;
    private readonly release;
    private task;
    private pulse;
    private activeId;
    private closed;
    private readonly dockerHandles;
    private readonly systemdHandles;
    private linuxResourcesLoaded;
    private linuxResources;
    private readonly packageUpdater;
    private readonly bootstrapUpdater;
    private packageCode;
    private readonly configurationUpdater;
    private configurationCode;
    private configurationRevision;
    private configurationHash;
    private retentionTask;
    private retentionCode;
    private storageCode;
    get config(): Host.HostConfig;
    constructor(config: Host.HostConfig, configPath: string, bootstrapRoot: string, ownInstanceId?: string | undefined, dockerAdapter?: DockerAdapter, systemdAdapter?: SystemdAdapter, packageCredential?: string);
    private status;
    start(): void;
    get completion(): Promise<void>;
    close(): Promise<void>;
    private advance;
    private dockerObservation;
    private stopDocker;
    /** Docker is mutated only by an explicit activation/stop operation. */
    private reconcileDockerOnce;
    private resourceScope;
    private systemdObservation;
    private stopSystemd;
    /** systemd owns restart policy; this path mutates a unit only for an explicit activation/stop. */
    private reconcileSystemdOnce;
    private waitFor;
    private serviceStorage;
    private compatibility;
    private rollback;
    private execute;
    private run;
}
