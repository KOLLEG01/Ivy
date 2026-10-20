// Private external-library adapter. The Rust owner provides verified bytes and
// owns this process's lifetime; no Secretary backend or service RPC runs here.
import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const sourceLimit = 32 * 1024 * 1024;
const projectionLimit = 6 * 1024 * 1024;
const check = (ok, code) => { if (!ok) throw Object.assign(Error(code), { code }); };

async function extract(bytes) {
  check(bytes.subarray(0, 5).toString('ascii') === '%PDF-', 'media_signature_mismatch');
  const { getDocument } = await import('pdfjs-dist/legacy/build/pdf.mjs');
  const root = dirname(fileURLToPath(import.meta.resolve('pdfjs-dist/package.json')));
  const task = getDocument({ data: new Uint8Array(bytes), verbosity: 0, stopAtErrors: true, useWorkerFetch: false,
    disableFontFace: true, useSystemFonts: false, enableXfa: false, disableAutoFetch: true, disableRange: true, disableStream: true,
    cMapUrl: join(root, 'cmaps') + '/', cMapPacked: true, standardFontDataUrl: join(root, 'standard_fonts') + '/', wasmUrl: join(root, 'wasm') + '/' });
  task.onPassword = () => { void task.destroy(); };
  try {
    const document = await task.promise;
    check(document.numPages > 0 && document.numPages <= 128, 'media_document_too_large');
    const sections = []; let size = 0;
    for (let index = 1; index <= document.numPages; index++) {
      const page = await document.getPage(index), content = await page.getTextContent(), parts = [];
      for (const item of content.items) if ('str' in item) {
        const part = item.str + (item.hasEOL ? '\n' : ' '); size += Buffer.byteLength(part);
        check(size <= projectionLimit, 'media_document_too_large'); parts.push(part);
      }
      sections.push({ label: 'Page ' + index, text: parts.join('').trim() }); page.cleanup();
    }
    check(sections.some(section => section.text.trim()), 'media_document_no_text');
    const value = { schemaVersion: 1, format: 'pdf', representation: 'document_text', sections,
      limitations: ['visual_layout_not_interpreted', 'embedded_media_not_interpreted', 'external_resources_not_loaded',
        'pdf_annotations_and_forms_not_interpreted', ...(sections.some(section => !section.text) ? ['empty_pdf_pages'] : [])] };
    // The Rust owner performs canonical encoding and its selected native budget
    // check. Bound this private transport before structured clone or stdout.
    check(Buffer.byteLength(JSON.stringify(value)) <= 8 * 1024 * 1024, 'media_document_invalid');
    return value;
  } finally { await task.destroy(); }
}

if (!isMainThread) {
  try { parentPort.postMessage({ value: await extract(Buffer.from(workerData)) }); }
  catch (error) { parentPort.postMessage({ code: /^media_[a-z_]+$/.test(error.code) ? error.code : 'media_document_invalid' }); }
} else {
  let finished = false, worker, timer;
  const finish = value => {
    if (finished) return; finished = true; clearTimeout(timer);
    process.stdin.destroy(); void worker?.terminate(); process.stdout.end(JSON.stringify(value) + '\n');
  };
  // The owner separately enforces its total process deadline and kill/reap path.
  timer = setTimeout(() => finish({ code: 'media_document_timeout' }), 30000);
  try {
    const chunks = []; let size = 0;
    for await (const chunk of process.stdin) {
      size += chunk.length;
      if (size > sourceLimit) { finish({ code: 'media_document_too_large' }); break; }
      chunks.push(chunk);
    }
    if (!finished) {
      worker = new Worker(new URL(import.meta.url), { workerData: Buffer.concat(chunks, size),
        resourceLimits: { maxOldGenerationSizeMb: 128, maxYoungGenerationSizeMb: 16, stackSizeMb: 4 }, stdout: true, stderr: true });
      // PDF diagnostics can contain source text; drain them without retention.
      worker.stdout.resume(); worker.stderr.resume();
      worker.once('message', finish);
      worker.once('error', () => finish({ code: 'media_document_parser_failed' }));
      worker.once('exit', () => finish({ code: 'media_document_parser_failed' }));
    }
  } catch { finish({ code: 'media_document_invalid' }); }
}
