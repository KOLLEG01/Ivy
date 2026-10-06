import { canonical } from '../../sdk/src/client.js';

export async function hashJson(value: unknown): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(canonical(value)));
  return 'sha256:' + Array.from(new Uint8Array(digest), value => value.toString(16).padStart(2, '0')).join('');
}
