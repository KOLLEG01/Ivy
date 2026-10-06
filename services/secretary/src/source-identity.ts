import { hashJson } from '../../../packages/sdk/src/node.js';
import { need } from './schema.js';

const record = (value: unknown): Record<string, unknown> => {
  need(value && typeof value === 'object' && !Array.isArray(value), 'secretary_account_response_invalid', 'Expected an account response.');
  return value as Record<string, unknown>;
};

export function catalogueAccountHash(value: unknown): string {
  const account = record(record(value)['account']);
  need(account['type'] === 'chatgpt' && typeof account['email'] === 'string' && account['email'].length > 0,
    'secretary_account_mismatch', 'The observed account differs from the configured account.');
  return hashJson({ type: 'chatgpt', email: account['email'] });
}
