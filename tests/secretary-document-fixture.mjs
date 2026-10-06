import { zipSync } from 'fflate';

export const docxType = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document';
export const word = text => Buffer.from('<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>' + text + '</w:t></w:r></w:p></w:body></w:document>');
export function docxBytes(parts = {}) {
  return Buffer.from(zipSync({
    '[Content_Types].xml': Buffer.from('<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>'),
    'word/document.xml': word('Original document ä and Ω.'), ...parts,
  }));
}
// Minimal actual PDF with a correct xref, three independently identified text pages and a standard font.
export function pdfBytes(texts = ['First original page.', 'Second original page.', 'Third original page.']) {
  const objects = ['<< /Type /Catalog /Pages 2 0 R >>', '<< /Type /Pages /Count ' + texts.length + ' /Kids [' + texts.map((_, i) => (4 + i * 2) + ' 0 R').join(' ') + '] >>',
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>'];
  texts.forEach((text, index) => {
    const stream = 'BT /F1 12 Tf 20 100 Td (' + text.replace(/[()\\]/g, char => '\\' + char) + ') Tj ET\n';
    objects.push('<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 3 0 R >> >> /Contents ' + (5 + index * 2) + ' 0 R >>');
    objects.push('<< /Length ' + Buffer.byteLength(stream) + ' >>\nstream\n' + stream + 'endstream');
  });
  let content = '%PDF-1.4\n', offsets = [0];
  objects.forEach((object, index) => { offsets.push(Buffer.byteLength(content)); content += (index + 1) + ' 0 obj\n' + object + '\nendobj\n'; });
  const start = Buffer.byteLength(content);
  content += 'xref\n0 ' + offsets.length + '\n0000000000 65535 f \n' + offsets.slice(1).map(offset => String(offset).padStart(10, '0') + ' 00000 n \n').join('')
    + 'trailer\n<< /Size ' + offsets.length + ' /Root 1 0 R >>\nstartxref\n' + start + '\n%%EOF\n';
  return Buffer.from(content);
}
