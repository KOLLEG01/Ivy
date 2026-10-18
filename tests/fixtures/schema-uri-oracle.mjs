import {createRequire} from 'node:module';
const require = createRequire(import.meta.url);
const uri = require('fast-uri/lib/utils.js');
const rows = [];
const inputs = new Set(['', '.', '..', '/', '//', '///', '/.', '/..', './', '../',
  'a/../b', 'a/.', 'a/..', '/a/./b/../../c', 'x:y/a:b', './x:y',
  'a//', 'a///', '%2e/%2E%2e/a', '%', '%a', '%ag', '%00', '%C0%AF',
  '\ud800', '\udfff', '\ud800\ud800\udc00', '\udc00\ud800', '😀', 'é', 'İ']);
// Every byte, escape spelling and ASCII delimiter, including malformed escapes.
for (let code = 0; code < 256; code++) {
  const escaped = '%' + code.toString(16).padStart(2, '0');
  for (const atom of [String.fromCharCode(code), escaped, escaped.toUpperCase()]) {
    inputs.add(atom);
    inputs.add('a:' + atom + '/b:' + atom);
  }
}
const segments = ['', '.', '..', 'a', 'b:c', '%2e', '%2E%2e', '\ud800'];
for (const a of segments) for (const b of segments) for (const c of segments) {
  inputs.add([a, b, c].join('/'));
  inputs.add('/' + [a, b, c, ''].join('/'));
}
for (const input of inputs) {
  rows.push({input: JSON.stringify(input), output: JSON.stringify([
    uri.normalizePercentEncoding(input), uri.normalizePercentEncoding(input, true),
    uri.normalizePathEncoding(input), uri.normalizeQueryFragmentEncoding(input),
    uri.serializePathEncoding(input, false), uri.serializePathEncoding(input, true),
    uri.encodeQuery(input), uri.encodeFragment(input), uri.encodeUserinfo(input),
    uri.removeDotSegments(input),
  ])});
}
process.stdout.write(JSON.stringify(rows));
