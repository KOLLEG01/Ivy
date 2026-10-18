import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,rootId,itemId,settings,owner,at} from './inbox-fixtures.mjs';
const {AssignmentRunner,executionFailureDisposition}=await load('services/secretary/src/assignment-runner.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {IvyError,hashJson,canonical,parseOperationId}=await load('packages/sdk/src/node.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const seed=JSON.parse(schemaSamples(ajv,[schema]).find(r=>r.valid&&r.reference.endsWith('#/$defs/Execution')).input);
const policy=[];
for(const code of ['secretary_assignment_result_invalid','secretary_execution_answer_missing','secretary_execution_turn_missing','secretary_execution_turn_changed','secretary_execution_thread_missing','secretary_execution_reuse_target_changed','secretary_reuse_target_changed','secretary_preflight_outcome_unknown','secretary_preflight_host_mismatch','secretary_execution_native_failed','native_owner_mismatch','native_evidence_mismatch','native_evidence_regressed','tool_definition_changed','secretary_assignment_result_changed','secretary_assignment_notice_mismatch','secretary_execution_native_pending','secretary_message_unread_pending','fixture_transient','secretary_execution_answer_invalid','secretary_execution_native_mismatch'])
 for(const attempts of [0,1,4,4.999,5,6])for(const age of [-1,0,1799999,1800000,1800001])policy.push({code,attempts,age,result:executionFailureDisposition(code,attempts,age)});
const calls='secretary/assignment-native-call',leases='secretary/thread-lease',rows=[];
const callKeys=['threadStart','turnStart','archive','delete'];
const nativeOperations=Object.fromEntries(callKeys.map(k=>[k,'original-'+k]));
const document=(id,key,name,value)=>({id,key,name,revision:1,value});
const retained=callKeys.map(k=>document(k,calls,nativeOperations[k],{operationId:nativeOperations[k],method:k,params:{original:'bytes'},definitionHash:'sha256:'+'a'.repeat(64)}));
// Unreferenced calls and similarly named records from another key must survive.
retained.push(document('unrelated',calls,'unrelated',{keep:true}),document('other-key','other/key','original-threadStart',{keep:true}));
async function add(label,config={}){
 const trace=[],steps=[];let revision=1,store;
 const initial={...structuredClone(seed),nativeOperations,...config.execution};let value=structuredClone(initial);
 const documents=structuredClone(config.documents??retained);
 const read=()=>({object:{id:itemId,parentId:rootId,effectivelyArchived:false,contractKey:'secretary/execution'},revision:{revision,contractVersion:contractVersion('secretary/execution'),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest('secretary/execution',value,{object:{objectId:itemId,revision}},'fixture:1800000000000:read').references,createdAt:at},content:{encoding:'json',value:structuredClone(value)}});
 const client={async request(method,params,options){
  const copied=structuredClone(params);if(method==='objects.write'){parseOperationId(copied.mutationId);copied.mutationId='<scoped-operation>';}
  trace.push({method,params:copied,timeout:options?.timeoutMs??35000});
  if(config.fail===method){const error={code:'fixture_failure',message:'Fixture failure.',outcome:'unknown'};steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  let result;
  if(method==='serviceNodes.get')result={...owner,...config.owner};
  else if(method==='system.status')result={runtimeEpoch:'fixture'};
  else if(method==='objects.write'){assert.equal(params.expectedRevision,revision);value=structuredClone(params.content.value);revision++;result={object:{id:itemId},revision:{revision}};}
  else if(method==='objects.read')result=read();
  else assert.fail('Unexpected RPC '+method);
  steps.push({method,value:structuredClone(result)});return result;
 }};
 store=new SecretaryStore(client,rootId);
 for(const d of documents){const encoded=canonical(d.value);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(d.id,d.key,d.name,d.revision,encoded);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(d.id,d.revision,encoded);}
 const current={pin:{objectId:itemId,revision:1},value:initial,writtenAt:at};
 const cancellation=new AbortController();if(config.cancel)cancellation.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,owner:{serviceNodeId:'fixture.secretary'},signal:cancellation.signal});
 const runner=new AssignmentRunner(engine);let result;
 const next={...initial,...config.next};
 try{const out=config.kind==='lease'?runner.releaseLease(current):config.kind==='update'?await runner.update(current,next):runner.releaseCalls(initial);result={value:out??null};}catch(cause){const e=IvyError.from(cause);result={code:e.code,message:e.message,outcome:e.outcome};}
 const remaining=store.db.prepare('SELECT id,key,name,current_revision,value_json FROM technical_documents ORDER BY id').all().map(r=>({id:r.id,key:r.key,name:r.name,revision:r.current_revision,value:JSON.parse(r.value_json)}));
 const revisions=store.db.prepare('SELECT id,revision,value_json FROM technical_revisions ORDER BY id,revision').all().map(r=>({id:r.id,revision:r.revision,value:JSON.parse(r.value_json)}));
 store.close();rows.push({input:{label,kind:config.kind??'calls',current,next,documents,steps,cancel:config.cancel??false},expected:{result,trace,remaining,revisions}});
}
const phases=['accepted','preflight','starting','running','completed','archiving','archived','settled','deleting','deleted','failed'];
for(const phase of phases)for(const turnId of [null,'original-turn'])await add(phase+'-'+turnId,{execution:{phase,turnId}});
for(const nativeOperations of [{},{threadStart:null,turnStart:null,archive:null,delete:null},{threadStart:'missing'},Object.fromEntries(callKeys.map(k=>[k,'original-threadStart']))])await add('partial-or-shared-ids',{execution:{phase:'running',turnId:null,nativeOperations}});
for(const reuse of [undefined,'new','main','assignment'])for(const activeExecutionObjectId of [itemId,'competitor',null])for(const threadId of [null,'lease-thread','']){
 const assignmentSnapshot={...seed.assignmentSnapshot,execution:{...seed.assignmentSnapshot.execution,reuse}};
 const name=reuse==='main'?'main':'assignment-'+assignmentSnapshot.assignmentId;
 const lease=document('lease',leases,name,{serviceNodeId:'fixture.agent',threadCwd:'fixture',createdByExecutionId:'original',activeExecutionObjectId,threadId,extra:'preserved'});
 await add('lease-'+reuse+'-'+activeExecutionObjectId+'-'+threadId,{kind:'lease',execution:{assignmentSnapshot,threadId:'execution-thread'},documents:[...retained,lease]});
}
await add('lease-missing',{kind:'lease',execution:{assignmentSnapshot:{...seed.assignmentSnapshot,execution:{...seed.assignmentSnapshot.execution,reuse:'main'}}}});
for(const execution of [null,undefined])await add('lease-default-'+execution,{kind:'lease',execution:{assignmentSnapshot:{...seed.assignmentSnapshot,execution}}});
await add('lease-null-thread',{kind:'lease',execution:{threadId:null,assignmentSnapshot:{...seed.assignmentSnapshot,execution:{...seed.assignmentSnapshot.execution,reuse:'main'}}},documents:[...retained,document('lease',leases,'main',{activeExecutionObjectId:itemId,threadId:null})]});
for(const phase of ['running','completed','archiving','deleting','failed'])await add('update-'+phase,{kind:'update',execution:{phase:'starting',turnId:null},next:{phase}});
await add('update-unchanged',{kind:'update',execution:{phase:'starting',turnId:null}});
await add('update-cancelled',{kind:'update',cancel:true,next:{phase:'running'}});
for(const owner of [{connected:false},{synced:false},{principalId:'other'},{hostId:'other'},{serviceName:'other'}])await add('update-owner-'+JSON.stringify(owner),{kind:'update',owner,next:{phase:'running'}});
for(const fail of ['serviceNodes.get','system.status','objects.write','objects.read'])await add('update-fail-'+fail,{kind:'update',fail,next:{phase:'running'}});
for(const row of rows.filter(r=>r.expected.result.code))assert.deepEqual(row.expected.remaining,row.input.documents.toSorted((a,b)=>a.id.localeCompare(b.id)),row.input.label);
process.stdout.write(JSON.stringify({settings,rootId,policy,rows}));
