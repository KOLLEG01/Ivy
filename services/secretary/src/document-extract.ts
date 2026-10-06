import { crc32 } from 'node:zlib';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { fromBufferPromise } from 'yauzl';
import { DOMParser, onWarningStopParsing } from '@xmldom/xmldom';
import type { Node as XmlNode } from '@xmldom/xmldom';
import { parse } from 'parse5';
import type { DefaultTreeAdapterMap } from 'parse5';
import { canonical } from '../../../packages/sdk/src/node.js';
import { validateDocument } from './document-schema.js';
import type { DocumentText } from './document-schema.js';

function check(value: unknown, code = 'media_document_invalid'): asserts value {
  if (!value) throw Object.assign(Error(code), { code });
}
const expandedLimit = 16 * 1024 * 1024;
const decode = (bytes: Uint8Array): string => new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes);
function xml(bytes: Uint8Array) {
  const text = decode(bytes); check(!/<!DOCTYPE|<!ENTITY/i.test(text));
  return new DOMParser({ onError: onWarningStopParsing }).parseFromString(text, 'application/xml');
}
function wordText(bytes: Uint8Array): string {
  const document = xml(bytes), parts: string[] = [], pending: (XmlNode | string)[] = [document]; let count = 0;
  const word = new Set(['http://schemas.openxmlformats.org/wordprocessingml/2006/main', 'http://purl.oclc.org/ooxml/wordprocessingml/main']);
  while (pending.length) {
    const node = pending.pop()!; if (typeof node === 'string') { parts.push(node); continue; }
    check(++count <= 250000, 'media_document_too_large');
    if (node.nodeType === 1) {
      const element = node as ReturnType<typeof xml>['documentElement']; check(element);
      if (word.has(element.namespaceURI ?? '')) {
        if (element.localName === 't') { parts.push(element.textContent ?? ''); continue; }
        if (element.localName === 'tab') parts.push('\t');
        if (['br', 'cr'].includes(element.localName ?? '')) parts.push('\n');
        if (element.localName === 'p' || element.localName === 'tr') pending.push('\n');
        if (element.localName === 'tc') pending.push('\t');
        // Deleted tracked text and field instructions are not the displayed document text.
        if (['del', 'instrText'].includes(element.localName ?? '')) continue;
      }
    }
    for (let child = node.lastChild; child; child = child.previousSibling) pending.push(child);
  }
  return parts.join('').trim();
}
async function docx(bytes: Buffer): Promise<DocumentText> {
  const zip = await fromBufferPromise(bytes, { validateEntrySizes: true, strictFileNames: true });
  const names = new Set<string>(), retained = new Map<string, Buffer>(); let total = 0;
  check(zip.entryCount > 0 && zip.entryCount <= 4096, 'media_document_too_large');
  try {
    for await (const entry of zip.eachEntry()) {
      check(!names.has(entry.fileName) && !entry.isEncrypted()); names.add(entry.fileName);
      check(Number.isSafeInteger(entry.uncompressedSize) && entry.uncompressedSize >= 0, 'media_document_invalid');
      total += entry.uncompressedSize; check(total <= expandedLimit, 'media_document_too_large');
      const selected = entry.fileName === '[Content_Types].xml' || /^word\/(document|header\d+|footer\d+|footnotes|endnotes|comments)\.xml$/.test(entry.fileName);
      if (!selected) continue;
      check(retained.size < 129, 'media_document_too_large');
      const stream = await zip.openReadStreamPromise(entry), chunks: Buffer[] = []; let size = 0, crc = 0;
      for await (const chunk of stream) {
        const value = Buffer.from(chunk); size += value.length;
        check(size <= entry.uncompressedSize && size <= expandedLimit, 'media_document_too_large');
        chunks.push(value); crc = crc32(value, crc);
      }
      check(size === entry.uncompressedSize && crc === entry.crc32); retained.set(entry.fileName, Buffer.concat(chunks));
    }
  } finally { zip.close(); }
  const types = retained.get('[Content_Types].xml'); check(types && retained.has('word/document.xml'));
  const declared = xml(types).getElementsByTagNameNS('http://schemas.openxmlformats.org/package/2006/content-types', 'Override'); let main = false;
  for (let index = 0; index < declared.length; index++) {
    const node = declared.item(index)!;
    if (node.getAttribute('PartName') === '/word/document.xml') main = node.getAttribute('ContentType') === 'application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml';
  }
  check(main);
  const sections = [...retained].filter(([name]) => name !== '[Content_Types].xml')
    .sort(([a], [b]) => a === b ? 0 : a === 'word/document.xml' ? -1 : b === 'word/document.xml' ? 1 : a < b ? -1 : 1)
    .map(([label, value]) => ({ label, text: wordText(value) }));
  return { schemaVersion: 1, format: 'docx', representation: 'document_text', sections,
    limitations: ['visual_layout_not_interpreted', 'embedded_media_not_interpreted', 'external_resources_not_loaded'] };
}
function html(bytes: Buffer): DocumentText {
  const document = parse(decode(bytes)), parts: string[] = [], pending: (DefaultTreeAdapterMap['node'] | string)[] = [document]; let count = 0;
  const blocks = new Set(['address', 'article', 'aside', 'blockquote', 'div', 'dl', 'dt', 'dd', 'footer', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'header', 'hr', 'li', 'main', 'ol', 'p', 'pre', 'section', 'table', 'tr', 'ul']);
  while (pending.length) {
    const node = pending.pop()!; if (typeof node === 'string') { parts.push(node); continue; }
    check(++count <= 250000, 'media_document_too_large');
    if ('value' in node) { parts.push(node.value); continue; }
    if ('tagName' in node) {
      if (node.tagName === 'meta') {
        const charset = node.attrs.find(attr => attr.name === 'charset')?.value
          ?? /charset\s*=\s*([^;\s]+)/i.exec(node.attrs.find(attr => attr.name === 'content')?.value ?? '')?.[1];
        check(!charset || /^utf-?8$/i.test(charset), 'media_text_encoding_unsupported');
      }
      if (['script', 'style', 'template', 'iframe', 'object', 'embed'].includes(node.tagName)) continue;
      if (node.attrs.some(attr => attr.name === 'hidden' || attr.name === 'aria-hidden' && attr.value === 'true')) continue;
      if (node.tagName === 'br' || blocks.has(node.tagName)) parts.push('\n');
      if (blocks.has(node.tagName)) pending.push('\n');
      if (node.tagName === 'td' || node.tagName === 'th') pending.push('\t');
      if (node.tagName === 'img') { const alt = node.attrs.find(attr => attr.name === 'alt')?.value; if (alt) parts.push(' [' + alt + '] '); }
      if (node.tagName === 'a') {
        const href = node.attrs.find(attr => attr.name === 'href')?.value;
        if (href && /^(https?:|mailto:)/i.test(href)) pending.push(' (' + href + ')');
      }
    }
    if ('childNodes' in node) for (let index = node.childNodes.length - 1; index >= 0; index--) pending.push(node.childNodes[index]!);
  }
  return { schemaVersion: 1, format: 'html', representation: 'document_text', sections: [{ label: 'HTML document', text: parts.join('').trim() }],
    limitations: ['visual_layout_not_interpreted', 'embedded_media_not_interpreted', 'external_resources_not_loaded'] };
}
async function pdf(bytes: Buffer): Promise<DocumentText> {
  check(bytes.subarray(0, 5).toString('ascii') === '%PDF-', 'media_signature_mismatch');
  // PDF.js documents this entrypoint for Node; its default export targets browser primitives.
  const { getDocument } = await import('pdfjs-dist/legacy/build/pdf.mjs');
  const root = dirname(fileURLToPath(import.meta.resolve('pdfjs-dist/package.json')));
  const options = { data: new Uint8Array(bytes), verbosity: 0, stopAtErrors: true, useWorkerFetch: false,
    disableFontFace: true, useSystemFonts: false, enableXfa: false, disableAutoFetch: true, disableRange: true, disableStream: true,
    cMapUrl: join(root, 'cmaps') + '/', cMapPacked: true, standardFontDataUrl: join(root, 'standard_fonts') + '/', wasmUrl: join(root, 'wasm') + '/' };
  const task = getDocument(options);
  task.onPassword = () => { void task.destroy(); };
  try {
    const document = await task.promise; check(document.numPages > 0 && document.numPages <= 128, 'media_document_too_large');
    const sections: DocumentText['sections'] = []; let size = 0;
    for (let index = 1; index <= document.numPages; index++) {
      const page = await document.getPage(index), content = await page.getTextContent();
      const parts: string[] = [];
      for (const item of content.items) if ('str' in item) {
        const part = item.str + (item.hasEOL ? '\n' : ' '); size += Buffer.byteLength(part);
        check(size <= 6 * 1024 * 1024, 'media_document_too_large'); parts.push(part);
      }
      sections.push({ label: 'Page ' + index, text: parts.join('').trim() }); page.cleanup();
    }
    return { schemaVersion: 1, format: 'pdf', representation: 'document_text', sections,
      limitations: ['visual_layout_not_interpreted', 'embedded_media_not_interpreted', 'external_resources_not_loaded', 'pdf_annotations_and_forms_not_interpreted',
        ...(sections.some(section => !section.text) ? ['empty_pdf_pages' as const] : [])] };
  } finally { await task.destroy(); }
}
/** Called only in a disposable worker. No document-provided paths or resources are opened. */
export async function extractDocument(format: DocumentText['format'], bytes: Buffer, maximumInputBytes: number): Promise<string> {
  check(bytes.length <= 32 * 1024 * 1024, 'media_document_too_large');
  const value = format === 'pdf' ? await pdf(bytes) : format === 'docx' ? await docx(bytes) : html(bytes);
  validateDocument(value); check(value.sections.some(section => section.text.trim()), 'media_document_no_text');
  const text = canonical(value, 8 * 1024 * 1024);
  check(Buffer.byteLength(JSON.stringify({ type: 'text', text, text_elements: [] })) <= maximumInputBytes, 'media_native_input_too_large');
  return text;
}
