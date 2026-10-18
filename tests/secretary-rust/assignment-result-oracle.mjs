import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,at,itemId} from './inbox-fixtures.mjs';
const {AssignmentRunner,assignmentPrompt}=await load('services/secretary/src/assignment-runner.js');
const {secretaryNotificationPolicy}=await load('instructions/secretary-notification-policy.js');
const {IvyError,canonical}=await load('packages/sdk/src/node.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const seed=JSON.parse(schemaSamples(ajv,[schema]).find(r=>r.valid&&r.reference.endsWith('#/$defs/Execution')).input);
const prompts=[],results=[];
function outcome(cause){const e=IvyError.from(cause);return {code:e.code,message:e.message,outcome:e.outcome};}
function prompt(label,execution){let expected;try{expected={value:assignmentPrompt(execution)};}catch(e){expected=outcome(e);}prompts.push({label,execution,expected});}
prompt('seed',seed);
for(const text of ['', 'Original instructions\nDo not change', '\ud800', '\udc00', 'é😀\0', 'A'.repeat(16384)])prompt('text-'+text.length,{...seed,assignmentSnapshot:{...seed.assignmentSnapshot,prompt:text},preflightOutput:text});
prompt('context-fields',{...seed,trigger:{...seed.trigger,payload:{'10':'ten','2':'two',z:'last',a:'first','é':true,'😀':'emoji'}},effectiveRules:{...seed.effectiveRules,whatsapp:{excluded:true},extra:'excluded'},preflightOutput:'original\noutput'});
const batch={kind:'secretary.channel.batch',channel:{key:'conversation',occurredAt:at,reference:{opaque:'original'},itemId:null},serviceNodeId:'fixture.source',dueAt:at,itemIds:['item'],eventIds:['event']};
for(const payload of [batch,{...batch,kind:'other'},{...batch,eventIds:[1]},{...batch,itemIds:[]},{...batch,channel:{...batch.channel,occurredAt:'invalid'}},{...batch,dueAt:1},null,[],{}])prompt('batch-'+prompts.length,{...seed,trigger:{...seed.trigger,payload}});
const context={assignmentId:seed.assignmentSnapshot.assignmentId,trigger:seed.trigger,effectiveRules:{main:seed.effectiveRules.main,voice:seed.effectiveRules.voice,researchMaxMinutes:seed.effectiveRules.researchMaxMinutes},preflightOutput:''};
const overhead=Buffer.byteLength(canonical(context));
for(const delta of [-1,0,1])prompt('context-byte-limit-'+delta,{...seed,preflightOutput:'x'.repeat(262144-overhead+delta)});
const decision={schemaVersion:1,urgency:'normal',notification:'main',text:'Notice',reason:'Useful'};
const answer=text=>({id:'final',type:'agentMessage',phase:'final_answer',text});
async function completion(label,config={}){
 const execution={...structuredClone(seed),phase:'running',threadId:'original-thread',turnId:'original-turn',...config.execution};
 const current={pin:{objectId:itemId,revision:1},writtenAt:at,value:execution};
 const items=config.items??[answer(JSON.stringify(decision))];
 const row={id:'original-turn',status:config.status??'completed',itemsView:'notLoaded',items:[],...config.row};
 const params={threadId:execution.threadId,cursor:null,limit:1,sortDirection:'desc',itemsView:'notLoaded'};
 const snapshot={method:'thread/turns/list',params,epoch:'original-epoch',reply:{result:{data:[row]}}};
 if(config.reply!==undefined)snapshot.reply=config.reply;
 const reads=[];let saves=0,releases=0;
 const owner={async status(){return {epoch:'original-epoch'};},checkRead(){},async read(method,p,epoch){
  reads.push({method,params:p,epoch});
  if(method==='thread/items/list')return {method,params:p,epoch,reply:{error:{code:-32601,message:'Fixture full-turn owner'}}};
  if(p.itemsView==='full')return {...snapshot,params:p,reply:{result:{data:[{...row,itemsView:'full',items}]}}};
  return snapshot;
 }};
 const runner=new AssignmentRunner({now:()=>new Date(at)});
 runner.owner=()=>owner;
 runner.update=async(c,value)=>{saves++;return {...c,value};};
 runner.releaseLease=()=>{releases++;};
 let expected;try{expected={value:await runner.run(current)};}catch(e){expected=outcome(e);}
 if(config.status==='inProgress'){assert.equal(saves,0);assert.equal(releases,0);}
 // Owner authentication is a separate shared API; this oracle exercises the real
 // runner and real full-turn contents helpers with a deterministic in-memory owner.
 results.push({label,current,read:snapshot,items,expected,saves,releases,reads});
}
for(const status of ['completed','failed','interrupted','inProgress'])await completion('status-'+status,{status});
for(const threadId of [null,''])await completion('missing-thread-'+threadId,{execution:{threadId}});
for(const reply of [{result:{data:[]}},{result:{data:[{},{}]}},{result:{data:[null]}},{result:{}},{error:{code:1,message:'failure'}},{result:{data:[{id:'different',status:'completed'}]}}])await completion('read-'+results.length,{reply});
for(const items of [[],[answer('')],[answer('not json')],[answer('null')],[answer(JSON.stringify({...decision,notification:'voice'}))],[answer(JSON.stringify({...decision,text:''}))],[answer(JSON.stringify({...decision,notification:'none',text:''}))],[answer('```json\n'+JSON.stringify(decision)+'\n```')],[answer(JSON.stringify(decision)),{...answer(JSON.stringify(decision)),id:'second'}],[{...answer(JSON.stringify(decision)),phase:'commentary'}],[{...answer(JSON.stringify(decision)),type:'toolMessage'}],[{...answer(42)}],[answer(JSON.stringify(decision)),{id:'other',type:'agentMessage',phase:'commentary',text:'ignored'}]])await completion('answer-'+results.length,{items});
for(const reuse of [undefined,null,'new','main','assignment'])for(const status of ['completed','failed'])await completion('reuse-'+reuse+'-'+status,{status,execution:{assignmentSnapshot:{...seed.assignmentSnapshot,execution:{...seed.assignmentSnapshot.execution,reuse}}},items:[answer('not json')]});
// Outer whitespace makes a valid answer reach the code-point boundary without
// violating the shorter text/reason field limits. Emoji and lone surrogates
// distinguish JavaScript string iteration from UTF-16 or byte length.
const valid=JSON.stringify(decision);
for(const length of [131071,131072,131073])await completion('answer-length-'+length,{items:[answer(' '.repeat(length-valid.length)+valid)]});
for(const text of ['😀'.repeat(65537),'\ud800'.repeat(131073),'\ud800','\udc00','é😀'])await completion('unicode-'+text.length,{items:[answer(text)]});
process.stdout.write(JSON.stringify({at,notificationPolicy:secretaryNotificationPolicy,prompts,results}));
