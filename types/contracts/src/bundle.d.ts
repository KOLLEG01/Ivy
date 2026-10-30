/** Produce a self-contained MCP schema; ivy.invalid identifiers never become network retrievals. */
export declare function bundledSchema(reference: string, document?: string, overrides?: Record<string, Record<string, unknown>>): Record<string, unknown>;
export declare function operationInputSchema(method: string): Record<string, unknown> & {
    type: 'object';
};
