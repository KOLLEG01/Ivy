import { wireSchema } from './wire-schema.js';
import type { Operation } from './generated.js';
export { wireSchema };
export declare const operationSchema: Record<string, unknown>;
export declare const transportSchema: Record<string, unknown>;
export interface OperationDefinition {
    input: string;
    output: string;
    access: 'client' | 'service';
    mutation: boolean;
    discoverable?: boolean;
}
export declare const operations: Record<string, OperationDefinition>;
export interface RpcRequest {
    jsonrpc: '2.0';
    id: string | number;
    method: string;
    params: Record<string, unknown>;
}
export declare function validateRequest(value: unknown): asserts value is RpcRequest;
/** Validate only the provider response envelope; the selected tool contract is checked separately in debug mode. */
export declare function validateResponseFrame(value: unknown): asserts value is {
    id: string;
    result?: unknown;
    error?: Record<string, unknown>;
};
export declare function validateInput(method: string, value: unknown): void;
export declare function validateOutput(method: string, value: unknown): void;
export declare function validateUiDefinition(value: unknown): void;
export declare function isUiRelease(value: unknown): value is Operation.UiRelease;
export declare function isUiMetadata(value: unknown): value is Operation.UiMetadata;
export declare function validateShared(name: string, value: unknown): void;
export declare function validateTransport(name: string, value: unknown): void;
/** Used by the live Hive path; the expensive service-domain schema trees stay out of this module. */
export declare const coreManagementFrameBytes = 33554432;
