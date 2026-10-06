import { canonical } from '../../sdk/src/client.js';
import type { Operation } from './runtime';
export const hashBytes = async (bytes: Uint8Array<ArrayBuffer>) => 'sha256:' + Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)), value => value.toString(16).padStart(2, '0')).join('');
export async function downloadRevision(value: Operation.ObjectRead) {
  const bytes = value.content.encoding === 'base64' ? Uint8Array.from(atob(value.content.value), character => character.charCodeAt(0))
    : new TextEncoder().encode(value.content.encoding === 'json' ? canonical(value.content.value) : value.content.value);
  if (bytes.length !== value.revision.byteLength || await hashBytes(bytes) !== value.revision.contentHash) throw new Error('The bytes do not match the saved revision.');
  const url = URL.createObjectURL(new Blob([bytes], { type: 'application/octet-stream' }));
  const anchor = document.createElement('a'); anchor.href = url; anchor.download = value.object.name; anchor.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
}
export function base64(bytes: Uint8Array) {
  let binary = ''; for (let offset = 0; offset < bytes.length; offset += 32768) binary += String.fromCharCode(...bytes.subarray(offset, offset + 32768));
  return btoa(binary);
}
