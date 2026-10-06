import assert from 'node:assert/strict';
import { hashJson } from '../dist/packages/sdk/src/node.js';
import { OutlookMediaNative } from '../dist/services/secretary/src/outlook-media-native.js';
import { mediaRegistry } from '../dist/services/secretary/src/media-store.js';
import { outlookFixture, mail, page, sourceRegistry } from './secretary-outlook-fixture.mjs';
import { spoolFixture } from './secretary-media-fixture.mjs';
export async function mediaNativeFixture(t, options = {}) {
  const n = await outlookFixture(t, () => { const source = sourceRegistry(), media = mediaRegistry();
    const extra = options.registry ?? { contracts: [], requiredContracts: [] };
    return { ...source, contracts: [...source.contracts, ...media.contracts, ...extra.contracts], requiredContracts: [...source.requiredContracts, ...media.requiredContracts, ...extra.requiredContracts] }; });
  n.state.pages.set(0, page([mail('message', { has_attachments: true, ...options.message })])); assert.equal(await n.settle(), 'complete');
  const head = await n.source.head(), proof = await n.source.evidence(head.value.lastPage);
  const publication = await n.source.store.find('secretary/outlook-publication', 'outlook-publication-' + hashJson(proof.pin).slice(7));
  const capture = await n.f.engine.store.read('secretary/operation', publication.value.captured[0]), item = await n.f.engine.item(capture.value.effect);
  const admission = { sourceId: 'mail', producerPrincipalId: n.f.settings.identity.principalId, accountId: n.configuration.profile.id, providerMessageId: 'message',
    itemObjectId: item.pin.objectId, identityHash: item.value.identityHash, messageHash: item.value.messageHash };
  const name = options.name ?? 'inert.html', mediaType = options.mediaType ?? 'text/html';
  const attachment = { id: 'file', attachment_type: 'fileAttachment', odata_type: '#microsoft.graph.fileAttachment', name, content_type: mediaType,
    size_bytes: 258, is_inline: false, content_id: null, last_modified_date_time: null, payload_fetch_supported: true, payload_status: 'not_requested', payload_error: null, file_uri: null };
  n.state.media = { metadata: { message_id: 'message', attachments: [attachment], errors: [], next_link: null },
    payload: { message_id: 'message', attachment_id: 'file', attachment_type: 'fileAttachment', filename: name, size_bytes: 258, mime_type: mediaType,
      is_inline: false, content_id: null, payload_status: 'materialized', file_uri: { file_id: 'original', file_name: name, mime_type: mediaType,
        download_url: 'https://fixture.oaiusercontent.com/file?sig=protected-original' } } };
  const s = await spoolFixture(t); let gets = 0; const bytes = options.bytes ?? Buffer.from('<b>inert</b>');
  const worker = () => new OutlookMediaNative(n.source, s.spool, async () => { gets++; return new Response(bytes); });
  const plan = await worker().prepare(admission, item.pin, capture.value.operationId);
  const settle = async () => { for (let i = 0; i < 40; i++) { const result = await worker().step(plan); if (result.phase === 'complete') return result.coverage; } throw Error('Media did not finish within bounded steps.'); };
  return { n, s, admission, attachment, plan, worker, settle, bytes, get gets() { return gets; } };
}
