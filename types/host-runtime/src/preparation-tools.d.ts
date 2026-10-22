/** Explicit host tools for nested build runners, never arbitrary parent environment. */
export declare function preparationTools(executables: Record<string, string>, bootstrapRoot: string, temporaryRoot: string): Promise<{
    environment: Record<string, string>;
}>;
