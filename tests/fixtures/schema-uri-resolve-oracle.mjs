import uri from 'fast-uri';
const rows = [];
function add(base, relative) {
  let result;
  try { result = {ok: true, value: uri.resolve(base, relative)}; }
  catch { result = {ok: false}; }
  rows.push({base: JSON.stringify(base), relative: JSON.stringify(relative), result: JSON.stringify(result)});
}
const relatives = ['', '#', '#/', '#a', '#a\u2028ignored%', '#a\nline', '?', '?q', '?q#',
  'g', './g', 'g/', '/g', '//g', '?y', 'g?y', '#s', 'g#s', 'g?y#s', ';x',
  'g;x', 'g;x?y#s', '.', './', '..', '../', '../g', '../..', '../../', '../../g',
  '../../../g', '../../../../g', '/./g', '/../g', 'g.', '.g', 'g..', '..g',
  './g/.', 'g/./h', 'g/../h', 'g;x=1/./y', 'g;x=1/../y', 'g?y/./x', 'g#s/../x',
  'http:g', './x:y', './a//', '/%2e/%2E%2e/g', 'a%2fb', 'a%23b', '%41%7a',
  '%', '%a', '%ag', '%ff', '%c0%af', '\ud800', '雪', '😀', '\\x', '/\\x',
  '\\\\x', '\t//x', '/\t/x', '//x\\y', '%68ttp://EXAMPLE.invalid/a',
  '%u0068ttp://EXAMPLE.invalid/a', '1scheme:x', 'a_b:x', ':x'];
const bases = ['', '#', 'a', 'a/b', 'a/b?x#f', 'http://a/b/c/d;p?q',
  'https://EXAMPLE.invalid:443', 'https://user:pass@example.invalid/a/',
  'custom://host:', 'custom:a/b', 'urn:x:a/b', 'ws://example.invalid:80/a?q#f',
  '//host', '/a/b', '%', 'bad_scheme:x', '\ud800'];
for (const base of bases) for (const relative of relatives) add(base, relative);
const hosts = ['', 'Example.INVALID', '127.0.0.1', '127.1', '0x7f000001',
  '0127.0.0.1', '1.2.3.999', '1.2.3.04', 'example.invalid.', 'EX%41MPLE.invalid',
  '%2f', '%25', '%23', '%3a', '%ff', '%', 'é.invalid', 'ΣΟΣ.invalid', '\ud800',
  'x y', 'x\ty', 'x\ny', 'a@b', 'a\\b', '[::1]', '[0:0:0:0:0:0:0:1]',
  '[2001:0DB8:0:0:1:0:0:1]', '[::ffff:192.0.2.128]', '[192.0.2.1::]',
  '[fe80::1%eth0]', '[fe80::1%25eth0]', '[fe80::1%25zone%41]', '[fe80::1%]',
  '[vF.address:future]', '[v1.foo%41]', '[v1.]', '[::]', '[:::]', '[::1::2]',
  '[1:2:3:4:5:6:7:8]', '[1:2:3:4:5:6:7]', '[]', '[abc]', '[::1', 'x[foo]'];
for (const host of hosts) for (const scheme of ['http', 'https', 'ws', 'custom']) {
  for (const port of ['', ':', ':00080', ':65535', ':65536', ':abc']) {
    const base = scheme + '://' + host + port + '/a/../b';
    add(base, '../c?x#y');
    add('https://base.invalid/a', base);
  }
}
for (const user of ['', 'a', 'a:b', 'a@b', '%ff', '%', '雪', '\ud800']) {
  add('http://' + user + '@example.invalid/a', '../b');
}
for (const fragment of ['a\u2028b', 'a\u2029b', '\u2028%', '\ud800', '\r\n', '?/#']) {
  add('custom:a/b#old', '#' + fragment);
}
process.stdout.write(JSON.stringify(rows));
