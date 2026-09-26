/** Produce a self-contained schema for Hive's core operations without loading service-domain schemas. */
export declare function bundledSchema(reference: string, document?: string): Record<string, unknown>;
export declare function operationInputSchema(method: string): Record<string, unknown> & {
    type: 'object';
};
