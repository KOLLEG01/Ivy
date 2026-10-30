import type { Agent, Wire } from '../../contracts/src/generated.js';
import type { NativeContract } from '../../contracts/src/native-contract.js';
import type { NativeTarget } from './native.js';
export type NativeOwnerIdentity = Omit<NativeTarget, 'hostId'> & {
    hostId?: string;
};
export interface NativeOperationIdentity extends NativeOwnerIdentity {
    callerPrincipalId: string;
    operationId: string;
    method: string;
    params: Wire.Json;
}
export interface NativeReadIdentity extends NativeOwnerIdentity {
    callerPrincipalId: string;
    epoch: string;
    method: string;
    params: Wire.Json;
}
export declare function nativeInstant(value: string, code?: string): number;
export declare function validateNativeStatus(value: unknown, target: NativeOwnerIdentity, code?: string): asserts value is Agent.Status & {
    epoch: string;
};
export declare function validateNativeOperation(value: unknown, expected: NativeOperationIdentity, code?: string): asserts value is Agent.Operation;
export declare function validateNativeRead(value: unknown, expected: NativeReadIdentity, code?: string): asserts value is Agent.ReadObservation;
/** These are timestamps from the same operation owner, never the caller host's wall clock. */
export declare function validateNativeProgress(previous: Agent.Operation, next: Agent.Operation, code?: string): void;
export declare function nativeOperationTerminal(value: Agent.Operation): boolean;
export declare function validateNativePayload(contract: NativeContract, observation: Agent.Operation | Agent.ReadObservation, limits: {
    requestBytes: number;
    responseBytes: number;
}): void;
