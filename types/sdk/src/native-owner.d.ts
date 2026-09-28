import type { Agent, Wire } from '../../contracts/src/generated.js';
import type { NativeContract } from '../../contracts/src/native-contract.js';
import type { RpcClient, RequestOptions } from './client.js';
import { nativeThreadState } from './native-observations.js';
import type { NativeOwnerIdentity } from './native-evidence.js';
export interface NativeOperationCall {
    operationId: string;
    method: string;
    params: Wire.Json;
    definitionHash: string;
}
/** Exact native owner transport. Domain claims and permission to act remain with the caller. */
export declare class NativeOwner {
    readonly client: RpcClient;
    readonly caller: string;
    readonly options: RequestOptions;
    readonly target: NativeOwnerIdentity;
    constructor(client: RpcClient, caller: string, target: NativeOwnerIdentity, options?: RequestOptions);
    private checkSignal;
    selectedContract(): Promise<NativeContract>;
    binding(method: string, expectedDefinitionHash?: string): Promise<import("./client.js").BoundTool>;
    management(name: string, args: Wire.Json, operationId?: string): Promise<Wire.Json>;
    status(): Promise<Agent.Status>;
    frameLimits(): Promise<Agent.FrameLimits>;
    /** Resolve membership before the caller retains its immutable start request. */
    threadStartParams(params: Record<string, Wire.Json>): Promise<Record<string, Wire.Json>>;
    checkOperation(value: unknown, call: NativeOperationCall): void;
    operation(call: NativeOperationCall): Promise<Agent.Operation | Agent.OperationAbsence>;
    dispatch(call: NativeOperationCall, guard: () => Promise<void>): Promise<Agent.Operation | undefined>;
    checkRead(value: unknown, method: string, params: Wire.Json, epoch: string, allowError?: boolean): void;
    /** Interactive calls never pass through the durable Hive operation store. */
    interact(call: NativeOperationCall, beforeSend?: () => Promise<void>): Promise<Agent.Interaction>;
    interaction(operationId: string): Promise<Agent.Interaction>;
    read(method: Agent.ReadObservation['method'], params: Wire.Json, epoch?: string, allowError?: boolean): Promise<Agent.ReadObservation>;
    threadState(observation: Agent.ReadObservation, epoch: string): Promise<ReturnType<typeof nativeThreadState>>;
}
