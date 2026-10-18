import {readFileSync,existsSync} from 'node:fs';
import {stripTypeScriptTypes,registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
import {schemas} from './data-collector-schema-oracle.mjs';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}return next(specifier,context);}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {canonical,digest}=await import('../../packages/contracts/src/canonical.ts');
const OriginalDate=Date;globalThis.Date=class extends OriginalDate{constructor(...args){super(...(args.length?args:[1000000]));}static now(){return 1000000;}};
const sdk=/import\s*\{[^}]+\}\s*from\s*"[^\"]*packages\/sdk\/src\/node\.js";/;
const load=async(name,replacements)=>{let source=stripTypeScriptTypes(readFileSync(new URL('../../services/data-collector/src/'+name+'.ts',import.meta.url),'utf8'),{mode:'transform'});for(const [pattern,replacement]of replacements)source=source.replace(pattern,replacement);return import('data:text/javascript;base64,'+Buffer.from(source).toString('base64'));};
let nextOperation=0;
globalThis.__collectorControl={IvyError,requireThat,canonical,digest,...schemas,newOperationId:async()=>`fresh-${++nextOperation}`,scopedOperationId:()=>{throw Error('Unexpected publication');},serviceTools:(...args)=>globalThis.__collectorControl.peerTools(...args)};
const {CollectorStore}=await load('store',[[sdk,'const {digest,requireThat}=globalThis.__collectorControl;']]);
globalThis.__collectorControl.CollectorStore=CollectorStore;
const {CollectorEngine}=await load('engine',[[sdk,'const {canonical,IvyError,requireThat,scopedOperationId}=globalThis.__collectorControl;'],[/import\s*\{\s*CollectorStore\s*\}\s*from\s*".\/store.js";/,'const {CollectorStore}=globalThis.__collectorControl;'],[/import\s*\{[^}]+\}\s*from\s*".\/schema.js";/,'const {effectiveRetention,runtimeReadSchema:readSchema,runtimeUpdateSchema:updateSchema}=globalThis.__collectorControl;'],[/import\s*\{\s*TaskRunner\s*\}\s*from\s*".\/runner.js";/,'']]);
const {CollectorDefinitions}=await load('definitions',[[sdk,'const {canonical,IvyError,requireThat,newOperationId,serviceTools}=globalThis.__collectorControl;'],[/import\s*\{[^}]+\}\s*from\s*".\/schema.js";/,'const {assignmentDocument,definitionDocument,observationDocument,readSchema,scriptDocument,taskSchema,updateSchema}=globalThis.__collectorControl;']]);
const handoffMode=process.argv[4]==='handoff';
const stores=[],nodes=new Map();let clears=0;
const makeNode=(id,path)=>{const store=new CollectorStore(path);stores.push(store);const node={store,online:true,fingerprint:null};node.engine=new CollectorEngine(store,{clearAuthentication(){clears++;},authenticationFingerprint(){return node.fingerprint;}},schemas.settingsSchema.parse({secrets:{KNOWN:'synthetic-do-not-store'}}));node.engine.rootId='root';node.definitions=new CollectorDefinitions(id,node.engine);nodes.set(id,node);return node;};
const primary=makeNode('fixture-node',process.argv[3]);const {store,engine,definitions}=primary;
if(handoffMode)makeNode('target-node',process.argv[3]+'.target');
const objects=new Map();objects.set('catalog',[{object:{id:'catalog',name:'Catalog',contractKey:'data-collector/catalog',effectivelyArchived:false},revision:{revision:1,references:{}},content:{encoding:'json',value:{schemaVersion:1}}}]);
let nextObject=0,trace=[];
const client={request:async(method,params)=>{const call={method,params:structuredClone(params)};trace.push(call);try{
 let result;
 if(method==='fixture.peer'){const peer=nodes.get(params.node);if(!peer?.online)throw new IvyError('provider_unavailable','Fixture source is offline.');result=params.name==='data-collector.read'?await peer.definitions.read(params.args):await peer.definitions.update(params.args);}
 else if(method==='objects.query')result={items:[...objects.values()].map(v=>v.at(-1)).filter(v=>v.object.contractKey===params.contractKey&&(!params.where||v.object.name===params.where.value)).map(document=>({objectId:document.object.id,document})),nextCursor:null};
 else if(method==='objects.read'){const versions=objects.get(params.objectId);if(!versions)throw new IvyError('not_found','Missing object.');result=params.revision?versions.find(d=>d.revision.revision===params.revision):versions.at(-1);if(!result)throw new IvyError('revision_not_found','Missing revision.');}
 else if(method==='objects.history')result={items:objects.get(params.objectId)??[],nextCursor:null};
 else if(method==='objects.write'){
   const id=params.create?'object-'+(++nextObject):params.objectId;const versions=objects.get(id)??[];const previous=versions.at(-1);
   if(!params.create&&previous?.revision.revision!==params.expectedRevision)throw new IvyError('revision_conflict','Stale CAS.');
   result={object:previous?.object??{id,name:params.create.name,contractKey:params.create.contractKey,effectivelyArchived:false},revision:{revision:versions.length+1,references:params.references},content:params.content};versions.push(structuredClone(result));objects.set(id,versions);
 }else throw Error('Unexpected method '+method);
 call.result=structuredClone(result);return structuredClone(result);
 }catch(e){const error=IvyError.from(e);call.error={code:error.code,message:error.message,outcome:error.outcome};throw e;}}};
for(const node of nodes.values())node.engine.client=client;
globalThis.__collectorControl.peerTools=(_client,node)=>({call:(name,args,operation)=>client.request('fixture.peer',{node,name,args,...(operation?{operation}:{})})});
const task={id:'task',name:'Task',enabled:true,intervalSeconds:60,timeoutSeconds:5,memoryMb:64,script:'export default()=>({data:1})',dependencies:{},config:{version:1},secretNames:[],retention:schemas.defaultRetention};
const steps=[];const add=(kind,input)=>steps.push({kind,input});const update=input=>add('update',input);const read=input=>add('read',input);const sync=()=>add('sync',null);
const save={action:'save',operationId:'save',expectedRevision:0,task};
if(!handoffMode){update(save);update(save);read({view:'tasks'});read({view:'definition',jobId:'object-2'});read({view:'script',objectId:'object-1',revision:1});
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
}else{
 const at=(node,kind,input)=>steps.push({node,kind,input});
 const source=(kind,input)=>at('fixture-node',kind,input),target=(kind,input)=>at('target-node',kind,input);
 const activate=(operationId,expectedRevision,serviceNodeId='target-node',enabled=true)=>({action:'activate',operationId,jobId:'object-2',definitionRevision:1,expectedRevision,enabled,serviceNodeId});
 source('update',save);source('update',activate('initial',0,'fixture-node'));source('sync',null);
 target('update',activate('enabled-source',1));
 source('localPatch',{id:'task',state:{page:42},nextAt:42424242});
 source('update',activate('disable',1,'fixture-node',false));
 target('update',activate('stale-observation',2));source('sync',null);
 source('read',{view:'handoff',jobId:'object-2',expectedRevision:2});
 source('update',{action:'freezeHandoff',operationId:'freeze',jobId:'object-2',expectedRevision:2});
 source('update',{action:'run',operationId:'frozen-run',id:'task'});
 source('fingerprint','sha256:private-cache');target('update',activate('auth-mismatch',2));
 source('offline',true);target('update',activate('unreachable-source',2));source('offline',false);
 target('fingerprint','sha256:private-cache');target('update',activate('move',2));
 source('sync',null);target('sync',null);target('update',activate('move',2));
 target('localPatch',{id:'task',state:{page:43},nextAt:43434343});target('sync',null);target('sync',null);
 source('update',{action:'run',operationId:'old-source-run',id:'task'});
 target('clearCache',null);target('sync',null);target('sync',null);
 target('update',{action:'recover',operationId:'recover-target',jobId:'object-2',expectedRevision:3,confirmedStopped:true});target('sync',null);
}
const results=[];
try{for(const step of steps){trace=[];let result=null,error=null;try{
 const node=nodes.get(step.node??'fixture-node'),{store,engine,definitions}=node;
 if(step.kind==='localSave')result=engine.update(step.input);else if(step.kind==='localPatch'){const row=store.get(step.input.id);store.save({...row,state:step.input.state,nextAt:step.input.nextAt});}else if(step.kind==='update')result=await definitions.update(step.input);else if(step.kind==='read')result=await definitions.read(step.input);else if(step.kind==='sync')await definitions.sync();else if(step.kind==='offline')engine.client=step.input?null:client;else if(step.kind==='clearCache')store.db.exec('DELETE FROM tasks');else if(step.kind==='stopQueued')for(const run of store.runs('queued'))store.saveRun({...run,status:'cancelled',error:'cancelled',finishedAt:new Date().toISOString()});
 if(step.kind==='offline')node.online=!step.input;else if(step.kind==='fingerprint')node.fingerprint=step.input;
 }catch(cause){const e=IvyError.from(cause);error={code:e.code,message:e.message,outcome:e.outcome};}
 const snapshot=node=>({tasks:node.store.list(true),runs:['queued','running','publishing','failed','cancelled'].flatMap(s=>node.store.runs(s)),handoff:Object.fromEntries(['handoff-lock:','handoff-state:','handoff-consumed:'].map(prefix=>[prefix,node.store.metadata(prefix+'object-2')]))});
 results.push({step,result,error,trace,clears,tasks:store.list(true),runs:['queued','running','publishing','failed','cancelled'].flatMap(s=>store.runs(s)),...(handoffMode?{nodes:Object.fromEntries([...nodes].map(([id,node])=>[id,snapshot(node)]))}:{})});}
 if(handoffMode){
   assert.equal(results.length,30);
   const errors=new Map([[3,'outcome_unknown'],[6,'outcome_unknown'],[8,'task_busy'],[10,'task_busy'],[12,'authentication_required'],[14,'provider_unavailable'],[24,'task_busy']]);
   for(const [index,row]of results.entries())assert.equal(row.error?.code,errors.get(index),`handoff step ${index}`);
   const target=index=>results[index].nodes['target-node'];
   assert.deepEqual(target(17).tasks[0].state,{page:42});
   assert.equal(target(17).tasks[0].nextAt,results[16].nodes['fixture-node'].tasks[0].nextAt);
   assert.equal(target(17).handoff['handoff-consumed:'],'true');
   assert.deepEqual(target(23).tasks[0].state,{page:43});
   assert.equal(target(23).tasks[0].nextAt,43434343);
   assert.deepEqual(target(27).tasks,[]);
   assert.equal(target(29).tasks[0].state,null);
 }
 process.stdout.write(JSON.stringify(results));
}finally{for(const store of stores)store.close();delete globalThis.__collectorControl;}
