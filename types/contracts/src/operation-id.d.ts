export interface OperationIdentity {
    runtimeEpoch: string;
    issuedAtUnixMs: number;
    nonce: string;
}
export declare function operationId(runtimeEpoch: string, issuedAtUnixMs?: number, nonce?: string): string;
export declare function parseOperationId(value: string): OperationIdentity;
export declare function deriveOperationId(parent: string, scope: unknown): string;
