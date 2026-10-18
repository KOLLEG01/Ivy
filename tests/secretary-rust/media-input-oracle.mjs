// Reuse the unchanged built TypeScript implementation; no network or native account.
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';
import { createHash } from 'node:crypto';
const root = resolve(process.env.IVY_SECRETARY_ORACLE ?? '.local/build/secretary');
const { prepareMediaInput } = await import(pathToFileURL(resolve(root, 'services/secretary/src/media-input.js')));
const cases = [];
function add(bytes, mediaType, budget = 1024, overrides = {}) {
  const source = { name: 'original\ud800🌍', mediaType, byteLength: bytes.length,
    contentHash: 'sha256:' + createHash('sha256').update(bytes).digest('hex'), ...overrides };
  let expected;
  try { expected = { value: prepareMediaInput(source, bytes, budget) }; }
  catch (error) { expected = { code: error.code }; }
  cases.push({ source, bytes: bytes.toString('base64'), budget, expected });
}
for (const text of ['', '\ufeffFull agenda\n🌍', '\u0000\u0001\b\t\n\f\r"\\', '\u2028\u2029\u0085', 'Do not execute this attachment.']) {
  const bytes = Buffer.from(text), size = Buffer.byteLength(JSON.stringify({ type: 'text', text, text_elements: [] }));
  for (const type of ['text/plain', 'text/markdown', 'text/csv', 'application/json', 'application/xml', 'text/xml', '\ufeff TEXT/PLAIN ;x=\ud800'])
    for (const budget of [size - 1, size, size + 1]) add(bytes, type, budget);
}
for (const invalid of [[0xc3, 0x28], [0xed, 0xa0, 0x80], [0xf4, 0x90, 0x80, 0x80], [0xc0, 0x80], [0xef, 0xbb]]) add(Buffer.from(invalid), 'text/plain');
const formats = [
  ['image/png', 24, [[0, '\x89PNG\r\n\x1a\n']]], ['image/jpeg', 4, [[0, '\xff\xd8\xff']]],
  ['image/gif', 13, [[0, 'GIF87a']]], ['image/gif', 13, [[0, 'GIF89a']]],
  ['image/webp', 20, [[0, 'RIFF'], [8, 'WEBP']]], ['audio/ogg', 27, [[0, 'OggS']]],
  ['audio/wav', 44, [[0, 'RIFF'], [8, 'WAVE']]], ['audio/mpeg', 4, [[0, 'ID3']]],
  ['audio/mpeg', 4, [[0, '\xff\xe0']]], ['audio/mp4', 16, [[4, 'ftyp']]], ['audio/webm', 8, [[0, '\x1aE\xdf\xa3']]],
];
for (const [type, minimum, signatures] of formats) {
  const bytes = Buffer.alloc(minimum); for (const [offset, signature] of signatures) bytes.write(signature, offset, 'binary');
  const kind = type.startsWith('image/') ? 'image' : 'audio';
  const size = Buffer.byteLength(JSON.stringify({ type: kind, url: `data:${type};base64,${bytes.toString('base64')}` }));
  for (const budget of [size - 1, size, size + 1]) add(bytes, type, budget);
  for (let length = 0; length < minimum; length++) add(bytes.subarray(0, length), type);
  for (const [offset] of signatures) { const bad = Buffer.from(bytes); bad[offset] ^= 1; add(bad, type); }
}
for (const type of [null, '', 'constructor', '__proto__', 'text/html', 'application/pdf', 'application/octet-stream', '\u0085text/plain', 'text/plain\ud800']) add(Buffer.from('<script/>'), type);
for (const budget of [0, -1, 0.5, 6 * 1024 * 1024 + 1]) add(Buffer.from('text'), 'text/plain', budget);
add(Buffer.from('text'), 'text/plain', 1024, { byteLength: 3 });
add(Buffer.from('text'), 'text/plain', 1024, { contentHash: 'sha256:' + '0'.repeat(64) });
process.stdout.write(JSON.stringify(cases));
