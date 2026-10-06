import test from 'node:test';
import assert from 'node:assert/strict';
import { digest } from '../dist/packages/sdk/src/node.js';
import { prepareAcquiredMediaInput } from '../dist/services/secretary/src/media-document.js';
import { validateDocument } from '../dist/services/secretary/src/document-schema.js';
import { docxBytes, docxType, pdfBytes, word } from './secretary-document-fixture.mjs';

const source = (bytes, mediaType) => ({ name: '../../untrusted attachment', mediaType, byteLength: bytes.length, contentHash: digest(bytes) });
const prepare = (bytes, type, maximum = 1024 * 1024) => prepareAcquiredMediaInput(source(bytes, type), bytes, maximum);
const projection = result => { assert.equal(result.kind, 'text', result.code); const value = JSON.parse(result.input.text); validateDocument(value); return value; };

test('actual PDF parser returns every page in order and explicitly identifies omitted visual and form content', { timeout: 45000 }, async () => {
  const original = pdfBytes(), result = await prepare(original, 'application/pdf'), value = projection(result);
  assert.equal(value.format, 'pdf'); assert.equal(value.representation, 'document_text');
  assert.deepEqual(value.sections.map(section => section.text), ['First original page.', 'Second original page.', 'Third original page.']);
  assert.deepEqual(value.sections.map(section => section.label), ['Page 1', 'Page 2', 'Page 3']);
  assert.ok(value.limitations.includes('pdf_annotations_and_forms_not_interpreted'));
  assert.ok(value.limitations.includes('embedded_media_not_interpreted')); assert.equal(result.source.contentHash, digest(original));
  const again = await prepare(original, 'application/pdf'); assert.deepEqual(again, result);
  again.input.text = 'changed caller copy'; assert.deepEqual(await prepare(original, 'application/pdf'), result);
});

test('PDF projection never truncates oversized output or claims that empty/scanned documents have readable text', { timeout: 90000 }, async () => {
  assert.equal((await prepare(pdfBytes(), 'application/pdf', 100)).code, 'media_native_input_too_large');
  assert.equal((await prepare(pdfBytes(['']), 'application/pdf')).code, 'media_document_no_text');
  const mixed = projection(await prepare(pdfBytes(['First', '']), 'application/pdf')); assert.equal(mixed.sections.length, 2); assert.ok(mixed.limitations.includes('empty_pdf_pages'));
  assert.equal((await prepare(Buffer.from('This is no PDF.'), 'application/pdf')).code, 'media_signature_mismatch');
});

test('DOCX extracts main text, tables and ancillary parts from exact ZIP members without following external relationships', { timeout: 45000 }, async () => {
  const main = Buffer.from('<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Visible ä</w:t></w:r><w:del><w:r><w:delText>Deleted secret</w:delText></w:r></w:del></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Cell A</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Cell B</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>');
  const bytes = docxBytes({ 'word/document.xml': main, 'word/header1.xml': word('Original header'), 'word/footnotes.xml': word('Original footnote'),
    'word/comments.xml': word('Original comment'), 'word/_rels/document.xml.rels': Buffer.from('<Relationships><Relationship Target="http://127.0.0.1:1/never-loaded" TargetMode="External"/></Relationships>') });
  const value = projection(await prepare(bytes, docxType));
  assert.deepEqual(value.sections.map(section => section.label), ['word/document.xml', 'word/comments.xml', 'word/footnotes.xml', 'word/header1.xml']);
  const mainText = value.sections[0].text; assert.match(mainText, /Visible ä/); assert.match(mainText, /Cell A[\s\S]+Cell B/); assert.doesNotMatch(mainText, /Deleted secret/);
  assert.ok(value.limitations.includes('external_resources_not_loaded'));
});

test('malformed XML, ZIP expansion and corrupt member CRC are rejected as complete-document failures', { timeout: 90000 }, async () => {
  for (const malformed of ['<!DOCTYPE document [<!ENTITY secret SYSTEM "file:///C:/never-read">]><document>&secret;</document>', '<w:document>unclosed']) {
    assert.equal((await prepare(docxBytes({ 'word/document.xml': Buffer.from(malformed) }), docxType)).kind, 'unavailable');
  }
  const expanded = docxBytes({ 'word/document.xml': word('x'.repeat(16 * 1024 * 1024)) });
  assert.equal((await prepare(expanded, docxType)).code, 'media_document_too_large');
  const corrupt = docxBytes(), signature = Buffer.from([0x50, 0x4b, 0x01, 0x02]), central = corrupt.indexOf(signature);
  assert.ok(central > 0); corrupt[central + 16] ^= 1;
  assert.equal((await prepare(corrupt, docxType)).code, 'media_document_invalid');
});

test('HTML text projection is inert, preserves meaningful text and declares resource/layout limits', { timeout: 45000 }, async () => {
  const bytes = Buffer.from('<!doctype html><meta charset="UTF-8"><h1>Original 🌍</h1><p>Agenda &amp; context</p><script>fetch("https://never.invalid")</script><img src="https://never.invalid/image" alt="Chart alternative"><a href="https://example.test/context">Source</a><p hidden>Hidden</p>');
  const value = projection(await prepare(bytes, 'text/html; charset=UTF-8')), text = value.sections[0].text;
  assert.match(text, /Original 🌍/); assert.match(text, /Agenda & context/); assert.match(text, /Chart alternative/); assert.match(text, /https:\/\/example.test\/context/);
  assert.doesNotMatch(text, /fetch|never.invalid|Hidden/); assert.ok(value.limitations.includes('visual_layout_not_interpreted'));
  assert.equal((await prepare(Buffer.from('<meta charset="windows-1252"><p>text</p>'), 'text/html')).code, 'media_text_encoding_unsupported');
  assert.equal((await prepare(bytes, 'text/html', 16)).code, 'media_native_input_too_large');
});

test('document parsing verifies source bytes before workers and validates its explicit projection contract', async () => {
  const bytes = pdfBytes();
  await assert.rejects(prepareAcquiredMediaInput({ ...source(bytes, 'application/pdf'), contentHash: digest('other') }, bytes, 1024), { code: 'media_input_integrity_invalid' });
  await assert.rejects(prepareAcquiredMediaInput(source(bytes, 'application/pdf'), bytes, 0), { code: 'media_input_limit_invalid' });
  const valid = { schemaVersion: 1, format: 'html', representation: 'document_text', sections: [{ label: 'body', text: 'Original' }], limitations: ['visual_layout_not_interpreted'] };
  validateDocument(valid);
  for (const invalid of [{ ...valid, limitations: [] }, { ...valid, representation: 'complete_original' }, { ...valid, sections: [] }, { ...valid, url: 'https://outside.invalid' }]) assert.throws(() => validateDocument(invalid));
});
