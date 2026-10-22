const units = value => Array.from({ length: value.length }, (_, i) => value.charCodeAt(i));
const mappings = [], contexts = [];
let scalarCount = 0;
for (let point = 0; point <= 0x10ffff; point++) {
  if (point >= 0xd800 && point <= 0xdfff) continue;
  scalarCount++;
  const input = String.fromCodePoint(point), nfc = input.normalize('NFC'), nfd = input.normalize('NFD');
  if (input !== nfc || input !== nfd) mappings.push([point, units(nfc), units(nfd)]);
}
const add = input => contexts.push({ input: units(input), nfc: units(input.normalize('NFC')) });
const marks = ['\u0300', '\u0301', '\u0307', '\u0308', '\u030a', '\u0315', '\u031b', '\u0323', '\u0327', '\u0334', '\u0345', '\u034f', '\u05b0', '\u05b1', '\u05bd', '\u05bf', '\u05c1', '\u0651', '\u0652', '\u093c', '\u0f71', '\u0f72', '\u1ab0', '\u1dc0', '\u20d0', '\ufe20'];
for (const starter of ['', 'A', 'a', 'e', 'Å', 'Ḋ', 'α', 'ש', 'ب', 'क', '\u0f40', '\u1100', '😀', '\u{1e030}'])
  for (const left of marks) for (const right of marks) add(starter + left + right + '\u0301');
for (const input of ['', '\0', 'e\u0301\0A\u030a', 'A\u030a\u0301', '\u212b', '\u2126', '\ufb01', 'A\u0315\u0300', 'a\u034f\u0308', '\u1100\u1161\u11a8', '\u0344', '👩\u200d💻', '\ufffe\uffff']) add(input);
for (let unit = 0xd800; unit <= 0xdfff; unit++) {
  const lone = String.fromCharCode(unit);
  for (const input of [lone, 'e\u0301' + lone + 'A\u030a', '\u0300' + lone + '\u0315\u0300', lone + '\ud800', '\udfff' + lone, lone + '\udc00']) add(input);
}
process.stdout.write(JSON.stringify({ node: process.version, icu: process.versions.icu, unicode: process.versions.unicode, scalarCount, mappings, contexts }));
