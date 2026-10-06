import { createHash } from 'node:crypto';
import { canonical, digest, hashJson, IvyError } from '../../../packages/sdk/src/node.js';
import { need } from './schema.js';
import { validateMedia } from './media-schema.js';
import type { MediaAttachment, MediaDownloadEvidence, MediaMaterialization } from './media-schema.js';

const maximumBytes = 32 * 1024 * 1024;
const check = (condition: unknown, code = 'media_response_invalid'): void => need(condition, code, 'Media acquisition requires complete original account-bound attachment evidence.');
function record(value: unknown): Record<string, unknown> { need(value && typeof value === 'object' && !Array.isArray(value), 'media_response_invalid', 'Media evidence must be an object.'); return value as Record<string, unknown>; }
function text(value: unknown, maximum: number): string {
  need(typeof value === 'string' && value.length > 0 && value.length <= maximum && !/[\u0000-\u001f\u007f\ud800-\udfff\ufffe\uffff]/u.test(value), 'media_response_invalid', 'Media metadata must be bounded lossless text.'); return value;
}
const nullableText = (value: unknown, maximum: number) => value === null ? null : text(value, maximum);
function integer(value: unknown): number { need(Number.isSafeInteger(value) && Number(value) >= 0 && Number(value) <= 2147483647, 'media_response_invalid', 'Attachment size must be a nonnegative provider integer.'); return Number(value); }

/** Takes only structuredContent from the already verified native metadata-only call. */
export function outlookAttachments(value: unknown, messageId: string): MediaAttachment[] {
  text(messageId, 4096); canonical(value, 1024 * 1024); const page = record(value);
  check(page['message_id'] === messageId && page['next_link'] === null && Array.isArray(page['errors']) && page['errors'].length === 0, 'media_metadata_incomplete');
  const entries = page['attachments']; need(Array.isArray(entries) && entries.length <= 32, 'media_metadata_incomplete', 'Attachment metadata needs a complete page of at most 32 entries.');
  const seen = new Set<string>();
  return entries.map(entry => {
    const raw = record(entry), type = raw['attachment_type'];
    check(['fileAttachment', 'itemAttachment', 'referenceAttachment', 'unknown'].includes(String(type)) &&
      (type === 'unknown' || raw['odata_type'] === '#microsoft.graph.' + String(type)));
    check(typeof raw['is_inline'] === 'boolean' && typeof raw['payload_fetch_supported'] === 'boolean' && raw['payload_status'] === 'not_requested' && raw['payload_error'] === null && raw['file_uri'] === null);
    const attachment: MediaAttachment = { messageId, attachmentId: text(raw['id'], 4096), attachmentType: type as MediaAttachment['attachmentType'],
      name: text(raw['name'], 1024), mediaType: nullableText(raw['content_type'], 256), providerSizeBytes: integer(raw['size_bytes']), inline: raw['is_inline'] as boolean,
      contentId: nullableText(raw['content_id'], 4096), modifiedAt: nullableText(raw['last_modified_date_time'], 40), payloadFetchSupported: raw['payload_fetch_supported'] as boolean };
    check(!seen.has(attachment.attachmentId), 'media_duplicate_attachment'); seen.add(attachment.attachmentId); validateMedia('MediaAttachment', attachment); return attachment;
  });
}

function materializationUrl(value: string): URL {
  let url: URL; try { url = new URL(value); } catch { throw new IvyError('media_download_url_invalid', 'The native materialization URL is invalid.'); }
  check(url.protocol === 'https:' && url.hostname.endsWith('.oaiusercontent.com') && url.hostname.length > '.oaiusercontent.com'.length &&
    !url.username && !url.password && !url.port && !url.hash, 'media_download_url_invalid'); return url;
}
/** Signed addresses are intentionally absent from every returned public evidence field. */
export function outlookMaterialization(value: unknown, attachment: MediaAttachment): MediaMaterialization {
  validateMedia('MediaAttachment', attachment); canonical(value, 1024 * 1024); const raw = record(value);
  check(attachment.attachmentType === 'fileAttachment' && attachment.payloadFetchSupported, 'media_attachment_unsupported');
  check(raw['payload_status'] === 'materialized' && raw['message_id'] === attachment.messageId && raw['attachment_id'] === attachment.attachmentId &&
    raw['attachment_type'] === attachment.attachmentType && raw['filename'] === attachment.name && raw['size_bytes'] === attachment.providerSizeBytes &&
    raw['mime_type'] === attachment.mediaType && raw['is_inline'] === attachment.inline && raw['content_id'] === attachment.contentId, 'media_materialization_mismatch');
  const uri = record(raw['file_uri']); check(uri['file_name'] === attachment.name && uri['mime_type'] === attachment.mediaType, 'media_materialization_mismatch');
  const result: MediaMaterialization = { attachment: structuredClone(attachment), fileId: text(uri['file_id'], 1024), downloadUrl: text(uri['download_url'], 16384), nativeResponseHash: hashJson(value) };
  materializationUrl(result.downloadUrl); validateMedia('MediaMaterialization', result); return result;
}

/** Read-only acquisition: caller must durably stage complete bytes/evidence before publication. */
export async function downloadOutlookMedia(materialization: MediaMaterialization, signal: AbortSignal, request: typeof fetch = fetch): Promise<{ bytes: Buffer; evidence: MediaDownloadEvidence }> {
  validateMedia('MediaMaterialization', materialization); const original = structuredClone(materialization), url = materializationUrl(original.downloadUrl);
  check(original.attachment.attachmentType === 'fileAttachment' && original.attachment.payloadFetchSupported, 'media_attachment_unsupported');
  check(original.attachment.providerSizeBytes <= maximumBytes, 'media_download_limit');
  const controller = new AbortController(), combined = AbortSignal.any([signal, controller.signal]), startedAt = new Date().toISOString();
  const timer = setTimeout(() => controller.abort(), 30000);
  let response: Response | undefined, reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
  try {
    combined.throwIfAborted(); response = await request(url, { method: 'GET', redirect: 'error', credentials: 'omit', headers: { 'Accept-Encoding': 'identity' }, signal: combined });
    check(response.status === 200 && !response.redirected, response.status === 403 ? 'media_materialization_expired_or_refused' : 'media_http_failed');
    need(response.body, 'media_download_incomplete', 'The materialization must contain a complete binary body.');
    const contentEncoding = response.headers.get('content-encoding'), rawLength = response.headers.get('content-length');
    check(contentEncoding === null || contentEncoding.toLowerCase() === 'identity', 'media_encoding_unsupported');
    check(rawLength === null || /^(?:0|[1-9][0-9]*)$/.test(rawLength), 'media_http_length_invalid');
    const contentLength = rawLength === null ? null : Number(rawLength);
    check(contentLength === null || Number.isSafeInteger(contentLength) && contentLength <= maximumBytes, 'media_download_limit');
    const chunks: Buffer[] = []; let byteLength = 0; reader = response.body.getReader();
    while (true) {
      combined.throwIfAborted(); const result = await reader.read(); if (result.done) break;
      byteLength += result.value.byteLength; check(byteLength <= maximumBytes && (contentLength === null || byteLength <= contentLength), 'media_download_limit');
      chunks.push(Buffer.from(result.value));
    }
    check(contentLength === null || contentLength === byteLength, 'media_download_incomplete');
    const bytes = Buffer.concat(chunks, byteLength), contentMd5 = response.headers.get('content-md5');
    check(contentMd5 === null || /^[A-Za-z0-9+/]{22}==$/.test(contentMd5) && createHash('md5').update(bytes).digest('base64') === contentMd5, 'media_http_digest_mismatch');
    const evidence: MediaDownloadEvidence = { schemaVersion: 1, attachment: original.attachment, fileId: original.fileId, materializationHash: hashJson(original), downloadUrlHash: digest(original.downloadUrl),
      startedAt, observedAt: new Date().toISOString(), byteLength, contentHash: digest(bytes), providerSizeRelation: original.attachment.providerSizeBytes === byteLength ? 'equal' : 'different',
      http: { status: 200, contentLength, contentEncoding, contentType: response.headers.get('content-type'), contentMd5 } };
    validateMedia('MediaDownloadEvidence', evidence); return { bytes, evidence };
  } catch (error) {
    if (error instanceof IvyError) throw error;
    throw new IvyError(signal.aborted ? 'media_download_cancelled' : controller.signal.aborted ? 'media_download_deadline' : 'media_download_incomplete', 'The original media download did not complete; no payload was published.');
  } finally { clearTimeout(timer); if (reader) await reader.cancel().catch(() => undefined); else await response?.body?.cancel().catch(() => undefined); }
}
