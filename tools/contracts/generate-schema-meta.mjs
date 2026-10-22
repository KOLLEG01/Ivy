import {readFileSync,writeFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {dirname,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';

// AJV is the pinned build-time source, not a native runtime dependency.
const require=createRequire(import.meta.url);
const root=fileURLToPath(new URL('../../',import.meta.url));
const sourceArg=process.argv.indexOf('--source');
const source=sourceArg<0?dirname(require.resolve('ajv/package.json')):resolve(process.argv[sourceArg+1]);
const version=JSON.parse(readFileSync(resolve(source,'package.json'),'utf8')).version;
const pinned=JSON.parse(readFileSync(resolve(root,'package.json'),'utf8')).dependencies.ajv;
if(version!==pinned)throw Error('Generate meta-schemas from the pinned AJV version.');
const files=['schema','meta/core','meta/applicator','meta/unevaluated','meta/validation','meta/meta-data','meta/format-annotation','meta/content'];
const schemas=files.map(file=>JSON.parse(readFileSync(resolve(source,'dist/refs/json-schema-2020-12',file+'.json'),'utf8')));
for(const [name,data] of [
  ['schema-meta-data.json',JSON.stringify({version,schemas})+'\n'],
  ['schema-meta-LICENSE.txt',readFileSync(resolve(source,'LICENSE'),'utf8').replaceAll('\r\n','\n').trimEnd()+'\n'],
]){
  const output=resolve(root,'packages/contracts/rust/src',name);
  if(process.argv.includes('--check')){if(readFileSync(output,'utf8')!==data)throw Error(name+' is stale.');}
  else writeFileSync(output,data);
}
