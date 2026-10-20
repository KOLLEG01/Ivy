import assert from 'node:assert/strict';
import {hashJson} from '../../packages/contracts/src/canonical.ts';
import {validateChat} from '../../packages/contracts/src/chat-validation.ts';
import {toolDefinitionHash} from '../../packages/contracts/src/tool-definition.ts';

// Only the isolated factory mode enters this finite, original-source-backed peer.
export function deliveryBoundary(){
 let row=null,request=null,replyRaw=null,inputRaw=null;
 const definitions=new Map();
 const template=()=>row.initial.current.value.chatRequest;
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
   assert.equal(row.name,'factory question lost notify recovers confirmed');
   assert.equal(row.expected.final.value.state,'confirmed');
   const raw=key=>row.boundaries.find(b=>b.kind==='rpc'&&b.args[0]==='objects.read'&&b.value.object.contractKey===key).value;
   replyRaw=structuredClone(raw('chat-bridge/reply'));inputRaw=structuredClone(raw('chat-bridge/input'));
   validateChat('Reply',replyRaw.content.value);validateChat('Input',inputRaw.content.value);
   reply(id,{});return true;
  }
  if(method==='events.publish'){
   assert.equal(params.topic,'task-board.browser-notification');
   emit({kind:'delivery-browser-notice',params});reply(id,{});return true;
  }
  if(method==='serviceNodes.get'&&params.serviceNodeId==='chat-node'){
   assert.ok(row);reply(id,{serviceNodeId:'chat-node',serviceName:'chat-bridge',principalId:expected().principalId,connected:true,synced:true,ready:true});return true;
  }
  if(method==='tools.list'&&params.namespace==='chat'){
   assert.ok(row);const name=params.namePrefix;
   assert.ok(['workspace','operation','notify','notice'].includes(name));
   const definition={namespace:'chat',name,interfaceVersion:'1.2.0',annotations:{readOnlyHint:name!=='notify'},inputSchema:{},outputSchema:{}};
   definitions.set('chat.'+name,definition);
   reply(id,{provider:{node:{serviceNodeId:'chat-node'}},items:[{qualifiedName:'chat.'+name,definition,definitionHash:toolDefinitionHash(definition),catalogHash:'sha256:'+'b'.repeat(64)}],nextCursor:null});return true;
  }
  if(method==='objects.read'&&row){
   const proof=params.objectId===row.expected.final.value.deliveryEvidence.objectId?replyRaw:
    params.objectId===replyRaw.content.value.origin.inputId?inputRaw:null;
   if(!proof)return false;
   emit({kind:'delivery-proof',address:params,raw:proof});reply(id,proof);return true;
  }
  if(method!=='tools.call'||!params.qualifiedName.startsWith('chat.'))return false;
  assert.ok(row);assert.equal(params.serviceNodeId,'chat-node');
  assert.equal(params.expectedDefinitionHash,toolDefinitionHash(definitions.get(params.qualifiedName)));
  const args=params.arguments;
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
