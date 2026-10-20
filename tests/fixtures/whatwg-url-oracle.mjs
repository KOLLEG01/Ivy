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
function formRows() {
  const rows = [];
  function add(input, fn) {
    let result;
    try { result = {ok: true, value: fn()}; }
    catch (error) { result = {ok: false, error: IvyError.from(error).toWire()}; }
    rows.push({input: JSON.stringify(input), result: JSON.stringify(result)});
  }
  const forms = new Set([
    '', '?', '??', '?a=1&a=2&b=3', '&&', '&a&&b=&=v&=',
    'a=b=c', 'a=1&a=2&a=3', 'a+b=c+d', 'a%2Bb=c%2Bd',
    '%=x&%zz=y&%2=z', '%EF%BB%BF=x', 'a=\0\r\n\t',
    '%ED%A0%80=%ED%BF%BF', '%F0%9F%98%80=😀',
    'a=%C0%AF&b=%E0%80%AF&c=%F4%90%80%80',
  ]);
  for (let byte = 0; byte < 256; byte++) {
    const encoded = '%' + byte.toString(16).padStart(2, '0');
    for (const value of [encoded, `%E2${encoded}%A1`, `%F0%9F${encoded}%80`])
      forms.add(`?${value}=v&k=${value}&${value}=last`);
  }
  for (let unit = 0xd800; unit <= 0xdfff; unit++) {
    const value = String.fromCharCode(unit);
    forms.add(`?${value}=x&k=${value}&${value}=last`);
  }
  for (const value of [' ', '+', '?', '#', '%', '&', '=', '~', '*', '-', '_', '.',
    '\0', '\t', '\r', '\n', 'é', '😀', '\ud800x\udfff', '\ud800\udfff', '\ufeff']) {
    forms.add(`?${value}=${value}&a=${value}&a=last`);
    forms.add(`??${value}=${value}`);
  }
  for (const form of forms) add({form}, () => [...new URLSearchParams(form)]);
  const updates = [
    [], [['code', 'token'], ['state', 'a b+~'], ['iss', 'https://issuer.invalid/prefix/']],
    [['a', 'first'], ['b', 'value'], ['a', 'last']], [['', '']],
    [['\ud800', 'first'], ['\udfff', 'last']],
    [['?', '+%&=#?\0é😀'], ['a', '\ud800x\udfff']],
  ];
  const urls = new Set(inputs);
  for (const prefix of ['https://u:p@example.invalid/prefix/', 'file://server/C:/a',
    'file:////C|/a', 'custom:opaque  ', 'custom:/a', 'custom://host/a']) {
    for (const suffix of ['', '?', '?#', '??a=1', '?a=1&b=2&a=3&c=4&a=5#kept?x',
      '?a=%zz&b=hello%20world&x=~#\ud800', '?\ud800=1&%EF%BF%BD=2#', '?&&#'])
      urls.add(prefix + suffix);
  }
  for (const url of urls) for (const pairs of updates) add({url, pairs}, () => {
    const parsed = new URL(url);
    for (const [name, value] of pairs) parsed.searchParams.set(name, value);
    return parsed.href;
  });
  return rows;
}

function formSerializeRows() {
  const rows=[];
  const add=pairs=>rows.push({input:JSON.stringify({pairs}),
    result:JSON.stringify({ok:true,value:new URLSearchParams(pairs).toString()})});
  add([]);
  add([['node','host.service'],['id','thread'],['node','again']]);
  add([['\ud800','first'],['\udfff','second'],['�','third']]);
  for(let unit=0;unit<=0xffff;unit++){
    const value=String.fromCharCode(unit);
    add([[value,'a +%&=#~*\0\r\n'],['node',value],[value,value]]);
  }
  for(const value of ['😀','\ud800\udfff','x\ud800y\udfff','',"\t\0\ufeff\r\n",'a +%&=#~*'])
    add([[value,value],['node',value],['id',value],[value,'last']]);
  return rows;
}

if(process.argv[1]&&pathToFileURL(resolve(process.argv[1])).href===import.meta.url){
const mode=process.argv[2];
if (mode === 'form') process.stdout.write(JSON.stringify(formRows()));
else if (mode === 'form-serialize') process.stdout.write(JSON.stringify(formSerializeRows()));
else {
const credentials=mode==='credentials';
if(credentials){
  for(const scheme of ['http','https','ws','wss','ftp','custom','file'])for(const credential of ['user:pass','user:','user',':pass',':','u@v:p:a','u%40v:p%3Aa','é:😀','\ud800:\udfff'])for(const suffix of ['/a/../repo?q#h','/a%2Fb','/a\\b'])inputs.add(`${scheme}://${credential}@example.invalid${suffix}`);
  for(const input of ['git@example.invalid:repo.git','user:pass@example.invalid','ssh://git:token@example.invalid/repo','git+ssh://git:token@example.invalid/repo','custom://user:pass@/repo','custom://user:pass@EXAMPLE.invalid:0/a','custom:opaque  ?q','custom:opaque  #h','mailto:user:pass@example.invalid','data:text/plain,user:pass@example.invalid','file://user:pass@server/share','file:////server/share','file://server/C|/repo','file:///C:/repo'])inputs.add(input);
}
if(mode==='components')for(const inner of ['https://u:p@bücher.invalid:443/a','http://[::1]:81/a','ftp://example.invalid/a','ws://example.invalid/a','file:///C:/a','custom:opaque','blob:https://example.invalid/a','invalid'])inputs.add(`blob:${inner}`);
const cases=[...inputs].map(input=>({input}));
if(mode==='relative'){
  cases.length=0;
  const bases=[...new Set([
    '', '/', 'null', 'https://', 'http://[::1', 'https://exa mple.invalid/',
    ...['http','https','ftp','ws','wss','custom'].flatMap(scheme=>[
      `${scheme}://user:pass@EXAMPLE.invalid:81/prefix/dir?q#h`,
      `${scheme}://bücher.invalid/a//b/`, `${scheme}://[::1]:80/`,
    ]),
    'custom:/prefix/dir?q#h','custom:///prefix/dir?q#h',
    'custom:opaque','custom:opaque  ?q','custom:opaque  #h','mailto:user@example.invalid',
    'custom:','custom:a/b/c','custom:a/../b','custom:a/b/','custom:a//b','custom:a b/c',
    'data:text/plain,hello world','blob:https://example.invalid/a',
    'file:///a/b?q#h','file:///C:/a/b?q#h','file://server/C:/a/b?q#h',
    'file://server/share/a/b?q#h','file:////a/b?q#h','file:////C|/a/b',
    'file:///C:','file:///','file://localhost/C|/a/b','file:///a/../C|/b',
    'https://example.invalid/\ud800/\udfff?q#h',
  ])];
  const references=new Set([
    '', '.', '..','./','../','../../..','../../../x','./?q#h','../?q#h',
    'x','x/','x/../y','x//..','x/./y','/x','//x/y','///x/y','////x/y',
    'x#h','x y#h','/x#h','//x#h','./#h','../x#h','../../x#h','../../../x#h',
    '?','?q','?q#h','#','#h','?#','?é#😀','x?q#h',
    'C:/x','C|/x','/C:/x','/C|/x','//C:/x','//C|/x','/D:/x','D|/x',
    'file:','file:/x','file://x/y','file:////x/y','file:C:/x','file:C|/x',
    'file:?q','file:#h','FILE:../x','https:x','https:/x','https://x/y',
    'custom:x','custom:/x','custom://x/y','blob:https://x/y',
    '\\x','\\\\x\\y','/\\x/y','\\/x/y','x\\y',
    'x%2Fy','%2e','%2E%2e/x','/%2e%2e/x','x/.. /y','a b',
    'é','😀','\ud800','\udfff','x\ud800y\udfff','\t../x\n','\0/x',
  ]);
  for(const scalar of [' ','\0','\t','\r','\n','é','😀','\ud800','\udfff','%','%zz','%20','@',':','/','?','#','[',']','\\',"'",'`']){
    for(const template of [x=>`x${x}y`,x=>`//u${x}:p${x}@example.invalid/a${x}?q=${x}#${x}`,x=>`${x}../x${x}`])references.add(template(scalar));
  }
  for(const base of bases)for(const input of references)cases.push({input,base});
  // An invalid supplied base must fail even when input is absolute.
  for(const base of ['', 'null','https://','https://example.invalid/prefix/'])for(const input of inputs)cases.push({input,base});
  // Exercise pathname-derived references, including repeated leading slashes.
  for(const base of inputs){try{const u=new URL(base);for(const suffix of ['/child','/other'])cases.push({input:u.pathname.replace(/\/$/,'')+suffix,base:u.origin});}catch{}}
}
const rows = cases.map(({input,base}) => {
  let result;
  try {
    const u = base===undefined?new URL(input):new URL(input,base);
    if(credentials){u.username='';u.password='';result={ok:true,value:u.href};}
    else result = {ok: true, value: Object.fromEntries(
      (mode==='components'?['origin','pathname']:['href', 'protocol', 'hostname', 'username', 'password', 'hash']).map(k => [k, u[k]]))};
  } catch (error) { result = {ok: false, error: IvyError.from(error).toWire()}; }
  return {input: JSON.stringify(input), ...(base===undefined?{}:{base:JSON.stringify(base)}), result: JSON.stringify(result)};
});
process.stdout.write(JSON.stringify(rows));
}
}
