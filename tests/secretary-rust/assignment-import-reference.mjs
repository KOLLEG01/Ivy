// Approved explicit import policy through actual original Store/SDK boundaries.
// Ordinary reference workers hide obsolete lookup keys after this phase. All
// event matching/effects/rebase behavior still executes the original processor.
import assert from 'node:assert/strict';
import {load} from './inbox-fixtures.mjs';
const {IvyError,hashJson,canonical,newOperationId,parseOperationId}=await load('packages/sdk/src/node.js');
const {assignmentName}=await load('services/secretary/src/assignment-schema.js');
const cursor='secretary/schedule-cursor',subscription='secretary/event-subscription';
const originalNamed=new WeakMap();
function retained(store,key,name){return(originalNamed.get(store)??store.technicalNamed.bind(store))(key,name);}
export function useCurrentAssignmentRecords(store){
 if(originalNamed.has(store))return;
 const named=store.technicalNamed.bind(store);originalNamed.set(store,named);
 store.technicalNamed=(key,name)=>key===cursor||key===subscription?null:named(key,name);
}
const need=ok=>{if(!ok)throw new IvyError('secretary_import_identity_conflict','The retained import identity changed.');};
export async function importScheduleProgress(scheduler,assignment,time){
 const engine=scheduler.engine,store=engine.store;await engine.verifyOwner();
 const latest=await store.read('secretary/assignment',assignment.pin.objectId),value=latest.value,id=value.assignmentId;
 need(id===assignment.value.assignmentId);
 const source=retained(store,cursor,id);if(!source)return null;
 const name='schedule-'+hashJson(id).slice(7),before=await store.named('secretary/schedule-progress',name);
 let after=before;
 if(!after){
  const current=source.value.updatedAt===value.updatedAt,original=current?latest:await store.original('secretary/assignment',latest);
  const checkedAt=current||latest.pin.revision===1?source.value.checkedAt:original.writtenAt??original.value.createdAt;
  const progress={schemaVersion:1,assignmentId:id,assignment:original.pin,assignmentSnapshot:structuredClone(original.value),checkedAt,updatedAt:new Date(time).toISOString()};
  after=await store.create('secretary/schedule-progress',name,progress,await scheduler.operation('schedule-progress',id));
 }
 if(after.value.assignmentId!==id||after.value.assignment.objectId!==latest.pin.objectId)throw new IvyError('secretary_schedule_progress_mismatch','The durable schedule cursor belongs to another assignment.');
 return{source,before,after};
}
export function appendTechnicalImport(store,key,current,value){
 const encoded=canonical(value);store.db.exec('BEGIN IMMEDIATE');
 try{
  const latest=store.technicalRead(key,current.pin.objectId);need(latest.pin.revision===current.pin.revision);
  if(canonical(latest.value)===encoded){store.db.exec('COMMIT');return latest;}
  const usage=store.usage();
  if(usage.count>=store.limits.count||usage.bytes+Buffer.byteLength(encoded)>store.limits.bytes)throw new IvyError('capacity_exceeded','Explicit import needs room to retain the original journal history.');
  const revision=current.pin.revision+1;
  need(store.db.prepare('UPDATE technical_documents SET current_revision=?,value_json=? WHERE id=? AND key=? AND current_revision=?').run(revision,encoded,current.pin.objectId,key,current.pin.revision).changes===1);
  store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(current.pin.objectId,revision,encoded);
  const after=store.technicalRead(key,{objectId:current.pin.objectId,revision});store.db.exec('COMMIT');return after;
 }catch(error){store.db.exec('ROLLBACK');throw error;}
}
export async function importEventGap(scheduler,assignment){
 const engine=scheduler.engine,store=engine.store;await engine.verifyOwner();
 const latest=await store.read('secretary/assignment',assignment.pin.objectId),id=latest.value.assignmentId;need(id===assignment.value.assignmentId);
 const before=retained(store,'secretary/event-gap',id);if(!before||Object.hasOwn(before.value,'relevant'))return null;
 const first=await engine.client.request('events.read',{afterSequence:before.value.prunedThroughSequence,filter:{},limit:1});
 const original=await store.original('secretary/assignment',latest),written=original.writtenAt??original.value.createdAt;
 const beforeAssignment=!first.gap&&first.items[0]&&Date.parse(first.items[0].occurredAt)<Date.parse(written);
 const value={...before.value,relevant:!beforeAssignment};await engine.verifyOwner();
 const after=appendTechnicalImport(store,'secretary/event-gap',before,value),key='event-gap:'+id;
 if(beforeAssignment)engine.recoveryIssues.delete(key);else engine.issue(key,'secretary_event_gap');
 return{before,after};
}
async function currentPage(scheduler,assignment,configuration,progress){
 const engine=scheduler.engine,store=engine.store,key=assignment.value.assignmentId,stable='secretary-assignment-'+hashJson(key).slice(7,48),name='event-'+hashJson(key).slice(7);
 // The import also establishes a stable subscription for an initial schedule
 // that had an obsolete subscription. This fixture bootstrap is intentionally
 // new import policy; it is restricted to an empty first page in the oracle.
 if(!progress&&assignment.pin.revision===1&&assignment.value.trigger.kind==='schedule'){
  const batch=await engine.client.request('events.subscribe',{name:stable,filter:{},initialSequence:0,durable:true,limit:100});
  assert.deepEqual(batch,{items:[],throughSequence:0,gap:null,hasMore:false});
  const initial={schemaVersion:1,assignmentId:key,assignment:assignment.pin,assignmentSnapshot:structuredClone(assignment.value),processedThrough:0,active:false,needsRebase:false,updatedAt:engine.now().toISOString()};
  await store.create('secretary/event-progress',name,initial,await scheduler.operation('event-progress',key));
  await engine.client.request('events.ack',{name:stable,throughSequence:0});return true;
 }
 const named=store.named.bind(store),request=engine.client.request.bind(engine.client);
 let loaded=false,through,ready=false;
 store.named=async(k,n)=>{if(!loaded&&k==='secretary/event-progress'&&n===name){loaded=true;return progress;}return named(k,n);};
 engine.client.request=async(method,args,options)=>{
  const result=await request(method,args,options);
  if(method==='events.subscribe')through=result.throughSequence;
  if(method==='events.ack')ready=(progress?.value.processedThrough??0)<=through;
  return result;
 };
 try{await scheduler.events(assignment,configuration,1);return ready;}
 finally{store.named=named;engine.client.request=request;}
}
export async function importEventSubscription(scheduler,selected){
 const engine=scheduler.engine,store=engine.store;await engine.verifyOwner();
 let id,objectId,archived,retirement,assignment,configuration;
 if(selected.active){
  assignment=await store.read('secretary/assignment',selected.active.pin.objectId);id=assignment.value.assignmentId;need(id===selected.active.value.assignmentId);
  objectId=assignment.pin.objectId;archived=false;retirement=null;configuration=selected.configuration;
 }else{
  const row=selected.archived,v=row.values;id=v['data:/assignmentId'];
  if(v['object.effectivelyArchived']!==true||v['object.name']!==assignmentName(id))throw new IvyError('secretary_assignment_identity_conflict','The archived assignment must retain its original identity.');
  objectId=row.objectId;archived=true;retirement=String(row.revision)+':'+String(v['object.archivedAt']);
 }
 const source=retained(store,subscription,id);if(!source)return null;
 const stable='secretary-assignment-'+hashJson(id).slice(7,48),oldName=source.value.name;
 const names=archived?oldName===stable?[stable]:[stable,oldName]:oldName===stable?[]:[oldName];
 const identity={rootObjectId:store.root,serviceNodeId:engine.owner.serviceNodeId,secretaryId:engine.settings.identity.scope.secretaryId,assignmentId:id,objectId,archived,source:source.pin,sourceHash:hashJson(source.value)};
 const key='secretary/import/event-subscription/'+source.pin.objectId;
 const read=()=>{const row=store.db.prepare('SELECT value FROM journal_state WHERE key=?').get(key);return row?JSON.parse(row.value):null;};
 const validate=state=>{
  need(state.schemaVersion===1&&canonical(state.identity)===canonical(identity)&&state.requests.length===names.length);
  for(let i=0;i<names.length;i++){need(state.requests[i].name===names[i]);parseOperationId(state.requests[i].mutationId);}
  need(Number.isInteger(state.completed)&&state.completed>=0&&state.completed<=names.length);return state.completed;
 };
 const before=read();let state=before?structuredClone(before):null;
 if(state)validate(state);else{
  if(!archived&&names.length){
   const progress=await store.named('secretary/event-progress','event-'+hashJson(id).slice(7));
   if(!await currentPage(scheduler,assignment,configuration,progress))return{source,before,after:null,completed:false};
  }
  const requests=[];
  for(const name of names)requests.push({name,mutationId:archived?await scheduler.operation('retire-assignment-subscription',objectId,retirement,name):await newOperationId(engine.client)});
  const prepared={schemaVersion:1,identity,requests,completed:0};
  store.db.prepare('INSERT OR IGNORE INTO journal_state(key,value) VALUES(?,?)').run(key,canonical(prepared));state=read();
 }
 for(let index=validate(state);index<names.length;index++){
  await engine.verifyOwner();
  await engine.client.request('events.unsubscribe',{name:state.requests[index].name,mutationId:state.requests[index].mutationId});
  const previous=canonical(state);state.completed=index+1;
  need(store.db.prepare('UPDATE journal_state SET value=? WHERE key=? AND value=?').run(canonical(state),key,previous).changes===1);
 }
 return{source,before,after:state,completed:true};
}
