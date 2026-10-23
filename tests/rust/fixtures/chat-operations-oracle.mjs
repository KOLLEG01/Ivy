import './chat-store-loader.mjs';
import {mkdtempSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatOperations}=await import('../../../services/chat-bridge/src/operations.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../../packages/contracts/src/canonical.ts');
const {unpersistedNativePlan}=await import('../../fixtures/native-plan.ts');
const now=1790000000123,at=new Date(now).toISOString(),RealDate=Date;
let clock=now;globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[clock]));}static now(){return clock;}};
const channel={adapter:'whatsapp',accountId:'account',channelId:'channel'};
const definition={workspaceId:'chat',principalId:'chat-service',rootObjectId:null,project:{serviceNodeId:'owner',namespace:'codex',kind:'project',nativeId:'/synthetic/project'},nativePlan:unpersistedNativePlan(),channels:[{channel,displayName:'Channel'}]};
const admission=new ChatAdmission(definition),operationId=`chat:${now}:original`;
const request={action:'acknowledge',operationId,expectedBridge:admission.expected('alice'),offerId:'offer',replyIds:['reply']};
const value={schemaVersion:1,workspaceId:'chat',definitionHash:admission.definitionHash,callerPrincipalId:'alice',operationId,request,requestHash:hashJson(request),phase:'accepted',nativeCall:null,outcome:null,error:null,createdAt:at,updatedAt:at};
const outcome={action:'acknowledge',operationId,acknowledgement:{object:{objectId:'ack',revision:1},data:{schemaVersion:1,operationId,callerPrincipalId:'alice',offer:{objectId:'offer',revision:1},replyIds:['reply'],createdAt:at}}};
const cases=[];
function add(label,kind,edit=()=>{}){const c=structuredClone({label,kind,definition,root:null,caller:'alice',operationId,value,steps:[]});edit(c);cases.push(c);}
add('record-identity','check');
for(const field of ['workspaceId','definitionHash','callerPrincipalId','operationId','requestHash'])add('record-'+field,'check',c=>c.value[field]='different');
for(const field of ['principalId','callerPrincipalId','workspaceId','definitionHash'])add('saved-bridge-'+field,'check',c=>c.value.request.expectedBridge[field]='different');
add('retained-definition-hash','check',c=>{c.value.request.expectedBridge.definitionHash='sha256:'+'a'.repeat(64);c.value.definitionHash=c.value.request.expectedBridge.definitionHash;c.value.requestHash=hashJson(c.value.request);});
add('request-identity','check',c=>{c.value.request.operationId='other';c.value.requestHash=hashJson(c.value.request);});
for(const shape of [null,4,[],{},true])add('request-shape-'+JSON.stringify(shape),'check',c=>c.value.request=shape);
add('missing-request','check',c=>delete c.value.request);
add('missing-bridge','check',c=>delete c.value.request.expectedBridge);
add('missing-bridge-short-circuit','check',c=>{delete c.value.request.expectedBridge;c.value.workspaceId='other';});
add('null-bridge','check',c=>c.value.request.expectedBridge=null);
add('saved-outcome-other-action','check',c=>c.value.outcome={action:'send',operationId});
add('saved-outcome-other-operation','check',c=>c.value.outcome={action:'acknowledge',operationId:'other'});
for(const part of [false,true,0,[],{}])add('saved-outcome-'+JSON.stringify(part),'check',c=>c.value.outcome=part);
for(const caller of ['alice','Alice','bob','\ud800','😀'])add('name-'+caller,'name',c=>{c.caller=caller;c.operationId='opaque\udfff';});
add('root-mismatch','check',c=>c.root='other');
const begin=(overrides={})=>({action:'begin',caller:'alice',request:structuredClone(request),...overrides});
const finish=(overrides={})=>({action:'finish',outcome:structuredClone(outcome),...overrides});
const refuse={action:'refuse',code:'synthetic_refusal',message:'Proven unapplied.'};
for(const shape of [null,{},true,4,'request',[]])add('admission-receiver-'+JSON.stringify(shape),'run',c=>c.steps=[begin({request:shape})]);
add('admission-missing-bridge','run',c=>{const missing=structuredClone(request);delete missing.expectedBridge;c.steps=[begin({request:missing})];});
add('admit-complete-and-reopen','run',c=>c.steps=[begin(),begin(),{action:'reopen'},{action:'find',caller:'alice',operationId},finish(),finish(),{action:'reopen'},{action:'find',caller:'alice',operationId},refuse]);
add('opaque-caller-and-operation-survive-reopen','run',c=>{
 const caller='\ud800',req=structuredClone(request),out=structuredClone(outcome);
 req.operationId=`chat:${now}:opaque\udfff`;req.expectedBridge.callerPrincipalId=caller;req.replyIds=['reply\udfff'];
 out.operationId=req.operationId;out.acknowledgement.data.operationId=req.operationId;out.acknowledgement.data.callerPrincipalId=caller;out.acknowledgement.data.replyIds=req.replyIds;
 c.steps=[begin({caller,request:req}),{action:'reopen'},{action:'find',caller,operationId:req.operationId},finish({outcome:out}),{action:'reopen'},{action:'find',caller,operationId:req.operationId}];
});
add('refuse-only-unapplied','run',c=>c.steps=[begin(),{...refuse,at:now+1000},refuse,finish()]);
add('begin-changed-arguments','run',c=>c.steps=[begin(),begin({request:{...structuredClone(request),replyIds:['other']}}),begin(),{action:'find',caller:'bob',operationId}]);
add('retained-definition-replay','run',c=>{const old=structuredClone(request);old.expectedBridge.definitionHash='sha256:'+'b'.repeat(64);c.steps=[begin({request:old}),begin({request:old}),begin()];});
add('authorization-before-shape','run',c=>{const invalid=structuredClone(request);invalid.expectedBridge.callerPrincipalId='other';invalid.replyIds=[];c.steps=[begin({request:invalid})];});
add('invalid-shape','run',c=>c.steps=[begin({request:{...structuredClone(request),replyIds:[]}}),begin({request:{...structuredClone(request),channel:null}})]);
add('outcome-conflicts','run',c=>{const other=structuredClone(outcome);other.acknowledgement.data.replyIds=['other'];c.steps=[begin(),finish({outcome:{...structuredClone(outcome),operationId:'other'}}),finish(),finish({outcome:other})];});
for(const phase of ['dispatching','outcome_unknown','succeeded'])add('refuse-'+phase,'run',c=>c.steps=[begin(),{action:'patch',patch:{phase}},refuse]);
add('refuse-with-native-evidence','run',c=>c.steps=[begin(),{action:'patch',patch:{nativeCall:{objectId:'native',revision:1}}},refuse]);
for(const c of cases){
 clock=now;const root=mkdtempSync(join(tmpdir(),'ivy-chat-operations-'));
 const client={request(){throw Error('Unexpected Hive RPC');}};
 let store=new ChatStore(client,c.root,root),ops,selected=null;
 const result=async action=>{try{return{value:await action()};}catch(error){const e=IvyError.from(error);return{error:{code:e.code,message:e.message,outcome:e.outcome}};}};
 const doc=d=>d?{value:d.value,revision:d.pin.revision}:null;
 try{
  if(c.kind!=='run')c.expected=await result(()=>{ops=new ChatOperations(store,new ChatAdmission(c.definition));if(c.kind==='name')return ops.name(c.caller,c.operationId);ops.check(c.value);return null;});
  else{
   ops=new ChatOperations(store,new ChatAdmission(c.definition));c.expected=[];
   for(const step of c.steps){clock=step.at??now;c.expected.push(await result(async()=>{switch(step.action){
    case 'begin':selected=await ops.begin(step.caller,step.request);return doc(selected);
    case 'finish':selected=await ops.finish(selected.pin.objectId,step.outcome);return doc(selected);
    case 'refuse':selected=await ops.rejectUnapplied(selected.pin.objectId,step.code,step.message);return doc(selected);
    case 'find':return doc(await ops.find(step.caller,step.operationId));
    case 'patch':{const pin=await store.write('chat-bridge/operation',{...selected.value,...step.patch},'patch',{objectId:selected.pin.objectId,expectedRevision:selected.pin.revision});selected=await store.read('chat-bridge/operation',pin);return doc(selected);}
    case 'reopen':store.close();store=new ChatStore(client,c.root,root);ops=new ChatOperations(store,new ChatAdmission(c.definition));return null;
    default:throw Error('Bad action');
   }}));}
  }
 }finally{store.close();rmSync(root,{recursive:true,force:true});}
}
const races=[];
for(const kind of ['begin-same','begin-different','finish-same','finish-different','finish-refuse']){
 const expected=[];
 for(const reverse of [false,true]){
  clock=now;const store=new ChatStore({},null),ops=new ChatOperations(store,admission);
  let actions;
  if(kind.startsWith('begin')){const other=structuredClone(request);if(kind==='begin-different')other.replyIds=['other'];actions=[()=>ops.begin('alice',request),()=>ops.begin('alice',other)];}
  else {const selected=await ops.begin('alice',request),other=structuredClone(outcome);if(kind==='finish-different')other.acknowledgement.data.replyIds=['other'];actions=[()=>ops.finish(selected.pin.objectId,outcome),kind==='finish-refuse'?()=>ops.rejectUnapplied(selected.pin.objectId,'synthetic_refusal','Proven unapplied.'):()=>ops.finish(selected.pin.objectId,other)];}
  if(reverse)actions.reverse();const results=await Promise.allSettled(actions.map(a=>a()));
  const saved=await ops.find('alice',operationId);
  expected.push({successes:results.filter(r=>r.status==='fulfilled').length,errors:results.filter(r=>r.status==='rejected').map(r=>IvyError.from(r.reason).code).sort(),phase:saved.value.phase,revision:saved.pin.revision,changes:store.changeVersion});
  store.close();
 }
 races.push({kind,expected});
}
process.stdout.write(JSON.stringify({now,cases,races,definition,request,outcome}));
