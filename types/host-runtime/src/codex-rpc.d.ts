import type { Readable, Writable } from 'node:stream';
import type { Agent, Wire } from '../../sdk/src/node.js';
export interface NativeRequest {
    id: Agent.RequestId;
    method: string;
    params: Wire.Json;
}
export interface NativeNotification {
    method: string;
    params: Wire.Json;
}
export interface NativeRequestHooks {
    requestId?: string;
    /** Background reads may expire without terminating unrelated accepted turns. */
    detachOnTimeout?: boolean;
    /** Reserve native capacity for foreground requests while background work is outstanding. */
    priority?: 'interaction';
    beforeSend?: (id: Agent.RequestId) => void;
    beforeResolve?: (id: Agent.RequestId, reply: Agent.Reply) => void;
}
/** Public native JSONL only. The owner supplies durable hooks; this layer never retries. */
export declare class NativeRpc {
    readonly catalog: Agent.Catalog;
    private readonly input;
    private readonly hooks;
    readonly closed: Promise<string>;
    private finishClosed;
    private ended;
    private fragments;
    private bufferedBytes;
    private readonly pending;
    private readonly expired;
    private readonly observationPrefix;
    private observationSequence;
    private readonly requests;
    private readonly serverRequests;
    private readonly notifications;
    private readonly clientNotifications;
    constructor(catalog: Agent.Catalog, input: Writable, output: Readable, hooks: {
        onRequest: (request: NativeRequest) => void;
        onNotification: (notification: NativeNotification) => void;
        onClose: (code: string) => void;
    });
    get connected(): boolean;
    private writable;
    private write;
    private observation;
    request(method: string, params: Wire.Json, hooks?: NativeRequestHooks, timeoutMs?: number): Promise<Agent.Reply>;
    notify(method: string, params?: Wire.Json): void;
    /** The owner must validate the current pending identity and persist answer intent in beforeSend. */
    answer(method: string, id: Agent.RequestId, reply: Agent.Reply, beforeSend: () => void): void;
    private receive;
    private frame;
    close(code?: string): void;
}
