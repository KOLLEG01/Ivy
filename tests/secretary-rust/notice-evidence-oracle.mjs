import assert from 'node:assert/strict';
import { load, settings, owner, rootId } from './inbox-fixtures.mjs';
const { SecretaryNotices }=await load('services/secretary/src/notices.js');
const { SecretaryEngine }=await load('services/secretary/src/engine.js');
const { IvyError,hashJson }=await load('packages/sdk/src/node.js');
const rows=[], channel={kind:'whatsapp',id:'synthetic-channel'}, binding={threadId:'synthetic-thread'}, result={turnId:'original-turn'};
const request={operationId:'original-notify',channel};
const data={origin:{kind:'native',inputId:'input',binding,result},channel,state:'confirmed'};
const handoff={operationId:request.operationId,identity:{senderPrincipalId:settings.identity.principalId,channel},binding,result};
const document=(key,value,version='1.3.0')=>({object:{contractKey:key,effectivelyArchived:false},revision:{contractVersion:version,contentHash:hashJson(value)},content:{encoding:'json',value}});
const step=(method,value)=>({method,value});
async function add(label,{kind='evidence',reply={object:{objectId:'reply',revision:4},data},notice={request},steps=[step('objects.read',document('chat-bridge/reply',reply.data)),step('objects.read',document('chat-bridge/input',handoff))],...extra}={}){
 const input=structuredClone({label,kind,reply,notice,steps,...extra}),pending=structuredClone(steps),trace=[];
 const client={async request(method,params,options){trace.push({method,params:structuredClone(params),timeout:options?.timeoutMs??35000});const s=pending.shift();assert.ok(s);assert.equal(method,s.method);if(s.error)throw new IvyError(s.error.code,s.error.message,s.error.outcome);return structuredClone(s.value);}};
 const signal=new AbortController().signal,engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,settings,owner:{serviceNodeId:'fixture.secretary'},signal});
 const notices=new SecretaryNotices(engine);let outcome;
 try{outcome={value:(kind==='evidence'?await notices.evidence(structuredClone(reply),structuredClone(notice)):await notices.call(extra.method,extra.target,extra.args,extra.operationId))??null};}
 catch(cause){const e=IvyError.from(cause);outcome={code:e.code,message:e.message,outcome:e.outcome};}
 rows.push({input,expected:{result:outcome,trace,remaining:pending.length}});
}
await add('valid');
for(const version of ['1.2.0','1.3.0','1.0.0','1.4.0'])for(const which of [0,1]){
 const steps=[step('objects.read',document('chat-bridge/reply',data)),step('objects.read',document('chat-bridge/input',handoff))];steps[which].value.revision.contractVersion=version;await add(`version-${which}-${version}`,{steps});
}
for(const which of [0,1])for(const change of ['archive','contract','encoding','hash']){
 const steps=[step('objects.read',document('chat-bridge/reply',data)),step('objects.read',document('chat-bridge/input',handoff))],doc=steps[which].value;
 if(change==='archive')doc.object.effectivelyArchived=true;if(change==='contract')doc.object.contractKey='other';if(change==='encoding')doc.content.encoding='text';if(change==='hash')doc.revision.contentHash='sha256:'+'0'.repeat(64);
 await add(`${which}-${change}`,{steps});
}
await add('view-mismatch',{steps:[step('objects.read',document('chat-bridge/reply',{...data,state:'pending'}))]});
await add('non-native',{reply:{object:{objectId:'reply',revision:4},data:{...data,origin:{...data.origin,kind:'manual'}}}});
await add('wrong-channel',{reply:{object:{objectId:'reply',revision:4},data:{...data,channel:{kind:'whatsapp',id:'other'}}}});
for(const change of ['operation','principal','channel','binding','result']){
 const value=structuredClone(handoff);if(change==='operation')value.operationId='other';if(change==='principal')value.identity.senderPrincipalId='other';if(change==='channel')value.identity.channel={id:'other'};if(change==='binding')value.binding={threadId:'other'};if(change==='result')value.result={turnId:'other'};
 await add(change,{steps:[step('objects.read',document('chat-bridge/reply',data)),step('objects.read',document('chat-bridge/input',value))]});
}
for(const outcome of ['not_executed','unknown','completed'])for(const index of [0,1]){
 const steps=[step('objects.read',document('chat-bridge/reply',data)),step('objects.read',document('chat-bridge/input',handoff))];steps[index]={method:'objects.read',error:{code:'fixture_failure',message:'Fixture failure.',outcome}};await add(`read-${index}-${outcome}`,{steps});
}
const target={serviceNodeId:'fixture.chat',expectedBridge:{principalId:'chat-principal'},channel};
const node={serviceName:'chat-bridge',principalId:'chat-principal',connected:true,synced:true,ready:true};
const args={operationId:'original-notify',source:{objectId:'original-source',revision:2}};
for(const method of ['notice','notify','operation'])for(const op of [undefined,'original-wire-operation']){
 const steps=[step('serviceNodes.get',owner),step('serviceNodes.get',node),step('tools.list',{items:[{qualifiedName:`chat.${method}`,definitionHash:'sha256:'+'a'.repeat(64),definition:{interfaceVersion:'1.2.0',annotations:{readOnlyHint:method!=='notify'}}}],provider:{node:{serviceNodeId:target.serviceNodeId}},nextCursor:null}),step('tools.call',{retained:'original-result'})];
 await add(`${method}-${op??'read'}`,{kind:'call',method,target,args,operationId:op,steps});
}
for(const key of ['serviceName','principalId','connected','synced','ready'])await add(`provider-${key}`,{kind:'call',method:'operation',target,args,steps:[step('serviceNodes.get',owner),step('serviceNodes.get',{...node,[key]:['serviceName','principalId'].includes(key)?'other':false})]});
await add('owner-mismatch',{kind:'call',method:'operation',target,args,steps:[step('serviceNodes.get',{...owner,principalId:'other'})]});
for(const outcome of ['not_executed','unknown','completed'])for(const index of [0,1,2,3]){
 const steps=[step('serviceNodes.get',owner),step('serviceNodes.get',node),step('tools.list',{items:[{qualifiedName:'chat.notify',definitionHash:'sha256:'+'a'.repeat(64),definition:{interfaceVersion:'1.2.0',annotations:{readOnlyHint:false}}}],provider:{node:{serviceNodeId:target.serviceNodeId}},nextCursor:null}),step('tools.call',{retained:'original-result'})];
 steps[index]={method:steps[index].method,error:{code:'fixture_failure',message:'Fixture failure.',outcome}};
 await add(`call-${index}-${outcome}`,{kind:'call',method:'notify',target,args,operationId:'original-wire-operation',steps});
}
assert.equal(rows.length,55);
process.stdout.write(JSON.stringify({rootId,settings,rows}));
