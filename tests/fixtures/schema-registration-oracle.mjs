import {Ajv2020} from 'ajv/dist/2020.js';
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
const {admitSchema}=await import('../../packages/contracts/src/schema.ts');
const initial=new Ajv2020({strict:false,validateFormats:false});
const documents=Object.values(initial.schemas).map(env=>env.schema);
const ids=['','normal','root','root#','root#/','#','#/','#anchor','#/$defs/a',
  'http://json-schema.org/schema',...documents.map(s=>s.$id)];
const containsId=value=>value!==null&&typeof value==='object'&&
  (Object.hasOwn(value,'$id')||Object.values(value).some(containsId));
const rows=[];
function add(schema){
  admitSchema(schema);
  const isolated=containsId(schema);
  const compiler=new Ajv2020({strict:false,allErrors:false,ownProperties:true,
    inlineRefs:false,validateFormats:false,loopRequired:64,loopEnum:64,addUsedSchema:isolated});
  const previous=new Map(Object.entries(compiler.refs));
  let result;
  try{
    // Exercise only _addSchema registration; meta validation and compilation
    // have separate differential tests and must not hide registration results.
    const env=compiler._addSchema(schema,false,undefined,false,isolated);
    const refs=Object.fromEntries(Object.entries(compiler.refs)
      .filter(([key,value])=>typeof value==='string'&&previous.get(key)!==value));
    result={ok:true,base:env.baseId,registered:compiler.refs[env.baseId]===env,refs,local:env.localRefs};
  }catch{result={ok:false};}
  rows.push({schema:JSON.stringify(schema),result:JSON.stringify(result)});
}
for(const schema of [true,false,{}])add(schema);
for(const root of ids){
  add({$id:root});
  for(const child of ids){
    add({$id:root,$defs:{a:{$id:child,type:'string'}}});
    add({$id:root,description:{$id:child,nested:{$anchor:'a'}}});
  }
}
for(const id of ids){
  add({description:{$id:id}});
  add({description:{a:{$id:id},b:{$id:id}}});
  add({description:{$anchor:id}});
}
for(const original of documents){
  add({description:original});
  add({description:{...original,title:'different'}});
  add({description:{...original,$id:original.$id+'#'}});
  add({description:{...original,$id:original.$id+'#/'}});
  add({description:{...original,$id:'http://json-schema.org/schema'}});
  add({description:{a:original,b:original}});
}
process.stdout.write(JSON.stringify({rows}));
