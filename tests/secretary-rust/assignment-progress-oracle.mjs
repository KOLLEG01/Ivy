import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,settings,owner,rootId,at} from './inbox-fixtures.mjs';
import {importScheduleProgress,importEventSubscription,useCurrentAssignmentRecords} from './assignment-import-reference.mjs';
const {AssignmentScheduler}=await load('services/secretary/src/assignment-scheduler.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {defaultConfiguration,assignmentName}=await load('services/secretary/src/assignment-schema.js');
const {IvyError,hashJson,canonical,parseOperationId}=await load('packages/sdk/src/node.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const samples=schemaSamples(ajv,[schema]),sample=name=>JSON.parse(samples.find(r=>r.valid&&r.reference.endsWith('#/$defs/'+name)).input);
const objectId='00000000-0000-4000-8000-000000000003',progressId='00000000-0000-4000-8000-000000000004';
const target={serviceNodeId:'fixture.agent',threadCwd:'/workspace',model:null,effort:'medium',permissions:':read-only'};
const interval={kind:'schedule',cadence:'interval',timeZone:'UTC',intervalMinutes:60,localTime:null,weekdays:[]};
const base={...sample('Assignment'),assignmentId:'scheduled-fixture',enabled:true,rules:{},trigger:interval,createdAt:at,updatedAt:at};
const minutes=n=>new Date(Date.parse(at)+n*60000).toISOString();
const scanRow=(id=objectId,archived=false,revision=1,key=base.assignmentId)=>({objectId:id,revision,values:{'object.effectivelyArchived':archived,'object.archivedAt':archived?at:null,'object.name':assignmentName(key),'data:/assignmentId':key}});
const stableName='secretary-assignment-'+hashJson(base.assignmentId).slice(7,48);
const errorResult=cause=>{const e=IvyError.from(cause);return cause.code==='secretary_execution_unconfigured'?{code:cause.code,message:cause.message}:{code:e.code,message:e.message,outcome:e.outcome};};
const rows=[],occurrences=[];
const pure=new AssignmentScheduler({settings});
for(const cadence of ['interval','daily','weekly'])for(const zone of ['UTC','Europe/Berlin','America/New_York'])for(const [start,end] of [
 ['2026-03-28T00:00:00.000Z','2026-03-30T12:00:00.000Z'],['2026-10-24T00:00:00.000Z','2026-10-26T12:00:00.000Z'],[at,minutes(15)],[minutes(-180),at],[at,'2026-02-01T00:00:00.000Z'],[minutes(30),at]
]){
 const assignment={...base,trigger:cadence==='interval'?{...interval,intervalMinutes:15}:{...interval,cadence,timeZone:zone,intervalMinutes:null,localTime:'02:30',weekdays:cadence==='weekly'?[0,1,6]:[]}};
 const value=pure.occurrences(assignment,new Date(end),start);
 occurrences.push({assignment,time:Date.parse(end),checkedAt:start,expected:{value}});
}
for(const [trigger,checkedAt,time] of [[interval,'invalid',at],[interval,minutes(60),at],[interval,'2026-01-01',at],[{kind:'event'},at,minutes(-60)],[{kind:'event'},at,minutes(60)]]){
 const assignment={...base,trigger};let expected;try{expected={value:pure.occurrences(assignment,new Date(time),checkedAt)};}catch(e){expected=errorResult(e);}
 occurrences.push({assignment,time:Date.parse(time),checkedAt,expected});
}
async function add(label,c={}){
 const action=c.action??'schedules',trace=[],steps=[],counts={},records=new Map(),technicalBefore=structuredClone(c.technical??[]),mutationIds=new Map();
 const caseSettings={...settings,recordsPerTick:c.limit??2};
 const configuration={...defaultConfiguration(caseSettings,at,target),...c.configuration};
 const versions=c.versions??[{value:{...base,...c.assignment},writtenAt:at}];
 records.set(objectId,{key:'secretary/assignment',name:assignmentName(base.assignmentId),versions:versions.map((v,i)=>({revision:i+1,value:structuredClone(v.value),writtenAt:v.writtenAt??v.value.updatedAt}))});
 const latest=records.get(objectId).versions.at(-1),assignment={pin:{objectId,revision:latest.revision},value:structuredClone(latest.value),writtenAt:latest.writtenAt};
 if(c.omitWrittenAt)delete assignment.writtenAt;
 if(c.saved){const progress={schemaVersion:1,assignmentId:base.assignmentId,assignment:{objectId,revision:c.retainedRevision??1},assignmentSnapshot:structuredClone(versions[(c.retainedRevision??1)-1]?.value??base),checkedAt:at,updatedAt:at,...c.saved};records.set(progressId,{key:'secretary/schedule-progress',name:'schedule-'+hashJson(base.assignmentId).slice(7),versions:[{revision:1,value:progress,writtenAt:at}]});}
 let store,queries=0,nextId=10;
 const read=(id,revision)=>{const record=records.get(id);assert.ok(record,'Unknown fixture object '+id);const rev=revision===undefined?record.versions.at(-1):record.versions.find(v=>v.revision===revision);assert.ok(rev,'Unknown fixture revision '+revision);const value=rev.value;return {object:{id,parentId:rootId,effectivelyArchived:false,contractKey:record.key},revision:{revision:rev.revision,contractVersion:contractVersion(record.key),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest(record.key,value,{object:{objectId:id,revision:rev.revision}},'fixture:1800000000000:read').references,createdAt:rev.writtenAt},content:{encoding:'json',value:structuredClone(value)}};};
 const controller=new AbortController();if(c.cancel)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const client={async request(method,p,options){
  const copied=structuredClone(p);if(method==='objects.write'||method==='events.unsubscribe'){parseOperationId(copied.mutationId);if(!mutationIds.has(copied.mutationId))mutationIds.set(copied.mutationId,mutationIds.size);copied.mutationId='<scoped-operation-'+mutationIds.get(copied.mutationId)+'>';}
  trace.push({method,params:copied,timeout:options?.timeoutMs??35000});counts[method]=(counts[method]??0)+1;
  const fault=c.faults?.find(f=>f.method===method&&f.n===counts[method]);let result,error;
  if(c.fail===method||fault)error={code:fault?.code??'fixture_failure',message:'Fixture failure.',outcome:fault?.outcome??'unknown'};
  else if(method==='system.status')result={runtimeEpoch:'fixture'};
  else if(method==='serviceNodes.get')result=owner;
  else if(method==='events.unsubscribe')result={removed:true};
  else if(method==='objects.query'){
   if(p.contractKey==='secretary/assignment'&&p.select)result=c.pages?.[queries++]??{items:[scanRow()],nextCursor:null};
   else {const name=p.where.args.find(a=>a.field==='object.name').value;result={items:[...records].filter(([,r])=>r.key===p.contractKey&&r.name===name).map(([id,r])=>({objectId:id,revision:r.versions.at(-1).revision})),nextCursor:null};}
  }else if(method==='objects.read'){
   result=read(p.objectId,p.revision);if(c.badRead)result={...result,object:{...result.object,contractKey:'other'}};
  }else if(method==='objects.write'){
   const id=p.create?(p.create.contractKey==='secretary/schedule-progress'?progressId:`00000000-0000-4000-8000-${String(nextId++).padStart(12,'0')}`):p.objectId;
   const record=p.create?{key:p.create.contractKey,name:p.create.name,versions:[]}:records.get(id);
   if(!p.create)assert.equal(p.expectedRevision,record.versions.at(-1).revision);
   const revision=p.create?1:record.versions.at(-1).revision+1;
   record.versions.push({revision,value:structuredClone(p.content.value),writtenAt:c.now??minutes(180)});records.set(id,record);
   if(c.race&&p.create)error={code:'revision_conflict',message:'Fixture competing create.',outcome:'not_executed'};
   else result={object:{id},revision:{revision}};
  }else assert.fail('Unexpected RPC '+method);
  if(error){steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  steps.push({method,value:structuredClone(result)});return result;
 }};
 store=new SecretaryStore(client,rootId);for(const doc of technicalBefore)store.technicalCreate(doc.key,doc.name,doc.value);
 const recoveryIssues=new Map(c.issues??[]);
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:caseSettings,owner:{serviceNodeId:'fixture.secretary'},signal:controller.signal,recoveryIssues,now:()=>new Date(c.now??minutes(180))});
 const scheduler=new AssignmentScheduler(engine);scheduler.cursor=c.cursor;scheduler.retiredAssignments=new Map(c.retired??[]);
 const archiveImportRow=technicalBefore.some(d=>d.key==='secretary/event-subscription')?c.pages?.flatMap(p=>p.items).find(r=>r.values['object.effectivelyArchived']):null;
 let result;
 try{
  if(archiveImportRow)await importEventSubscription(scheduler,{archived:archiveImportRow});
  if(action==='schedules'&&technicalBefore.some(d=>d.key==='secretary/schedule-cursor'))await importScheduleProgress(scheduler,assignment,Date.parse(c.now??minutes(180)));
  useCurrentAssignmentRecords(store);
  if(action==='assignments'){const values=[];for(let i=0;i<(c.scans??1);i++)values.push(await scheduler.assignments());result={value:values};}
  else {for(let i=0;i<(c.scans??1);i++)await scheduler.schedules(assignment,configuration,new Date(c.now??minutes(180)));result={value:null};}
 }catch(e){result=errorResult(e);}
 const technical=[...new Set(technicalBefore.map(d=>d.key))].flatMap(key=>store.technicalList(key).map(d=>({key,name:d.name,value:d.value}))).sort((a,b)=>a.name.localeCompare(b.name));
 const state={cursor:scheduler.cursor??null,retired:[...scheduler.retiredAssignments],issues:[...recoveryIssues]};
 rows.push({input:{label,action,settings:caseSettings,configuration,assignment,archiveImportRow:archiveImportRow??null,time:Date.parse(c.now??minutes(180)),scans:c.scans??1,cursor:c.cursor??null,retired:c.retired??[],issues:c.issues??[],cancel:c.cancel??false,technical:technicalBefore,steps},expected:{result,trace,state,technical},serverRecords:[...records].map(([id,r])=>({id,...r}))});
 store.close();
}
await add('new-progress-original');
await add('new-progress-written-at-fallback',{omitWrittenAt:true,now:minutes(30)});
await add('new-progress-sub-hour-no-save',{now:minutes(30)});
await add('progress-hourly-save-no-occurrence',{assignment:{trigger:{kind:'event',topic:'fixture',topicVersion:'1.0.0',sourceServiceNodeId:null,eventKind:null}},now:minutes(70)});
await add('catchup-four-pages',{now:minutes(1440)});
await add('catchup-repeat-no-duplicate',{now:minutes(240),scans:2});
await add('create-progress-race',{race:true});
await add('no-execution-before-progress-save',{configuration:{execution:null}});
await add('legacy-current',{technical:[{key:'secretary/schedule-cursor',name:base.assignmentId,value:{updatedAt:at,checkedAt:minutes(60)}}]});
await add('legacy-rev-one-identity',{technical:[{key:'secretary/schedule-cursor',name:base.assignmentId,value:{updatedAt:minutes(-60),checkedAt:minutes(60)}}]});
await add('legacy-original-history',{versions:[{value:base,writtenAt:at},{value:{...base,updatedAt:minutes(120),prompt:'Edited'},writtenAt:minutes(120)}],technical:[{key:'secretary/schedule-cursor',name:base.assignmentId,value:{updatedAt:at,checkedAt:minutes(60)}}]});
await add('legacy-latest-snapshot',{versions:[{value:base,writtenAt:at},{value:{...base,updatedAt:minutes(120),prompt:'Edited'},writtenAt:minutes(120)}],technical:[{key:'secretary/schedule-cursor',name:base.assignmentId,value:{updatedAt:minutes(120),checkedAt:minutes(150)}}]});
for(const enabled of [false,true])await add('revision-switch-'+enabled,{versions:[{value:{...base,enabled},writtenAt:at},{value:{...base,updatedAt:minutes(90),trigger:{...interval,intervalMinutes:30},prompt:'Edited'},writtenAt:minutes(90)}]});
await add('future-revision',{versions:[{value:base,writtenAt:at},{value:{...base,updatedAt:minutes(300),prompt:'Future'},writtenAt:minutes(300)}]});
await add('many-revision-four-page-bound',{now:minutes(30),versions:Array.from({length:7},(_,i)=>({value:{...base,updatedAt:minutes(i),prompt:'revision '+i},writtenAt:minutes(i)}))});
await add('schedule-to-event-revision',{versions:[{value:base,writtenAt:at},{value:{...base,updatedAt:minutes(90),trigger:{kind:'event',topic:'fixture',topicVersion:'1.0.0',sourceServiceNodeId:null,eventKind:null}},writtenAt:minutes(90)}]});
await add('disabled-invalid-date',{saved:{assignmentSnapshot:{...base,enabled:false},checkedAt:'2026-99-01T00:00:00.000Z'}});
for(const saved of [{},{assignmentId:'different'},{assignment:{objectId:rootId,revision:1}},{assignment:{objectId,revision:2}},{checkedAt:minutes(300)},{checkedAt:'2026-99-01T00:00:00.000Z'}])await add('saved-'+rows.length,{saved});
for(const [method,n] of [['objects.query',1],['objects.write',1],['objects.write',2],['objects.write',3],['objects.read',1],['objects.read',3],['system.status',1]])await add('failure-'+method+'-'+n,{faults:[{method,n}]});
await add('assignment-active',{action:'assignments'});
await add('assignment-page-cursor',{action:'assignments',cursor:'original-cursor',pages:[{items:[scanRow()],nextCursor:'next'},{items:[],nextCursor:''}],scans:2});
await add('archive-retired-once',{action:'assignments',pages:Array.from({length:2},()=>({items:[scanRow(objectId,true)],nextCursor:null})),scans:2,issues:[['assignment:'+base.assignmentId,'old'],['assignment-record:'+objectId,'old']]});
for(const name of ['legacy-subscription',stableName])await add('archive-legacy-'+name,{action:'assignments',pages:[{items:[scanRow(objectId,true)],nextCursor:null}],technical:[{key:'secretary/event-subscription',name:base.assignmentId,value:{name}}]});
await add('archive-revision-retires-again',{action:'assignments',pages:[{items:[scanRow(objectId,true,1)],nextCursor:null},{items:[scanRow(objectId,true,2)],nextCursor:null}],scans:2});
await add('archive-restore-archive',{action:'assignments',pages:[{items:[scanRow(objectId,true)],nextCursor:null},{items:[scanRow()],nextCursor:null},{items:[scanRow(objectId,true)],nextCursor:null}],scans:3});
await add('archive-identity-rejected',{action:'assignments',pages:[{items:[{...scanRow(objectId,true),values:{...scanRow(objectId,true).values,'object.name':'different'}}],nextCursor:null}]});
await add('assignment-invalid-keeps-other-work',{action:'assignments',badRead:true});
await add('archive-unsubscribe-failure-retry',{action:'assignments',pages:Array.from({length:2},()=>({items:[scanRow(objectId,true)],nextCursor:null})),scans:2,faults:[{method:'events.unsubscribe',n:1}]});
await add('archive-legacy-second-failure',{action:'assignments',pages:[{items:[scanRow(objectId,true)],nextCursor:null}],technical:[{key:'secretary/event-subscription',name:base.assignmentId,value:{name:'legacy'}}],faults:[{method:'events.unsubscribe',n:2}]});
await add('archive-cancel-rethrows',{action:'assignments',cancel:true,pages:[{items:[scanRow(objectId,true)],nextCursor:null}],fail:'events.unsubscribe'});
await add('assignment-query-error',{action:'assignments',fail:'objects.query'});
await add('assignment-read-error',{action:'assignments',fail:'objects.read'});
await add('assignment-issues-bounded',{action:'assignments',badRead:true,issues:Array.from({length:31},(_,i)=>['original-'+i,'fixture'])});
process.stdout.write(JSON.stringify({rootId,settings,rows,occurrences}));
