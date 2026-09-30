/** Hive-owned top-level routes cannot be claimed by a UI. */
export declare const reservedUiSlugs: string[];
export declare const uiPath: (metadata: {
    uiId: string;
    slug?: string;
}) => string;
