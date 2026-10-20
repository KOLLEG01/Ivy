import { zipSync } from 'fflate';

export const docxType = 'application/vnd.openxmlformats-officedocument.wordprocessingml.document';
export const word = text => Buffer.from('<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>' + text + '</w:t></w:r></w:p></w:body></w:document>');
export function docxBytes(parts = {}) {
  return Buffer.from(zipSync({
    '[Content_Types].xml': Buffer.from('<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>'),
    'word/document.xml': word('Original document ä and Ω.'), ...parts,
  }));
}
export { pdfBytes } from './secretary-pdf-fixture.mjs';
