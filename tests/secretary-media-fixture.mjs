import { mkdtemp, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { hashJson } from '../dist/packages/sdk/src/node.js';
import { MediaSpool } from '../dist/services/secretary/src/media-spool.js';
export const materialization = {
  attachment: { messageId: 'message', attachmentId: 'attachment', attachmentType: 'fileAttachment', name: '../../inert.html', mediaType: 'text/html',
    providerSizeBytes: 15, inline: false, contentId: null, modifiedAt: null, payloadFetchSupported: true },
  fileId: 'original-file', downloadUrl: 'https://fixture.oaiusercontent.com/original?sig=private-capability', nativeResponseHash: hashJson('native-response'),
};
export const admission = { sourceId: 'mail', producerPrincipalId: 'producer', accountId: 'account', providerMessageId: 'message',
  itemObjectId: '00000000-0000-4000-8000-000000000003',
  identityHash: hashJson('identity'), messageHash: hashJson('message') };
export const intent = { operationId: 'original-acquisition', captureOperationId: 'original-capture', item: { objectId: admission.itemObjectId, revision: 1 }, admission, materializationHash: hashJson(materialization) };
export const owner = { principalId: 'producer', rootObjectId: 'secretary-root' };
export async function spoolFixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'ivy-media-')); let spool = await MediaSpool.open(root, owner);
  t.after(async () => { spool?.close(); await rm(root, { recursive: true, force: true }); });
  return { root, get spool() { return spool; }, close() { spool.close(); spool = null; },
    async reopen() { spool?.close(); spool = await MediaSpool.open(root, owner); return spool; } };
}
