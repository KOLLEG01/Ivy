// Disposable original reset storage qualification; no Host admission or clearing.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync,unlinkSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {join} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const url=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(url)))return next(url.href,context);}
  return next(specifier,context);
},load(url,context,next){if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};return next(url,context);}});
const {HiveStore}=await import('../../services/hive/src/store.ts');
const {Objects}=await import('../../services/hive/src/objects.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {cases,observations}=await import('./hive-reset-storage-cases.mjs');
const source=readFileSync(new URL('../../packages/host-runtime/src/runtime-reset.ts',import.meta.url),'utf8');
const start=source.indexOf('function savedInstructions('),end=source.indexOf('\nasync function sendStop(',start);
if(start<0||end<start)throw Error('Original reset source boundaries changed');
const original=new Function('existsSync','HiveStore','requireThat',stripTypeScriptTypes(source.slice(start,end),{mode:'strip'})+';return {savedInstructions,restoreInstructions};')(existsSync,HiveStore,requireThat);
// Extract the exact clearLocal Hive block; surrounding Host reset phases stay
// outside this storage fixture. The clock observation hooks only its default Date.
const clearStart=source.indexOf('  if (hive) {',source.indexOf('async function clearLocal('));
const clearBodyStart=source.indexOf('    try {',clearStart);
const clearBodyEnd=source.indexOf('\n  }',clearBodyStart);
if(clearStart<0||clearBodyStart<0||clearBodyEnd<clearBodyStart)throw Error('Original clearLocal Hive block changed');
const clearHive=new Function('HiveStore','Objects','restoreInstructions','path','record','bootstrapContracts',stripTypeScriptTypes('const store = new HiveStore(path);\n'+source.slice(clearBodyStart,clearBodyEnd),{mode:'strip'}));
const input=JSON.parse(readFileSync(0,'utf8'));
if(input.cross){console.log(JSON.stringify(original.savedInstructions(input.cross)));process.exit(0);}
const traces=[];
for(const [index,trace] of cases().entries()){
 const path=join(input.root,`original-${index}.sqlite`);let store,saved=[],clockCalls=0;const rows=[],diagnostics=[];
 for(const step of trace.steps){
  let value=null,error=null;
  try{
   if(step.op==='open'){store=new HiveStore(path);store.setMetadata('runtime_epoch','epoch-before');}
   else if(step.op==='sql')store.db.exec(step.sql);
   else if(step.op==='capture'){store?.close();store=undefined;saved=original.savedInstructions(path);value=saved;}
   else if(step.op==='discard'){store?.close();store=undefined;unlinkSync(path);}
   else if(step.op==='restore')original.restoreInstructions(store,step.values??saved);
   else if(step.op==='roots')new Objects(store).bootstrapScopeRoots(step.idUnits?.map(units=>String.fromCharCode(...units))??step.ids,step.at);
   else if(step.op==='contracts')new Objects(store).bootstrapServiceContracts(step.definitions);
   else if(step.op==='rebootstrap'){
    store?.close();store=undefined;const RealDate=globalThis.Date;
    globalThis.Date=class extends RealDate{constructor(...args){if(args.length){super(...args);return;}clockCalls++;if(step.clockError)throw Error('Clock unavailable.');super(step.at);}};
    try{clearHive(HiveStore,Objects,original.restoreInstructions,path,{runtimeEpoch:step.epoch,instructions:step.values??saved,plan:{scopeRootIds:step.ids}},step.definitions);}finally{globalThis.Date=RealDate;}
   }else throw Error('Unknown reset fixture operation');
  }catch(cause){error=IvyError.from(cause).toWire();diagnostics.push({step:rows.length,name:cause?.constructor?.name,message:cause?.message,code:cause?.code??null});}
  let state=null;
  if(existsSync(path)){
   const db=store?.db??new DatabaseSync(path,{readOnly:true});
   try{state=Object.fromEntries(Object.entries(observations).map(([key,sql])=>[key,db.prepare(sql).all().map(row=>Object.fromEntries(Object.entries(row).map(([k,v])=>[k,v instanceof Uint8Array?{blob:[...v]}:v])))]));}finally{if(!store)db.close();}
  }
  rows.push({value,error,state,clockCalls});
 }
 store?.close();traces.push({...trace,path,rows,diagnostics});
}
console.log(JSON.stringify({node:process.version,sqlite:process.versions.sqlite,traces}));
