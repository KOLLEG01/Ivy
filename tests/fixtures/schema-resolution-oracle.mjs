import {Ajv2020} from 'ajv/dist/2020.js';
import {resolveSchema} from 'ajv/dist/compile/index.js';
import {resolveUrl} from 'ajv/dist/compile/resolve.js';
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
const rows=[];
const containsId=value=>value!==null&&typeof value==='object'&&
  (Object.hasOwn(value,'$id')||Object.values(value).some(containsId));
function add(schema,queries){
  schema=JSON.parse(JSON.stringify(schema));
  admitSchema(schema);
  let compiler,env;
  try{
    const isolated=containsId(schema);
    compiler=new Ajv2020({strict:false,allErrors:false,ownProperties:true,inlineRefs:false,
      validateFormats:false,loopRequired:64,loopEnum:64,addUsedSchema:isolated});
    env=compiler._addSchema(schema,false,undefined,false,isolated);
    // Resolution is observed before target compilation. Mark existing schema
    // environments ready so resolveSchema does not compile their validators.
    for(const value of [...Object.values(compiler.refs),...Object.values(compiler.schemas),env])
      if(value&&typeof value==='object'&&'schema'in value)value.validate=()=>true;
  }catch{rows.push({schema:JSON.stringify(schema),registered:false,queries:[]});return;}
  const source=new WeakSet();
  function index(value){if(value!==null&&typeof value==='object'){source.add(value);for(const child of Object.values(value))index(child);}}
  index(schema);for(const value of Object.values(compiler.schemas))index(value.schema);
  const results=[];
  for(const [base,ref] of queries){
    let result;
    try{
      const absolute=resolveUrl(compiler.opts.uriResolver,base,ref);
      let current=absolute,target,seen=new Set();
      while(typeof (target=compiler.refs[current])==='string'){
        if(seen.has(current))throw Error('alias cycle');seen.add(current);current=target;
      }
      target=target||compiler.schemas[current]||resolveSchema.call(compiler,env,current);
      if(target===undefined&&env.localRefs?.[absolute])
        target={schema:env.localRefs[absolute],baseId:base,root:env};
      if(target===undefined)result={ok:true,found:false};
      else{
        if(!target||typeof target!=='object'||!('schema'in target)||!target.root)throw Error('Invalid target');
        const value=target.schema;
        const projected=typeof value==='boolean'||value&&typeof value==='object'&&!Array.isArray(value)&&source.has(value)?value:true;
        result={ok:true,found:true,schema:JSON.stringify(projected),base:target.baseId,
          root:target.root.schema===schema?'submitted':target.root.schema.$id,rootBase:target.root.baseId};
      }
    }catch{result={ok:false};}
    results.push({base:JSON.stringify(base),ref:JSON.stringify(ref),result:JSON.stringify(result)});
  }
  rows.push({schema:JSON.stringify(schema),registered:true,queries:results});
}
const pointers=['/$defs/value','/$defs/value/type','/$defs/resource','/$defs/resource/type',
  '/$defs/wrapper','/$defs/decorated','/properties/item/items','/description/properties/nested',
  '/description/$defs/x','/description/constructor','/description/__proto__',
  '/description/empty','/description/text/length','/description/text/0','/description/list/length',
  '/description/list/0','/description/null','/description/null/x','/description/number/x',
  '/description/rawid','/missing','/description/a~1b','/description/a~1b/type',
  '/description/text/constructor/prototype','/description/__proto__/constructor',
  '/description/%74ext','/description/%FF','/description/%','/description/~2',
  '/description/%EF%BF%BD'];
const ids=['','root','https://example.invalid/a/root','urn:x:root','ws://example.invalid/a','#tag'];
for(const root of ids)for(const child of ['child','child#part','../other','urn:x:child','https://example.invalid/sub']){
  const schema={$id:root,$defs:{value:{type:'string'},resource:{$id:child,type:'number'},
    wrapper:{$ref:'#/$defs/value'},decorated:{$ref:'#/$defs/value',title:'ignored'}},
    properties:{item:{$id:'local-level',items:{$ref:'#/$defs/value'}}},
    description:{'a/b':{$id:'annotation-resource',$anchor:'anchor',type:'boolean'},
      properties:{$id:'prevented',nested:{type:'boolean'}},$defs:{$id:'scope-change',x:{type:'object'}},
      empty:{},text:'abc',list:[{type:'integer'}],null:null,number:7,rawid:{$id:true,type:'number'},'�':{const:1}}};
  const queries=[];
  for(const base of [root||'#','',child,'local-level']){
    for(const pointer of pointers)queries.push([base,'#'+pointer]);
    for(const ref of ['', '#','#/','#anchor','child','annotation-resource#anchor',
      'constructor','__proto__','http://json-schema.org/schema#/properties/type',
      'https://json-schema.org/draft/2020-12/schema#/title',
      'https://json-schema.org/draft/2020-12/meta/validation#/$defs/nonNegativeInteger'])queries.push([base,ref]);
  }
  add(schema,queries);
}
for(const schema of [true,false,{}, {description:{$anchor:'a',type:'string'}},
  {$id:false,description:{$anchor:'a',type:'string'}},
  {description:{a:{$ref:'#/description/b'},b:{$ref:'#/description/a'}}}]){
  add(schema,[['#','#/description'],['','#/description/a'],['','#a'],['',''],['','constructor'],['','__proto__']]);
}
process.stdout.write(JSON.stringify(rows));
