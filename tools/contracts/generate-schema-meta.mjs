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
const {Ajv2020}=createRequire(resolve(source,'package.json'))('./dist/2020.js');
const compiler=new Ajv2020({strict:false,allErrors:false,ownProperties:true,inlineRefs:false,validateFormats:false,loopRequired:64,loopEnum:64});
// JSON-pointer selection in AJV uses JS property lookup, including prototypes.
// The registered documents and reachable built-ins form a finite graph. Embed
// that graph instead of reproducing a JS object model in the native runtime.
const roots={},nodes=[],pending=[],seen=new Map(),jsonObjects=new Set();
for(const document of schemas){const visit=value=>{if(value&&typeof value==='object'){jsonObjects.add(value);for(const child of Object.values(value))visit(child);}};visit(document);}
const intern=(value,reference)=>{
  if(seen.has(value))return seen.get(value);
  const index=nodes.length;seen.set(value,index);nodes.push(null);pending.push({value,reference,index});return index;
};
for(const document of schemas)roots[document.$id]=intern(document,document.$id+'#');
for(let cursor=0;cursor<pending.length;cursor++){
  const {value,reference,index}=pending[cursor];
  let mode='invalid';
  try{if(compiler.getSchema(reference))mode=jsonObjects.has(value)&&!Array.isArray(value)?'schema':value===false?'reject':'accept';}catch{}
  const node={mode,...(mode==='schema'?{reference:decodeURIComponent(reference)}:{}),own:Object.create(null),prototype:null};
  nodes[index]=node;
  // getJsonPointer refuses to descend through booleans. Null/undefined throw.
  if(value==null||typeof value==='boolean')continue;
  const boxed=Object(value),prototype=Object.getPrototypeOf(boxed);
  if(prototype!==null)node.prototype=intern(prototype,reference+'/__proto__');
  for(const [key,descriptor] of Object.entries(Object.getOwnPropertyDescriptors(boxed))){
    const childRef=reference+'/'+encodeURIComponent(key.replaceAll('~','~0').replaceAll('/','~1'));
    node.own[key]='value' in descriptor?intern(descriptor.value,childRef):key==='__proto__'?-2:-1;
  }
}
for(const [name,data] of [
  ['schema-meta-data.json',JSON.stringify({version,schemas,roots,nodes})+'\n'],
  ['schema-meta-LICENSE.txt',readFileSync(resolve(source,'LICENSE'),'utf8').replaceAll('\r\n','\n').trimEnd()+'\n'],
]){
  const output=resolve(root,'packages/contracts/rust/src',name);
  if(process.argv.includes('--check')){if(readFileSync(output,'utf8')!==data)throw Error(name+' is stale.');}
  else writeFileSync(output,data);
}
