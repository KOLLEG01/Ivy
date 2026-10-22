import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,settings,rootId,at} from './inbox-fixtures.mjs';
const {AssignmentScheduler}=await load('services/secretary/src/assignment-scheduler.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {defaultConfiguration}=await load('services/secretary/src/assignment-schema.js');
const {objectProjection}=await load('services/secretary/src/assignment-object.js');
const {IvyError,hashJson,canonical,parseOperationId}=await load('packages/sdk/src/node.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const samples=schemaSamples(ajv,[schema]),sample=name=>JSON.parse(samples.find(r=>r.valid&&r.reference.endsWith('#/$defs/'+name)).input);
const assignmentId='00000000-0000-4000-8000-000000000003',watched='00000000-0000-4000-8000-000000000004';
const objectKey='secretary/object-window',messageKey='secretary/channel-window';
const target={serviceNodeId:'fixture.agent',threadCwd:'/workspace',model:null,effort:'medium',permissions:':read-only'};
const objectTrigger={kind:'object-change',objectIds:[watched],paths:['/v'],delaySeconds:30,observation:null};
const assignment={pin:{objectId:assignmentId,revision:2},value:{...sample('Assignment'),assignmentId:'window-fixture',enabled:true,rules:{},trigger:objectTrigger,createdAt:at,updatedAt:at},writtenAt:at};
const seconds=n=>new Date(Date.parse(at)+n*1000).toISOString();
const objectName=(a=assignment,id=watched)=>hashJson([a.value.assignmentId,a.pin.revision,id]);
const messageName=(channel='channel',source='fixture.source')=>hashJson([assignment.value.assignmentId,source,channel]);
const objectEvent=(sequence=1,revision=2,extra={})=>({sequence,source:'service:fixture.source',occurredAt:at,topic:'hive.object.changed',topicVersion:'1.0.0',payload:{operation:'write',objectId:watched,revision},...extra});
const channel=(time=at,itemId='item-1',reference={conversation:'original'})=>({key:'channel',reference,occurredAt:time,itemId});
const messageEvent=(sequence=1,time=at,itemId='item-1',extra={})=>({sequence,source:'service:fixture.source',occurredAt:at,topic:'fixture.messages',topicVersion:'1.0.0',payload:{eventId:'event-'+sequence,messageChannel:channel(time,itemId)},...extra});
const objectWindow=(overrides={},a=assignment)=>{
 const before=objectProjection({v:0},a.value.trigger),after=objectProjection({v:1},a.value.trigger);
 return {assignmentId:a.value.assignmentId,objectId:watched,lastSequence:1,fingerprint:hashJson(after),projection:after,revision:2,pending:{assignment:a.pin,assignmentSnapshot:a.value,objectId:watched,firstSequence:1,firstRevision:1,lastRevision:2,occurredAt:at,dueAt:seconds(30),before,after},...overrides};
};
const messageWindow=overrides=>({assignmentId:assignment.value.assignmentId,lastSequence:1,pending:{assignment:assignment.pin,assignmentSnapshot:assignment.value,firstSequence:1,firstMessageAt:at,dueAt:seconds(300),serviceNodeId:'fixture.source',channel:channel(),itemIds:['item-1'],eventIds:['event-1']},...overrides});
const doc=(key,name,value,id='window')=>({id,key,name,value});
const collectObject=(event=objectEvent(),extra={})=>({kind:'collectObjectEvent',event,...extra});
const collectMessage=(event=messageEvent(),extra={})=>({kind:'collectMessageEvent',event,...extra});
const flushObject=(extra={})=>({kind:'flushDueObjectWindows',now:seconds(60),...extra});
const flushMessage=(extra={})=>({kind:'flushDueMessageWindows',now:seconds(300),...extra});
const rows=[];
async function add(label,actions,c={}){
 let currentAction={},store,nextId=10,reads=0;
 const records=new Map(),trace=[],steps=[],counts={},mutations=new Map(),documents=structuredClone(c.documents??[]);
 const selected={...structuredClone(assignment),...c.assignment};
 const configuration={...defaultConfiguration(settings,at,target),...c.configuration};
 const history=c.history??[{v:0},{v:1}];
 const read=(id,r)=>{const version=r.versions.at(-1),value=version.value;return {object:{id,parentId:rootId,effectivelyArchived:false,contractKey:r.key},revision:{revision:version.revision,contractVersion:contractVersion(r.key),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest(r.key,value,{object:{objectId:id,revision:version.revision}},'fixture:1800000000000:read').references,createdAt:at},content:{encoding:'json',value:structuredClone(value)}};};
 const client={async request(method,p,options){
  const copied=structuredClone(p);if(method==='objects.write'){parseOperationId(copied.mutationId);if(!mutations.has(copied.mutationId))mutations.set(copied.mutationId,mutations.size);copied.mutationId='<scoped-operation-'+mutations.get(copied.mutationId)+'>';}
  trace.push({method,params:copied,timeout:options?.timeoutMs??35000});counts[method]=(counts[method]??0)+1;
  const fault=c.faults?.find(f=>f.method===method&&f.n===counts[method]);let result,error;
  if(fault&&!fault.commit)error={code:fault.code??'fixture_failure',message:'Fixture failure.',outcome:fault.outcome??'unknown'};
  else if(method==='system.status')result={runtimeEpoch:'fixture'};
  else if(method==='objects.query'){
   const name=p.where.args.find(a=>a.field==='object.name').value;result={items:[...records].filter(([,r])=>r.name===name&&r.key===p.contractKey).map(([id,r])=>({objectId:id,revision:r.versions.at(-1).revision})),nextCursor:null};
  }else if(method==='objects.read'){
   if(records.has(p.objectId))result=read(p.objectId,records.get(p.objectId));
   else {
    const override=c.reads?.[reads++];
    const revision=p.revision??currentAction.current?.revision??history.length;
    const data=override?.value??currentAction.current?.value??history[revision-1]??{v:revision};
    result={object:{id:p.objectId,effectivelyArchived:override?.archived??currentAction.current?.archived??false},revision:{revision:override?.revision??revision},content:{encoding:override?.encoding??currentAction.current?.encoding??'json',value:structuredClone(data)}};
   }
  }else if(method==='objects.write'){
   const id=p.create?`00000000-0000-4000-8000-${String(nextId++).padStart(12,'0')}`:p.objectId;
   const record=p.create?{key:p.create.contractKey,name:p.create.name,versions:[]}:records.get(id);
   const revision=p.create?1:record.versions.at(-1).revision+1;
   if(!p.create)assert.equal(p.expectedRevision,revision-1);
   record.versions.push({revision,value:structuredClone(p.content.value)});records.set(id,record);
   if(fault?.commit)error={code:fault.code??'fixture_failure',message:'Fixture failure.',outcome:fault.outcome??'unknown'};
   else result={object:{id},revision:{revision}};
  }else assert.fail('Unexpected RPC '+method);
  if(error){steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  steps.push({method,value:structuredClone(result)});return result;
 }};
 store=new SecretaryStore(client,rootId);
 for(const d of documents){const encoded=canonical(d.value);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(d.id,d.key,d.name,1,encoded);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(d.id,1,encoded);}
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,signal:new AbortController().signal,now:()=>new Date(currentAction.enqueueAt??at)});
 const scheduler=new AssignmentScheduler(engine),snapshots=[],outcomes=[];
 const snapshot=()=>store.db.prepare('SELECT key,name,current_revision,value_json FROM technical_documents ORDER BY key,name').all().map(r=>({key:r.key,name:r.name,revision:r.current_revision,value:JSON.parse(r.value_json)}));
 for(const action of actions){
  if(action.kind==='flushMessageWindow')action.name??=messageName();
  currentAction=action;store.limits=action.limits??{count:10000,bytes:256*1024*1024};
  const a=action.assignment??selected,config=action.configuration??configuration;
  try{
   if(action.kind==='collectObjectEvent')await scheduler.collectObjectEvent(a,action.event);
   else if(action.kind==='collectMessageEvent')await scheduler.collectMessageEvent(a,action.event,config);
   else if(action.kind==='flushMessageWindow')await scheduler.flushMessageWindow(store.technicalNamed(messageKey,action.name??messageName()),config);
   else await scheduler[action.kind](a,config,new Date(action.now??seconds(600)));
   outcomes.push({value:null});
  }catch(cause){const e=IvyError.from(cause);outcomes.push(cause.code==='secretary_execution_unconfigured'?{code:cause.code,message:cause.message}:{code:e.code,message:e.message,outcome:e.outcome});}
  snapshots.push(snapshot());
 }
 rows.push({input:{label,settings,assignment:selected,configuration,documents,actions,steps},expected:{outcomes,snapshots,trace},serverRecords:[...records].map(([id,r])=>({id,...r}))});store.close();
}
await add('object-collect-and-flush',[collectObject(),flushObject(),flushObject()]);
await add('object-initial-revision',[collectObject(objectEvent(1,1)),flushObject()]);
await add('object-unchanged-projection',[collectObject(),flushObject()],{history:[{v:1,ignored:0},{v:1,ignored:1}]});
await add('object-duplicate-sequence',[collectObject(),collectObject(),collectObject(objectEvent(0)),flushObject()]);
await add('object-update-and-revert',[collectObject(),collectObject(objectEvent(2,3)),flushObject()],{history:[{v:0},{v:1},{v:0}]});
await add('object-update-keeps-first-boundary',[collectObject(),collectObject(objectEvent(2,3,{occurredAt:seconds(20)})),flushObject()],{history:[{v:0},{v:1},{v:2}]});
await add('object-not-due',[collectObject(),flushObject({now:seconds(29)})]);
await add('object-racing-current',[collectObject(),flushObject({current:{revision:3,value:{v:2}}}),flushObject()]);
await add('object-current-archived',[collectObject(),flushObject({current:{archived:true}})]);
await add('object-current-not-json',[collectObject(),flushObject({current:{encoding:'text'}})]);
await add('object-observed-not-json',[collectObject()],{reads:[{encoding:'text'}]});
await add('object-prior-not-json',[collectObject()],{reads:[{}, {encoding:'text'}]});
await add('object-bad-event-time-retains-initial',[collectObject(objectEvent(1,2,{occurredAt:'invalid'}))]);
await add('object-retry-after-create-unknown',[collectObject(),flushObject(),flushObject()],{faults:[{method:'objects.write',n:1,commit:true}]});
await add('object-retry-after-write-failure',[collectObject(),flushObject(),flushObject()],{faults:[{method:'objects.write',n:1}]});
await add('object-retry-after-local-save-failure',[collectObject(),flushObject({limits:{count:10000,bytes:0}}),flushObject()]);
await add('object-journal-create-full',[collectObject(objectEvent(),{limits:{count:0,bytes:0}})]);
await add('object-foreign-assignment-skipped',[flushObject()],{documents:[doc(objectKey,objectName(),objectWindow({assignmentId:'foreign'}))]});
await add('object-empty-window',[flushObject()],{documents:[doc(objectKey,objectName(),objectWindow({pending:null}))]});
await add('object-old-snapshot-used',[flushObject({assignment:{...assignment,pin:{...assignment.pin,revision:3},value:{...assignment.value,prompt:'Current edited prompt',trigger:{kind:'event',topic:'fixture',topicVersion:'1.0.0',sourceServiceNodeId:null,eventKind:null}}}})],{documents:[doc(objectKey,objectName(),objectWindow())]});
await add('object-pending-wrong-trigger',[flushObject()],{documents:[doc(objectKey,objectName(),objectWindow({pending:{...objectWindow().pending,assignmentSnapshot:{...assignment.value,trigger:{kind:'schedule'}}}}))]});
const observed={...assignment,value:{...assignment.value,trigger:{...objectTrigger,observation:{completePath:'/complete',observedAtPath:'/observedAt',maximumAgeSeconds:30}}}};
for(const [label,data] of [['ready',{v:1,complete:true,observedAt:seconds(60)}],['incomplete',{v:1,complete:false,observedAt:seconds(60)}],['stale',{v:1,complete:true,observedAt:seconds(29)}],['future',{v:1,complete:true,observedAt:seconds(66)}],['old',{v:1,complete:true,observedAt:seconds(30)}]])await add('object-observation-'+label,[collectObject(),flushObject({now:seconds(61),current:{revision:2,value:data}})],{assignment:observed});
for(const revision of [0,-1,1.5,9007199254740992,null,false,true,'2',' 2 ','\uFEFF0x2','0b10','0o2','+2','2.0','2e0','+0x2','2x','',[],[2],[[2]],[null],{}, {toString:'shadow'},['2','3'],'\ud800'])await add('object-revision-'+rows.length,[collectObject(objectEvent(1,revision))]);
for(const payload of [null,{},[],{objectId:watched}])await add('object-payload-'+rows.length,[collectObject(objectEvent(1,2,{payload}))]);
await add('object-irrelevant-trigger-no-work',[collectObject()],{assignment:{...assignment,value:{...assignment.value,trigger:{kind:'event'}}}});
await add('message-open-append-flush',[collectMessage(),collectMessage(messageEvent(2,seconds(120),'item-2')),flushMessage(),flushMessage()]);
await add('message-duplicate-and-order',[collectMessage(),collectMessage(messageEvent(1,seconds(400))),collectMessage(messageEvent(0)),collectMessage(messageEvent(2,seconds(-20),'item-1')),flushMessage()]);
await add('message-boundary-new-window',[collectMessage(),collectMessage(messageEvent(2,seconds(300),'item-2')),flushMessage({now:seconds(600)})]);
await add('message-before-boundary-same-window',[collectMessage(),collectMessage(messageEvent(2,seconds(299),'item-2')),flushMessage()]);
await add('message-not-due',[collectMessage(),flushMessage({now:seconds(299)})]);
await add('message-retained-snapshot',[collectMessage(),collectMessage(messageEvent(2,seconds(60),'item-2'),{assignment:{...assignment,pin:{...assignment.pin,revision:3},value:{...assignment.value,prompt:'Edited'}}}),flushMessage()]);
for(const itemId of [null,'','\ud800'])await add('message-item-'+rows.length,[collectMessage(messageEvent(1,at,itemId)),flushMessage()]);
await add('message-reference-conflict-before-flush',[collectMessage(),collectMessage(messageEvent(2,seconds(300),'item-2',{payload:{eventId:'event-2',messageChannel:channel(seconds(300),'item-2',{conversation:'changed'})}}))]);
await add('message-reference-order-same',[collectMessage(messageEvent(1,at,'item-1',{payload:{eventId:'event-1',messageChannel:channel(at,'item-1',{a:1,b:2})}})),collectMessage(messageEvent(2,seconds(60),'item-2',{payload:{eventId:'event-2',messageChannel:channel(seconds(60),'item-2',{b:2,a:1})}})),flushMessage()]);
await add('message-boundary-flush-retry',[collectMessage(),collectMessage(messageEvent(2,seconds(300))),collectMessage(messageEvent(2,seconds(300))),flushMessage({now:seconds(600)})],{faults:[{method:'objects.write',n:1}]});
await add('message-local-save-after-enqueue-retry',[collectMessage(),flushMessage({limits:{count:10000,bytes:0}}),flushMessage()]);
await add('message-create-journal-full',[collectMessage(messageEvent(),{limits:{count:0,bytes:0}})]);
await add('message-foreign-window',[flushMessage()],{documents:[doc(messageKey,messageName(),messageWindow({assignmentId:'foreign'}))]});
await add('message-empty-window',[{kind:'flushMessageWindow'}],{documents:[doc(messageKey,messageName(),messageWindow({pending:null}))]});
await add('message-retained-windows-ordered',[flushMessage()],{documents:[doc(messageKey,messageName(),messageWindow(), 'a'),doc(messageKey,messageName('second'),messageWindow({pending:{...messageWindow().pending,firstSequence:2,eventIds:['second'],channel:{...channel(),key:'second'}}}),'b')]});
for(const payload of [null,[],{}, {eventId:1,messageChannel:channel()},{eventId:'event',messageChannel:{...channel(),occurredAt:'invalid'}},{eventId:'event',messageChannel:{...channel(),key:''}},{eventId:'event',messageChannel:{...channel(),reference:[]}},{eventId:'event',messageChannel:{...channel(),itemId:3}}])await add('message-invalid-'+rows.length,[collectMessage(messageEvent(1,at,'item',{payload}))]);
await add('message-wrong-source',[collectMessage(messageEvent(1,at,'item',{source:'principal:source'}))]);
await add('message-empty-event-id',[collectMessage(messageEvent(1,at,'item',{payload:{eventId:'',messageChannel:channel()}})),flushMessage()]);
await add('message-date-overflow',[collectMessage(messageEvent(1,'+275760-09-13T00:00:00.000Z'))]);
for(const method of ['objects.query','objects.read','objects.write','system.status']){
 await add('object-flush-failure-'+method,[collectObject(),flushObject(),flushObject()],{faults:[{method,n:method==='objects.read'?3:1}]});
 await add('message-flush-failure-'+method,[collectMessage(),flushMessage(),flushMessage()],{faults:[{method,n:1}]});
}
for(const action of [collectMessage(),collectObject()])await add('unconfigured-retains-window-'+rows.length,[action,action.kind==='collectMessageEvent'?flushMessage():flushObject()],{configuration:{execution:null}});
process.stdout.write(JSON.stringify({rootId,rows}));
