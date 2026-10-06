import type { Wire } from '../../../packages/sdk/src/node.js';

export const record = (value: unknown): Record<string, Wire.Json> =>
  value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, Wire.Json> : {};
