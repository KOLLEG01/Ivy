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
const rows=[];
const meta=new Ajv2020({strict:false,validateFormats:false,logger:false});
const values=[null,false,true,0,-0,1,-1,1.5,'','string','integer','number','object',
  'array','null','boolean','String','unknown','\ud800',[],['string'],['string','null'],
  ['string','string'],['integer',null],[false],[1],['string','unknown'],{},
  {minimum:1},{x:true},['x'],[null],[{}],[[]],['__proto__']];
const keywords=['type','minimum','maximum','exclusiveMinimum','exclusiveMaximum',
  'multipleOf','minLength','maxLength','minItems','maxItems','minProperties','maxProperties',
  'uniqueItems','enum','required','pattern','format','title','description','default',
  'examples','deprecated','readOnly','writeOnly','const'];
function add(schema){
  try{admitSchema(schema);}catch{return;}
  // Malformed keyword shapes have native admission checks; compare compilation
  // here only for actual JSON Schema values.
  if(!meta.validateSchema(schema))return;
  let ok,reason;
  try{
    const compiler=new Ajv2020({strict:false,allErrors:false,ownProperties:true,
      inlineRefs:false,validateFormats:false,loopRequired:64,loopEnum:64,
      validateSchema:false,meta:false,logger:false});
    compiler.compile(schema);ok=true;
  }catch(error){ok=false;reason=JSON.stringify(error.message);}
  rows.push({schema:JSON.stringify(schema),ok,...(!ok?{reason}:{})});
}
add(true);add(false);add({});
for(const key of keywords)for(const value of values){
  add({[key]:value});
  for(const type of ['string','object','array','number'])add({type,[key]:value});
}
for(const value of values){
  for(const size of [1,63,64,65])add({required:Array.from({length:size},()=>value)});
  add({enum:[value]});
}
for(const key of ['items','additionalProperties','not','if','properties','$defs',
  'anyOf','oneOf','allOf','then','else']){
  for(const value of [true,false,{}, {type:'string'}, {a:true},[true],[false],[],null])
    add({[key]:value});
}
process.stdout.write(JSON.stringify(rows));
