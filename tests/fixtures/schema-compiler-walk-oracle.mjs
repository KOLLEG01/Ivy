import {Ajv2020} from 'ajv/dist/2020.js';
import subschema from 'ajv/dist/compile/validate/subschema.js';
import {resolveUrl,getFullPath} from 'ajv/dist/compile/resolve.js';
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
const containsId=value=>value!==null&&typeof value==='object'&&
  (Object.hasOwn(value,'$id')||Object.values(value).some(containsId));
const original=subschema.getSubschema;
let observe;
subschema.getSubschema=(it,args)=>{const result=original(it,args);observe?.(it,result);return result;};
const rows=[];
function add(input){
  const schema=JSON.parse(JSON.stringify(input));
  try{admitSchema(schema);}catch{return;}
  let result;
  try{
    const compiler=new Ajv2020({strict:false,allErrors:false,ownProperties:true,inlineRefs:false,
      validateFormats:false,loopRequired:64,loopEnum:64,validateSchema:false,meta:false,
      addUsedSchema:containsId(schema),logger:false});
    // Reference compilation is a later stage. Keep the keyword present in RULES
    // while observing the original inline branch selection and context updates.
    compiler.removeKeyword('$ref').addKeyword({keyword:'$ref',schemaType:'string',code(){}});
    const pointers=new WeakMap();
    function index(value,path=''){
      if(value!==null&&typeof value==='object'){
        pointers.set(value,path);
        for(const [key,child] of Object.entries(value))index(child,path+'/'+key.replaceAll('~','~0').replaceAll('/','~1'));
      }
    }
    index(schema);
    const nodes=[];
    const append=(node,base,pointer)=>nodes.push({pointer,base,schema:JSON.stringify(node),ref:typeof node?.$ref==='string'?node.$ref:null});
    const env=compiler._addSchema(schema,false,undefined,false,containsId(schema));
    append(schema,env.baseId||getFullPath(compiler.opts.uriResolver,env.baseId),'');
    observe=(it,ctx)=>{
      const child=ctx.schema;
      if(child===null||typeof child!=='object')return;
      let base=it.baseId;
      if(Object.keys(child).some(key=>compiler.RULES.all[key])&&child.$id)
        base=resolveUrl(compiler.opts.uriResolver,base,child.$id);
      append(child,base,pointers.get(child));
    };
    compiler._compileSchemaEnv(env);
    nodes.sort((a,b)=>a.pointer<b.pointer?-1:a.pointer>b.pointer?1:0);
    result={ok:true,base:env.baseId,nodes};
  }catch{result={ok:false};}
  finally{observe=null;}
  rows.push({schema:JSON.stringify(schema),result:JSON.stringify(result)});
}
const leaves=[true,false,{}, {title:'annotation'},{$id:'child'}, {type:'string'},
  {minimum:'bad'}, {type:[]},{enum:[]},{format:false},{required:1},
  {$defs:{unused:{minimum:'bad'}}}, {description:{minimum:'bad'}}];
for(const leaf of leaves){
  for(const keyword of ['not','items','additionalProperties','if','then','else'])add({[keyword]:leaf});
  for(const keyword of ['allOf','anyOf','oneOf']){
    add({[keyword]:[leaf]});add({[keyword]:[true,leaf]});add({[keyword]:[leaf,{}]});
  }
  for(const other of leaves){
    add({if:leaf,then:other});add({if:leaf,else:other});
    add({if:leaf,then:other,else:{type:'number'}});
    add({properties:{x:leaf,y:other}});
  }
}
const ids=['','root','root#/','https://example.invalid/a/root','urn:x:root','#resource'];
for(const root of ids)for(const child of ids)for(const leaf of leaves){
  for(const keyword of ['properties','allOf','anyOf','if','items']){
    const node={$id:child,...(typeof leaf==='object'?leaf:{const:leaf})};
    add({$id:root,[keyword]:keyword==='properties'?{'a/b~c':node}:keyword.endsWith('Of')?[node]:node,
      ...(keyword==='if'?{then:{type:'string'}}:{})});
  }
}
add({$defs:{x:{type:'string'}},properties:{x:{$ref:'#/$defs/x'}}});
add({$defs:{x:{type:'string'}},if:{$ref:'#/$defs/x'},then:{title:'unused'}});
for(const key of ['a/b','a~b','','2','10','01','\ud800','\udc00','😀','雪','\0','é','e\u0301']){
  for(const child of [{type:'string'}, {}, {$id:'child'}, {format:false}])
    add({properties:{[key]:child}});
}
add(true);add(false);add({});
process.stdout.write(JSON.stringify(rows));
