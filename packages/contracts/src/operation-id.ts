import { canonical } from './canonical-json.js';
import { requireThat } from './errors.js';

export interface OperationIdentity { runtimeEpoch: string; issuedAtUnixMs: number; nonce: string }

export function operationId(runtimeEpoch: string, issuedAtUnixMs = Date.now(), nonce: string = crypto.randomUUID()): string {
  requireThat(runtimeEpoch.length > 0 && !runtimeEpoch.includes(':') && Number.isSafeInteger(issuedAtUnixMs) && issuedAtUnixMs >= 0 &&
    nonce.length > 0 && nonce.length <= 128 && !nonce.includes(':'), 'invalid_arguments', 'Invalid operation identity fields.');
  return `${runtimeEpoch}:${issuedAtUnixMs}:${nonce}`;
}

export function parseOperationId(value: string): OperationIdentity {
  const parts = value.split(':');
  requireThat(parts.length === 3 && parts[0] && /^\d{1,16}$/.test(parts[1] ?? '') && parts[2] && parts[2]!.length <= 128,
    'invalid_arguments', 'Operation identity must be <runtimeEpoch>:<issuedAtUnixMs>:<nonce>.');
  const issuedAtUnixMs = Number(parts[1]);
  requireThat(Number.isSafeInteger(issuedAtUnixMs), 'invalid_arguments', 'Operation identity has an invalid issue time.');
  return { runtimeEpoch: parts[0], issuedAtUnixMs, nonce: parts[2] };
}

export function deriveOperationId(parent: string, scope: unknown): string {
  const identity = parseOperationId(parent);
  const text = canonical({ nonce: identity.nonce, scope });
  let first = 2166136261, second = 2246822519;
  for (let index = 0; index < text.length; index++) {
    const code = text.charCodeAt(index);
    first = Math.imul(first ^ code, 16777619);
    second = Math.imul(second ^ code, 3266489917);
  }
  return operationId(identity.runtimeEpoch, identity.issuedAtUnixMs,
    (first >>> 0).toString(16).padStart(8, '0') + (second >>> 0).toString(16).padStart(8, '0'));
}
