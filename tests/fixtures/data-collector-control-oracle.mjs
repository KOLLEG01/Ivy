import {readFileSync,existsSync} from 'node:fs';
import {stripTypeScriptTypes,registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import {schemas} from './data-collector-schema-oracle.mjs';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}return next(specifier,context);}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {canonical,digest}=await import('../../packages/contracts/src/canonical.ts');
const OriginalDate=Date;globalThis.Date=class extends OriginalDate{constructor(...args){super(...(args.length?args:[1000000]));}static now(){return 1000000;}};
const sdk=/import\s*\{[^}]+\}\s*from\s*"[^\"]*packages\/sdk\/src\/node\.js";/;
const load=async(name,replacements)=>{let source=stripTypeScriptTypes(readFileSync(new URL('../../services/data-collector/src/'+name+'.ts',import.meta.url),'utf8'),{mode:'transform'});for(const [pattern,replacement]of replacements)source=source.replace(pattern,replacement);return import('data:text/javascript;base64,'+Buffer.from(source).toString('base64'));};
let nextOperation=0;
globalThis.__collectorControl={IvyError,requireThat,canonical,digest,...schemas,newOperationId:async()=>`fresh-${++nextOperation}`,scopedOperationId:()=>{throw Error('Unexpected publication');},serviceTools:()=>{throw Error('Unexpected peer dispatch');}};
const {CollectorStore}=await load('store',[[sdk,'const {digest,requireThat}=globalThis.__collectorControl;']]);
globalThis.__collectorControl.CollectorStore=CollectorStore;
const {CollectorEngine}=await load('engine',[[sdk,'const {canonical,IvyError,requireThat,scopedOperationId}=globalThis.__collectorControl;'],[/import\s*\{\s*CollectorStore\s*\}\s*from\s*".\/store.js";/,'const {CollectorStore}=globalThis.__collectorControl;'],[/import\s*\{[^}]+\}\s*from\s*".\/schema.js";/,'const {effectiveRetention,runtimeReadSchema:readSchema,runtimeUpdateSchema:updateSchema}=globalThis.__collectorControl;'],[/import\s*\{\s*TaskRunner\s*\}\s*from\s*".\/runner.js";/,'']]);
const {CollectorDefinitions}=await load('definitions',[[sdk,'const {canonical,IvyError,requireThat,newOperationId,serviceTools}=globalThis.__collectorControl;'],[/import\s*\{[^}]+\}\s*from\s*".\/schema.js";/,'const {assignmentDocument,definitionDocument,observationDocument,readSchema,scriptDocument,taskSchema,updateSchema}=globalThis.__collectorControl;']]);
const store=new CollectorStore(process.argv[3]);let clears=0;
const engine=new CollectorEngine(store,{clearAuthentication(){clears++;},authenticationFingerprint(){return null;}},schemas.settingsSchema.parse({secrets:{KNOWN:'synthetic-do-not-store'}}));
const objects=new Map();objects.set('catalog',[{object:{id:'catalog',name:'Catalog',contractKey:'data-collector/catalog',effectivelyArchived:false},revision:{revision:1,references:{}},content:{encoding:'json',value:{schemaVersion:1}}}]);
let nextObject=0,trace=[];
const client={request:async(method,params)=>{const call={method,params:structuredClone(params)};trace.push(call);try{
 let result;
 if(method==='objects.query')result={items:[...objects.values()].map(v=>v.at(-1)).filter(v=>v.object.contractKey===params.contractKey&&(!params.where||v.object.name===params.where.value)).map(document=>({objectId:document.object.id,document})),nextCursor:null};
 else if(method==='objects.read'){const versions=objects.get(params.objectId);if(!versions)throw new IvyError('not_found','Missing object.');result=params.revision?versions.find(d=>d.revision.revision===params.revision):versions.at(-1);if(!result)throw new IvyError('revision_not_found','Missing revision.');}
 else if(method==='objects.history')result={items:objects.get(params.objectId)??[],nextCursor:null};
 else if(method==='objects.write'){
   const id=params.create?'object-'+(++nextObject):params.objectId;const versions=objects.get(id)??[];const previous=versions.at(-1);
   if(!params.create&&previous?.revision.revision!==params.expectedRevision)throw new IvyError('revision_conflict','Stale CAS.');
   result={object:previous?.object??{id,name:params.create.name,contractKey:params.create.contractKey,effectivelyArchived:false},revision:{revision:versions.length+1,references:params.references},content:params.content};versions.push(structuredClone(result));objects.set(id,versions);
 }else throw Error('Unexpected method '+method);
 call.result=structuredClone(result);return structuredClone(result);
 }catch(e){const error=IvyError.from(e);call.error={code:error.code,message:error.message,outcome:error.outcome};throw e;}}};
engine.rootId='root';engine.client=client;const definitions=new CollectorDefinitions('fixture-node',engine);
const task={id:'task',name:'Task',enabled:true,intervalSeconds:60,timeoutSeconds:5,memoryMb:64,script:'export default()=>({data:1})',dependencies:{},config:{version:1},secretNames:[],retention:schemas.defaultRetention};
const steps=[];const add=(kind,input)=>steps.push({kind,input});const update=input=>add('update',input);const read=input=>add('read',input);const sync=()=>add('sync',null);
const save={action:'save',operationId:'save',expectedRevision:0,task};update(save);update(save);read({view:'tasks'});read({view:'definition',jobId:'object-2'});read({view:'script',objectId:'object-1',revision:1});
update({action:'activate',operationId:'activate',jobId:'object-2',definitionRevision:1,expectedRevision:0,enabled:true});sync();sync();read({view:'task',id:'task'});
update({action:'save',operationId:'draft-2',expectedRevision:1,task:{...task,config:{version:2},script:'export default()=>({data:2})'}});read({view:'task',id:'task'});
update({action:'activate',operationId:'activate-2',jobId:'object-2',definitionRevision:2,expectedRevision:1,enabled:true});sync();
update({action:'disable',operationId:'disable',id:'task',expectedRevision:2});sync();
update({action:'freezeHandoff',operationId:'freeze',jobId:'object-2',expectedRevision:3});update({action:'run',operationId:'frozen-run',id:'task'});read({view:'handoff',jobId:'object-2',expectedRevision:3});
update({action:'activate',operationId:'unfreeze',jobId:'object-2',definitionRevision:2,expectedRevision:3,enabled:false});
update({action:'run',operationId:'manual',id:'task',input:false});update({action:'run',operationId:'manual',id:'task',input:false});update({action:'delete',operationId:'busy-delete',id:'task',expectedRevision:2});
update({action:'disable',operationId:'cancel-queued',id:'task',expectedRevision:2});
read({view:'definitionHistory',jobId:'object-2'});read({view:'definitions'});read({view:'operation',operationId:'manual'});
add('offline',true);update(save);read({view:'operation',operationId:'save'});add('offline',false);
update({action:'saveScript',operationId:'script',expectedRevision:0,script:'export default()=>({data:3})'});
add('stopQueued',null);sync();add('clearCache',null);sync();read({view:'task',id:'task'});update({action:'recover',operationId:'recover',jobId:'object-2',expectedRevision:5,confirmedStopped:true});sync();
update({action:'delete',operationId:'delete',id:'task',expectedRevision:2});sync();read({view:'tasks'});
add('localSave',{action:'save',operationId:'legacy-local',expectedRevision:0,task:{...task,id:'legacy',enabled:false}});
add('localPatch',{id:'legacy',state:{page:8},nextAt:424242});
update({action:'import',operationId:'import'});read({view:'tasks'});update({action:'import',operationId:'import'});update({action:'import',operationId:'import-again'});
update({action:'save',operationId:'stale',expectedRevision:0,jobId:'object-7',task:{...task,id:'legacy',enabled:false}});
update({action:'saveScript',operationId:'stale-script',objectId:'object-1',expectedRevision:0,script:'export default()=>({data:4})'});
const results=[];
try{for(const step of steps){trace=[];let result=null,error=null;try{
 if(step.kind==='localSave')result=engine.update(step.input);else if(step.kind==='localPatch'){const row=store.get(step.input.id);store.save({...row,state:step.input.state,nextAt:step.input.nextAt});}else if(step.kind==='update')result=await definitions.update(step.input);else if(step.kind==='read')result=await definitions.read(step.input);else if(step.kind==='sync')await definitions.sync();else if(step.kind==='offline')engine.client=step.input?null:client;else if(step.kind==='clearCache')store.db.exec('DELETE FROM tasks');else if(step.kind==='stopQueued')for(const run of store.runs('queued'))store.saveRun({...run,status:'cancelled',error:'cancelled',finishedAt:new Date().toISOString()});
 }catch(cause){const e=IvyError.from(cause);error={code:e.code,message:e.message,outcome:e.outcome};}
 results.push({step,result,error,trace,clears,tasks:store.list(true),runs:['queued','running','publishing','failed','cancelled'].flatMap(s=>store.runs(s))});}
 process.stdout.write(JSON.stringify(results));
}finally{store.close();delete globalThis.__collectorControl;}
