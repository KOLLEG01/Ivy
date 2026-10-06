import { fail } from './errors.js';
import { managementFrameBytes } from './limits.js';
const encoder = new TextEncoder();
const utf8ByteLength = (globalThis as { Buffer?: { byteLength(text: string): number } }).Buffer?.byteLength
  ?? ((text: string) => encoder.encode(text).byteLength);

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type JsonObject = { [key: string]: Json };

/** Transport encoding: byte bounds belong to the wire; canonical ordering belongs to storage identities. */
export function encodeJson(value: unknown, maximumBytes = managementFrameBytes): string {
  const text = JSON.stringify(value);
  if (text === undefined) fail('invalid_arguments', 'Expected a JSON message.');
  if (text.length > maximumBytes || utf8ByteLength(text) > maximumBytes) fail('content_too_large', 'JSON exceeds the configured byte limit.');
  return text;
}

/** Validate JSON-domain values and their wire size without sorting keys or building a canonical string. */
export function validateJson(value: unknown, maximumBytes = managementFrameBytes): void {
  const ancestors = new Set<object>(); let nodes = 0;
  const visit = (item: unknown, depth: number): void => {
    if (++nodes > 400_000 || depth > 128) fail('limit_exceeded', 'JSON structure is too complex.');
    if (item === null || typeof item === 'boolean' || typeof item === 'string') return;
    if (typeof item === 'number') {
      if (!Number.isFinite(item) || (Number.isInteger(item) && !Number.isSafeInteger(item))) fail('invalid_arguments', 'Numbers must be finite and integers must be safe.');
      return;
    }
    if (typeof item !== 'object') fail('invalid_arguments', 'Value is outside the JSON domain.');
    if (ancestors.has(item)) fail('invalid_arguments', 'Cyclic JSON value.');
    ancestors.add(item);
    try {
      if (Array.isArray(item)) {
        if (Object.getOwnPropertySymbols(item).length || Object.keys(item).length !== item.length) fail('invalid_arguments', 'Arrays must be dense and have no extra properties.');
        for (let i = 0; i < item.length; i++) {
          const descriptor = Object.getOwnPropertyDescriptor(item, String(i));
          if (!descriptor || !('value' in descriptor)) fail('invalid_arguments', 'JSON cannot contain accessors.');
          visit(descriptor.value, depth + 1);
        }
      } else {
        const prototype = Object.getPrototypeOf(item);
        if (prototype !== Object.prototype && prototype !== null) fail('invalid_arguments', 'JSON objects must be plain objects.');
        if (Object.getOwnPropertySymbols(item).length) fail('invalid_arguments', 'JSON cannot contain symbol keys.');
        for (const key of Object.getOwnPropertyNames(item)) {
          const descriptor = Object.getOwnPropertyDescriptor(item, key)!;
          if (!descriptor.enumerable || !('value' in descriptor)) fail('invalid_arguments', 'JSON properties must be enumerable values.');
          visit(descriptor.value, depth + 1);
        }
      }
    } finally { ancestors.delete(item); }
  };
  visit(value, 0);
  const encoded = JSON.stringify(value);
  if (encoded === undefined || encoded.length > maximumBytes || utf8ByteLength(encoded) > maximumBytes) fail('content_too_large', 'JSON exceeds the configured byte limit.');
}

/** One encoding for hashes, durable request identity and JSON content. Rejects non-JSON JS inputs. */
export function canonical(value: unknown, maximumBytes = managementFrameBytes): string {
  const ancestors = new Set<object>();
  let nodes = 0;
  let bytes = 0;
  const account = (text: string): string => {
    // Schema keys, punctuation and most values are ASCII. Avoid allocating a UTF-8
    // buffer per fragment while retaining exact byte limits for all Unicode text.
    bytes += /^[\x00-\x7f]*$/.test(text) ? text.length : encoder.encode(text).byteLength;
    if (bytes > maximumBytes) fail('content_too_large', 'JSON exceeds the configured byte limit.');
    return text;
  };
  const visit = (item: unknown, depth: number): string => {
    if (++nodes > 400_000 || depth > 128) fail('limit_exceeded', 'JSON structure is too complex.');
    if (item === null) return account('null');
    if (typeof item === 'boolean') return account(String(item));
    if (typeof item === 'string') return account(JSON.stringify(item));
    if (typeof item === 'number') {
      if (!Number.isFinite(item) || (Number.isInteger(item) && !Number.isSafeInteger(item))) {
        fail('invalid_arguments', 'Numbers must be finite and integers must be safe.');
      }
      return account(JSON.stringify(item));
    }
    if (typeof item !== 'object') fail('invalid_arguments', 'Value is outside the JSON domain.');
    if (ancestors.has(item)) fail('invalid_arguments', 'Cyclic JSON value.');
    ancestors.add(item);
    try {
      if (Array.isArray(item)) {
        if (Object.getOwnPropertySymbols(item).length || Object.keys(item).length !== item.length) {
          fail('invalid_arguments', 'Arrays must be dense and have no extra properties.');
        }
        const parts: string[] = [];
        for (let i = 0; i < item.length; i++) {
          const descriptor = Object.getOwnPropertyDescriptor(item, String(i));
          if (!descriptor || !('value' in descriptor)) fail('invalid_arguments', 'JSON cannot contain accessors.');
          parts.push(visit(descriptor.value, depth + 1));
        }
        account('[' + ','.repeat(Math.max(0, parts.length - 1)) + ']');
        return '[' + parts.join(',') + ']';
      }
      const prototype = Object.getPrototypeOf(item);
      if (prototype !== Object.prototype && prototype !== null) fail('invalid_arguments', 'JSON objects must be plain objects.');
      if (Object.getOwnPropertySymbols(item).length) fail('invalid_arguments', 'JSON cannot contain symbol keys.');
      const keys = Object.getOwnPropertyNames(item).sort();
      const pairs = keys.map(key => {
        const descriptor = Object.getOwnPropertyDescriptor(item, key)!;
        if (!descriptor.enumerable || !('value' in descriptor)) fail('invalid_arguments', 'JSON properties must be enumerable values.');
        return account(JSON.stringify(key) + ':') + visit(descriptor.value, depth + 1);
      });
      account('{' + ','.repeat(Math.max(0, pairs.length - 1)) + '}');
      return '{' + pairs.join(',') + '}';
    } finally { ancestors.delete(item); }
  };
  return visit(value, 0);
}

export function compareVersions(a: string, b: string): number {
  const parse = (value: string): bigint[] => {
    if (value.length > 128 || !/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(value)) {
      fail('invalid_arguments', 'Expected an exact numeric contract version.');
    }
    return value.split('.').map(BigInt);
  };
  const aa = parse(a), bb = parse(b);
  for (let i = 0; i < 3; i++) if (aa[i] !== bb[i]) return aa[i]! < bb[i]! ? -1 : 1;
  return 0;
}
