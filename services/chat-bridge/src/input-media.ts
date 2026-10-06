import { digest } from '../../../packages/sdk/src/node.js';
import { requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import type { RpcClient } from '../../../packages/sdk/src/client.js';
import { ChatAdmission } from './admission.js';

const contracts: Record<Chat.Image['mediaType'], string> = {
  'image/png': 'chat-bridge/image-png', 'image/jpeg': 'chat-bridge/image-jpeg', 'image/webp': 'chat-bridge/image-webp',
};
/** Container/signature admission only. Native decoding can still reject a corrupt image explicitly. */
function header(bytes: Buffer, type: Chat.Image['mediaType']): boolean {
  if (type === 'image/png') return bytes.length >= 8 && bytes.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
  if (type === 'image/jpeg') return bytes.length >= 3 && bytes[0] === 255 && bytes[1] === 216 && bytes[2] === 255;
  if (bytes.length < 20 || bytes.toString('latin1', 0, 4) !== 'RIFF' || bytes.toString('latin1', 8, 12) !== 'WEBP' || bytes.readUInt32LE(4) + 8 !== bytes.length) return false;
  if (!['VP8 ', 'VP8L', 'VP8X'].includes(bytes.toString('latin1', 12, 16))) return false;
  let offset = 12;
  while (offset < bytes.length) {
    if (bytes.length - offset < 8) return false;
    const size = bytes.readUInt32LE(offset + 4); offset += 8 + size + (size % 2);
    if (offset > bytes.length) return false;
  }
  return offset === bytes.length;
}

/** Original revision reads only; never reads files, URLs, latest-image replacements or native state. */
export async function admittedNativeInput(admission: ChatAdmission, client: RpcClient, callerPrincipalId: string, request: Chat.SendRequest): Promise<{ identity: Chat.InputIdentity; requestHash: string; input: Wire.Json[] }> {
  // Freeze caller input before the first await so later mutations cannot substitute a different pin.
  const original = structuredClone(request), identity = admission.send(callerPrincipalId, original);
  const input: Wire.Json[] = original.payload.text.length ? [{ type: 'text', text: original.payload.text }] : [];
  for (const image of original.payload.images) {
    const value = await client.request('objects.read', { objectId: image.object.objectId, revision: image.object.revision });
    requireThat(value.object.id === image.object.objectId && value.revision.revision === image.object.revision &&
      value.object.contractKey === contracts[image.mediaType] && value.revision.contractVersion === '1.0.0' && value.revision.mediaType === image.mediaType && value.content.encoding === 'base64',
      'chat_image_mismatch', 'The selected immutable image revision does not have its exact supported media contract.');
    const bytes = Buffer.from(value.content.value, 'base64');
    requireThat(bytes.toString('base64') === value.content.value && bytes.length === image.byteLength && bytes.length === value.revision.byteLength &&
      digest(bytes) === image.contentHash && image.contentHash === value.revision.contentHash,
      'chat_image_mismatch', 'The image bytes differ from their declared immutable hash or length.');
    requireThat(header(bytes, image.mediaType), 'chat_image_invalid', 'The image signature or complete container header is invalid.');
    input.push({ type: 'image', url: 'data:' + image.mediaType + ';base64,' + bytes.toString('base64') });
  }
  return { ...identity, input };
}
