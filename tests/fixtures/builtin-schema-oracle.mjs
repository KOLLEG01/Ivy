import {schemaSamples} from './schema-samples.mjs';
import {readFileSync,existsSync} from 'node:fs';
import {createRequire,registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.endsWith('.js')&&specifier.startsWith('.')&&context.parentURL){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const require=createRequire(import.meta.url);
const {Ajv2020}=require('ajv/dist/2020.js');
const ajv=new Ajv2020({strict:false,allErrors:false,validateFormats:false,ownProperties:true});
const documents=['hive-wire','hive-operations','hive-transport','host'].map(name=>JSON.parse(readFileSync(new URL('../../specs/schemas/'+name+'.schema.json',import.meta.url),'utf8')));
for(const document of documents)ajv.addSchema(document);
const rows=schemaSamples(ajv,documents);
for(const row of rows){
  if(row.reference.startsWith('https://ivy.invalid/schemas/host.schema.json#/$defs/')){
    row.hostName=row.reference.split('#/$defs/')[1];
    try{validateHost(row.hostName,JSON.parse(row.input));row.hostError=null;}
    catch(error){row.hostError={code:error.code,message:error.message};}
  }
}
process.stdout.write(JSON.stringify(rows));
