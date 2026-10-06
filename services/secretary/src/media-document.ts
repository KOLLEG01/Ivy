import { Worker } from 'node:worker_threads';
import { digest } from '../../../packages/sdk/src/node.js';
import { documentFormats, validateDocument } from './document-schema.js';
import { need } from './schema.js';
import { prepareMediaInput } from './media-input.js';
import type { MediaInputSource, PreparedMediaInput } from './media-input.js';
import { videoFormats } from './video-schema.js';
import type { VideoPreparer } from './media-video.js';
const cache = new Map<string, { text: string; bytes: number }>(); let cacheBytes = 0;
const cacheLimit = 16 * 1024 * 1024;

/** All source integrity gates precede parsing. The worker cannot block the Secretary event loop. */
export type AcquiredMediaPreparer = (source: MediaInputSource, bytes: Buffer, maximumInputBytes: number) => Promise<PreparedMediaInput>;
export async function prepareAcquiredMediaInput(source: MediaInputSource, bytes: Buffer, maximumInputBytes: number, video?: VideoPreparer | null): Promise<PreparedMediaInput> {
  const mediaType = source.mediaType?.split(';', 1)[0]?.trim().toLowerCase() ?? '';
  if (videoFormats.has(mediaType)) {
    if (video) return video.prepare(source, bytes, maximumInputBytes);
    prepareMediaInput(source, bytes, maximumInputBytes);
    return { kind: 'unavailable', source: structuredClone(source), code: 'media_video_decoder_unconfigured' };
  }
  if (!documentFormats.has(mediaType)) return prepareMediaInput(source, bytes, maximumInputBytes);
  need(Number.isInteger(maximumInputBytes) && maximumInputBytes > 0 && maximumInputBytes <= 6 * 1024 * 1024,
    'media_input_limit_invalid', 'Media input needs its bounded selected native request budget.');
  need(bytes.length === source.byteLength && digest(bytes) === source.contentHash,
    'media_input_integrity_invalid', 'Semantic input must retain the complete verified original attachment bytes.');
  const original = structuredClone(source), unavailable = (code: string): PreparedMediaInput => ({ kind: 'unavailable', source: original, code });
  if (bytes.length > 32 * 1024 * 1024) return unavailable('media_document_too_large');
  const cacheKey = source.contentHash + '\0' + mediaType + '\0' + maximumInputBytes, cached = cache.get(cacheKey);
  if (cached) { cache.delete(cacheKey); cache.set(cacheKey, cached); return { kind: 'text', source: original, input: { type: 'text', text: cached.text, text_elements: [] } }; }
  return new Promise(resolve => {
    const worker = new Worker(new URL('./document-worker.js', import.meta.url), { workerData: { mediaType, bytes, maximumInputBytes },
      resourceLimits: { maxOldGenerationSizeMb: 128, maxYoungGenerationSizeMb: 16, stackSizeMb: 4 }, stdout: true, stderr: true });
    // Parser diagnostics can include document text. Drain them without forwarding or retaining them.
    worker.stdout.resume(); worker.stderr.resume(); let done = false;
    const finish = (value: PreparedMediaInput) => {
      if (done) return; done = true; clearTimeout(timer); void worker.terminate();
      if (value.kind === 'text') {
        const text = value.input['text'] as string, size = Buffer.byteLength(text), old = cache.get(cacheKey);
        if (old) { cacheBytes -= old.bytes; cache.delete(cacheKey); }
        while (cache.size && (cacheBytes + size > cacheLimit || cache.size >= 16)) {
          const key = cache.keys().next().value!; cacheBytes -= cache.get(key)!.bytes; cache.delete(key);
        }
        cache.set(cacheKey, { text, bytes: size }); cacheBytes += size;
      }
      resolve(value);
    };
    const timer = setTimeout(() => finish(unavailable('media_document_timeout')), 30000);
    worker.once('error', () => finish(unavailable('media_document_parser_failed')));
    worker.once('exit', () => finish(unavailable('media_document_parser_failed')));
    worker.once('message', (value: unknown) => {
      try {
        need(value && typeof value === 'object', 'media_document_invalid', 'The document worker needs a complete projection.');
        const reply = value as { code?: unknown; text?: unknown };
        if (typeof reply.code === 'string' && /^media_[a-z_]+$/.test(reply.code)) { finish(unavailable(reply.code)); return; }
        need(typeof reply.text === 'string' && Buffer.byteLength(reply.text) <= 6 * 1024 * 1024, 'media_document_invalid', 'The document projection is bounded.');
        const document = JSON.parse(reply.text as string); validateDocument(document);
        need(document.format === documentFormats.get(mediaType), 'media_document_invalid', 'The projection must keep its original document format.');
        const input = { type: 'text', text: reply.text as string, text_elements: [] };
        finish(Buffer.byteLength(JSON.stringify(input)) > maximumInputBytes ? unavailable('media_native_input_too_large') : { kind: 'text', source: original, input });
      } catch { finish(unavailable('media_document_invalid')); }
    });
  });
}
