import {createRequire,registerHooks} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {schemaSamples} from './schema-samples.mjs';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const core=await import('../../packages/contracts/src/core-bundle.ts');
const coreValidation=await import('../../packages/contracts/src/core-validation.ts');
const host=await import('../../packages/contracts/src/host-bundle.ts');
const agent=await import('../../packages/contracts/src/agent-bundle.ts');
const load=name=>JSON.parse(readFileSync(new URL(`../../specs/schemas/${name}.schema.json`,import.meta.url),'utf8'));
const wire=load('hive-wire'),operations=load('hive-operations'),hostSchema=load('host'),agentSchema=load('agent');
const rows=[];
function add(kind,reference,document){
  let expected;
  try{const schema=kind==='operation'?core.operationInputSchema(reference):({core,host,agent}[kind]).bundledSchema(reference,document);const text=JSON.stringify(schema);
    expected={ok:true,sha256:createHash('sha256').update(text).digest('hex'),bytes:Buffer.byteLength(text),definitions:Object.keys(schema.$defs)};
  }catch(cause){const e=IvyError.from(cause);expected={ok:false,code:e.code,message:e.message,outcome:e.outcome};}
  rows.push({kind,reference:JSON.stringify(reference),...(document===undefined?{}:{document:JSON.stringify(document)}),expected});
}
for(const [kind,documents] of [['core',[operations,wire,hostSchema]],['host',[hostSchema,wire]],['agent',[agentSchema,wire]]]){
  for(const document of documents)for(const name of Object.keys(document.$defs))add(kind,`#/$defs/${name}`,document.$id);
  const doc=documents[0],name=Object.keys(doc.$defs)[0],ref=`#/$defs/${name}`;
  for(const reference of [ref,doc.$id+ref,doc.$id+'?query=1'+ref,doc.$id.replace('/schemas/','/schemas/x/../')+ref,doc.$id.replace('https:','https:')+ref,' '+ref+' ',ref+'\t\n',ref.replace('$defs','%24defs'),ref+'/',ref+'?',ref.toLowerCase(),'#/$defs/AbsentDefinition','#/$defs/__proto__','#/$defs/constructor','#/$defs/toString','#/$defs/hasOwnProperty','#/$defs/valueOf','#/$defs/toLocaleString','#/$defs/isPrototypeOf','#/$defs/propertyIsEnumerable','#/$defs/A_B','#/$defs/','', '#','other.schema.json'+ref,'https://other.invalid/'+ref,'http://[', '\ud800', doc.$id.replace('https://','https://user:password@')+ref,doc.$id.replace('https://','https://').replace('ivy.invalid','IVY.INVALID:443')+ref,doc.$id.replaceAll('/','\\')+ref])add(kind,reference);
  for(const base of ['relative','http://[','https://ivy.invalid/schemas/elsewhere.schema.json','https://ivy.invalid/schemas/'+doc.$id.split('/').at(-1)+'?query=1','https://ivy.invalid/schemas/x/../'+doc.$id.split('/').at(-1),'data:text/plain,x','ftp://ivy.invalid/schemas/x','https://ivy.invalid/schemas/\ud800'])add(kind,ref,base);
}
const catalog=JSON.parse(readFileSync(new URL('../../specs/schemas/operations.json',import.meta.url),'utf8')).operations;
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const ajv=new Ajv2020({strict:false,allErrors:false,validateFormats:false,ownProperties:true});
const documents=[wire,operations,hostSchema,load('hive-transport')];
for(const document of documents)ajv.addSchema(document);
const samples=new Map();
for(const sample of schemaSamples(ajv,documents,[operations])){
  const values=samples.get(sample.reference)??[];values.push(sample.input);samples.set(sample.reference,values);
}
for(const method of [...Object.keys(catalog),'absent.operation','','__proto__','constructor','toString','hasOwnProperty','__defineGetter__']){
  add('operation',method);
  const row=rows.at(-1),own=Object.hasOwn(coreValidation.operations,method);
  row.ownOperation=own;row.descriptor=own?coreValidation.operations[method]:null;
  if(method===Object.keys(catalog)[0])row.catalogNames=Object.keys(coreValidation.operations);
  row.validation=[];
  for(const direction of ['input','output']){
    const reference=own?operations.$id+coreValidation.operations[method][direction]:null;
    const inputs=samples.get(reference)??['{}','null','[]','true','0','""'];
    for(const input of inputs){
      let expected;
      try{coreValidation[direction==='input'?'validateInput':'validateOutput'](method,JSON.parse(input));expected={ok:true};}
      catch(cause){const e=IvyError.from(cause);expected={ok:false,code:e.code,message:e.message,outcome:e.outcome};}
      row.validation.push({direction,input,expected});
    }
  }
}
process.stdout.write(JSON.stringify(rows));
