// Actual Kernel and source-extracted worker/push startup. No transport or crypto surrogate.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync,mkdirSync,writeFileSync,lstatSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {dirname} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
import {tree} from './hive-ui-files-fixture.mjs';
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
 return next(specifier,context);
},load(url,context,next){if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};return next(url,context);}});
const {HiveKernel}=await import('../../services/hive/src/kernel.ts');
const {HiveStore}=await import('../../services/hive/src/store.ts');
const {Objects}=await import('../../services/hive/src/objects.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const input=JSON.parse(readFileSync(0,'utf8'));
if(input.rawSql){const db=new DatabaseSync(input.filename);try{db.exec(input.rawSql);}finally{db.close();}}
if(input.seed?.length){const seed=new HiveStore(input.filename),objects=new Objects(seed);try{for(const step of input.seed){if(step.op==='sql')seed.db.exec(step.sql);else if(step.op==='register')objects.register(step.definition,step.owner);else throw Error('Unexpected seed action');}}finally{seed.close();}}
if(input.rootFile){mkdirSync(dirname(input.uiRoot),{recursive:true});writeFileSync(input.uiRoot,'root-file');}
function snapshot(db){
 const all=sql=>db.prepare(sql).all().map(row=>Object.fromEntries(Object.entries(row).map(([k,v])=>[k,v instanceof Uint8Array?{blob:Buffer.from(v).toString('hex')}:v])));
 const definitions=all("SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name"),tables={};
 for(const {name}of all("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'object_fts_%' ORDER BY name"))tables[name]=all(`SELECT ${name==='object_fts'?'rowid,':''}* FROM "${name.replaceAll('"','""')}"${name==='metadata'?" WHERE key NOT IN ('cursor_secret','runtime_epoch')":''} ORDER BY rowid`);
 return{definitions,tables};
}
function files(){if(!existsSync(input.uiRoot))return{kind:'absent'};if(lstatSync(input.uiRoot).isFile())return{kind:'file',data:readFileSync(input.uiRoot).toString('base64')};return{kind:'directory',tree:tree(input.uiRoot)};}
function locked(){
 const db=new DatabaseSync(input.filename,{readOnly:true,timeout:0});
 try{db.prepare('SELECT COUNT(*) FROM sqlite_schema').get();return false;}
 catch(e){if(e.errcode!==5&&e.errcode!==6)throw e;return true;}
 finally{db.close();}
}
const pushSource=readFileSync(new URL('../../services/hive/src/browser-push-store.ts',import.meta.url),'utf8');
const pushClass=stripTypeScriptTypes(pushSource.slice(pushSource.indexOf('export class BrowserPushStore')).replace('export class','class'),{mode:'transform'});
const BrowserPushStore=new Function(pushClass+'\nreturn BrowserPushStore;')();
const worker=readFileSync(new URL('../../services/hive/src/worker.ts',import.meta.url),'utf8');
const startup=stripTypeScriptTypes(worker.slice(worker.indexOf('if (!parentPort)'),worker.indexOf('const oauthPrefix')),{mode:'transform'});
const run=new Function('parentPort','workerData','HiveKernel','BrowserPushStore','IvyError',startup+'\nreturn {kernel,announcedEventSequence};');
const RealDate=Date,realConfigure=HiveStore.prototype.configureCredentials;
let captured,exception=null,portClosed=false,options=null;const publications=[];
HiveStore.prototype.configureCredentials=function(...args){captured=this;return realConfigure.apply(this,args);};
globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[100000]));}static now(){return 100000;}};
try{
 try{
  const workerData=input.workerDataJson?JSON.parse(input.workerDataJson):{filename:input.filename,uiRoot:input.uiRoot,publicBaseUrl:input.publicBaseUrl,version:'1.0.0',buildId:'sha256:'+'a'.repeat(64),credentials:input.credentials,validateMessages:true};
  const {kernel}=run({postMessage(frame){publications.push({frame,databaseLocked:locked(),files:files()});},close(){portClosed=true;}},workerData,HiveKernel,BrowserPushStore,IvyError);
  if(input.workerDataJson){const units=value=>Array.from({length:value.length},(_,i)=>value.charCodeAt(i));options={version:units(kernel.options.version),buildId:units(kernel.options.buildId),publicBaseUrl:units(kernel.options.publicBaseUrl),uiRoot:units(kernel.uiFiles.root),validateMessages:!!kernel.options.validateMessages,resetBootstrapCredentialDigest:kernel.options.resetBootstrapCredentialDigest==null?null:units(kernel.options.resetBootstrapCredentialDigest)};}
 }
 catch(e){exception=IvyError.from(e).toWire();}
 // Record the original post/throw boundary before harness-only cleanup.
 const afterReadyError=!!exception&&publications.some(p=>p.frame.ready===true);
 if(captured?.db.isOpen)captured.close();
 const databaseLocked=locked(),db=new DatabaseSync(input.filename,{readOnly:true});
 try{process.stdout.write(JSON.stringify({publications,afterReadyError,state:snapshot(db),files:files(),databaseLocked,exception,portClosed,...(input.workerDataJson?{options}:{})}));}finally{db.close();}
}finally{globalThis.Date=RealDate;HiveStore.prototype.configureCredentials=realConfigure;}
