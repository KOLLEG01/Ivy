import { requireThat } from './errors.js';

/** Native JSONL has already been decoded and size-bounded by the transport. Keep this check structural. */
export function validateAgentFrame(value: unknown, kind: 'request-id' | 'reply' = 'reply'): void {
  if (kind === 'request-id') {
    requireThat((typeof value === 'string' && value.length > 0 && value.length <= 256) ||
      (typeof value === 'number' && Number.isSafeInteger(value)), 'native_invalid_frame', 'Native request ID is invalid.');
    return;
  }
  requireThat(value !== null && typeof value === 'object' && !Array.isArray(value), 'native_invalid_frame', 'Native reply must be an object.');
  const reply = value as Record<string, unknown>;
  const result = Object.hasOwn(reply, 'result'), error = Object.hasOwn(reply, 'error');
  requireThat(result !== error, 'native_invalid_frame', 'Native reply requires exactly one result or error.');
  if (error) {
    const detail = reply['error'];
    requireThat(detail !== null && typeof detail === 'object' && !Array.isArray(detail), 'native_invalid_frame', 'Native error must be an object.');
    const item = detail as Record<string, unknown>;
    requireThat(Number.isSafeInteger(item['code']) && typeof item['message'] === 'string', 'native_invalid_frame', 'Native error envelope is incomplete.');
  }
}
