import type { Operation, OperationName, Params, Result, Wire } from "../../contracts/src/generated.js";
import { deriveOperationId, operationId, parseOperationId } from "../../contracts/src/operation-id.js";
import { canonical } from "../../contracts/src/canonical-json.js";
import { nativeVersions } from "../../contracts/src/native-versions.js";
export type { Agent, Chat, Secretary, TaskBoard, Automation, Operation, OperationName, Params, Result, Transport, Wire, } from "../../contracts/src/generated.js";
export { IvyError, requireThat } from "../../contracts/src/errors.js";
export { canonical, deriveOperationId, operationId, parseOperationId, nativeVersions, };
export { binaryObjectContentBytes, connectionInFlightRequests, jsonObjectContentBytes, managementFrameBytes, mcpDiscoveryResultBytes, textObjectContentBytes, } from "../../contracts/src/limits.js";
export interface RequestOptions {
    signal?: AbortSignal;
    timeoutMs?: number;
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
/** One selected provider, with immutable bindings and no automatic rediscovery or invocation retry. */
export declare class BoundToolClient {
    readonly client: RpcClient;
    readonly serviceNodeId: string;
    private readonly dynamicNamespace;
    private readonly bindings;
    private readonly interfaces;
    constructor(client: RpcClient, serviceNodeId: string, requirements: readonly InterfaceRequirement[], dynamicNamespace?: string | null);
    binding(qualifiedName: string, expectedDefinitionHash?: string): Promise<BoundTool>;
    call(qualifiedName: string, args: Wire.Json, operationId?: string, options?: ToolCallOptions): Promise<Wire.Json>;
}
/** Reused within one RPC client/connection. Stable service namespaces always name their exact interface version. */
export declare function serviceTools(client: RpcClient, serviceNodeId: string, requirements: readonly InterfaceRequirement[]): BoundToolClient;
/** Native Codex definitions are versioned and hash-fenced by the installed native catalog. */
export declare function nativeServiceTools(client: RpcClient, serviceNodeId: string): BoundToolClient;
export declare function responseValue(frame: unknown, id: string): unknown;
export declare function baseUrl(value: string): URL;
/** No retry queue: a timeout/disconnect requires reconciliation of the original mutation/domain ID. */
export declare class HiveClient implements RpcClient {
    private readonly options;
    readonly base: URL;
    private active;
    constructor(value: string, options?: {
        credential?: string;
        fetch?: typeof fetch;
    });
    request<M extends OperationName>(method: M, params: Params<M>, options?: RequestOptions): Promise<Result<M>>;
}
/** Browser code receives no API credential and stores no authentication material. */
export declare function browserClient(value: string): HiveClient;
/** Browser-only provider notifications. Commands keep using the reconciled HTTP client. */
export declare class BrowserNotifications {
    readonly base: URL;
    private readonly listeners;
    private socket;
    private reconnect;
    private reconnectMs;
    private pending;
    private syncedSignature;
    constructor(value: string);
    subscribe(filter: ProviderNotificationFilter, listener: ProviderNotificationListener): () => void;
    private filters;
    private sync;
    private connect;
}
export declare function browserNotifications(value: string): BrowserNotifications;
