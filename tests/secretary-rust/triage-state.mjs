import assert from 'node:assert/strict';
import {canonical,digest,hashJson,parseOperationId} from '../../dist/packages/sdk/src/node.js';
const name=(kind,v)=>'triage-'+kind+'-'+hashJson(v).slice(7);
export function normalizeTriage(state,input){
 for(const d of state.core)if(d.completedAt!==null){assert(Number.isSafeInteger(d.completedAt));assert(d.completedAt>=state.clockBounds[0]&&d.completedAt<=state.clockBounds[1]);d.completedAt='completed';}
 delete state.clockBounds;
 const ids=new Map(),ops=new Map(),docs=state.retained.documents,blobs=state.retained.blobs;
 for(const d of docs){
  let selected=d.name;
  if(d.key==='secretary/triage-call'){assert.equal(d.name,name('call',[d.value.plan,d.value.slot]));selected='slot:'+d.value.slot;}
  if(d.key==='secretary/triage-completion'){assert.equal(d.name,name('completion',d.value.plan));selected='completion';}
  ids.set(d.id,'document:'+d.key+':'+selected);d.name=selected;
  if(['secretary/triage-call','secretary/triage-work'].includes(d.key)){
   const id=d.value.operationId;
   if(!input.documents.some(r=>r.value.operationId===id)){const parsed=parseOperationId(id);assert.equal(parsed.runtimeEpoch,'fixture-epoch');ops.set(id,'fresh-operation:'+(d.value.slot??'assessment'));}
  }
 }
 for(const d of state.core)ids.set(d.id,'core:'+d.key+':'+d.name);
 const replace=v=>typeof v==='string'?(ops.get(v)??ids.get(v)??v):Array.isArray(v)?v.map(replace):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,c])=>[k,replace(c)])):v;
 // Fresh local IDs may occur inside canonical native blobs. Check their actual
 // names/bytes first; normalize only those IDs, never an original operation ID.
 for(const b of blobs){
  const bytes=Buffer.from(b.bytes,'base64');let value;try{value=JSON.parse(bytes.toString('utf8').replace(/^\uFEFF/,''));}catch{}
  const initial=input.blobs.find(r=>r.id===b.id);
  if(!initial&&b.key.startsWith('secretary/triage-native-')){
   assert.equal(bytes.toString('utf8'),canonical(value,8*1024*1024));
   const kind=b.key.slice('secretary/triage-native-'.length);
   let identity;
   if(kind==='parameters')identity=value.item;
   else if(kind==='request')identity=[value.plan,value.slot];
   else {const call=docs.find(d=>d.key==='secretary/triage-call'&&d.value.operationId===value.operationId);assert(call);identity=[call.value.plan,call.value.slot];}
   assert.equal(b.name,name('native-'+kind,[identity,digest(bytes)]));
  }
  if(value!==undefined&&ops.size){b.bytes=Buffer.from(canonical(replace(value),8*1024*1024)).toString('base64');}
  // Blob identity includes raw UUIDs. Its normalized content is the stable key.
  if(value!==undefined){b.name='content:'+hashJson(replace(value));}
  ids.set(b.id,'blob:'+b.key+':'+b.name);
 }
 const result=replace(state);
 result.retained.documents.sort((a,b)=>a.id.localeCompare(b.id,'en'));
 result.retained.blobs.sort((a,b)=>a.id.localeCompare(b.id,'en'));
 result.retained.revisions.sort((a,b)=>a.id.localeCompare(b.id,'en')||a.revision-b.revision);
 result.core.sort((a,b)=>a.id.localeCompare(b.id,'en'));
 result.cleanup.sort((a,b)=>a.id.localeCompare(b.id,'en'));
 return result;
}
if(process.argv.includes('--normalize')){let source='';for await(const part of process.stdin)source+=part;const rows=JSON.parse(source);process.stdout.write(JSON.stringify(rows.map(r=>normalizeTriage(r.state,r.input))));}
