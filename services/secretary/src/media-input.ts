import { digest } from '../../../packages/sdk/src/node.js';
import type { Wire } from '../../../packages/sdk/src/node.js';
import { need } from './schema.js';

export interface MediaInputSource { name: string; mediaType: string | null; byteLength: number; contentHash: string }
export type PreparedMediaInput = { kind: 'text' | 'image' | 'audio'; source: MediaInputSource; input: Record<string, Wire.Json> }
  | { kind: 'video'; source: MediaInputSource; inputs: Record<string, Wire.Json>[] }
  | { kind: 'unavailable'; source: MediaInputSource; code: string };
const prefix = (bytes: Buffer, value: string, offset = 0) => bytes.subarray(offset, offset + value.length).equals(Buffer.from(value, 'binary'));

/** Prepare inert, complete acquired bytes. This does not fetch, execute, or grant model tools. */
export function prepareMediaInput(source: MediaInputSource, bytes: Buffer, maximumInputBytes: number): PreparedMediaInput {
  need(Number.isInteger(maximumInputBytes) && maximumInputBytes > 0 && maximumInputBytes <= 6 * 1024 * 1024,
    'media_input_limit_invalid', 'Media input needs its bounded selected native request budget.');
  need(bytes.length === source.byteLength && digest(bytes) === source.contentHash,
    'media_input_integrity_invalid', 'Semantic input must retain the complete verified original attachment bytes.');
  const original = structuredClone(source), unavailable = (code: string): PreparedMediaInput => ({ kind: 'unavailable', source: original, code });
  const mediaType = source.mediaType?.split(';', 1)[0]?.trim().toLowerCase() ?? '';
  if (['text/plain', 'text/markdown', 'text/csv', 'application/json', 'application/xml', 'text/xml'].includes(mediaType)) {
    let text: string;
    try { text = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes); }
    catch { return unavailable('media_text_encoding_unsupported'); }
    // No truncation: the caller retains an explicit gap when the complete content cannot fit.
    const input = { type: 'text', text, text_elements: [] };
    if (Buffer.byteLength(JSON.stringify(input)) > maximumInputBytes) return unavailable('media_native_input_too_large');
    return { kind: 'text', source: original, input };
  }
  const formats: Record<string, { kind: 'image' | 'audio'; valid: () => boolean }> = {
    'image/png': { kind: 'image', valid: () => bytes.length >= 24 && prefix(bytes, '\x89PNG\r\n\x1a\n') },
    'image/jpeg': { kind: 'image', valid: () => bytes.length >= 4 && bytes[0] === 0xff && bytes[1] === 0xd8 && bytes[2] === 0xff },
    'image/gif': { kind: 'image', valid: () => bytes.length >= 13 && (prefix(bytes, 'GIF87a') || prefix(bytes, 'GIF89a')) },
    'image/webp': { kind: 'image', valid: () => bytes.length >= 20 && prefix(bytes, 'RIFF') && prefix(bytes, 'WEBP', 8) },
    'audio/ogg': { kind: 'audio', valid: () => bytes.length >= 27 && prefix(bytes, 'OggS') },
    'audio/wav': { kind: 'audio', valid: () => bytes.length >= 44 && prefix(bytes, 'RIFF') && prefix(bytes, 'WAVE', 8) },
    'audio/mpeg': { kind: 'audio', valid: () => bytes.length >= 4 && (prefix(bytes, 'ID3') || bytes[0] === 0xff && (bytes[1]! & 0xe0) === 0xe0) },
    'audio/mp4': { kind: 'audio', valid: () => bytes.length >= 16 && prefix(bytes, 'ftyp', 4) },
    'audio/webm': { kind: 'audio', valid: () => bytes.length >= 8 && prefix(bytes, '\x1a\x45\xdf\xa3') },
  };
  const format = Object.hasOwn(formats, mediaType) ? formats[mediaType] : undefined;
  if (!format) return unavailable('media_type_unsupported');
  if (!format.valid()) return unavailable('media_signature_mismatch');
  const input = { type: format.kind, url: 'data:' + mediaType + ';base64,' + bytes.toString('base64') };
  if (Buffer.byteLength(JSON.stringify(input)) > maximumInputBytes) return unavailable('media_native_input_too_large');
  // Signatures identify containers only. Actual decoding and selected-model support remain native gates.
  return { kind: format.kind, source: original, input };
}
