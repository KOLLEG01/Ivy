import type { Host, Operation } from "../../contracts/src/generated.js";
import type { DataContract } from "../../contracts/src/types.js";
import type { RpcClient } from "../../sdk/src/client.js";
import { readUiBundle } from "../../cli/src/publish-ui.js";
type ResetPath = {
    instanceId: string;
    componentId: string;
    area: "host" | "data" | "work" | "logs";
    path: string;
    absolutePath: string;
};
type PreservedPath = ResetPath & {
    checksum: string | null;
};
type CompletionMarker = {
    instanceId: string;
    componentId: string;
    path: string;
    absolutePath: string;
};
type SavedRuntime = {
    instanceId: string;
    candidate: Host.Candidate;
    manifest: Host.ReleaseManifest;
    enabled: boolean;
};
type SavedInstruction = {
    family: {
        key: string;
        owner: string;
        mediaType: string;
        objectMode: string;
    };
    contract: {
        version: string;
        definition: string;
    };
    object: {
        id: string;
        parentId: string | null;
        ownerObjectId: string | null;
        name: string;
        path: string;
        position: number;
        icon: string | null;
        depth: number;
        currentRevision: number;
        createdAt: string;
        updatedAt: string;
    };
    revision: {
        contractVersion: string;
        encoding: string;
        content: string;
        contentHash: string;
        byteLength: number;
        createdAt: string;
        referencesJson: string;
    };
};
export type RuntimeResetPlan = {
    schemaVersion: 1;
    deploymentIdentity: string;
    hostId: string;
    configPath: string;
    instances: {
        instanceId: string;
        componentId: string;
        enabled: boolean;
    }[];
    scopeRootIds: string[];
    delete: ResetPath[];
    preserve: PreservedPath[];
    completionMarkers: CompletionMarker[];
};
type ResetRecord = {
    schemaVersion: 1;
    deploymentIdentity: string;
    resetId: string;
    runtimeEpoch: string;
    hostId: string;
    phase: "prepared" | "cleared" | "complete" | "released";
    updatedAt: string;
    planHash: string;
    plan: RuntimeResetPlan;
    saved: SavedRuntime[];
    instructions: SavedInstruction[];
    publisherCredentialDigest: string;
    instructionsCaptured?: boolean;
    deploymentComplete?: boolean;
};
type ResetUiBundle = {
    directory: string;
    definition: Operation.UiDefinition;
    checkedBundle: Awaited<ReturnType<typeof readUiBundle>>;
};
export declare function resetBootstrapContracts(bundles: readonly ResetUiBundle[]): DataContract[];
export declare function retainedResetIdentity(previous: Pick<ResetRecord, "resetId" | "runtimeEpoch" | "phase" | "planHash" | "deploymentComplete"> | null, planHash: string): {
    resetId: string;
    runtimeEpoch: string;
    resumeFromReleased: boolean;
} | null;
export declare function configuredScopeRootIds(config: Pick<Host.HostConfig, "instances">): string[];
export declare function coordinatedRuntimeResetPlans(plans: RuntimeResetPlan[]): RuntimeResetPlan[];
export declare function resetUiBundles(distributionRoot: string, uiIds: readonly string[]): Promise<ResetUiBundle[]>;
export declare function republishResetUis(client: RpcClient, bundles: readonly ResetUiBundle[], baseMutationId: string): Promise<string[]>;
export declare function releaseResetHosts<T extends {
    hostId: string;
}>(hosts: readonly T[], release: (host: T) => Promise<unknown>, reprepare: (host: T) => Promise<unknown>, afterRelease?: () => Promise<unknown>): Promise<void>;
export declare function bootstrapResetUis<T extends {
    hostId: string;
}>(hosts: readonly T[], bootstrap: (host: T) => Promise<unknown>, reprepare: (host: T) => Promise<unknown>, restoreUis: () => Promise<unknown>): Promise<void>;
export declare function localRuntimeResetPlan(config: Host.HostConfig, configPath: string, distributionRoot: string): Promise<RuntimeResetPlan>;
export declare function restoredBootstrapTarget(plan: Host.BootstrapPlan, instanceId: string, candidateId: string, enabled: boolean, requestedAt: string): Host.RuntimeTarget | null;
export declare function runtimeResetHost(config: Host.HostConfig, configPath: string, distributionRoot: string, phase: "preview" | "prepare" | "clear" | "complete" | "bootstrap" | "release" | "confirm", resetId?: string, runtimeEpoch?: string, scopeRoots?: string, publisherCredentialDigest?: string): Promise<unknown>;
export declare function runtimeReset(config: Host.HostConfig, configPath: string, distributionRoot: string, apply: boolean, confirmation?: string): Promise<unknown>;
export {};
