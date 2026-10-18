import assert from 'node:assert/strict';
import { load } from './inbox-fixtures.mjs';
const { voicePrompt,forwardedVoiceOutcome,AssignmentVoice }=await load('services/secretary/src/assignment-voice.js');
const { IvyError }=await load('packages/sdk/src/node.js');
const rows=[];
async function add(input){
 const steps=structuredClone(input.steps??[]),trace=[];
 const client={async request(method,params,options){trace.push({method,params:structuredClone(params),timeout:options?.timeoutMs??35000});const s=steps.shift();assert.ok(s);assert.equal(method,s.method);if(s.error)throw new IvyError(s.error.code,s.error.message,s.error.outcome);return structuredClone(s.value);}};
 let result;try{const value=input.kind==='prompt'?voicePrompt(input.text):input.kind==='proof'?forwardedVoiceOutcome(input.operation,input.callId,input.operationId,input.prompt):await new AssignmentVoice({client}).call(input.method,input.target,input.args,input.operationId);result={value};}catch(cause){const e=IvyError.from(cause);result={code:e.code,message:e.message,outcome:e.outcome};}
 rows.push({input,expected:{result,trace,remaining:steps.length}});
}
for(const text of ['', '  notice \r\n', '\ud800', '\udc00', 'a\0b', 'é😀', 'a'.repeat(3399)+'😀tail','😀'.repeat(1701), ' '.repeat(4000), '\u0085 x \u0085', '\u200b x \u200b'])await add({kind:'prompt',text});
for(const point of [9,10,11,12,13,32,160,5760,...Array.from({length:11},(_,i)=>8192+i),8232,8233,8239,8287,12288,65279])await add({kind:'prompt',text:String.fromCharCode(point)+'voice'+String.fromCharCode(point)});
const callId='original-call',operationId='original-operation',prompt=voicePrompt('critical notice'),intent={method:'call.forwardVoice',callId,operationId,prompt};
const proof=operation=>add({kind:'proof',operation,callId,operationId,prompt});
await proof({intent,phase:'submitted',receipt:null});await proof({intent,phase:'result',receipt:{ok:true,result:{state:'sent'}}});
for(const phase of ['submitted','result','outcome_unknown','failed',null])for(const receipt of [null,{}, {ok:false}, {ok:true,result:null}, {ok:true,result:{state:'queued'}}, {ok:true,result:{state:'sent'}}])await proof({intent,phase,receipt});
for(const key of ['method','callId','operationId','prompt'])await proof({intent:{...intent,[key]:'other'},phase:'submitted'});
for(const value of [null,{}, {intent:null}, {intent:[]}, {intent:{}}])await proof(value);
const target={serviceNodeId:'fixture.phone',requestDefinitionHash:'sha256:'+'a'.repeat(64),recipientId:'recipient'},args={callId,operationId};
const node={serviceName:'phone-bridge',connected:true,synced:true,ready:true};
const step=(method,value)=>({method,value});
const steps=(method,hash=target.requestDefinitionHash)=>[step('serviceNodes.get',node),step('tools.list',{items:[{qualifiedName:`phone.${method}`,definitionHash:hash,definition:{interfaceVersion:'1.0.0',annotations:{readOnlyHint:method==='operation'}}}],provider:{node:{serviceNodeId:target.serviceNodeId}},nextCursor:null}),step('tools.call',{original:true})];
for(const method of ['request','operation'])for(const op of [undefined,'original-operation'])await add({kind:'call',method,target,args,operationId:op,steps:steps(method)});
for(const method of ['request','operation'])await add({kind:'call',method,target,args,steps:steps(method,'sha256:'+'b'.repeat(64))});
for(const key of ['serviceName','connected','synced','ready'])await add({kind:'call',method:'operation',target,args,steps:[step('serviceNodes.get',{...node,[key]:key==='serviceName'?'other':false})]});
for(const outcome of ['not_executed','unknown','completed'])for(const index of [0,1,2]){
 const calls=steps('request');calls[index]={method:calls[index].method,error:{code:'fixture_failure',message:'Fixture failure.',outcome}};
 await add({kind:'call',method:'request',target,args,operationId:'original-operation',steps:calls});
}
process.stdout.write(JSON.stringify({rows}));
