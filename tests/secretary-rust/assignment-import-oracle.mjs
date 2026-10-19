import assert from 'node:assert/strict';
import {mkdtempSync,mkdirSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,settings,owner,rootId,at} from './inbox-fixtures.mjs';
import {snapshot,omitImportReceipts,seedRetainedValues,seedTechnicalHistory} from './import-core-fixtures.mjs';
import {importScheduleProgress,importEventSubscription,importEventGap,useCurrentAssignmentRecords} from './assignment-import-reference.mjs';
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {AssignmentScheduler}=await load('services/secretary/src/assignment-scheduler.js');
const {assignmentName,defaultConfiguration}=await load('services/secretary/src/assignment-schema.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {IvyError,canonical,hashJson,parseOperationId}=await load('packages/sdk/src/node.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js'),schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const sample=JSON.parse(schemaSamples(ajv,[schema]).find(r=>r.valid&&r.reference.endsWith('#/$defs/Assignment')).input);
const objectId='00000000-0000-4000-8000-000000000003',progressId='00000000-0000-4000-8000-000000000004';
const minutes=n=>new Date(Date.parse(at)+n*60000).toISOString();
const trigger={kind:'schedule',cadence:'interval',timeZone:'UTC',intervalMinutes:60,localTime:null,weekdays:[]};
const eventTrigger={kind:'event',topic:'fixture.events',topicVersion:'1.0.0',sourceServiceNodeId:null,eventKind:null};
const target={serviceNodeId:'fixture.agent',threadCwd:'/workspace',model:null,effort:'medium',permissions:':read-only'};
const base={...sample,assignmentId:'retained-assignment',enabled:true,rules:{},trigger,createdAt:at,updatedAt:at};
const directory=mkdtempSync(join(tmpdir(),'secretary-assignment-import-')),rows=[];
const empty={items:[],throughSequence:0,gap:null,hasMore:false};
const errorResult=cause=>{const e=IvyError.from(cause);return{error:{code:e.code,message:e.message,outcome:e.outcome}};};
async function add(label,mode,options={}){
 const location=join(directory,String(rows.length));mkdirSync(location);
 const original=join(location,'original'),copy=join(location,'copy');mkdirSync(original);mkdirSync(copy);
 const trace=[],steps=[],counts={},mutationIds=new Map(),records=new Map();let store,attempt=0,batchIndex=0;
 const assignmentValues=[{...base,trigger:mode==='active'?eventTrigger:trigger}];
 if(options.revisionTwo)assignmentValues.push({...assignmentValues[0],updatedAt:minutes(120),prompt:'Retained edit'});
 if(options.initialSchedule)assignmentValues[0].trigger=trigger;
 records.set(objectId,{key:'secretary/assignment',name:assignmentName(base.assignmentId),versions:assignmentValues.map((value,i)=>({revision:i+1,value,writtenAt:i?minutes(120):at}))});
 const latest=records.get(objectId).versions.at(-1),assignment={pin:{objectId,revision:latest.revision},value:structuredClone(latest.value),writtenAt:latest.writtenAt};
 const archiveRow={objectId,revision:latest.revision,values:{'object.effectivelyArchived':true,'object.archivedAt':at,'object.name':options.badArchiveName?'foreign':assignmentName(base.assignmentId),'data:/assignmentId':base.assignmentId}};
 const configuration=defaultConfiguration(settings,at,target);
 const kind=mode==='schedule'?'schedule':'event';
 if(options.progress){
  const value=mode==='schedule'?{schemaVersion:1,assignmentId:base.assignmentId,assignment:assignment.pin,assignmentSnapshot:structuredClone(assignment.value),checkedAt:minutes(150),updatedAt:at,...options.progress}:{schemaVersion:1,assignmentId:base.assignmentId,assignment:assignment.pin,assignmentSnapshot:structuredClone(assignment.value),processedThrough:0,active:false,needsRebase:false,updatedAt:at,...options.progress};
  records.set(progressId,{key:'secretary/'+kind+'-progress',name:kind+'-'+hashJson(base.assignmentId).slice(7),versions:[{revision:1,value,writtenAt:at}]});
 }
 const read=(id,revision)=>{
  const record=records.get(id);assert(record);const v=revision?record.versions.find(v=>v.revision===revision):record.versions.at(-1);assert(v);
  return{object:{id,parentId:rootId,effectivelyArchived:false,contractKey:record.key},revision:{revision:v.revision,contractVersion:contractVersion(record.key),mediaType:'application/json',contentHash:hashJson(v.value),byteLength:Buffer.byteLength(canonical(v.value)),references:store.objectWriteRequest(record.key,v.value,{object:{objectId:id,revision:v.revision}},'fixture:1800000000000:read').references,createdAt:v.writtenAt},content:{encoding:'json',value:structuredClone(v.value)}};
 };
 async function request(method,p,requestOptions){
  const normalized=structuredClone(p);
  if(method==='objects.write'||method==='events.unsubscribe'){
   parseOperationId(p.mutationId);if(!mutationIds.has(p.mutationId))mutationIds.set(p.mutationId,'<operation-'+mutationIds.size+'>');normalized.mutationId=mutationIds.get(p.mutationId);
  }
  trace.push({method,params:normalized,timeout:requestOptions?.timeoutMs??35000});counts[method]=(counts[method]??0)+1;
  const fault=options.faults?.find(f=>f.method===method&&f.n===counts[method]);let result,error;
  if(fault&&!fault.applied)error=new IvyError(fault.code??'fixture_failure','Fixture failure.',fault.outcome??'unknown');
  else if(method==='serviceNodes.get')result=options.ownerChanged?{...owner,principalId:'foreign'}:owner;
  else if(method==='system.status')result={runtimeEpoch:'fixture-'+attempt};
  else if(method==='objects.read')result=read(p.objectId,p.revision);
  else if(method==='objects.query'){
   const name=p.where.args.find(a=>a.field==='object.name').value;
   result={items:[...records].filter(([,r])=>r.key===p.contractKey&&r.name===name).map(([id,r])=>({objectId:id,revision:r.versions.at(-1).revision})),nextCursor:null};
  }else if(method==='objects.write'){
   const id=p.create?progressId:p.objectId;
   const record=p.create?{key:p.create.contractKey,name:p.create.name,versions:[]}:records.get(id);
   if(!p.create)assert.equal(p.expectedRevision,record.versions.at(-1).revision);
   const revision=p.create?1:record.versions.at(-1).revision+1;
   record.versions.push({revision,value:structuredClone(p.content.value),writtenAt:minutes(180)});records.set(id,record);
   if(fault)error=new IvyError(fault.code??'fixture_failure','Fixture failure.',fault.outcome??'unknown');else result={object:{id},revision:{revision}};
  }else if(method==='events.subscribe')result=structuredClone(options.batches?.[batchIndex++]??empty);
  else if(method==='events.read')result=structuredClone(options.firstBatch??{...empty,items:[{occurredAt:minutes(-1)}]});
  else if(method==='events.ack'||method==='events.unsubscribe'){
   if(method==='events.unsubscribe'){
    const receipt=store.db.prepare('SELECT value FROM journal_state WHERE key=?').get('secretary/import/event-subscription/legacy-import');assert(receipt,'Persist before unsubscribe');
    const state=JSON.parse(receipt.value);assert(state.requests.some(r=>canonical(r)===canonical(p)),'Dispatch only the frozen request');
   }
   if(fault)error=new IvyError(fault.code??'fixture_failure','Fixture failure.',fault.outcome??'unknown');else result={ok:true};
  }else assert.fail('Import must not dispatch '+method);
  if(error){steps.push({method,error:{code:error.code,message:error.message,outcome:error.outcome}});throw error;}
  steps.push({method,value:structuredClone(result)});return structuredClone(result);
 }
 store=new SecretaryStore({request},rootId,original);seedRetainedValues(store);
 const sourceKey=mode==='schedule'?'secretary/schedule-cursor':mode==='gap'?'secretary/event-gap':'secretary/event-subscription';
 const stable='secretary-assignment-'+hashJson(base.assignmentId).slice(7,48);
 const sourceValue=mode==='schedule'?{updatedAt:options.current?latest.value.updatedAt:minutes(-60),checkedAt:options.invalidCursor?'invalid':minutes(60)}:mode==='gap'?{prunedThroughSequence:3,...options.gapValue}:{name:options.sameName?stable:'retained-obsolete'};
 if(!options.noLegacy)seedTechnicalHistory(store,sourceKey,base.assignmentId,sourceValue);
 if(options.corruptReceipt)store.db.prepare('INSERT INTO journal_state VALUES(?,?)').run('secretary/import/event-subscription/legacy-import',canonical({schemaVersion:1,identity:{},requests:[],completed:0}));
 const before=snapshot(store.db);store.close();
 const sourceBytes=readFileSync(join(original,'secretary.sqlite'));writeFileSync(join(copy,'secretary.sqlite'),sourceBytes);
 const limits=options.noRoom?{count:0,bytes:0}:{count:10000,bytes:256*1024*1024};
 const rawResults=[],snapshots=[],issues=new Map();
 for(attempt=0;attempt<(options.attempts??1);attempt++){
  // Reopen the same physical copy with a fresh RPC identity after every attempt.
  // Pending unsubscribe must use the earlier saved ID despite the new epoch.
  const client={request};store=new SecretaryStore(client,rootId,copy,limits);useCurrentAssignmentRecords(store);
  const controller=new AbortController();if(options.cancel)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
  const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,owner:{serviceNodeId:'fixture.secretary'},signal:controller.signal,recoveryIssues:issues,now:()=>new Date(minutes(180))});
  const scheduler=new AssignmentScheduler(engine);
  try{
   const value=mode==='schedule'?await importScheduleProgress(scheduler,assignment,Date.parse(minutes(180))):mode==='gap'?await importEventGap(scheduler,assignment):await importEventSubscription(scheduler,mode==='archived'?{archived:archiveRow}:{active:assignment,configuration});
   rawResults.push({value});
  }catch(error){rawResults.push(errorResult(error));}
  const receipts=store.db.prepare("SELECT key,value FROM journal_state WHERE key LIKE 'secretary/import/%' ORDER BY key").all().map(r=>({key:r.key,value:JSON.parse(r.value)}));
  for(const receipt of receipts)for(const r of receipt.value.requests??[])if(!mutationIds.has(r.mutationId))mutationIds.set(r.mutationId,'<operation-'+mutationIds.size+'>');
  snapshots.push({core:omitImportReceipts(snapshot(store.db)),receipts,issues:[...issues]});store.close();
 }
 assert.deepEqual(readFileSync(join(original,'secretary.sqlite')),sourceBytes);
 const normalize=value=>typeof value==='string'?mutationIds.get(value)??value:Array.isArray(value)?value.map(normalize):value&&typeof value==='object'?Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalize(v)])):value;
 rows.push({input:{label,mode,settings,assignment,archiveRow,configuration,time:Date.parse(minutes(180)),attempts:options.attempts??1,cancel:!!options.cancel,limits,sourceKey,coreBase64:sourceBytes.toString('base64'),steps},expected:normalize({results:rawResults,snapshots,trace,before:omitImportReceipts(before),history:[...records]})});
}
try{
 await add('schedule-no-legacy','schedule',{noLegacy:true});
 await add('schedule-current','schedule',{current:true,attempts:2});
 await add('schedule-revision-one-cursor','schedule');
 await add('schedule-original-revision','schedule',{revisionTwo:true});
 await add('schedule-latest-revision','schedule',{revisionTwo:true,current:true});
 await add('schedule-current-progress-wins','schedule',{progress:{},attempts:2});
 await add('schedule-foreign-progress','schedule',{progress:{assignmentId:'foreign'}});
 await add('schedule-invalid-time-preserved','schedule',{invalidCursor:true});
 await add('schedule-owner-changed','schedule',{ownerChanged:true});
 for(const applied of [false,true])await add('schedule-unknown-write-'+applied,'schedule',{attempts:2,faults:[{method:'objects.write',n:1,applied}]});
 await add('active-retire-after-ack','active',{attempts:2});
 await add('active-current-name','active',{sameName:true,attempts:2});
 await add('active-initial-schedule','active',{initialSchedule:true});
 await add('active-behind-then-caught-up','active',{attempts:2,progress:{processedThrough:5},batches:[{...empty,throughSequence:2,hasMore:true},{...empty,throughSequence:5}]});
 await add('active-ack-unknown','active',{attempts:2,faults:[{method:'events.ack',n:1}]});
 await add('active-unsubscribe-unknown','active',{attempts:2,faults:[{method:'events.unsubscribe',n:1,applied:true}]});
 await add('active-unsubscribe-expired','active',{attempts:2,faults:[{method:'events.unsubscribe',n:1},{method:'events.unsubscribe',n:2,code:'operation_expired',outcome:'not_executed'}]});
 await add('active-progress-foreign','active',{progress:{assignmentId:'foreign'}});
 await add('active-receipt-foreign','active',{corruptReceipt:true});
 await add('active-cancelled','active',{cancel:true});
 await add('archived-two-names','archived',{attempts:2});
 await add('archived-current-name','archived',{sameName:true});
 await add('archived-invalid-name','archived',{badArchiveName:true});
 await add('archived-first-unknown','archived',{attempts:2,faults:[{method:'events.unsubscribe',n:1}]});
 await add('archived-second-unknown','archived',{attempts:3,faults:[{method:'events.unsubscribe',n:2}]});
 await add('gap-before-assignment-retained','gap',{attempts:2});
 await add('gap-empty-is-relevant','gap',{firstBatch:empty});
 await add('gap-after-assignment','gap',{firstBatch:{...empty,items:[{occurredAt:minutes(1)}]}});
 await add('gap-source-journal-gap','gap',{firstBatch:{...empty,gap:{prunedThroughSequence:4}}});
 for(const relevant of [false,true,null])await add('gap-current-'+relevant,'gap',{gapValue:{relevant}});
 await add('gap-no-room-retains-history','gap',{noRoom:true});
 await add('gap-unavailable-read','gap',{faults:[{method:'events.read',n:1}]});
 process.stdout.write(JSON.stringify({rootId,rows})+'\n');
}finally{rmSync(directory,{recursive:true,force:true});}
