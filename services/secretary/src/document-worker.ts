import { parentPort, workerData } from 'node:worker_threads';
import { extractDocument } from './document-extract.js';
import { documentFormats } from './document-schema.js';
const { mediaType, bytes, maximumInputBytes } = workerData;
try {
  const format = documentFormats.get(mediaType);
  if (!format || !(bytes instanceof Uint8Array) || !Number.isInteger(maximumInputBytes) || maximumInputBytes < 1 || maximumInputBytes > 6 * 1024 * 1024) throw Error('invalid request');
  parentPort!.postMessage({ text: await extractDocument(format, Buffer.from(bytes), maximumInputBytes) });
} catch (error) {
  const code = (error as { code?: string }).code;
  parentPort!.postMessage({ code: typeof code === 'string' && /^media_[a-z_]+$/.test(code) ? code : 'media_document_invalid' });
}
