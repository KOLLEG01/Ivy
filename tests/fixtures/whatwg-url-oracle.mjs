import {IvyError} from '../../packages/contracts/src/errors.ts';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';

export const inputs = new Set([
  '', '/', '/mcp', '//localhost/mcp', 'https:', 'https://', 'http://[::1',
  'http://[::1%25lo0]/', 'https://example.invalid:65536/', 'https://exa mple.invalid/',
  'https://%FF/', 'https://%/', 'http://1.2.3.999/', 'http://09/',
  'https://EXAMPLE.invalid:443/a/../mcp', 'http://localhost:80/mcp',
  'https://user:pass@example.invalid/mcp', 'https://u@v:p:a@example.invalid/mcp',
  'https://:@example.invalid/mcp', 'https://example.invalid:/mcp',
  'file:///C:/a/../b', 'file://localhost/C|/mcp', 'file://server/share',
  'file:///C|/mcp', 'file:///C%7c/mcp', 'file://server/C|/mcp', 'file://server/C:/mcp',
  'file:////C|/mcp', 'file:///a/../C|/mcp', 'file:///C|more/mcp', 'file:///c|',
  'file:///a /b', 'file:///a ?q', 'file:///a #h', 'file:///a%20/b',
  'mailto:user@example.invalid', 'data:text/plain,hello world', 'custom:opaque  ',
  'custom:opaque  ?q', 'custom:opaque  #h', 'custom://EXAMPLE.invalid:443/a',
  'custom:/a', 'custom:///a', 'https:\\example.invalid\\mcp',
]);
for (const scheme of ['http', 'https', 'ws', 'wss', 'ftp', 'custom']) {
  for (const host of ['localhost', 'LOCALHOST', '127.0.0.1', '127.1', '2130706433',
    '0x7f000001', '0177.0.0.1', '[::1]', '[0:0:0:0:0:0:0:1]',
    '[::ffff:127.0.0.1]', 'example.invalid.', 'bücher.invalid', 'faß.invalid',
    '例子.invalid', 'ｅｘａｍｐｌｅ.invalid', '%65xample.invalid', 'a_b.invalid']) {
    for (const suffix of ['', ':80', ':443', ':000443', ':0', ':65535'])
      inputs.add(`${scheme}://${host}${suffix}/mcp`);
  }
}
for (const part of ['', '#', '#a', '?', '?#', '?q=é#', '/.', '/..', '/%2e',
  '/a/%2E%2e/mcp', '/%2F', '/%zz', '/a b', '/😀', '/\ud800', '/\udc00', '/a\\b'])
  inputs.add(`https://example.invalid/mcp${part}`);
for (const value of [' ', '\t', '\r', '\n', '\0', 'é', '😀', '\ud800', '\udc00',
  '\ud800x\udc00', '%', '%zz', '%20', '@', ':', '/', '?', '#', '[', ']', '\\', "'", '`']) {
  inputs.add(`https://${value}example.invalid/mcp`);
  inputs.add(`https://example.invalid/a${value}b?q=${value}#${value}`);
  inputs.add(`https://u${value}:p${value}@example.invalid/mcp`);
  inputs.add(`${value}https://example.invalid/mcp${value}`);
}
if(process.argv[1]&&pathToFileURL(resolve(process.argv[1])).href===import.meta.url){
const credentials=process.argv[2]==='credentials';
if(credentials){
  for(const scheme of ['http','https','ws','wss','ftp','custom','file'])for(const credential of ['user:pass','user:','user',':pass',':','u@v:p:a','u%40v:p%3Aa','é:😀','\ud800:\udfff'])for(const suffix of ['/a/../repo?q#h','/a%2Fb','/a\\b'])inputs.add(`${scheme}://${credential}@example.invalid${suffix}`);
  for(const input of ['git@example.invalid:repo.git','user:pass@example.invalid','ssh://git:token@example.invalid/repo','git+ssh://git:token@example.invalid/repo','custom://user:pass@/repo','custom://user:pass@EXAMPLE.invalid:0/a','custom:opaque  ?q','custom:opaque  #h','mailto:user:pass@example.invalid','data:text/plain,user:pass@example.invalid','file://user:pass@server/share','file:////server/share','file://server/C|/repo','file:///C:/repo'])inputs.add(input);
}
const rows = [...inputs].map(input => {
  let result;
  try {
    const u = new URL(input);
    if(credentials){u.username='';u.password='';result={ok:true,value:u.href};}
    else result = {ok: true, value: Object.fromEntries(
      ['href', 'protocol', 'hostname', 'username', 'password', 'hash'].map(k => [k, u[k]]))};
  } catch (error) { result = {ok: false, error: IvyError.from(error).toWire()}; }
  return {input: JSON.stringify(input), result: JSON.stringify(result)};
});
process.stdout.write(JSON.stringify(rows));
}
