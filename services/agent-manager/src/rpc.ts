export * from '../../../packages/host-runtime/src/codex-rpc.js';
import type { Wire } from '../../../packages/sdk/src/node.js';

/** Small metadata reads must release their slots promptly; history and mutations keep their own budgets. */
export const nativeReadTimeoutMs = (method: string, params: Wire.Json): number | undefined =>
  method === 'thread/read' && params !== null && typeof params === 'object' && !Array.isArray(params) && params['includeTurns'] === false
    ? 15_000 : undefined;
