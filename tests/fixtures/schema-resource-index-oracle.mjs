import uri from 'fast-uri';
import {getSchemaRefs, getFullPath, normalizeId} from 'ajv/dist/compile/resolve.js';
import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s,c,n){
  if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){
    const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);
    if(existsSync(fileURLToPath(u)))return n(u.href,c);
  }
  return n(s,c);
}});
const {admitSchema} = await import('../../packages/contracts/src/schema.ts');
const ids = ['', '#', '#/', '#x', 'a', 'a/', 'a#', 'a#/', './a:b', 'http://x',
  'http://x:80', 'http://x:65536', 'http://x:abc', 'http://x:999999999999999999999',
  'https://x:443/a/../b', 'ws://x:80/', 'wss://x:443/?', 'ws://x/a?x#y',
  'urn:x:a/../b', 'urn:UUID:FOO', 'urn:', 'urn:a', 'urn:x:%ab', 'urn:x:雪',
  'urn:ſ:K', '%68ttp://x', '%u0068ttp://x', 'bad_scheme:x', '1x:a', ':a',
  '%', '%a', '%xx', '%2e/%2f', '//x', '///a', 'http:/\\x', 'http://a\\b/c',
  'http://[::1]/a', 'http://[invalid]/a', 'custom://%41/a', 'http://%41/a',
  'http://%ff/a', 'http://é.invalid/a', 'custom://ΣΟΣ/a', '\ud800',
  'a#\n', 'a#/\u2028', '__proto__', 'constructor'];
const fullPaths = [];
for (const id of ids) for (const suffix of ['', '#', '#/', '?q#fragment', '/..', '%']) {
  const input = id + suffix;
  let result;
  try { result = {ok: true, id: normalizeId(input), path: getFullPath(uri, input, false)}; }
  catch { result = {ok: false}; }
  fullPaths.push({input: JSON.stringify(input), result: JSON.stringify(result)});
}
const indexes = [];
function add(schema) {
  admitSchema(schema);
  let result;
  try {
    // Test the local indexing stage with an empty external registration table.
    // Compiler-wide meta-schema and inherited registrations are a later stage.
    const refs = Object.create(null);
    const local = getSchemaRefs.call({opts:{schemaId:'$id',uriResolver:uri},refs}, schema);
    result = {ok:true, refs, local};
  } catch { result = {ok:false}; }
  indexes.push({schema:JSON.stringify(schema),result:JSON.stringify(result)});
}
add(true); add(false); add({});
for (const root of ids) for (const child of ids) {
  add({$id:root,$defs:{child:{$id:child,type:'string'}}});
}
for (const id of ids) {
  add({$defs:{a:{$id:id},b:{$id:id}}});
  add({$id:id,description:{$id:'a',nested:{$id:'b'}}});
}
for (const anchor of ['', 'a', 'A_1.b-c', '-a', '1a', 'a/b', '\ud800', 'ſ']) {
  add({description:{$anchor:anchor}});
  add({$id:'https://example.invalid/root',description:{$id:'nested',$anchor:anchor,$dynamicAnchor:anchor}});
}
for (const id of [null,false,0,1,true,[],{}]) add({$id:id,$defs:{child:{$id:'c'}}});
add({description:{'a/b':{$id:'https://first.invalid/base'},a:{$id:'https://second.invalid/base',b:{nested:{$id:'child'}}}}});
add({description:{a:{b:{$id:'https://first.invalid/base'}},'a/b':{nested:{$id:'child'}}}});
add({$id:'https://example.invalid/root',properties:{'a/b':{$id:'one',items:{$id:'two'}}}});
process.stdout.write(JSON.stringify({fullPaths,indexes}));
