const units = value => Array.from({ length: value.length }, (_, i) => value.charCodeAt(i));
const mappings = [], contexts = [], whitespace = [];
let scalarCount = 0;
for (let point = 0; point <= 0x10ffff; point++) {
  if (point >= 0xd800 && point <= 0xdfff) continue;
  const input = String.fromCodePoint(point), output = input.toUpperCase(); scalarCount++;
  if (input !== output) mappings.push([point, units(output)]);
}
for (let unit = 0; unit <= 0xffff; unit++) if ((String.fromCharCode(unit) + 'x').trimStart() === 'x') whitespace.push(unit);
const add = input => contexts.push({ input: units(input), upper: units(input.toUpperCase()), trim: units(input.trimStart()) });
for (let unit = 0xd800; unit <= 0xdfff; unit++) {
  const boundary = String.fromCharCode(unit);
  add(' \ufeff' + boundary + 'ßΐ𐐨'); add(boundary + 'ςİ' + boundary); add('a' + boundary + '́');
}
for (const input of ['', 'ßﬃᾀΐ', 'iıİςσ', '\ud800\udfff', '\ufeff\n\t\r x \t', String.fromCharCode(...whitespace), '\u0085\u180e\u200bx', 'a\ud800\udfffß']) add(input);
process.stdout.write(JSON.stringify({ node: process.version, unicode: process.versions.unicode, icu: process.versions.icu, scalarCount, mappings, whitespace, contexts }));
