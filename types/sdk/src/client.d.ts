import type { Operation, OperationName, Params, Result, Wire } from "../../contracts/src/generated.js";
import { deriveOperationId, operationId, parseOperationId } from "../../contracts/src/operation-id.js";
import { canonical } from "../../contracts/src/canonical-json.js";
import { nativeVersions } from "../../contracts/src/native-versions.js";
export type { Agent, Chat, Secretary, TaskBoard, Automation, Operation, OperationName, Params, Result, Transport, Wire, } from "../../contracts/src/generated.js";
export { IvyError, requireThat } from "../../contracts/src/errors.js";
export { upgradeTaskRecord, upgradeRunRecord, upgradeCommentRecord, upgradeConfigurationRecord, upgradeResultRecord, upgradeResultContent } from "../../contracts/src/task-board-records.js";
export { openTaskQuestions } from "../../contracts/src/task-board-questions.js";
export { canonical, deriveOperationId, operationId, parseOperationId, nativeVersions, };
export { binaryObjectContentBytes, connectionInFlightRequests, consumerInFlightRequests, jsonObjectContentBytes, managementFrameBytes, mcpDiscoveryResultBytes, rpcBatchRequests, rpcBatchRequestBytes, textObjectContentBytes, } from "../../contracts/src/limits.js";
declare const boundRead: unique symbol;
export interface RequestOptions {
    signal?: AbortSignal;
    timeoutMs?: number;
    [boundRead]?: boolean;
}
export interface ToolCallOptions extends RequestOptions {
    expectedCallerPrincipalId?: string;
}
export interface BoundTool extends Operation.ToolBinding {
    serviceNodeId: string;
    resourceRef?: Wire.ResourceRef;
}
export interface RpcClient {
    request<M extends OperationName>(method: M, params: Params<M>, options?: RequestOptions): Promise<Result<M>>;
}
/** A bundled query document must describe the same exact revision as its query row. */
export declare function queryDocument(item: Operation.QueryItem): Operation.ObjectRead;
export declare function runtimeEpoch(client: RpcClient): Promise<string>;
export declare function newOperationId(client: RpcClient, nonce?: string): Promise<string>;
/** Stable for one live workflow owner; durable workflows persist the returned value before effects. */
export declare function scopedOperationId(client: RpcClient, scope: unknown): Promise<string>;
export type ProviderNotificationListener = (frame: import("../../contracts/src/generated.js").Transport.ProviderNotification) => void;
export type ProviderNotificationFilter = import("../../contracts/src/generated.js").Operation.NotificationFilter;
export interface InterfaceRequirement {
    namespace: string;
    interfaceVersion: string;
}
export type DiscoveryTarget = Omit<Params<"tools.list">, "namespace" | "namePrefix" | "cursor" | "limit"> & {
    interfaceVersion?: string;
};
/** Discovery is explicit; a changed definition can never silently authorize another invocation. */
export declare function discover(client: RpcClient, qualifiedName: string, target?: DiscoveryTarget): Promise<BoundTool>;
export declare function callBound(client: RpcClient, binding: BoundTool, args: Wire.Json, operationId?: string, options?: ToolCallOptions): Promise<Wire.Json>;
/** One selected provider. Mutations retain exact bindings; reads may refresh a rejected binding. */
export declare class BoundToolClient {
    readonly client: RpcClient;
    readonly serviceNodeId: string;
    private readonly dynamicNamespace;
    private readonly bindings;
    private readonly interfaces;
    constructor(client: RpcClient, serviceNodeId: string, requirements: readonly InterfaceRequirement[], dynamicNamespace?: string | null);
    binding(qualifiedName: string, expectedDefinitionHash?: string): Promise<BoundTool>;
    /** Explicit rediscovery for a consumer that will reassess the replacement contract. */
    refresh(binding: BoundTool): Promise<BoundTool>;
    call(qualifiedName: string, args: Wire.Json, operationId?: string, options?: ToolCallOptions): Promise<Wire.Json>;
    /** Refresh a definitively rejected read once, retaining provider, interface and arguments. */
    read(qualifiedName: string, args: Wire.Json, options?: ToolCallOptions): Promise<Wire.Json>;
    private readBound;
}
/** Reused within one RPC client/connection. Stable service namespaces always name their exact interface version. */
export declare function serviceTools(client: RpcClient, serviceNodeId: string, requirements: readonly InterfaceRequirement[]): BoundToolClient;
/** Native Codex definitions are versioned and hash-fenced by the installed native catalog. */
export declare function nativeServiceTools(client: RpcClient, serviceNodeId: string): BoundToolClient;
export declare function responseValue(frame: unknown, id: string): unknown;
export declare function baseUrl(value: string): URL;
/** Admission never retries sent work; uncertain mutations keep their original identity. */
export declare class HiveClient implements RpcClient {
    private readonly options;
    readonly base: URL;
    private active;
    private readonly queue;
    private readonly reads;
    constructor(value: string, options?: {
        credential?: string;
        fetch?: typeof fetch;
        /** Queue calls routed to providers beyond this many; Hive holds each one until the provider answers. */
        providerCalls?: number;
        /** Bound all sent requests, while allowing ordinary reads past waiting provider calls. */
        requestCalls?: number;
        /** Maximum requests waiting to be sent. */
        queuedRequests?: number;
        /** Share concurrent reads and gather small read batches; writes are always sent individually. */
        batchReads?: boolean;
    });
    request<M extends OperationName>(method: M, params: Params<M>, options?: RequestOptions): Promise<Result<M>>;
    private send;
    private exchange;
}
export declare function browserClient(value: string): HiveClient;
/** Browser-only provider notifications. Commands keep using the reconciled HTTP client. */
export declare class BrowserNotifications {
    readonly base: URL;
    private readonly listeners;
    private socket;
    private readonly changeListeners;
    private readonly statusListeners;
    private syncedChanges;
    private syncedFilters;
    private ready;
    private deadline;
    private heartbeat;
    private reconnect;
    private reconnectMs;
    private pending;
    private syncedSignature;
    constructor(value: string);
    subscribe(filter: ProviderNotificationFilter, listener: ProviderNotificationListener): () => void;
    get connected(): boolean;
    onStatus(listener: (ready: boolean) => void, scopes?: readonly string[], filters?: readonly ProviderNotificationFilter[]): () => void;
    subscribeChanges(scopes: readonly string[], listener: (scopes: string[]) => void): () => void;
    /** Resume after browser suspension or a network change with a fresh subscription. */
    reconnectNow(): void;
    private setReady;
    private acknowledged;
    private wanted;
    private changed;
    private disconnected;
    private retry;
    private filters;
    private sync;
    private connect;
}
export declare function browserNotifications(value: string): BrowserNotifications;
