import {readFileSync,writeFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
if(process.versions.node!=='24.18.0')throw Error('Generate schema lookup data with the pinned Node24.18.0 oracle.');
// Finite built-in property graph for AJV's public canonical-schema name lookup.
// This is build-time compatibility data, not a JavaScript runtime dependency.
const roots={},nodes=[],pending=[],seen=new Map();
function intern(value){
  if(seen.has(value))return seen.get(value);
  const index=nodes.length;seen.set(value,index);nodes.push(null);pending.push({value,index});return index;
}
for(const [name,value]of Object.entries({object:Object.prototype,array:Array.prototype,string:String.prototype,number:Number.prototype}))roots[name]=intern(value);
for(let i=0;i<pending.length;i++){
  const {value,index}=pending[i];
  const node={kind:value===null?'null':typeof value,own:Object.create(null),prototype:null};nodes[index]=node;
  if(value===null||value===undefined||typeof value==='boolean')continue;
  const object=Object(value),prototype=Object.getPrototypeOf(object);
  if(prototype!==null)node.prototype=intern(prototype);
  for(const [key,descriptor]of Object.entries(Object.getOwnPropertyDescriptors(object))){
    node.own[key]='value' in descriptor?intern(descriptor.value):key==='__proto__'?-2:-1;
  }
}
const output=fileURLToPath(new URL('../../packages/contracts/rust/src/schema-lookup-builtins.json',import.meta.url));
const data=JSON.stringify({roots,nodes})+'\n';
if(process.argv.includes('--check')){if(readFileSync(output,'utf8')!==data)throw Error('Schema lookup built-ins are stale.');}
else writeFileSync(output,data);
