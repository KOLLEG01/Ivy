import assert from 'node:assert/strict';
import {hashJson} from '../../packages/contracts/src/canonical.ts';
import {validateChat} from '../../packages/contracts/src/chat-validation.ts';
import {toolDefinitionHash} from '../../packages/contracts/src/tool-definition.ts';

// Only the isolated factory mode enters this finite, original-source-backed peer.
export function deliveryBoundary(){
 let row=null,request=null,replyRaw=null,inputRaw=null,phoneRequest=null,forwardReads=0,retainedDelivery=null,retentionError=null,retentionConfiguration=false;
 const definitions=new Map();
 const phone=()=>row.input.mode==='phonePrepared';
 const fallback=()=>phone()&&row.input.fault?.outcome==='not_executed';
 const forwarded=()=>phone()&&row.input.differentAdmission===true;
 const template=()=>row.initial.current.value.chatRequest??row.boundaries.find(b=>b.kind==='write'&&b.args[1].chatRequest)?.args[1].chatRequest;
 const expected=()=>({...template().expectedBridge,callerPrincipalId:'fixture-principal'});
 const channel=()=>structuredClone(template().channel);
 const operation=()=>{
  const value={schemaVersion:1,workspaceId:expected().workspaceId,definitionHash:expected().definitionHash,
   callerPrincipalId:'fixture-principal',operationId:request.operationId,request:structuredClone(request),
   requestHash:hashJson(request),phase:'succeeded',nativeCall:null,
   outcome:{action:'notify',operationId:request.operationId,reply:{object:row.expected.final.value.deliveryEvidence,data:replyRaw.content.value}},
   error:null,createdAt:row.initial.first.createdAt,updatedAt:row.initial.first.createdAt};
  validateChat('Operation',value);return value;
 };
 return ({method,params,id,selected,send,reply,emit})=>{
  const fail=(code,message,outcome='not_executed')=>send({jsonrpc:'2.0',id,error:{code:-32000,message,data:{code,outcome}}});
  if(method==='test.deliveryFixture'){
   assert.equal(row,null);row=structuredClone(params.row);
   assert.equal(row.name,forwarded()?'factory phone different admission recovers original forwarded prompt':fallback()?'factory phone refused question falls back and recovers original chat':phone()?'factory phone question lost admission recovers confirmed':'factory question lost notify recovers confirmed');
   assert.equal(row.expected.final.value.state,'confirmed');
   if(phone()&&!fallback()){reply(id,{});return true;}
   const raw=key=>row.boundaries.find(b=>b.kind==='rpc'&&b.args[0]==='objects.read'&&b.value.object.contractKey===key).value;
   replyRaw=structuredClone(raw('chat-bridge/reply'));inputRaw=structuredClone(raw('chat-bridge/input'));
   validateChat('Reply',replyRaw.content.value);validateChat('Input',inputRaw.content.value);
   reply(id,{});return true;
  }
  if(method==='events.publish'){
   assert.equal(params.topic,'task-board.browser-notification');
   emit({kind:'delivery-browser-notice',params});reply(id,{});return true;
  }
  if(method==='test.retentionDelivery'){
   assert.ok(row);assert.deepEqual(Object.keys(params).sort(),'error'in params?['error','raw']:['raw']);
   const raw=params.raw;assert.equal(raw.object.contractKey,'task-board/delivery');assert.equal(raw.content.encoding,'json');
   assert.ok(['pending','outcome_unknown','confirmed'].includes(raw.content.value.state));
   retainedDelivery=structuredClone(raw);retentionError=params.error??null;
   if(retentionError){assert.deepEqual(Object.keys(retentionError).sort(),['code','message','outcome']);assert.ok(['service_unavailable','not_found'].includes(retentionError.code));assert.equal(retentionError.outcome,'not_executed');}
   reply(id,{});return true;
  }
  if(method==='test.retentionConfiguration'){
   assert.ok(row);assert.deepEqual(params,{});assert.equal(retentionConfiguration,false);
   retentionConfiguration=true;reply(id,{});return true;
  }
  if(method==='serviceNodes.list'&&retentionConfiguration){
   assert.deepEqual(params,{serviceName:'agent-manager',limit:200});
   emit({kind:'retention-configuration-hosts',params});reply(id,{items:[],nextCursor:null});return true;
  }
  if(method==='objects.read'&&retainedDelivery&&params.objectId===retainedDelivery.object.objectId){
   assert.deepEqual(params,{objectId:retainedDelivery.object.objectId});
   if(retentionError){emit({kind:'delivery-retention-read',address:params,error:retentionError});fail(retentionError.code,retentionError.message,retentionError.outcome);}
   else{emit({kind:'delivery-retention-read',address:params,raw:retainedDelivery});reply(id,retainedDelivery);}return true;
  }
  if(method==='serviceNodes.get'&&params.serviceNodeId==='chat-node'){
   assert.ok(row);assert.ok(!phone()||fallback());reply(id,{serviceNodeId:'chat-node',serviceName:'chat-bridge',principalId:expected().principalId,connected:true,synced:true,ready:true});return true;
  }
  if(method==='serviceNodes.get'&&params.serviceNodeId==='phone-node'){
   assert.ok(row);assert.equal(phone(),true);reply(id,{serviceNodeId:'phone-node',serviceName:'phone-bridge',connected:true,synced:true,ready:true});return true;
  }
  if(method==='tools.list'&&['chat','phone'].includes(params.namespace)){
   assert.ok(row);const name=params.namePrefix,namespace=params.namespace;
   assert.ok(namespace==='phone'?phone():!phone()||fallback());
   assert.ok((namespace==='phone'?['request','operation']:['workspace','operation','notify','notice']).includes(name));
   const definition={namespace,name,interfaceVersion:namespace==='phone'?'1.0.0':'1.2.0',annotations:{readOnlyHint:!['notify','request'].includes(name)},inputSchema:{},outputSchema:{}};
   definitions.set(namespace+'.'+name,definition);
   reply(id,{provider:{node:{serviceNodeId:namespace+'-node'}},items:[{qualifiedName:namespace+'.'+name,definition,definitionHash:toolDefinitionHash(definition),catalogHash:'sha256:'+'b'.repeat(64)}],nextCursor:null});return true;
  }
  if(method==='objects.read'&&row&&(!phone()||fallback())){
   const proof=params.objectId===row.expected.final.value.deliveryEvidence.objectId?replyRaw:
    params.objectId===replyRaw.content.value.origin.inputId?inputRaw:null;
   if(!proof)return false;
   emit({kind:'delivery-proof',address:params,raw:proof});reply(id,proof);return true;
  }
  if(method!=='tools.call'||!['chat.','phone.'].some(prefix=>params.qualifiedName.startsWith(prefix)))return false;
  assert.ok(row);const isPhone=params.qualifiedName.startsWith('phone.');assert.equal(params.serviceNodeId,isPhone?'phone-node':'chat-node');
  assert.equal(params.expectedDefinitionHash,toolDefinitionHash(definitions.get(params.qualifiedName)));
  const args=params.arguments;
  if(isPhone){
   assert.ok(phone());
   if(params.qualifiedName==='phone.request'){
    const source=row.initial.current.value.phoneRequest;
    assert.deepEqual(args,{...source,operationId:args.operationId});
    assert.equal(params.operationId,args.operationId);
    if(!phoneRequest){
     phoneRequest=structuredClone(args);emit({kind:'delivery-phone-first-admission',request:phoneRequest});
    }else assert.deepEqual(args,phoneRequest);
    emit({kind:'delivery-phone-request',request:structuredClone(args)});
    if(fallback()){
     assert.equal(selected,1);assert.equal(phoneRequest.operationId,args.operationId);
     const original=row.boundaries.find(b=>b.kind==='rpc'&&b.args[0]==='tools.call'&&b.args[1].qualifiedName==='phone.request'&&b.error);
     assert.ok(original);assert.equal(original.error.outcome,'not_executed');
     emit({kind:'delivery-phone-refusal',error:structuredClone(original.error)});
     fail(original.error.code,original.error.message,original.error.outcome);return true;
    }
    if(selected===1&&!forwarded()){fail('service_unavailable','Fixture failure.','unknown');return true;}
    const original=row.boundaries.find(b=>b.kind==='rpc'&&b.args[0]==='tools.call'&&b.args[1].qualifiedName==='phone.request'&&b.value);
    if(forwarded()){assert.equal(selected,1);assert.equal(forwardReads,0);}
    const value={...structuredClone(original.value),...(forwarded()?{}:{operationId:phoneRequest.operationId})};
    emit({kind:'delivery-phone-admission-reply',value});reply(id,value);
   }else if(params.qualifiedName==='phone.operation'){
    if(forwarded()){
     assert.ok(phoneRequest);assert.ok(selected===1||selected===2);
     const original=row.boundaries.filter(b=>b.kind==='rpc'&&b.args[0]==='tools.call'&&b.args[1].qualifiedName==='phone.operation')[selected===2?2:forwardReads===0?0:1];
     assert.ok(original);assert.equal(original.value.intent.operationId,row.initial.current.value.phoneRequest.operationId);
     assert.deepEqual(args,{...original.args[1].arguments,operationId:phoneRequest.operationId});
     assert.deepEqual(Object.keys(args).sort(),['callId','operationId']);
     assert.equal(params.operationId,undefined);
     const value=structuredClone(original.value);value.intent.operationId=phoneRequest.operationId;
     assert.equal(value.intent.prompt,phoneRequest.voicePrompt);assert.equal(value.intent.callId,args.callId);assert.equal(value.intent.method,'call.forwardVoice');
     forwardReads++;emit({kind:'delivery-phone-forwarded',arguments:structuredClone(args),value});reply(id,value);return true;
    }
    assert.ok(phoneRequest);assert.equal(selected,2);
    const sourceArgs={...args};if('operationId'in sourceArgs)sourceArgs.operationId=row.initial.current.value.phoneRequest.operationId;
    const original=row.boundaries.find(b=>b.kind==='rpc'&&b.args[0]==='tools.call'&&b.args[1].qualifiedName==='phone.operation'&&hashJson(b.args[1].arguments)===hashJson(sourceArgs));
    assert.ok(original,'Unbound original Phone operation');
    emit({kind:'delivery-phone-operation',arguments:structuredClone(args),value:structuredClone(original.value)});reply(id,original.value);
   }else throw Error('Unbound Phone delivery tool '+params.qualifiedName);
   return true;
  }
  if(params.qualifiedName==='chat.workspace'){
   assert.deepEqual(args,{});reply(id,{expectedBridge:expected(),nativeOwnerReady:true,channels:[{channel:channel()}]});
  }else if(params.qualifiedName==='chat.operation'){
   assert.deepEqual(args.expectedBridge,expected());
   if(!request){fail('not_found','Original operation unavailable.');return true;}
   assert.equal(args.operationId,request.operationId);
   if(selected===1){fail('outcome_unknown','Lost original response.','unknown');return true;}
   const value=operation();emit({kind:'delivery-operation',operation:value});reply(id,value);
  }else if(params.qualifiedName==='chat.notify'){
   assert.equal(request,null,'Reconnect must look up the saved operation before resending');
   assert.equal(selected,1);assert.equal(params.operationId,args.operationId);
   assert.deepEqual(Object.keys(args).sort(),['action','operationId','source','text']);
   assert.equal(args.action,'notify');assert.equal(args.text,template().text);
   request={...structuredClone(args),expectedBridge:expected(),channel:channel()};
   validateChat('AdmittedNotifyRequest',request);
   inputRaw.content.value.operationId=request.operationId;
   inputRaw.content.value.identity.senderPrincipalId='fixture-principal';
   validateChat('Input',inputRaw.content.value);
   emit({kind:'delivery-notify',request:structuredClone(request)});
   fail('outcome_unknown','Lost original response.','unknown');
  }else if(params.qualifiedName==='chat.notice'){
   assert.deepEqual(args,{operationId:request.operationId});
   reply(id,{object:row.expected.final.value.deliveryEvidence,data:replyRaw.content.value});
  }else throw Error('Unbound delivery tool '+params.qualifiedName);
  return true;
 };
}
