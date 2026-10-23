import {createRequire,registerHooks} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {Ajv2020} from 'ajv/dist/2020.js';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {admitSchema}=await import('../../packages/contracts/src/schema.ts');
const require=createRequire(import.meta.url);
const compiler=new Ajv2020({strict:false,allErrors:false,ownProperties:true,inlineRefs:false,validateFormats:false,loopRequired:64,loopEnum:64});
const rows=[];
function add(name,schema){
  let result;
  try{
    admitSchema(schema);
    try{if(!compiler.validateSchema(schema))throw Error();result={ok:true};}
    catch{result={ok:false,code:'registry_invalid',message:'Invalid or unsupported JSON schema.'};}
  }catch(error){result={ok:false,code:error.code,message:error.message};}
  rows.push({name:JSON.stringify(name),schema:JSON.stringify(schema),result:JSON.stringify(result)});
}
const values=[null,true,false,0,-1,0.5,1,9007199254740991,'','x','string','integer','\ud800',[],[1],['x'],['string'],['string','null'],['string','string'],['x','x'],{}, {type:'string'}, {x:true}];
const keywords=['$id','$schema','title','description','default','examples','deprecated','readOnly','writeOnly','format','type','properties','required','additionalProperties','minProperties','maxProperties','items','minItems','maxItems','uniqueItems','enum','const','anyOf','oneOf','allOf','if','then','else','not','minimum','maximum','exclusiveMinimum','exclusiveMaximum','multipleOf','minLength','maxLength','pattern','$defs'];
for(const keyword of keywords)for(const value of values){
  const schema={[keyword]:value};
  add(keyword+' '+JSON.stringify(value),schema);
  for(const wrapper of ['definitions','properties','items','anyOf','then','default']){
    const outer=wrapper==='definitions'?{$defs:{unused:schema}}:wrapper==='properties'?{properties:{x:schema}}:wrapper==='anyOf'?{anyOf:[schema]}:{[wrapper]:schema};
    add(wrapper+' '+keyword+' '+JSON.stringify(value),outer);
  }
}
const prefix='https://json-schema.org/draft/2020-12/';
for(const meta of ['schema','meta/core','meta/applicator','meta/validation','meta/meta-data','meta/unevaluated','meta/format-annotation','meta/content']){
  for(const suffix of ['','#','#/'])for(const schema of [{},true,{type:'bad'},{$defs:{unused:{type:'bad'}}},{properties:{x:{type:'bad'}}},{enum:[]},{$defs:{unused:{enum:[]}}},{$id:'a#bad'},{required:3},{minimum:'x'},{title:1},{format:3},{examples:0}]){
    if(schema!==true)add(meta+suffix+' '+JSON.stringify(schema),{$schema:prefix+meta+suffix,...schema});
  }
}
// Walk the pinned source documents, independently of the generated native lookup.
for(const file of ['schema','meta/core','meta/applicator','meta/validation','meta/meta-data','meta/unevaluated','meta/format-annotation','meta/content']){
  const document=JSON.parse(readFileSync(require.resolve('ajv/dist/refs/json-schema-2020-12/'+file+'.json'),'utf8'));
  const walk=(value,path)=>{
    if(typeof value!=='boolean'&&(!value||typeof value!=='object'||Array.isArray(value)))return;
    for(const body of [{},{type:'bad'},{properties:{x:{type:'bad'}}}])add('source pointer '+file+path+' '+JSON.stringify(body),{$schema:prefix+file+'#'+path,...body});
    const escape=key=>key.replaceAll('~','~0').replaceAll('/','~1');
    for(const keyword of ['$defs','properties','patternProperties','dependentSchemas'])
      for(const [key,child] of Object.entries(value[keyword]??{}))walk(child,path+'/'+keyword+'/'+escape(key));
    for(const keyword of ['items','contains','additionalProperties','unevaluatedProperties','unevaluatedItems','propertyNames','not','if','then','else'])
      if(Object.hasOwn(value,keyword))walk(value[keyword],path+'/'+keyword);
    for(const keyword of ['allOf','anyOf','oneOf','prefixItems'])
      if(Array.isArray(value[keyword]))value[keyword].forEach((child,index)=>walk(child,path+'/'+keyword+'/'+index));
  };walk(document,'');
}
for(const selected of ['http://json-schema.org/schema','https://json-schema.org/schema',prefix+'schema#unknown',prefix+'meta/validation#/$defs/nonNegativeInteger',prefix+'meta/meta-data#/properties/default',prefix+'meta/core#/$defs/uriString','\ud800','missing'])add('selected '+selected,{$schema:selected});
for(const selected of [
  'HTTPS://JSON-SCHEMA.ORG/draft/2020-12/schema',
  'https://json-schema.org:443/draft/2020-12/schema',
  'https://json-schema.org/draft/2020-12/./schema',
  'https://json-schema.org/draft/2020-12/x/../schema',
  'https://json-schema.org/draft/2020-12/%73chema',
  prefix+'meta/meta-data#/properties/%64efault',
  prefix+'meta/meta-data#/properties/default%2F',
  prefix+'meta/meta-data#/properties/%FF',
  prefix+'meta/meta-data#/properties/%ZZ',
  prefix+'meta/meta-data#/properties/default/unknown',
  prefix+'schema?query',
])add('normalized '+selected,{$schema:selected});
for(const meta of ['schema','meta/core','meta/applicator','meta/validation','meta/meta-data','meta/unevaluated','meta/format-annotation','meta/content']){
  for(const fragment of ['#meta','#/properties/default','#/properties/properties','#/$defs/schemaArray','#/$defs/stringArray','#/$defs/nonNegativeInteger','#/$defs/uriReferenceString'])add('fragment '+meta+fragment,{$schema:prefix+meta+fragment,type:'bad'});
}
for(const id of ['','#','a','a#','a#b','a b','\ud800'])add('id '+JSON.stringify(id),{$id:id});
add('deep literal remains data',{default:{type:'bad',examples:false,$id:'bad#id'}});
add('duplicate enum permitted by meta',{enum:[{a:1,b:2},{b:2,a:1}]});
add('empty enum permitted by meta',{enum:[]});
for(const depth of [1,32,64]){let schema={type:'bad'};for(let i=0;i<depth;i++)schema={items:schema};add('nested meta '+depth,schema);}
for(const schema of [true,false])add('boolean '+schema,schema);
process.stdout.write(JSON.stringify(rows));
