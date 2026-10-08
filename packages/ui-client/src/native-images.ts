import type { RpcClient } from '../../sdk/src/client.js';
import { nativeRead, record, text } from './native.js';

/** Resolve paths on the owning host, independently of the browser's operating system. */
export function nativeImagePath(source: string, cwd: string): string | null {
  let path: string;
  try {
    if (/^file:/i.test(source)) {
      const url = new URL(source);
      path = (url.hostname && url.hostname !== 'localhost' ? '//' + url.hostname : '') + decodeURIComponent(url.pathname);
      if (/^\/[a-z]:\//i.test(path)) path = path.slice(1);
    } else path = decodeURIComponent(source);
  } catch { throw new Error('The image path is invalid.'); }
  if (!path || /[\u0000-\u001f]/.test(path)) throw new Error('The image path is invalid.');
  if (/^[a-z][a-z\d+.-]*:/i.test(path) && !/^[a-z]:[\\/]/i.test(path)) return null;
  path = path.replaceAll('\\', '/');
  if (!/^(?:[a-z]:\/|\/)/i.test(path)) {
    if (!/^(?:[a-z]:[\\/]|\/|\\\\)/i.test(cwd)) throw new Error('The task folder is not available yet.');
    path = cwd.replaceAll('\\', '/').replace(/\/$/, '') + '/' + path;
  }
  const prefix = /^[a-z]:\//i.exec(path)?.[0] ?? (path.startsWith('//') ? '//' : '/');
  const parts: string[] = [];
  for (const part of path.slice(prefix.length).split('/')) {
    if (part === '..') parts.pop();
    else if (part && part !== '.') parts.push(part);
  }
  return prefix + parts.join('/');
}

/** Only decoded raster image bytes become a trusted browser blob. */
export function nativeImageBlob(base64: string): Blob {
  if (base64.length > Math.ceil(8388608 / 3) * 4) throw new Error('The image is larger than 8 MiB.');
  if (!base64 || base64.length % 4 || !/^[A-Za-z\d+/]*={0,2}$/.test(base64)) throw new Error('The image data is invalid.');
  const bytes = Uint8Array.from(atob(base64), character => character.charCodeAt(0));
  if (bytes.length > 8388608) throw new Error('The image is larger than 8 MiB.');
  const signature = String.fromCharCode(...bytes.subarray(0, 16));
  const type = signature.startsWith('\x89PNG\r\n\x1a\n') ? 'image/png'
    : signature.startsWith('\xff\xd8\xff') ? 'image/jpeg'
    : /^GIF8[79]a/.test(signature) ? 'image/gif'
    : signature.startsWith('RIFF') && signature.slice(8, 12) === 'WEBP' ? 'image/webp'
    : signature.slice(4, 8) === 'ftyp' && /^(avif|avis)$/.test(signature.slice(8, 12)) ? 'image/avif' : null;
  if (!type) throw new Error('The file is not a supported image.');
  return new Blob([bytes], { type });
}

export async function readNativeImage(client: RpcClient, node: string, source: string, cwd: string, signal: AbortSignal): Promise<Blob | null> {
  const inline = /^data:image\/(?:png|jpeg|gif|webp|avif);base64,(.*)$/i.exec(source);
  if (inline) return nativeImageBlob(inline[1]!);
  const path = nativeImagePath(source, cwd);
  if (!path) return null;
  const file = record(await nativeRead(client, node, 'codex.fs/readFile', { path }, signal));
  return nativeImageBlob(text(file.dataBase64));
}
