import type { Operation } from '../../../packages/ui-client/src/runtime';
import { hashBytes } from '../../../packages/ui-client/src/content';

export function imageMime(bytes: Uint8Array): string | null {
  if (bytes.length >= 8 && [137, 80, 78, 71, 13, 10, 26, 10].every((value, index) => bytes[index] === value)) return 'image/png';
  if (bytes.length >= 3 && bytes[0] === 255 && bytes[1] === 216 && bytes[2] === 255) return 'image/jpeg';
  if (bytes.length >= 6 && new TextDecoder('ascii').decode(bytes.subarray(0, 6)).match(/^GIF8[79]a$/)) return 'image/gif';
  if (bytes.length >= 12 && new TextDecoder('ascii').decode(bytes.subarray(0, 4)) === 'RIFF' && new TextDecoder('ascii').decode(bytes.subarray(8, 12)) === 'WEBP') return 'image/webp';
  return null;
}

export async function verifiedImageUrl(value: Operation.ObjectRead): Promise<string | null> {
  if (value.object.contractKey !== 'wiki/attachment' || value.content.encoding !== 'base64') return null;
  const bytes = Uint8Array.from(atob(value.content.value), character => character.charCodeAt(0));
  if (bytes.length !== value.revision.byteLength || await hashBytes(bytes) !== value.revision.contentHash) throw new Error('Attachment bytes failed verification.');
  const mime = imageMime(bytes);
  if (!mime) return null;
  const url = URL.createObjectURL(new Blob([bytes], { type: mime }));
  try {
    const image = new Image(); image.src = url; await image.decode();
    return url;
  } catch { URL.revokeObjectURL(url); throw new Error('This file could not be decoded as an image. Its original bytes remain available for download.'); }
}
