// Actual original UI collection and failure journal; no replacement I/O policy.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
},load(url,context,next){
  if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
  return next(url,context);
}});
const {HiveStore}=await import('../../services/hive/src/store.ts');
const {Registry}=await import('../../services/hive/src/registry.ts');
const {UiFiles}=await import('../../services/hive/src/ui-files.ts');
const {Uis}=await import('../../services/hive/src/uis.ts');
const {Retention}=await import('../../services/hive/src/retention.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const input=JSON.parse(readFileSync(0,'utf8')),store=new HiveStore(':memory:');
const registry=new Registry(store),owner=new Uis(store,registry,new UiFiles(input.root));
const realNow=Date.now;Date.now=()=>172800000;
let message=null,error=null,journalError=null;
try{
  if(input.seedSql)store.db.exec(input.seedSql);
  try{owner.collectFiles();}
  catch(cause){
    message=cause.message;error=IvyError.from(cause).toWire();
    try{new Retention(store,registry).recordFailure(cause,172800000);}
    catch(cause){journalError=IvyError.from(cause).toWire();}
  }
  process.stdout.write(JSON.stringify({message,error,journalError,journal:store.metadata('retention_last_failure')??null}));
}finally{Date.now=realNow;store.close();}
