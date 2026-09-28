export type Json = null | boolean | number | string | Json[] | {
    [key: string]: Json;
};
export type JsonObject = {
    [key: string]: Json;
};
/** Transport encoding: byte bounds belong to the wire; canonical ordering belongs to storage identities. */
export declare function encodeJson(value: unknown, maximumBytes?: number): string;
/** Validate JSON-domain values and their wire size without sorting keys or building a canonical string. */
export declare function validateJson(value: unknown, maximumBytes?: number): void;
/** One encoding for hashes, durable request identity and JSON content. Rejects non-JSON JS inputs. */
export declare function canonical(value: unknown, maximumBytes?: number): string;
export declare function compareVersions(a: string, b: string): number;
