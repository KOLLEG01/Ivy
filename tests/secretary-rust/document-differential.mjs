// Usage: node tests/secretary-rust/document-differential.mjs <Rust adapter executable>
// Uses the unchanged built TypeScript extractor as the oracle. No native accounts.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';
const { extractDocument } = await import(pathToFileURL(resolve(process.env.IVY_SECRETARY_ORACLE ?? 'dist', 'services/secretary/src/document-extract.js')).href);
import { docxBytes, word, pdfBytes } from '../secretary-document-fixture.mjs';

const executable = process.argv[2];
assert.ok(executable, 'Pass the test-only Rust document adapter executable.');
const pdfWorker = process.argv.includes('--pdf-worker');
function pdfStream(stream) {
  const objects = [...pdfBytes(['']).toString().matchAll(/\d+ 0 obj\n([\s\S]*?)\nendobj\n/g)].map(m => m[1]);
  objects[4] = `<< /Length ${Buffer.byteLength(stream)} >>\nstream\n${stream}endstream`;
  let content = '%PDF-1.4\n'; const offsets = [0];
  objects.forEach((object, i) => { offsets.push(Buffer.byteLength(content)); content += `${i + 1} 0 obj\n${object}\nendobj\n`; });
  const start = Buffer.byteLength(content);
  content += `xref\n0 ${offsets.length}\n0000000000 65535 f \n` + offsets.slice(1).map(n => String(n).padStart(10, '0') + ' 00000 n \n').join('')
    + `trailer\n<< /Size ${offsets.length} /Root 1 0 R >>\nstartxref\n${start}\n%%EOF\n`;
  return Buffer.from(content);
}
const cases = [
  ['pdf-pages', 'pdf', pdfBytes()],
  ['pdf-empty', 'pdf', pdfBytes([''])],
  ['pdf-empty-page', 'pdf', pdfBytes(['First', ''])],
  ['pdf-signature', 'pdf', Buffer.from('not PDF')],
  ['pdf-text-runs', 'pdf', pdfStream('BT /F1 12 Tf 20 100 Td (Hello) Tj (world) Tj ET\n')],
  ['pdf-positioned-text', 'pdf', pdfStream('BT /F1 12 Tf 20 100 Td (Left) Tj 100 0 Td (Right) Tj 0 -20 Td (Next line) Tj ET\n')],
  ['pdf-spaced-array', 'pdf', pdfStream('BT /F1 12 Tf 20 100 Td [(Hello) -700 (world)] TJ ET\n')],
  ['pdf-standard-encoding', 'pdf', pdfStream('BT /F1 12 Tf 20 100 Td (\\200\\226\\227\\223\\224) Tj ET\n')],
  ['docx-default', 'docx', docxBytes()],
  ['docx-ancillary', 'docx', docxBytes({ 'word/header1.xml': word('Header'), 'word/comments.xml': word('Comment') })],
  ['docx-namespace', 'docx', docxBytes({ 'word/document.xml': Buffer.from('<s:document xmlns:s="http://purl.oclc.org/ooxml/wordprocessingml/main"><s:p><s:t>Strict namespace</s:t><s:tab/><s:t>second</s:t><s:del><s:t>hidden</s:t></s:del></s:p></s:document>') })],
  ['docx-malformed', 'docx', docxBytes({ 'word/document.xml': Buffer.from('<w:document>unclosed') })],
  ['docx-dtd', 'docx', docxBytes({ 'word/document.xml': Buffer.from('<!DOCTYPE doc><doc/>') })],
  ['html-projection', 'html', Buffer.from('<h1>Original 🌍</h1><p>Agenda &amp; context</p><script>fetch("never")</script><img alt="Chart alternative"><a href="https://example.test">Source</a><p hidden>Hidden</p>')],
  ['html-malformed', 'html', Buffer.from('<table><p>fostered<td>Cell A<td>Cell B</table><b>unclosed<i>italic &amp;')],
  ['html-foreign', 'html', Buffer.from('<svg><title>Title</title><text>Shape text</text></svg><template>Hidden</template><p aria-hidden=true>Hidden too</p>')],
  ['html-encoding', 'html', Buffer.from('<meta charset=windows-1252><p>text</p>')],
  ['html-bom', 'html', Buffer.from('\ufeff<p>visible\u0085</p>')],
  ['html-invalid-utf8', 'html', Buffer.from([255])],
];
const results = [];
for (const [name, format, bytes] of cases) {
  if (pdfWorker && format !== 'pdf') continue;
  let oracle;
  try { oracle = { value: JSON.parse(await extractDocument(format, bytes, 6 * 1024 * 1024)) }; }
  catch (error) { oracle = { code: /^media_[a-z_]+$/.test(error.code) ? error.code : 'media_document_invalid' }; }
  const run = pdfWorker
    ? spawnSync(process.execPath, [executable], { input: bytes, windowsHide: true, timeout: 35000, maxBuffer: 8 * 1024 * 1024 })
    : spawnSync(executable, [format], { input: bytes, windowsHide: true, timeout: 30000, maxBuffer: 8 * 1024 * 1024 });
  assert.equal(run.status, 0, `${name}: ${run.error ?? run.stderr}`);
  const actual = JSON.parse(run.stdout.toString());
  try { assert.deepEqual(actual, oracle); results.push({ name, pass: true }); }
  catch { results.push({ name, pass: false, expected: oracle, actual }); }
}
console.log(JSON.stringify({ cases: results.length, failures: results.filter(r => !r.pass) }, null, 2));
if (results.some(r => !r.pass)) process.exitCode = 1;
