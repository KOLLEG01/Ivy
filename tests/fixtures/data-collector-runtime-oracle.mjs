import {readFileSync,existsSync} from 'node:fs';
import {stripTypeScriptTypes,registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import {schemas} from './data-collector-schema-oracle.mjs';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {canonical,digest}=await import('../../packages/contracts/src/canonical.ts');
const FixedDate=Date;globalThis.Date=class extends FixedDate {constructor(...args){super(...(args.length?args:[1000000]));}static now(){return 1000000;}};
const load=async(name,replacements)=>{
  let js=stripTypeScriptTypes(readFileSync(new URL('../../services/data-collector/src/'+name+'.ts',import.meta.url),'utf8'),{mode:'transform'});
  for(const [pattern,replacement] of replacements)js=js.replace(pattern,replacement);
  return import('data:text/javascript;base64,'+Buffer.from(js).toString('base64'));
};
globalThis.__collectorRuntime={IvyError,requireThat,canonical,digest,...schemas,scopedOperationId:()=>{throw Error('Unexpected remote mutation');}};
const sdk=/import\s*\{[^}]+\}\s*from\s*"[^\"]*packages\/sdk\/src\/node\.js";/;
const {CollectorStore}=await load('store',[[sdk,'const {digest,requireThat}=globalThis.__collectorRuntime;']]);
globalThis.__collectorRuntime.CollectorStore=CollectorStore;
const {CollectorEngine}=await load('engine',[
  [sdk,'const {canonical,IvyError,requireThat,scopedOperationId}=globalThis.__collectorRuntime;'],
  [/import\s*\{\s*CollectorStore\s*\}\s*from\s*".\/store.js";/,'const {CollectorStore}=globalThis.__collectorRuntime;'],
  [/import\s*\{[^}]+\}\s*from\s*".\/schema.js";/,'const {effectiveRetention,runtimeReadSchema:readSchema,runtimeUpdateSchema:updateSchema}=globalThis.__collectorRuntime;'],
]);
const store=new CollectorStore(process.argv[3]);
let clears=0,cancelled=0,clearFailure=false;
const engine=new CollectorEngine(store,{clearAuthentication(){clears++;if(clearFailure)throw new IvyError('fixture_cleanup','Cleanup failed.');}},schemas.settingsSchema.parse({maximumTasks:1}));
engine.rootId='root';engine.client={request:async(method,params)=>({method,params})};
const task={id:'task',name:'Task',enabled:false,intervalSeconds:10,timeoutSeconds:5,memoryMb:128,script:'export default ()=>({data:null});',dependencies:{},config:{},secretNames:[],retention:schemas.defaultRetention};
const save={action:'save',operationId:'save',expectedRevision:0,task};
const disable={action:'disable',operationId:'disable',expectedRevision:1,id:'task'};
const steps=[];
const add=(kind,input,extra={})=>steps.push({kind,input,...extra});
add('read',{view:'tasks'});add('update',save);add('update',save);add('update',{...save,task:{...task,name:'Changed'}});
add('update',{action:'run',id:'task',operationId:'run-\ud800',input:false},{context:{callerPrincipalId:'caller-\udfff',operationId:'mismatch'}});
add('update',{action:'run',id:'task',operationId:'run-\ud800',input:false},{context:{callerPrincipalId:'caller-\udfff',operationId:'run-\ud800'}});
add('read',{view:'operation',operationId:'run-\ud800'});add('update',{action:'delete',id:'task',operationId:'busy-delete',expectedRevision:1});
add('active',true);add('update',disable);add('update',disable);add('read',{view:'task',id:'task'});
add('update',{action:'enable',id:'task',operationId:'enable',expectedRevision:2});add('update',disable);
add('update',{action:'run',id:'task',operationId:'run2'});add('patchRun',{id:'run2',status:'failed',error:'outcome_unknown'});
add('update',{action:'enable',id:'task',operationId:'unknown-enable',expectedRevision:3});
add('update',{action:'disable',id:'task',operationId:'bad-confirm',expectedRevision:3,confirmedStoppedRunId:'run-\ud800'});
const confirm={action:'disable',id:'task',operationId:'confirm',expectedRevision:3,confirmedStoppedRunId:'run2'};
add('update',confirm);add('active',false);add('update',confirm);add('update',confirm);
add('bindResult','result');add('read',{view:'history',id:'task',cursor:''});add('read',{view:'history',id:'task',cursor:'cursor-\ud800'});add('read',{view:'result',id:'task',revision:4});
add('read',{view:'tasks',extra:true});add('read',{view:'operation',operationId:'unknown'});
const deletion={action:'delete',id:'task',operationId:'delete',expectedRevision:4};
add('clearFailure',true);add('update',deletion);add('clearFailure',false);add('update',deletion);add('update',deletion);
add('update',{...save,operationId:'recreate'});add('update',save);add('update',{...save,operationId:'over-capacity',task:{...task,id:'other'}});
add('read',{view:'task',id:'missing'});add('read',{view:'task',id:'task'});
const results=[];
try{
for(const step of steps){let value=null,error=null;
  try{
    if(step.kind==='read')value=await engine.read(step.input);
    else if(step.kind==='update')value=engine.update(step.input,step.context);
    else if(step.kind==='patchRun'){store.saveRun({...store.run(step.input.id),...step.input});}
    else if(step.kind==='active'){
      if(step.input){const controller={abort(){cancelled++;}};engine.active.set('task',{controller,work:Promise.resolve()});}else engine.active.delete('task');
    }else if(step.kind==='bindResult'){const row=store.get('task');row.resultObjectId=step.input;store.save(row);}
    else if(step.kind==='clearFailure')clearFailure=step.input;
  }catch(cause){const e=IvyError.from(cause);error={code:e.code,message:e.message,outcome:e.outcome};}
  results.push({step,value,error,clears,cancelled,tasks:store.list(true),runs:['queued','running','publishing','failed','cancelled'].flatMap(status=>store.runs(status))});
}
process.stdout.write(JSON.stringify(results));
}finally{store.close();delete globalThis.__collectorRuntime;}
