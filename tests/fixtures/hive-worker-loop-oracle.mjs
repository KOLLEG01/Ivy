// Observe the unchanged original worker request chain over actual SQLite domains.
// Kernel construction, transport/thread startup and temporary UI ownership are excluded.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync,rmSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {EventEmitter} from 'node:events';
import crypto from 'node:crypto';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
},load(url,context,next){
  if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
  return next(url,context);
}});
const {HiveStore}=await import('../../services/hive/src/store.ts');
const {Registry}=await import('../../services/hive/src/registry.ts');
const {PackageCatalogObject}=await import('../../services/hive/src/package-catalog-object.ts');
const {Objects}=await import('../../services/hive/src/objects.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {canonical,digest}=await import('../../packages/contracts/src/canonical.ts');
const input=JSON.parse(readFileSync(0,'utf8')),store=new HiveStore(input.path??':memory:');
store.configureCredentials(input.credentials??[]);
const registry=new Registry(store);
const kernelSource=readFileSync(new URL('../../services/hive/src/kernel.ts',import.meta.url),'utf8');
const closeStart=kernelSource.indexOf('  close(): void {'),closeEnd=kernelSource.indexOf('  private add<',closeStart);
if(closeStart<0||closeEnd<closeStart)throw Error('Original kernel close boundary changed');
const Close=new Function('rmSync','return '+stripTypeScriptTypes('class Close {'+kernelSource.slice(closeStart,closeEnd)+'}',{mode:'strip'}))(rmSync);
const kernel=Object.assign(new Close(),{store,registry,temporaryUiRoot:false});
kernel.packageCatalog=new PackageCatalogObject(store,new Objects(store),()=>{throw Error('Protected builds are not invoked by this loop fixture');});
const frames=[];let portClosed=false;
const parentPort=Object.assign(new EventEmitter(),{postMessage:value=>frames.push({...structuredClone(value),...(value.id===undefined?{}:{id:String(value.id)})}),close:()=>{portClosed=true;}});
const worker=readFileSync(new URL('../../services/hive/src/worker.ts',import.meta.url),'utf8');
const start=worker.indexOf('let announcedEventSequence = ');
if(start<0||!worker.endsWith('});\n'))throw Error('Original worker loop boundary changed');
const body=stripTypeScriptTypes(worker.slice(start),{mode:'strip'});
const observe=new Function('kernel','parentPort','canonical','digest','createHmac','randomBytes','IvyError','requireThat','browserPush',body+'\nreturn ()=>chain;')(kernel,parentPort,canonical,digest,crypto.createHmac,crypto.randomBytes,IvyError,requireThat,undefined);
let calls=0;store.db.function('fixture_sequence',()=>{calls++;if(calls<=(input.failSequenceCalls??0))throw Error('Original sequence fixture failed');return 7;});
const states=[];
try{
  for(const step of input.steps){
    if(step.sql)store.db.exec(step.sql);
    for(const scope of step.invalidate??[])store.invalidate(scope);
    const before=frames.length;
    parentPort.emit('message',{id:step.id,payload:step.payload});
    let rejected=null;try{await observe();}catch(error){rejected={message:error.message,code:error.code??null};}
    states.push({frames:frames.slice(before),rejected,portClosed});
    if(rejected||portClosed)break;
  }
  process.stdout.write(JSON.stringify({states,frames,portClosed,sequenceCalls:calls,databaseOpen:store.db.isOpen}));
}finally{if(store.db.isOpen)store.close();}
