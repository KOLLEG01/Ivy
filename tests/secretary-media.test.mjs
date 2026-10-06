import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { digest, hashJson } from '../dist/packages/sdk/src/node.js';
import { outlookAttachments, outlookMaterialization, downloadOutlookMedia } from '../dist/services/secretary/src/outlook-media.js';
import { validateMedia } from '../dist/services/secretary/src/media-schema.js';

const raw = { id: 'attachment', attachment_type: 'fileAttachment', odata_type: '#microsoft.graph.fileAttachment', name: 'inert.html', content_type: 'text/html',
  size_bytes: 253, is_inline: false, content_id: null, last_modified_date_time: '2026-09-08T00:00:00Z', payload_fetch_supported: true, payload_status: 'not_requested', payload_error: null, file_uri: null };
const metadata = (attachment = raw) => ({ message_id: 'message', attachments: [attachment], next_link: null, errors: [] });
const selected = outlookAttachments(metadata(), 'message')[0];
const response = { message_id: 'message', attachment_id: 'attachment', attachment_type: 'fileAttachment', filename: 'inert.html', size_bytes: 253,
  mime_type: 'text/html', is_inline: false, content_id: null, payload_status: 'materialized',
  file_uri: { file_id: 'original-file', file_name: 'inert.html', mime_type: 'text/html', download_url: 'https://fixture.oaiusercontent.com/files/original?sig=private-capability' } };
const materialization = outlookMaterialization(response, selected);
const failure = expected => error => error.code === expected;

test('Outlook media admits a complete exact metadata page and refuses changed, duplicate or incomplete identity', () => {
  validateMedia('MediaAttachment', selected); assert.equal(selected.providerSizeBytes, 253);
  for (const changed of [{ message_id: 'other' }, { next_link: 'https://graph.microsoft.com/next' }, { errors: ['provider failure'] }, { attachments: [raw, raw] }, { attachments: Array(33).fill(raw) }])
    assert.throws(() => outlookAttachments({ ...metadata(), ...changed }, 'message'));
  for (const changed of [{ file_uri: response.file_uri }, { payload_error: 'failure' }, { size_bytes: -1 }, { is_inline: null }, { name: 'bad\u0000name' }, { last_modified_date_time: 'not a date' }, { last_modified_date_time: '2026-02-30T00:00:00Z' }])
    assert.throws(() => outlookAttachments(metadata({ ...raw, ...changed }), 'message'));
  assert.deepEqual(outlookAttachments({ ...metadata(), attachments: [] }, 'message'), []);
});

test('Outlook materialization retains source identity and refuses unsupported or foreign payloads and unsafe URLs', () => {
  assert.equal(materialization.nativeResponseHash, hashJson(response));
  for (const changed of [{ message_id: 'other' }, { attachment_id: 'other' }, { filename: 'other' }, { size_bytes: 1 }, { mime_type: 'image/png' }, { content_id: 'different' }, { is_inline: true }, { payload_status: 'failed' }])
    assert.throws(() => outlookMaterialization({ ...response, ...changed }, selected), failure('media_materialization_mismatch'));
  for (const url of ['http://fixture.oaiusercontent.com/a', 'https://oaiusercontent.com.evil.test/a', 'https://a@fixture.oaiusercontent.com/a', 'https://fixture.oaiusercontent.com:444/a', 'https://fixture.oaiusercontent.com/a#fragment', 'file:///tmp/media'])
    assert.throws(() => outlookMaterialization({ ...response, file_uri: { ...response.file_uri, download_url: url } }, selected), failure('media_download_url_invalid'));
  assert.throws(() => outlookMaterialization(response, { ...selected, attachmentType: 'referenceAttachment' }), failure('media_attachment_unsupported'));
  assert.throws(() => outlookMaterialization(response, { ...selected, payloadFetchSupported: false }), failure('media_attachment_unsupported'));
});

test('complete inert bytes use HTTP length/digest, retain different provider size, and exclude the signed URL from evidence', async () => {
  const bytes = Buffer.from('<script>never execute()</script>'); let calls = 0;
  const acquired = await downloadOutlookMedia(materialization, new AbortController().signal, async (url, options) => {
    calls++; assert.equal(url.href, response.file_uri.download_url); assert.equal(options.redirect, 'error'); assert.equal(options.credentials, 'omit');
    assert.deepEqual(options.headers, { 'Accept-Encoding': 'identity' });
    return new Response(bytes, { headers: { 'Content-Length': String(bytes.length), 'Content-MD5': createHash('md5').update(bytes).digest('base64'), 'Content-Type': 'text/html' } });
  });
  assert.equal(calls, 1); assert.deepEqual(acquired.bytes, bytes); validateMedia('MediaDownloadEvidence', acquired.evidence);
  assert.equal(acquired.evidence.contentHash, digest(bytes)); assert.equal(acquired.evidence.byteLength, bytes.length);
  assert.equal(acquired.evidence.attachment.providerSizeBytes, 253); assert.equal(acquired.evidence.providerSizeRelation, 'different');
  assert.equal(acquired.evidence.materializationHash, hashJson(materialization));
  assert.ok(!JSON.stringify(acquired.evidence).includes('private-capability')); assert.ok(!JSON.stringify(acquired.evidence).includes('https://'));
});

test('expired, truncated, oversized, encoded or changed-digest bodies fail without obtaining a substitute URL', async () => {
  const attempts = [
    ['media_materialization_expired_or_refused', () => new Response('expired', { status: 403 })],
    ['media_http_failed', () => new Response('redirect', { status: 302, headers: { location: 'https://outside.invalid/' } })],
    ['media_download_incomplete', () => new Response('ab', { headers: { 'Content-Length': '3' } })],
    ['media_download_limit', () => new Response('abcd', { headers: { 'Content-Length': '3' } })],
    ['media_download_limit', () => new Response('', { headers: { 'Content-Length': '33554433' } })],
    ['media_http_length_invalid', () => new Response('', { headers: { 'Content-Length': '1.5' } })],
    ['media_encoding_unsupported', () => new Response('abc', { headers: { 'Content-Encoding': 'gzip' } })],
    ['media_http_digest_mismatch', () => new Response('abc', { headers: { 'Content-MD5': createHash('md5').update('other').digest('base64') } })],
  ];
  for (const [code, reply] of attempts) { let calls = 0; await assert.rejects(downloadOutlookMedia(materialization, new AbortController().signal, async () => { calls++; return reply(); }), failure(code)); assert.equal(calls, 1); }
  let emitted = 0;
  await assert.rejects(downloadOutlookMedia(materialization, new AbortController().signal, async () => new Response(new ReadableStream({ pull(controller) {
    if (emitted++ <= 32) controller.enqueue(new Uint8Array(1024 * 1024)); else controller.close();
  } }))), failure('media_download_limit'));
});

test('cancelled reads return no partial payload and empty valid files retain their exact digest', async () => {
  const controller = new AbortController(); controller.abort(); let calls = 0;
  await assert.rejects(downloadOutlookMedia(materialization, controller.signal, async () => { calls++; return new Response('not fetched'); }), failure('media_download_cancelled')); assert.equal(calls, 0);
  const zero = { ...materialization, attachment: { ...materialization.attachment, providerSizeBytes: 0 } };
  const empty = await downloadOutlookMedia(zero, new AbortController().signal, async () => new Response(new Uint8Array(), { headers: { 'Content-Length': '0' } }));
  assert.equal(empty.bytes.length, 0); assert.equal(empty.evidence.contentHash, digest(Buffer.alloc(0))); assert.equal(empty.evidence.providerSizeRelation, 'equal');
});

test('media acquisition rejects capability injection and invalid dates while retaining original timestamp precision', async () => {
  const acquired = await downloadOutlookMedia(materialization, new AbortController().signal, async () => new Response('complete'));
  assert.throws(() => validateMedia('MediaDownloadEvidence', { ...acquired.evidence, downloadUrl: materialization.downloadUrl }));
  assert.throws(() => validateMedia('MediaDownloadEvidence', { ...acquired.evidence, observedAt: '2026-02-30T00:00:00.000Z' }));
  assert.throws(() => validateMedia('MediaMaterialization', { ...materialization, downloadUrl: 'x'.repeat(16385) }));
  const exact = '2026-09-08T00:00:00.1234567Z';
  assert.equal(outlookAttachments(metadata({ ...raw, last_modified_date_time: exact }), 'message')[0].modifiedAt, exact);
});
