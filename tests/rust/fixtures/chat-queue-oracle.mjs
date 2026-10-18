import './chat-store-loader.mjs';
import {mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatOperations}=await import('../../../services/chat-bridge/src/operations.ts');
const {ChatQueue,mainName,conversationName,inputName}=await import('../../../services/chat-bridge/src/queue.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../../packages/contracts/src/canonical.ts');
const {unpersistedNativePlan}=await import('../../fixtures/native-plan.ts');
const now=1790000000123,at=new Date(now).toISOString(),RealDate=Date;
globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[now]));}static now(){return now;}};
const channel={adapter:'whatsapp',accountId:'account',channelId:'channel'};
const definition={workspaceId:'chat',principalId:'chat-service',rootObjectId:null,project:{serviceNodeId:'owner',namespace:'codex',kind:'project',nativeId:'/synthetic/project'},nativePlan:unpersistedNativePlan(),channels:[{channel,displayName:'Channel'}]};
const admission=new ChatAdmission(definition);
const basePath=mkdtempSync(join(tmpdir(),'ivy-chat-queue-seed-'));
const baseStore=new ChatStore({},null,basePath);
const bindingValue={schemaVersion:1,workspaceId:'chat',definitionHash:admission.definitionHash,project:definition.project,
 primary:{...definition.project,kind:'thread',nativeId:'fixture-main-thread'},nativePlan:definition.nativePlan,createdAt:at,operationId:'fixture-main-creation',predecessor:null};
const binding=await baseStore.write('chat-bridge/binding',bindingValue,'fixture-binding',{create:{parentId:null,name:'fixture-binding'}});
const mainValue={schemaVersion:1,definition,definitionHash:admission.definitionHash,binding,conversation:{objectId:'fixture-conversation',revision:1},pendingAction:null,publication:null,queue:[],nextSequence:1,updatedAt:at};
const main=await baseStore.write('chat-bridge/main',mainValue,'fixture-main',{create:{parentId:null,name:mainName(definition)}});
baseStore.close();const database=readFileSync(join(basePath,'chat-bridge.sqlite')).toString('base64');rmSync(basePath,{recursive:true,force:true});
const request=(label,message=label,caller='alice')=>({action:'send',operationId:`chat:${now}:${label}`,expectedBridge:admission.expected(caller),channel,messageId:message,expectedBinding:binding,payload:{text:'Synthetic '+message,images:[]}});
const requests={a:request('a','original'),alias:request('alias','original'),b:request('b'),bob:request('bob','original','bob'),conflict:{...request('conflict','original'),payload:{text:'Different content',images:[]}},reused:{...request('a','different')},noBinding:{...request('no-binding'),expectedBinding:null},wrongBinding:{...request('wrong-binding'),expectedBinding:{objectId:'wrong',revision:1}},badCaller:{...request('caller'),expectedBridge:admission.expected('other')}};
const cases=[],pure=[];
function check(label,edit){const value=structuredClone(mainValue);edit(value);pure.push({label,kind:'check',value});}
const ticket=(n,id='input-'+n)=>({inputId:id,identity:{channel,callerPrincipalId:'alice',messageId:'message-'+n},requestHash:'sha256:'+'1'.repeat(64),binding,sequence:n,admittedAt:at});
check('empty',()=>{});
check('ordered',v=>{v.queue=[ticket(1),ticket(2)];v.nextSequence=3;});
check('definition-hash',v=>v.definitionHash='sha256:'+'2'.repeat(64));
check('definition-content',v=>v.definition.channels[0].displayName='Changed');
check('duplicate-input',v=>{v.queue=[ticket(1,'same'),ticket(2,'same')];v.nextSequence=3;});
check('duplicate-identity',v=>{v.queue=[ticket(1),{...ticket(2),identity:ticket(1).identity}];v.nextSequence=3;});
check('reversed-sequences',v=>{v.queue=[ticket(2),ticket(1)];v.nextSequence=3;});
check('equal-sequences',v=>{v.queue=[ticket(1),{...ticket(2),sequence:1}];v.nextSequence=3;});
check('unreserved-ticket',v=>{v.queue=[ticket(1)];v.nextSequence=1;});
for(const [label,change] of [
 ['valid',()=>{}],['wrong-head',v=>v.publication.inputId='other'],['too-many-parts',v=>v.publication.publishedParts=4],
 ['beyond-next',v=>v.nextSequence=4],['overlapping-ticket',v=>v.queue.push(ticket(3))],
 ['end-overflow',v=>{v.publication.firstSequence=9007199254740990;v.publication.partCount=3;v.nextSequence=9007199254740991;}],
 ['at-end-ticket',v=>v.queue.push(ticket(5))],['no-head',v=>v.queue=[]],
])check('publication-'+label,v=>{v.queue=[ticket(1)];v.nextSequence=6;v.publication={inputId:'input-1',firstSequence:2,partCount:3,publishedParts:1};change(v);});
for(const [label,edit] of [['same',()=>{}],['root',v=>v.rootObjectId='root'],['project',v=>v.project.nativeId='/other/project'],['display',v=>v.channels[0].displayName='Changed']]){
 const def=structuredClone(definition);edit(def);pure.push({label:'names-'+label,kind:'names',definition:def,identity:{channel,callerPrincipalId:'\ud800',messageId:'opaque\udfff'},retainedHash:'sha256:'+'3'.repeat(64)});
}
async function result(action){try{return{value:await action()};}catch(error){const e=IvyError.from(error);return{error:{code:e.code,message:e.message,outcome:e.outcome}};}}
for(const c of pure)c.expected=await result(async()=>{if(c.kind==='names')return{main:mainName(c.definition),conversation:conversationName(c.definition),input:inputName(c.definition,c.identity),retained:inputName(c.definition,c.identity,c.retainedHash)};return new ChatQueue(new ChatOperations(baseStore,admission)).checkMain({key:'chat-bridge/main',pin:main,value:c.value,metadata:{},revision:{}}).value;});

// Fixture Hive supports only this queue's public record operations. Local Main,
// binding and operation receipts always use the original production SQLite store.
class Client{
 records=[];calls=0;fault=null;
 constructor(records=[]){this.records=structuredClone(records);}
 async request(method,p){
  this.calls++;
  if(method==='system.status')return{runtimeEpoch:'chat'};
  if(method==='objects.query'){
   const filters=p.where.args,name=filters.find(f=>f.field==='object.name').value,parent=filters[0].value??null;
   return{items:this.records.filter(r=>r.object.contractKey===p.contractKey&&r.object.parentId===parent&&r.object.name===name).map(r=>({objectId:r.object.id})),nextCursor:null};
  }
  if(method==='objects.read'){
   const record=this.records.find(r=>r.object.id===p.objectId);if(!record)throw new IvyError('object_missing','Fixture object is missing.');
   return structuredClone(record);
  }
  if(method!=='objects.write')throw Error('Unexpected Hive RPC '+method);
  if(this.fault==='contention-input')throw new IvyError('revision_conflict','Fixture writer retained the competing name.');
  if(this.fault==='before-input'){this.fault=null;throw new IvyError('fixture_unavailable','Fixture write was not executed.','not_executed');}
  let record;
  if(p.create){
   if(this.records.some(r=>r.object.name===p.create.name&&r.object.contractKey===p.create.contractKey&&r.object.parentId===p.create.parentId))throw new IvyError('revision_conflict','Fixture name already exists.');
   record={object:{id:'fixture-input-'+(this.records.length+1),name:p.create.name,parentId:p.create.parentId,contractKey:p.create.contractKey,effectivelyArchived:false},revision:{revision:1,contractVersion:p.contractVersion,mediaType:'application/json'},content:structuredClone(p.content)};this.records.push(record);
  }else{
   record=this.records.find(r=>r.object.id===p.objectId);if(record.revision.revision!==p.expectedRevision)throw new IvyError('revision_conflict','Fixture revision changed.');
   record.revision.revision++;record.content=structuredClone(p.content);
  }
  if(this.fault==='after-input'){this.fault=null;throw new IvyError('outcome_unknown','Fixture reply was lost after the input committed.','unknown');}
  return structuredClone(record);
 }
}
const send=name=>({action:'send',name}),begin=name=>({action:'begin',name}),claim=name=>({action:'claim',name}),patchMain=patch=>({action:'patch-main',patch});
const recover={action:'recover'},reopen={action:'reopen'};
function add(label,steps,options={}){cases.push({label,steps,...options});}
add('replay-alias-and-caller-identity',[send('a'),send('a'),send('alias'),send('bob'),send('b'),send('conflict'),send('reused'),reopen,send('a')]);
add('authorization-before-io',[send('badCaller')]);
add('no-first-main-creator',[send('noBinding')]);
add('selected-binding-creator',[send('noBinding'),reopen,send('noBinding')],{ensure:true});
add('stale-displayed-binding',[send('wrongBinding')]);
for(const [label,patch] of [['workspace',{workspaceId:'other'}],['definition',{definitionHash:'sha256:'+'4'.repeat(64)}],['project',{project:{...definition.project,nativeId:'/other'}}],['plan',{nativePlan:{...definition.nativePlan,nativeVersion:'0.0.0'}}],['owner',{primary:{...bindingValue.primary,serviceNodeId:'foreign'}}],['namespace',{primary:{...bindingValue.primary,namespace:'other'}}],['kind',{primary:{...bindingValue.primary,kind:'project'}}]])add('binding-'+label,[{action:'patch-binding',patch},send('a')]);
const full=Array.from({length:64},(_,i)=>ticket(i+1));
add('known-full-refused-before-retention',[patchMain({queue:full,nextSequence:65}),send('a')]);
add('sequence-exhausted',[patchMain({nextSequence:9007199254740991}),send('a')]);
add('accepted-full-refused-under-claim',[begin('a'),claim('a'),patchMain({queue:full,nextSequence:65}),recover,send('a')]);
add('accepted-binding-changed-under-claim',[begin('a'),claim('a'),patchMain({binding:{objectId:'replacement',revision:1}}),recover,send('a')]);
add('uncertain-failed-preserves-claim',[begin('a'),{action:'patch-operation',name:'a',patch:{phase:'failed',error:{code:'uncertain',message:'Uncertain.',outcome:'unknown'}}},claim('a'),recover]);
add('proven-unapplied-releases-claim',[begin('a'),{action:'refuse',name:'a'},claim('a'),recover,send('a')]);
add('unresolved-operation-phase',[begin('a'),{action:'patch-operation',name:'a',patch:{phase:'outcome_unknown'}},send('a')]);
add('another-main-action-needs-recovery',[{action:'begin-other'}, {action:'claim-other'},recover]);
add('lost-input-write-reply',[{action:'fault',when:'after-input'},send('a'),reopen,recover,send('a'),send('alias')]);
add('input-write-not-executed',[{action:'fault',when:'before-input'},send('a'),reopen,recover,send('a')]);
add('alias-after-terminal-input-and-main-replacement',[send('a'),{action:'terminal-input',name:'a'},patchMain({binding:{objectId:'replacement',revision:1},queue:[]}),send('alias'),send('b')]);
add('archived-input-before-retaining-alias',[send('a'),{action:'archive-input'},send('alias')]);
for(const stage of ['accept','claim','publish-input','publish-ticket','finish','release'])add('recover-original-crash-'+stage,[reopen,recover,send('a'),send('alias')],{crash:stage});
for(const [label,patch] of [['id',{inputId:'other'}],['hash',{requestHash:'sha256:'+'5'.repeat(64)}],['binding',{binding:{objectId:'other',revision:1}}],['timestamp',{admittedAt:'2026-01-01T00:00:00.000Z'}]])add('claimed-ticket-'+label,[{action:'patch-ticket',patch},reopen,recover],{crash:'publish-ticket'});
for(const [label,patch] of [['hash',{requestHash:'sha256:'+'5'.repeat(64)}],['identity',{identity:{channel,callerPrincipalId:'alice',messageId:'changed'}}],['owner',{operationId:`chat:${now}:missing-owner`}]])add('claimed-input-'+label,[{action:'patch-input',patch},reopen,recover],{crash:'publish-input'});
add('claimed-input-keeps-original-sequence',[patchMain({nextSequence:2}),reopen,recover],{crash:'publish-input'});
add('claimed-input-keeps-original-binding',[patchMain({binding:{objectId:'replacement',revision:1}}),reopen,recover],{crash:'publish-input'});
add('completed-claim-keeps-exact-input-receipt',[{action:'terminal-input'},reopen,recover],{crash:'finish'});
add('bounded-contention-preserves-recoverable-claim',[{action:'fault',when:'contention-input'},send('a'),{action:'fault',when:'off'},reopen,recover,send('a')]);

for(const c of cases){
 const root=mkdtempSync(join(tmpdir(),'ivy-chat-queue-'));writeFileSync(join(root,'chat-bridge.sqlite'),Buffer.from(database,'base64'));
 const client=new Client();let store=new ChatStore(client,null,root),ops=new ChatOperations(store,admission),other=null;
 const makeQueue=()=>new ChatQueue(ops,c.ensure?async()=>({object:binding,data:bindingValue}):undefined);
 const rawMain=()=>store.named('chat-bridge/main',mainName(definition));
 const op=name=>ops.find(requests[name].expectedBridge.callerPrincipalId,requests[name].operationId);
 const update=async(key,doc,patch)=>store.write(key,{...doc.value,...patch},'fixture-update',{objectId:doc.pin.objectId,expectedRevision:doc.pin.revision});
 async function snapshot(output){
  const documents={},ids=new Map();
  for(const [name,req] of Object.entries(requests)){
   const doc=await store.named('chat-bridge/operation',ops.name(req.expectedBridge.callerPrincipalId,req.operationId));
   if(doc){documents[name]={value:doc.value,revision:doc.pin.revision};ids.set(doc.pin.objectId,'operation:'+req.operationId);}
  }
  if(other)ids.set(other.pin.objectId,'operation:other');
  const doc=await rawMain();
  const normalize=value=>{if(typeof value==='string')return ids.get(value)??value;if(Array.isArray(value))return value.map(normalize);if(value&&typeof value==='object')return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalize(v)]));return value;};
  return normalize({output,main:{value:doc.value,revision:doc.pin.revision},operations:documents,inputs:client.records});
 }
 try{
  if(c.crash){
   const original=store.write.bind(store);let stopped=false;
   store.write=async(key,value,mutation,destination)=>{
    const pin=await original(key,value,mutation,destination);
    const stage=key==='chat-bridge/operation'?(value.phase==='accepted'?'accept':value.phase==='succeeded'?'finish':null):key==='chat-bridge/input'?'publish-input':key==='chat-bridge/main'?(value.pendingAction?(value.queue.length?'publish-ticket':'claim'):'release'):null;
    if(stage===c.crash&&!stopped){stopped=true;throw new IvyError('outcome_unknown','Fixture reply was lost after '+stage+'.','unknown');}return pin;
   };
   c.interrupted=await result(()=>makeQueue().send('alice',requests.a));
   if(!stopped)throw Error('Crash barrier was not reached: '+c.crash);
   store.close();c.database=readFileSync(join(root,'chat-bridge.sqlite')).toString('base64');c.records=structuredClone(client.records);
   store=new ChatStore(client,null,root);ops=new ChatOperations(store,admission);
  }
  c.expected=[];
  for(const step of c.steps){
   const output=await result(async()=>{switch(step.action){
    case 'send':return makeQueue().send(requests[step.name].expectedBridge.callerPrincipalId==='bob'?'bob':'alice',requests[step.name]);
    case 'recover':await makeQueue().recoverPending();return null;
    case 'begin':return (await ops.begin('alice',requests[step.name])).value;
    case 'claim':{const doc=await op(step.name);await update('chat-bridge/main',await rawMain(),{pendingAction:doc.pin});return null;}
    case 'patch-main':await update('chat-bridge/main',await rawMain(),step.patch);return null;
    case 'patch-ticket':{const doc=await rawMain();await update('chat-bridge/main',doc,{queue:[{...doc.value.queue[0],...step.patch}]});return null;}
    case 'patch-input':client.records[0].content.value={...client.records[0].content.value,...step.patch};return null;
    case 'patch-binding':await update('chat-bridge/binding',await store.read('chat-bridge/binding',binding),step.patch);return null;
    case 'patch-operation':await update('chat-bridge/operation',await op(step.name),step.patch);return null;
    case 'refuse':{const doc=await op(step.name);return(await ops.rejectUnapplied(doc.pin.objectId,'synthetic_refusal','Proven unapplied.')).value;}
    case 'begin-other':{const request={action:'acknowledge',operationId:`chat:${now}:other`,expectedBridge:admission.expected('alice'),offerId:'offer',replyIds:['reply']};other=await ops.begin('alice',request);return null;}
    case 'claim-other':await update('chat-bridge/main',await rawMain(),{pendingAction:other.pin});return null;
    case 'fault':client.fault=step.when;return null;
    case 'terminal-input':{const record=client.records[0],value=record.content.value;record.content.value={...value,state:'cancelled',finishedAt:at,updatedAt:at};record.revision.revision++;return null;}
    case 'archive-input':client.records[0].object.effectivelyArchived=true;return null;
    case 'reopen':store.close();store=new ChatStore(client,null,root);ops=new ChatOperations(store,admission);return null;
    default:throw Error('Bad step '+step.action);
   }});
   c.expected.push(await snapshot(output));
  }
 }finally{store.close();rmSync(root,{recursive:true,force:true});}
}
const races=[];
for(const [label,names,filled] of [['same-operation',['a','a'],false],['same-input-alias',['a','alias'],false],['different-inputs',['a','b'],false],['conflicting-content',['a','conflict'],false],['last-slot',['a','b'],true]]){
 const root=mkdtempSync(join(tmpdir(),'ivy-chat-queue-race-'));writeFileSync(join(root,'chat-bridge.sqlite'),Buffer.from(database,'base64'));
 const client=new Client(),stores=[new ChatStore(client,null,root),new ChatStore(client,null,root)],operations=stores.map(s=>new ChatOperations(s,admission)),queues=operations.map(o=>new ChatQueue(o));
 const queue=filled?full.slice(0,63):[];
 try{
  if(filled){const doc=await queues[0].main();await stores[0].write('chat-bridge/main',{...doc.value,queue,nextSequence:64},'fixture-full',{objectId:doc.pin.objectId,expectedRevision:doc.pin.revision});}
  await Promise.all(operations.map((o,i)=>o.begin('alice',requests[names[i]])));
  const values=await Promise.all(queues.map((q,i)=>q.send('alice',requests[names[i]])));await queues[0].recoverPending();
  const saved=(await queues[0].main()).value,successes=values.filter(v=>v.phase==='succeeded');
  races.push({label,names,queue,expected:{phases:values.map(v=>v.phase).sort(),errors:values.filter(v=>v.phase==='failed').map(v=>v.error).sort((a,b)=>a.code.localeCompare(b.code)),queueLength:saved.queue.length,nextSequence:saved.nextSequence,pending:saved.pendingAction!==null,inputCount:client.records.length,sameInput:successes.length===2&&successes[0].outcome.input.object.objectId===successes[1].outcome.input.object.objectId}});
 }finally{for(const store of stores)store.close();rmSync(root,{recursive:true,force:true});}
}
process.stdout.write(JSON.stringify({now,definition,binding,bindingValue,main,mainValue,database,requests,pure,cases,races}));
