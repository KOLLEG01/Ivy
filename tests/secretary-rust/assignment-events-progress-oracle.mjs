import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,settings,owner,rootId,at} from './inbox-fixtures.mjs';
import {importEventSubscription,useCurrentAssignmentRecords} from './assignment-import-reference.mjs';
const {AssignmentScheduler}=await load('services/secretary/src/assignment-scheduler.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {defaultConfiguration}=await load('services/secretary/src/assignment-schema.js');
const {IvyError,hashJson,canonical,parseOperationId}=await load('packages/sdk/src/node.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js'),schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const samples=schemaSamples(ajv,[schema]),sample=name=>JSON.parse(samples.find(r=>r.valid&&r.reference.endsWith('#/$defs/'+name)).input);
const assignmentId='00000000-0000-4000-8000-000000000003',progressId='00000000-0000-4000-8000-000000000004',watched='00000000-0000-4000-8000-000000000005';
const trigger={kind:'event',topic:'fixture.events',topicVersion:'1.0.0',sourceServiceNodeId:null,eventKind:null};
const base={...sample('Assignment'),assignmentId:'event-fixture',enabled:true,rules:{},trigger,createdAt:at,updatedAt:at};
const target={serviceNodeId:'fixture.agent',threadCwd:'/workspace',model:null,effort:'medium',permissions:':read-only'};
const seconds=n=>new Date(Date.parse(at)+n*1000).toISOString();
const event=(sequence=2,extra={})=>({sequence,topic:'fixture.events',topicVersion:'1.0.0',source:'service:fixture.source',mutationId:'mutation-'+sequence,occurredAt:seconds(90),payload:{kind:'fixture',text:'Original\ud800'},...extra});
const changed=(sequence=1,revision=1,extra={})=>event(sequence,{topic:'hive.object.changed',occurredAt:extra.occurredAt??seconds((Number(revision)-1)*60),payload:{objectId:assignmentId,revision,operation:'write'},...extra});
const batch=(items=[changed(),event()],extra={})=>({items,throughSequence:items.at(-1)?.sequence??0,gap:null,hasMore:false,...extra});
const gap=prunedThroughSequence=>({prunedThroughSequence});
const doc=(key,value,name=base.assignmentId)=>({id:key,key,name,value});
const stableName='secretary-assignment-'+hashJson(base.assignmentId).slice(7,48);
const rows=[],matches=[];
const pure=new AssignmentScheduler({settings});
for(const selected of [trigger,{...trigger,sourceServiceNodeId:'fixture.source'},{...trigger,eventKind:'fixture'},{...trigger,eventKind:'other'},{...trigger,topic:'hive.object.changed'}, {kind:'object-change',objectIds:[watched],paths:[],delaySeconds:0,observation:null},{kind:'schedule'}])for(const e of [event(),event(2,{source:'principal:'+settings.identity.principalId}),event(2,{source:'service:other'}),event(2,{topicVersion:'2.0.0'}),event(2,{payload:null}),event(2,{payload:[]}),event(2,{payload:{kind:'other'}}),event(2,{topic:'hive.object.changed',payload:{operation:'write',objectId:watched}}),event(2,{topic:'hive.object.changed',source:'principal:'+settings.identity.principalId,payload:{operation:'write',objectId:watched}}),event(2,{topic:'hive.object.changed',payload:{operation:'archive',objectId:watched}}),event(2,{topic:'hive.object.changed',payload:{operation:'write',objectId:7}})]){
 const a={...base,trigger:selected};matches.push({assignment:a,event:e,expected:Boolean(pure.eventMatches(a,e))});
}
async function add(label,c={}){
 const action=c.action??'events',trace=[],steps=[],counts={},records=new Map(),documents=structuredClone(c.documents??[]),mutationIds=new Map();let store,subscribe=0,callIndex=0,nextId=10;
 const versions=c.versions??[{value:structuredClone(base),writtenAt:at},{value:{...base,updatedAt:seconds(60),prompt:'Edited'},writtenAt:seconds(60)}];
 records.set(assignmentId,{key:'secretary/assignment',name:'fixture-assignment',versions:versions.map((v,i)=>({revision:i+1,value:structuredClone(v.value),writtenAt:v.writtenAt}))});
 const latest=records.get(assignmentId).versions.at(-1),assignment={pin:{objectId:assignmentId,revision:latest.revision},value:structuredClone(latest.value),writtenAt:latest.writtenAt};if(c.omitWrittenAt)delete assignment.writtenAt;
 const initialProgress=c.saved===undefined?null:{schemaVersion:1,assignmentId:base.assignmentId,assignment:{objectId:assignmentId,revision:1},assignmentSnapshot:structuredClone(base),processedThrough:0,active:false,needsRebase:false,updatedAt:at,...c.saved};
 if(initialProgress)records.set(progressId,{key:'secretary/event-progress',name:'event-'+hashJson(base.assignmentId).slice(7),versions:[{revision:1,value:initialProgress,writtenAt:at}]});
 const configuration={...defaultConfiguration(settings,at,target),...c.configuration};
 const read=(id,revision)=>{const r=records.get(id);assert.ok(r,'Unknown fixture object '+id);const version=revision===undefined?r.versions.at(-1):r.versions.find(v=>v.revision===revision);assert.ok(version,'Unknown fixture revision '+revision);const value=version.value;return {object:{id,parentId:rootId,effectivelyArchived:false,contractKey:r.key},revision:{revision:version.revision,contractVersion:contractVersion(r.key),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest(r.key,value,{object:{objectId:id,revision:version.revision}},'fixture:1800000000000:read').references,createdAt:version.writtenAt},content:{encoding:'json',value:structuredClone(value)}};};
 const client={async request(method,p,options){
  const copied=structuredClone(p);if(method==='objects.write'||method==='events.unsubscribe'){parseOperationId(copied.mutationId);if(!mutationIds.has(copied.mutationId))mutationIds.set(copied.mutationId,mutationIds.size);copied.mutationId='<operation-'+mutationIds.get(copied.mutationId)+'>';}
  trace.push({method,params:copied,timeout:options?.timeoutMs??35000});counts[method]=(counts[method]??0)+1;
  const fault=c.faults?.find(f=>f.method===method&&f.n===counts[method]);let result,error;
  if(fault&&!fault.commit)error={code:fault.code??'fixture_failure',message:'Fixture failure.',outcome:fault.outcome??'unknown'};
  else if(method==='system.status')result={runtimeEpoch:'fixture'};
  else if(method==='serviceNodes.get')result=owner;
  else if(method==='events.subscribe')result=structuredClone(c.batches?.[subscribe++]??batch());
  else if(method==='events.ack'||method==='events.unsubscribe')result={ok:true};
  else if(method==='objects.query'){
   const name=p.where.args.find(a=>a.field==='object.name').value;result={items:[...records].filter(([,r])=>r.name===name&&r.key===p.contractKey).map(([id,r])=>({objectId:id,revision:r.versions.at(-1).revision})),nextCursor:null};
  }else if(method==='objects.read'){
   if(p.objectId===watched)result={object:{id:watched,effectivelyArchived:false},revision:{revision:p.revision??2},content:{encoding:'json',value:{v:p.revision??2}}};
   else result=read(p.objectId,p.revision);
  }else if(method==='objects.write'){
   const id=p.create?(p.create.contractKey==='secretary/event-progress'?progressId:`00000000-0000-4000-8000-${String(nextId++).padStart(12,'0')}`):p.objectId;
   const r=p.create?{key:p.create.contractKey,name:p.create.name,versions:[]}:records.get(id);
   const revision=p.create?1:r.versions.at(-1).revision+1;if(!p.create)assert.equal(p.expectedRevision,revision-1);
   r.versions.push({revision,value:structuredClone(p.content.value),writtenAt:c.times?.[callIndex]??seconds(120)});records.set(id,r);
   if(fault?.commit)error={code:fault.code??'fixture_failure',message:'Fixture failure.',outcome:fault.outcome??'unknown'};
   else result={object:{id},revision:{revision}};
  }else assert.fail('Unexpected RPC '+method);
  if(error){steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  steps.push({method,value:structuredClone(result)});return result;
 }};
 store=new SecretaryStore(client,rootId);
 for(const d of documents){const encoded=canonical(d.value);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(d.id,d.key,d.name,1,encoded);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(d.id,1,encoded);}
 const recoveryIssues=new Map(c.issues??[]);
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,owner:{serviceNodeId:'fixture.secretary'},recoveryIssues,signal:new AbortController().signal,now:()=>new Date(c.times?.[callIndex]??seconds(120))});
 const scheduler=new AssignmentScheduler(engine),outcomes=[],snapshots=[];
 useCurrentAssignmentRecords(store);
 for(callIndex=0;callIndex<(c.calls??1);callIndex++){
  try{const imported=action!=='revisionAt'&&documents.some(d=>d.key==='secretary/event-subscription')?await importEventSubscription(scheduler,{active:assignment,configuration}):null;
   const value=action==='revisionAt'?await scheduler.revisionAt(assignment,c.at??at):imported&&!imported.completed?false:await scheduler.events(assignment,configuration,c.budget??2);outcomes.push({value});}
  catch(cause){const e=IvyError.from(cause);outcomes.push(cause.code==='secretary_execution_unconfigured'?{code:cause.code,message:cause.message}:{code:e.code,message:e.message,outcome:e.outcome});}
  snapshots.push({issues:[...recoveryIssues],technical:store.db.prepare('SELECT key,name,current_revision,value_json FROM technical_documents ORDER BY key,name').all().map(r=>({key:r.key,name:r.name,revision:r.current_revision,value:JSON.parse(r.value_json)}))});
 }
 rows.push({input:{label,action,settings,assignment,configuration,documents,issues:c.issues??[],budget:c.budget??2,calls:c.calls??1,times:Array.from({length:c.calls??1},(_,i)=>Date.parse(c.times?.[i]??seconds(120))),at:c.at??at,steps},expected:{outcomes,snapshots,trace},serverRecords:[...records].map(([id,r])=>({id,...r}))});store.close();
}
await add('new-progress-activates-then-enqueues');
await add('multiple-snapshots-one-batch',{batches:[batch([changed(),event(),changed(3,2),event(4)])]});
await add('no-progress-empty-batch',{batches:[batch([])]});
await add('no-progress-advanced-subscription-rejected',{batches:[batch([event(5)])]});
await add('no-progress-schedule-revision-one',{versions:[{value:{...base,trigger:{kind:'schedule',cadence:'interval',timeZone:'UTC',intervalMinutes:1,localTime:null,weekdays:[]}},writtenAt:at}]});
await add('zero-budget',{budget:0});
await add('disabled-snapshot-advances-state',{versions:[{value:{...base,enabled:false},writtenAt:at},{value:{...base,enabled:false,updatedAt:seconds(60)},writtenAt:seconds(60)}]});
await add('active-progress-effects-no-state-write',{saved:{active:true},batches:[batch([event()])]});
await add('durable-progress-already-covered',{saved:{processedThrough:2,active:true},batches:[batch([event()])]});
await add('restore-behind-progress-pages',{saved:{processedThrough:5,active:true},batches:[batch([event(2)],{hasMore:true}),batch([event(5)])]});
await add('restore-behind-progress-conflict',{saved:{processedThrough:5,active:true},batches:[batch([event(2)])]});
await add('restore-budget-exhausted',{saved:{processedThrough:5,active:true},budget:1,batches:[batch([event(2)],{hasMore:true})]});
await add('restore-behind-progress-with-gap',{saved:{processedThrough:5,active:true},budget:1,batches:[batch([event(2)],{hasMore:true,gap:gap(1)})]});
await add('progress-identity-mismatch',{saved:{assignmentId:'different'}});
await add('progress-object-mismatch',{saved:{assignment:{objectId:rootId,revision:1}}});
await add('ack-lost-replays-no-effects',{calls:2,faults:[{method:'events.ack',n:1}]});
await add('effect-write-failed-replays',{calls:2,faults:[{method:'objects.write',n:2}]});
await add('effect-write-unknown-reconciles',{calls:2,faults:[{method:'objects.write',n:2,commit:true}]});
await add('progress-save-failed-replays-existing-effect',{calls:2,faults:[{method:'objects.write',n:3}]});
await add('progress-create-unknown-reconciles',{faults:[{method:'objects.write',n:1,commit:true}]});
await add('page-budget-exhaustion',{budget:1,batches:[batch([changed(),event()],{hasMore:true})]});
await add('page-two-historical-edit',{budget:2,batches:[batch([changed(),event()],{hasMore:true}),batch([changed(3,2),event(4)])]});
await add('skip-duplicate-before-saved-progress',{saved:{processedThrough:2,active:true},batches:[batch([event(2),changed(3,2),event(4)])]});
for(const relevant of [undefined,null,false,true])await add('gap-inactive-'+relevant,{saved:{},batches:[batch([event(4)],{gap:gap(3)})],documents:relevant===undefined?[]:[doc('secretary/event-gap',{prunedThroughSequence:1,relevant})]});
await add('gap-active-remains-relevant',{saved:{active:true},batches:[batch([event(4)],{gap:gap(3)})]});
await add('gap-before-assignment-deleted',{saved:{},batches:[batch([event(4,{occurredAt:seconds(-60)})],{gap:gap(3)})],issues:[['event-gap:'+base.assignmentId,'secretary_event_gap']]});
await add('relevant-gap-before-assignment-retained',{saved:{active:true},batches:[batch([event(4,{occurredAt:seconds(-60)})],{gap:gap(3)})]});
await add('gap-rebases-original-snapshot',{saved:{},batches:[batch([event(4,{occurredAt:seconds(30)})],{gap:gap(3)})]});
await add('gap-no-items-then-rebase',{budget:2,saved:{},batches:[batch([],{throughSequence:3,gap:gap(3),hasMore:true}),batch([event(4)])]});
await add('retained-needs-rebase',{saved:{needsRebase:true},batches:[batch([event(4)])],documents:[doc('secretary/event-gap',{prunedThroughSequence:3,relevant:null})]});
await add('legacy-subscription-retired-after-ack',{documents:[doc('secretary/event-subscription',{name:'legacy'})]});
await add('same-subscription-kept',{documents:[doc('secretary/event-subscription',{name:stableName})]});
await add('legacy-not-retired-on-skip-page-zero',{saved:{processedThrough:5,active:true},batches:[batch([event(2)],{hasMore:true}),batch([event(5)])],documents:[doc('secretary/event-subscription',{name:'legacy'})]});
await add('legacy-unsubscribe-failure-retry',{calls:2,documents:[doc('secretary/event-subscription',{name:'legacy'})],faults:[{method:'events.unsubscribe',n:1}]});
await add('unconfigured-does-not-ack-effects',{configuration:{execution:null}});
const objectAssignment={...base,trigger:{kind:'object-change',objectIds:[watched],paths:['/v'],delaySeconds:30,observation:null}};
await add('object-effect-journal-before-progress',{versions:[{value:objectAssignment,writtenAt:at}],batches:[batch([changed(),event(2,{topic:'hive.object.changed',payload:{operation:'write',objectId:watched,revision:2}})])]});
const message={eventId:'message-2',messageChannel:{key:'channel',reference:{conversation:'original'},occurredAt:at,itemId:'item-2'}};
await add('message-effect-journal-before-progress',{batches:[batch([changed(),event(2,{payload:message})])]});
await add('message-invalid-event-identity-no-ack',{batches:[batch([changed(),event(2,{payload:{messageChannel:message.messageChannel}})])]});
await add('non-service-channel-is-generic-event',{batches:[batch([changed(),event(2,{source:'principal:source',payload:message})])]});
await add('null-payload-is-generic-event',{batches:[batch([changed(),event(2,{payload:null})])]});
await add('self-object-event-filtered',{saved:{active:true,assignmentSnapshot:{...base,trigger:{...trigger,topic:'hive.object.changed'}}},batches:[batch([event(2,{source:'principal:'+settings.identity.principalId,topic:'hive.object.changed',payload:{objectId:watched,operation:'write',revision:2}})])]});
for(const revision of ['2','bad',null,false,true,1.5,{toString:'shadow'}])await add('own-revision-'+rows.length,{batches:[batch([changed(1,revision,{occurredAt:at}),event()])]});
for(const method of ['events.subscribe','events.ack','events.unsubscribe','objects.query','objects.read','objects.write','system.status'])await add('fault-'+method,{documents:[doc('secretary/event-subscription',{name:'legacy'})],faults:[{method,n:1}]});
await add('gap-issues-bounded',{saved:{active:true},batches:[batch([event(4)],{gap:gap(3)})],issues:Array.from({length:31},(_,i)=>['original-'+i,'fixture'])});
for(const time of [seconds(-1),at,seconds(30),seconds(60),seconds(90),'invalid'])await add('revision-at-'+time,{action:'revisionAt',at:time});
await add('revision-at-written-time-fallback',{action:'revisionAt',at:seconds(60),omitWrittenAt:true});
process.stdout.write(JSON.stringify({rootId,settings,matches,rows}));
