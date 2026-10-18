import {Ajv2020} from 'ajv/dist/2020.js';
import {RE2JS} from 're2js';
import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s,c,n){
  if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){
    const source=new URL(s.slice(0,-3)+'.ts',c.parentURL);
    if(existsSync(fileURLToPath(source)))return n(source.href,c);
  }
  return n(s,c);
}});
const {admitSchema}=await import('../../packages/contracts/src/schema.ts');
const containsId=value=>value!==null&&typeof value==='object'&&
  (Object.hasOwn(value,'$id')||Object.values(value).some(containsId));
const re2=Object.assign(pattern=>{const expression=RE2JS.compile(pattern);return{test:input=>expression.matcher(input).find(),toString:()=>pattern};},{code:'ivyRE2'});
const options={strict:false,allErrors:false,ownProperties:true,inlineRefs:false,
  validateFormats:false,loopRequired:64,loopEnum:64,validateSchema:false,logger:false,code:{regExp:re2}};
let anonymous,count=0;
const rows=[];
function add(input){
  const schema=JSON.parse(JSON.stringify(input));
  try{admitSchema(schema);}catch{return;}
  if(count++%128===0)anonymous=undefined;
  const isolated=containsId(schema);
  const compiler=isolated?new Ajv2020(options):anonymous??=new Ajv2020({...options,addUsedSchema:false});
  let ok;
  try{compiler.compile(schema);ok=true;}catch{ok=false;}
  finally{if(!isolated&&typeof schema==='object')compiler.removeSchema(schema);}
  rows.push({schema:JSON.stringify(schema),ok});
}
const leaves=[true,false,{}, {type:'string'}, {enum:[]}, {minimum:'bad'},
  {type:[]}, {format:false}, {required:1}, {$defs:{unused:{minimum:'bad'}}},
  {properties:{x:{type:'number'}}}, {if:{format:false},then:{title:'unused'}}];
for(const root of [null,'','root','https://example.invalid/a/root','urn:x:root','#tag']){
  for(const child of [null,'child','../other','#local'])for(const leaf of leaves){
    const target=typeof leaf==='object'&&child!==null?{$id:child,...leaf}:leaf;
    const base={...(root===null?{}:{$id:root}),$defs:{value:target,unused:{minimum:'bad'}}};
    const ref={$ref:'#/$defs/value'};
    for(const active of [ref,{properties:{x:ref}}, {items:ref},
      {if:{type:'string'},then:ref},{not:ref}, {allOf:[true,ref]}])add({...base,...active});
    add({...base,$defs:{...base.$defs,wrapper:ref},$ref:'#/$defs/wrapper'});
  }
}
for(const leaf of leaves)for(const name of ['x','a/b','a~b','%78','雪','\ud800']){
  const encoded=name.replaceAll('~','~0').replaceAll('/','~1');
  add({$defs:{[name]:leaf},$ref:'#/$defs/'+encoded});
  add({$defs:{x:{minimum:'bad'},[name]:leaf},$ref:'#/$defs/'+encoded});
  add({description:{value:leaf},$ref:'#/description/value'});
}
for(const id of [null,'','root','https://example.invalid/root','constructor','__proto__']){
  for(const keyword of ['properties','items','additionalProperties']){
    const recursive=keyword==='properties'?{properties:{next:{$ref:'#/$defs/node'}}}:{[keyword]:{$ref:'#/$defs/node'}};
    const target={type:keyword==='items'?'array':'object',...recursive};
    add({...(id===null?{}:{$id:id}),$defs:{node:target},$ref:'#/$defs/node'});
    add({...(id===null?{}:{$id:id}),$defs:{node:target},properties:{one:{$ref:'#/$defs/node'},two:{$ref:'#/$defs/node'}}});
  }
}
add(true);add(false);add({});
process.stdout.write(JSON.stringify(rows));
