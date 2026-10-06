import { createHash } from 'node:crypto';
import { canonical } from './canonical-json.js';
export { canonical, encodeJson, validateJson, compareVersions } from './canonical-json.js';
export type { Json, JsonObject } from './canonical-json.js';
export const digest = (bytes: string | Uint8Array): string => 'sha256:' + createHash('sha256').update(bytes).digest('hex');
export const hashJson = (value: unknown): string => digest(canonical(value));
