import type { Agent, Wire } from './generated.js';
import type { Schema } from './types.js';
export declare function isReadOnlyNativeMethod(method: string): boolean;
/** The provider's unchanged native definition. All consumers use this same projection. */
export declare function nativeToolDefinition(catalog: Agent.Catalog, method: Agent.Catalog['clientRequests'][number], catalogHash?: string): Wire.ToolDefinition;
/** Exact selected native contract, shared by the SDK, AgentManager and native consumers. */
export declare class NativeContract {
    readonly catalog: Agent.Catalog;
    readonly catalogHash: string;
    private readonly methods;
    private readonly definitionHashes;
    private readonly validators;
    private readonly schemas;
    constructor(catalog: Agent.Catalog);
    definition(method: string): Wire.ToolDefinition;
    definitions(): ReadonlyMap<string, Wire.ToolDefinition>;
    /** Retained definitions are deeply frozen; only their immutable local hashes are cached. */
    definitionHash(method: string): string;
    /** Check a discovery snapshot against the retained contract; never adopt a replacement. */
    verifyDefinition(method: string, definition: Wire.ToolDefinition, definitionHash: string): void;
    /** Close only this adapter's outer arguments. Nested extension objects retain native semantics. */
    inputSchema(method: string, reserved?: readonly string[]): Schema;
    validateInput(method: string, params: unknown, maximumBytes?: number): void;
    validateTemplate(method: string, params: unknown, reserved: readonly string[], maximumBytes?: number): void;
    validateResult(method: string, result: unknown, maximumBytes?: number): void;
}
