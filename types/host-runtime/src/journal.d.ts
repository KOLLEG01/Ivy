import { DatabaseSync } from "node:sqlite";
import type { Host, Wire } from "../../contracts/src/generated.js";
export declare const terminalPhases: Set<"failed" | "draining" | "checking" | "succeeded" | "needs_attention" | "preparing" | "prepared" | "activating" | "verifying" | "rolling_back" | "rolled_back">;
/** Windows bootstrap owns these roots directly; normal target deployment cannot observe their drain. */
export declare const requiresBootstrapReplacement: (instance: Pick<Host.Instance, "componentId">) => boolean;
export interface InstalledInstance {
    instanceId: string;
    candidateId: string;
    buildId: string;
    enabled: boolean;
    installedAt: string;
}
export interface ConfigurationAdoption {
    configuration: Host.HostConfig;
    revision: number;
    contentHash: string;
    actions: Array<{
        instanceId: string;
        action: "restart" | "enable" | "disable";
        operationId: string;
    }>;
    bootstrapRequired: string[];
}
export declare function freshExecutorStatus(value: Host.ExecutorStatus): boolean;
/** Stable launch identity used to bind a direct Linux bootstrap unit to its target. */
export declare function bootstrapLaunchId(plan: Host.BootstrapPlan, instanceId: string): string;
/** One independent OS file lock. Kernel process death releases it; stale PID files never grant a lock. */
export declare class ExecutorLock {
    private readonly db;
    constructor(root: string);
    close(): void;
}
/** Independent host-local authority. CLI accepts intents; the OS executor alone advances them. */
export declare class HostJournal {
    readonly db: DatabaseSync;
    private activeConfig;
    get config(): Host.HostConfig;
    constructor(config: Host.HostConfig);
    close(): void;
    acceptedConfiguration(path: string): Host.HostConfig;
    useConfiguration(config: Host.HostConfig): void;
    configurationAdoption(): ConfigurationAdoption | null;
    stageConfigurationAdoption(value: ConfigurationAdoption): void;
    finishConfigurationAdoption(value: ConfigurationAdoption): void;
    status(executor: Host.ExecutorStatus | null): Host.HostStatus;
    /** Read the current owner files for a status projection without changing the journal. */
    currentObservations(executor?: Host.ExecutorStatus | null): Record<string, Host.RuntimeObservation>;
    managementSnapshot(executor: Host.ExecutorStatus | null, minimumSequence?: number, status?: Host.HostStatus): Host.ManagementSnapshot;
    private transaction;
    instance(instanceId: string): Host.Instance;
    private normalizeEntry;
    get(deploymentId: string): Host.JournalEntry;
    operation(operationId: string): Host.JournalEntry | null;
    list(limit?: number): Host.JournalEntry[];
    private pendingEntries;
    unfinished(): Host.JournalEntry[];
    installed(instanceId: string): InstalledInstance | null;
    setInstalled(value: InstalledInstance): void;
    /** Record the candidates that the reviewed OS bootstrap actually installed. */
    syncBootstrap(plan: Host.BootstrapPlan, clearTargets?: boolean): void;
    recordObservation(instanceId: string, value: Host.RuntimeObservation): void;
    observations(): Record<string, Host.RuntimeObservation>;
    target(instanceId: string): Host.RuntimeTarget | null;
    restoreTarget(value: Host.RuntimeTarget): void;
    saveCandidate(candidate: Host.Candidate, manifest: unknown): void;
    candidate(candidateId: string): Host.Candidate;
    manifest(candidateId: string): Host.ReleaseManifest;
    candidateForBuild(componentId: string, buildId: string): Host.Candidate;
    accept(request: Host.LocalRequest, sourceSnapshot?: Host.SourceSnapshot): Host.JournalEntry;
    advance(deploymentId: string, expectedPhase: Wire.DeploymentRecord["phase"], phase: Wire.DeploymentRecord["phase"], change?: {
        candidateId?: string;
        observedBuild?: string | null;
        readiness?: Wire.DeploymentRecord["readiness"];
        errorCode?: string;
        rollbackTargetRevision?: string;
        target?: Host.RuntimeTarget;
        installed?: InstalledInstance;
    }): Host.JournalEntry;
}
