/** Bundle one local definition and its reachable definitions without unrelated contract inputs. */
export declare function bundleLocalSchema(document: {
    $schema?: string;
    $defs: Record<string, unknown>;
}, name: string): Record<string, unknown>;
