export { canonical, encodeJson, validateJson, compareVersions } from './canonical-json.js';
export type { Json, JsonObject } from './canonical-json.js';
export declare const digest: (bytes: string | Uint8Array) => string;
export declare const hashJson: (value: unknown) => string;
