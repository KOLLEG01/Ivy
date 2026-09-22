import { WebSocket } from "ws";
import { IvyError } from "../../contracts/src/errors.js";
import type { Operation, OperationName, Params, Result, Transport, Wire } from "../../contracts/src/generated.js";
import type { RequestOptions, RpcClient } from "./client.js";
export interface InvocationContext {
    callerPrincipalId: string;
    operationId?: string;
    generation: number;
    signal: AbortSignal;
}
export type ToolHandler = (args: Record<string, Wire.Json>, context: InvocationContext) => Promise<Wire.Json> | Wire.Json;
export type JsonToolHandler = (args: Wire.Json, context: InvocationContext) => Promise<Wire.Json> | Wire.Json;
export interface ServiceOptions {
    publicBaseUrl: string;
    credential: () => string | Promise<string>;
    identity: Wire.ServiceConnect;
    registry: () => Wire.RegistrySync | Promise<Wire.RegistrySync>;
    handlers: Record<string, ToolHandler>;
    jsonHandlers?: Record<string, JsonToolHandler>;
    reconcile: (connection: ServiceConnection) => Promise<void>;
    readiness?: (connection: ServiceConnection) => Promise<{
        ready: boolean;
        diagnostics: Wire.Diagnostic[];
    }>;
    onState?: (state: {
        status: "connecting" | "syncing" | "ready" | "degraded" | "offline" | "stopped";
        generation?: number;
        code?: string;
    }) => void;
    notificationFilters?: () => Operation.NotificationFilter[] | Promise<Operation.NotificationFilter[]>;
    onNotification?: (value: Transport.ProviderNotification) => void;
    reconnectMinMs?: number;
    reconnectMaxMs?: number;
    heartbeatMs?: number;
}
/** One socket generation. Closing it never queues requests for its replacement. */
export declare class ServiceConnection implements RpcClient {
    private readonly owner;
    readonly socket: WebSocket;
    readonly controller: AbortController;
    readonly closed: Promise<void>;
    readonly opened: Promise<void>;
    generation: number;
    get serviceNodeId(): string;
    ready: boolean;
    private readonly pending;
    private readonly eventWaiters;
    private eventsAvailableThroughSequence;
    private bindings;
    private finish;
    private openReject;
    private ended;
    constructor(url: URL, credential: string, owner: ServiceClient);
    get signal(): AbortSignal;
    private registryBindings;
    private registrySnapshot;
    setRegistry(registry: Wire.RegistrySync): void;
    prepareRegistry(registry: Wire.RegistrySync): Promise<Wire.RegistrySync>;
    private send;
    request<M extends OperationName>(method: M, params: Params<M>, options?: RequestOptions): Promise<Result<M>>;
    notification(namespace: string, name: string, version: string, payload: Wire.Json): void;
    private eventsAvailable;
    waitForEvents(afterSequence: number, timeoutMs: number): Promise<void>;
    private invoke;
    close(error?: IvyError): void;
}
export declare class ServiceClient {
    readonly options: ServiceOptions;
    private readonly controller;
    private task;
    private current;
    private acknowledged;
    private invocations;
    constructor(options: ServiceOptions);
    get connection(): ServiceConnection;
    get ready(): boolean;
    start(): void;
    stop(): Promise<void>;
    waitReady(options?: RequestOptions): Promise<ServiceConnection>;
    enterInvocation(): void;
    leaveInvocation(): void;
    notify(frame: Transport.ProviderNotification): void;
    private state;
    private run;
}
/** Resolve persistBatch only after its durable result commits. A failure explicitly withholds ack. */
export declare function consumeEvents(connection: ServiceConnection, subscription: Params<"events.subscribe">, persistBatch: (batch: Result<"events.subscribe">, signal: AbortSignal) => Promise<void>, options?: {
    pollMs?: number;
    onFailure?: (error: IvyError, batch: Result<"events.subscribe">) => void;
}): Promise<void>;
