import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,settings,owner,rootId,at} from './inbox-fixtures.mjs';
const {AssignmentScheduler}=await load('services/secretary/src/assignment-scheduler.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {defaultConfiguration,assignmentName}=await load('services/secretary/src/assignment-schema.js');
const {IvyError,hashJson,canonical,parseOperationId}=await load('packages/sdk/src/node.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js'),schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const samples=schemaSamples(ajv,[schema]),sample=name=>JSON.parse(samples.find(r=>r.valid&&r.reference.endsWith('#/$defs/'+name)).input);
const target={serviceNodeId:'fixture.agent',threadCwd:'/workspace',model:null,effort:'medium',permissions:':read-only'};
const configurationId='00000000-0000-4000-8000-000000000002',watched='00000000-0000-4000-8000-000000000099';
const seconds=n=>new Date(Date.parse(at)+n*1000).toISOString();
const eventTrigger={kind:'event',topic:'fixture.events',topicVersion:'1.0.0',sourceServiceNodeId:null,eventKind:null};
const scheduleTrigger={kind:'schedule',cadence:'interval',timeZone:'UTC',intervalMinutes:1,localTime:null,weekdays:[]};
const objectTrigger={kind:'object-change',objectIds:[watched],paths:['/v'],delaySeconds:30,observation:null};
const event=(sequence=2,extra={})=>({sequence,topic:'fixture.events',topicVersion:'1.0.0',source:'service:fixture.source',mutationId:'mutation-'+sequence,occurredAt:seconds(90),payload:{kind:'fixture'},...extra});
const ownEvent=(assignment,revision=1)=>event(1,{topic:'hive.object.changed',occurredAt:at,payload:{operation:'write',objectId:assignment.pin.objectId,revision}});
const batch=(items=[],extra={})=>({items,throughSequence:items.at(-1)?.sequence??0,gap:null,hasMore:false,...extra});
const doc=(key,value,name='assignment-0')=>({id:key+'-'+name,key,name,value});
const rows=[];
async function add(label,c={}){
 const records=new Map(),trace=[],steps=[],counts={},mutationIds=new Map(),subscribeCounts={},nowSamples=[],documents=structuredClone(c.documents??[]);let store,nextId=1000,nowIndex=0,tickIndex=0;
 const caseSettings={...settings,recordsPerTick:100};
 const assignments=Array.from({length:c.n??1},(_,i)=>{
  const value={...sample('Assignment'),assignmentId:'assignment-'+i,enabled:true,rules:{},trigger:eventTrigger,createdAt:at,updatedAt:at,...c.assignment,...c.assignmentOverrides?.[i]};
  const pin={objectId:`00000000-0000-4000-8000-${String(100+i).padStart(12,'0')}`,revision:c.revision??1};
  const versions=Array.from({length:pin.revision},(_,n)=>({revision:n+1,value:structuredClone(value),writtenAt:seconds(n*60)}));
  records.set(pin.objectId,{key:'secretary/assignment',name:assignmentName(value.assignmentId),versions});
  return {pin,value,writtenAt:versions.at(-1).writtenAt};
 });
 const configuration={...defaultConfiguration(caseSettings,at,target),...c.configuration};
 if(!c.missingConfig)records.set(configurationId,{key:'secretary/configuration',name:'configuration',versions:[{revision:1,value:structuredClone(configuration),writtenAt:at}]});
 for(const retained of c.progress??[]){
  const assignment=assignments[retained.index??0],kind=retained.kind??'schedule';
  const value=kind==='schedule'?{schemaVersion:1,assignmentId:assignment.value.assignmentId,assignment:assignment.pin,assignmentSnapshot:structuredClone(assignment.value),checkedAt:at,updatedAt:at,...retained.value}:{schemaVersion:1,assignmentId:assignment.value.assignmentId,assignment:assignment.pin,assignmentSnapshot:structuredClone(assignment.value),processedThrough:0,active:false,needsRebase:false,updatedAt:at,...retained.value};
  records.set('00000000-0000-4000-8000-'+String(nextId++).padStart(12,'0'),{key:'secretary/'+(kind==='schedule'?'schedule-progress':'event-progress'),name:kind+'-'+hashJson(assignment.value.assignmentId).slice(7),versions:[{revision:1,value,writtenAt:at}]});
 }
 if(c.windows){
  const a=assignments[0],channel={key:'channel',reference:{conversation:'original'},occurredAt:seconds(-200),itemId:'item'};
  documents.push(doc('secretary/channel-window',{assignmentId:a.value.assignmentId,lastSequence:1,pending:{assignment:a.pin,assignmentSnapshot:a.value,firstSequence:1,firstMessageAt:seconds(-200),dueAt:seconds(100),serviceNodeId:'fixture.source',channel,itemIds:['item'],eventIds:['event']}},'message'));
  const before={'/v':0},after={'/v':1};
  documents.push(doc('secretary/object-window',{assignmentId:a.value.assignmentId,objectId:watched,lastSequence:1,fingerprint:hashJson(after),projection:after,revision:2,pending:{assignment:a.pin,assignmentSnapshot:{...a.value,trigger:objectTrigger},objectId:watched,firstSequence:1,firstRevision:1,lastRevision:2,occurredAt:at,dueAt:seconds(30),before,after}},'object'));
 }
 const controller=new AbortController();if(c.cancel)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const read=(id,revision)=>{
  const r=records.get(id);assert.ok(r,'Unknown fixture object '+id);const version=revision===undefined?r.versions.at(-1):r.versions.find(v=>v.revision===revision);assert.ok(version,'Unknown fixture revision '+revision);const value=version.value;
  return {object:{id,parentId:rootId,effectivelyArchived:false,contractKey:c.badRead&&r.key==='secretary/assignment'?'wrong':r.key},revision:{revision:version.revision,contractVersion:contractVersion(r.key),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest(r.key,value,{object:{objectId:id,revision:version.revision}},'fixture:1800000000000:read').references,createdAt:version.writtenAt},content:{encoding:'json',value:structuredClone(value)}};
 };
 const client={async request(method,p,options){
  const copied=structuredClone(p);if(method==='objects.write'||method==='events.unsubscribe'){parseOperationId(copied.mutationId);if(!mutationIds.has(copied.mutationId))mutationIds.set(copied.mutationId,mutationIds.size);copied.mutationId='<operation-'+mutationIds.get(copied.mutationId)+'>';}
  trace.push({method,params:copied,timeout:options?.timeoutMs??35000});counts[method]=(counts[method]??0)+1;
  if(c.cancelAfter===trace.length)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
  const fault=c.faults?.find(f=>f.method===method&&f.n===counts[method]);let result,error;
  if(fault)error={code:fault.code??'fixture_failure',message:'Fixture failure.',outcome:'unknown'};
  else if(method==='system.status')result={runtimeEpoch:'fixture'};
  else if(method==='serviceNodes.get')result={...owner,...c.owner};
  else if(method==='serviceNodes.list')result={items:[],nextCursor:null};
  else if(method==='objects.query'){
   if(p.contractKey==='secretary/assignment'&&p.select){
    const items=assignments.map(a=>({objectId:a.pin.objectId,revision:a.pin.revision,values:{'object.effectivelyArchived':false,'object.archivedAt':null,'object.name':assignmentName(a.value.assignmentId),'data:/assignmentId':a.value.assignmentId}}));
    if(c.archived)items.unshift({objectId:'00000000-0000-4000-8000-000000000090',revision:2,values:{'object.effectivelyArchived':true,'object.archivedAt':c.archivedCoercion?{toString:'shadow'}:at,'object.name':assignmentName('archived'),'data:/assignmentId':'archived'}});
    result={items,nextCursor:c.nextCursor??null};
   }else{
    const name=p.where.args.find(a=>a.field==='object.name').value;
    result={items:[...records].filter(([,r])=>r.key===p.contractKey&&r.name===name).map(([id,r])=>({objectId:id,revision:r.versions.at(-1).revision})),nextCursor:null};
   }
  }else if(method==='objects.read')result=p.objectId===watched?{object:{id:watched,effectivelyArchived:false},revision:{revision:p.revision??2},content:{encoding:'json',value:{v:p.revision===1?0:1}}}:read(p.objectId,p.revision);
  else if(method==='objects.write'){
   const id=p.create?(p.create.contractKey==='secretary/configuration'?configurationId:'00000000-0000-4000-8000-'+String(nextId++).padStart(12,'0')):p.objectId;
   const r=p.create?{key:p.create.contractKey,name:p.create.name,versions:[]}:records.get(id);
   const revision=p.create?1:r.versions.at(-1).revision+1;if(!p.create)assert.equal(p.expectedRevision,revision-1);
   r.versions.push({revision,value:structuredClone(p.content.value),writtenAt:seconds(120)});records.set(id,r);result={object:{id},revision:{revision}};
  }else if(method==='events.subscribe'){
   const a=assignments.find(a=>'secretary-assignment-'+hashJson(a.value.assignmentId).slice(7,48)===p.name);assert.ok(a,'Unknown subscription');
   const index=subscribeCounts[a.value.assignmentId]??0;subscribeCounts[a.value.assignmentId]=index+1;
   const configured=c.batches?.[a.value.assignmentId]?.[index];
   result=configured??(c.objectError?batch([ownEvent(a),event(2,{topic:'hive.object.changed',occurredAt:'invalid',payload:{operation:'write',objectId:watched,revision:2}})]):c.revisionCoercion?batch([ownEvent(a,{toString:'shadow'})]):c.activate?batch([ownEvent(a),event()]):batch([],{hasMore:c.hasMore??false}));
  }else if(method==='events.read')result=c.firstBatch??batch([event(1,{occurredAt:seconds(-60)})]);
  else if(method==='events.ack'||method==='events.unsubscribe')result={ok:true};
  else assert.fail('Unexpected RPC '+method);
  if(error){steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  steps.push({method,value:structuredClone(result)});return result;
 }};
 store=new SecretaryStore(client,rootId);
 for(const d of documents){const encoded=canonical(d.value);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(d.id,d.key,d.name,1,encoded);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(d.id,1,encoded);}
 const recoveryIssues=new Map(c.issues??[]);
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:caseSettings,owner:{serviceNodeId:'fixture.secretary'},signal:controller.signal,recoveryIssues,now:()=>{const time=c.clockTimes?.[Math.min(nowIndex++,c.clockTimes.length-1)]??seconds(120);nowSamples.push(Date.parse(time));return new Date(time);}});
 const scheduler=new AssignmentScheduler(engine,c.noFallback?null:target);scheduler.cursor=c.cursor;
 const outcomes=[],snapshots=[];
 for(tickIndex=0;tickIndex<(c.ticks??1);tickIndex++){
  try{await scheduler.tick();outcomes.push({value:null});}catch(e){const error=IvyError.from(e);outcomes.push({code:error.code,message:error.message,outcome:error.outcome});}
  snapshots.push({cursor:scheduler.cursor??null,retired:[...scheduler.retiredAssignments],issues:[...recoveryIssues],technical:store.db.prepare('SELECT key,name,current_revision,value_json FROM technical_documents ORDER BY key,name').all().map(r=>({key:r.key,name:r.name,revision:r.current_revision,value:JSON.parse(r.value_json)}))});
 }
 rows.push({input:{label,settings:caseSettings,documents,issues:c.issues??[],ticks:c.ticks??1,cancel:c.cancel??false,cancelAfter:c.cancelAfter??null,cursor:c.cursor??null,fallback:c.noFallback?null:target,clockTimes:(c.clockTimes??[seconds(120)]).map(Date.parse),steps},expected:{outcomes,snapshots,trace,nowSamples},serverRecords:[...records].map(([id,r])=>({id,...r})),subscribeCounts});store.close();
}
await add('empty-assignment-page',{n:0});
await add('ordinary-event-assignment');
await add('repeat-tick-retains-progress',{ticks:2});
await add('fresh-configuration',{missingConfig:true});
await add('refresh-missing-target',{configuration:{execution:null}});
await add('missing-target-no-manager',{configuration:{execution:null},noFallback:true});
await add('missing-config-no-manager',{missingConfig:true,noFallback:true});
await add('invalid-configuration-stops-before-page',{configuration:{execution:{...target,threadCwd:'relative'}}});
await add('schedule-occurrences',{assignment:{trigger:scheduleTrigger}});
await add('snapshot-time-frozen-through-enqueues',{assignment:{trigger:scheduleTrigger},clockTimes:[seconds(120),seconds(600),seconds(900)]});
await add('windows-message-before-object-before-schedule',{windows:true});
await add('behind-events-do-not-flush-windows',{windows:true,hasMore:true});
await add('eight-assignments-share-budget',{n:8,hasMore:true});
await add('minimum-one-page-per-assignment',{n:33,hasMore:true});
await add('paged-assignment-cursor',{cursor:'original-cursor',nextCursor:'next-cursor'});
await add('archived-retired-before-active',{archived:true,ticks:2});
await add('archived-uncoded-coercion-issue',{archived:true,archivedCoercion:true});
await add('bad-assignment-recovery-issue',{badRead:true});
await add('gap-legacy-irrelevant-deleted',{documents:[doc('secretary/event-gap',{prunedThroughSequence:3})],issues:[['event-gap:assignment-0','secretary_event_gap']]});
await add('gap-legacy-relevant-empty',{documents:[doc('secretary/event-gap',{prunedThroughSequence:3})],firstBatch:batch([])});
await add('gap-legacy-relevant-journal-gap',{documents:[doc('secretary/event-gap',{prunedThroughSequence:3})],firstBatch:batch([event()],{gap:{prunedThroughSequence:4}})});
await add('gap-legacy-original-revision-time',{revision:2,documents:[doc('secretary/event-gap',{prunedThroughSequence:3})]});
for(const relevant of [null,false,true])await add('gap-explicit-'+relevant,{documents:[doc('secretary/event-gap',{prunedThroughSequence:3,relevant})]});
await add('old-assignment-issue-cleared',{issues:[['assignment:assignment-0','old']]});
await add('per-assignment-failure-does-not-block-peer',{n:2,faults:[{method:'events.subscribe',n:1}]});
await add('coded-internal-error-retains-code',{faults:[{method:'events.subscribe',n:1,code:'internal_error'}]});
await add('uncoded-object-time-falls-back',{assignment:{trigger:objectTrigger},objectError:true});
await add('uncoded-revision-coercion-falls-back',{revisionCoercion:true});
const invalidSnapshot={...sample('Assignment'),assignmentId:'assignment-0',enabled:false,rules:{},trigger:scheduleTrigger,createdAt:at,updatedAt:at};
await add('uncoded-disabled-schedule-date-falls-back',{assignment:{trigger:scheduleTrigger},progress:[{value:{checkedAt:'2026-99-01T00:00:00.000Z',assignmentSnapshot:invalidSnapshot}}]});
await add('uncoded-retained-timezone-falls-back',{assignment:{trigger:scheduleTrigger},progress:[{value:{assignmentSnapshot:{...invalidSnapshot,enabled:true,trigger:{kind:'schedule',cadence:'daily',timeZone:'not/a-zone',intervalMinutes:null,localTime:'00:00',weekdays:[]}}}}]});
await add('unconfigured-diagnostic-code-retained',{activate:true,noFallback:true,configuration:{execution:null}});
await add('cancelled-before-owner',{cancel:true});
await add('owner-mismatch',{owner:{principalId:'other'}});
await add('cancelled-error-rethrown',{cancelAfter:5,faults:[{method:'events.subscribe',n:1}]});
await add('bounded-per-assignment-issues',{issues:Array.from({length:31},(_,i)=>['original-'+i,'fixture']),faults:[{method:'events.subscribe',n:1}]});
for(const [method,n] of [['serviceNodes.get',1],['objects.query',1],['objects.query',2],['objects.read',1],['objects.read',2],['objects.write',1],['events.subscribe',1],['events.ack',1],['system.status',1]])await add('failure-'+method+'-'+n,{faults:[{method,n}]});
await add('legacy-gap-read-error-recoverable',{documents:[doc('secretary/event-gap',{prunedThroughSequence:3})],faults:[{method:'events.read',n:1}]});
assert.equal(rows.find(r=>r.input.label==='uncoded-object-time-falls-back').expected.snapshots[0].issues[0][1],'secretary_assignment_failed');
assert.equal(rows.find(r=>r.input.label==='coded-internal-error-retains-code').expected.snapshots[0].issues[0][1],'internal_error');
process.stdout.write(JSON.stringify({rootId,rows}));
