import assert from 'node:assert/strict';
import {checkedNativeContract} from '../../packages/contracts/src/checked-native-contract.ts';
import {hashJson} from '../../packages/contracts/src/canonical.ts';
import {toolDefinitionHash} from '../../packages/contracts/src/tool-definition.ts';

// Only the isolated production-factory driver mode enters this boundary.
export function driverBoundary(){
 let catalog=null,threadMethod=null,read=0;
 const operations=new Map();
 return ({method,params,id,selected,send,reply,emit})=>{
  const fail=(code,message,outcome='not_executed',details={})=>send({jsonrpc:'2.0',id,error:{code:-32000,message,data:{code,outcome,details}}});
  if(method==='test.driver'){
   assert.equal(catalog,null);
   catalog=checkedNativeContract(params.plan.nativeVersion,{sourceHash:params.plan.catalogSourceHash});
   threadMethod=params.resume?'thread/resume':'thread/start';
   reply(id,{});return true;
  }
  if(method==='tools.list'){
   assert.ok(catalog,'Configure the driver before releasing the Run lane');
   const name=params.namePrefix,namespace=params.namespace;
   const definition=namespace==='codex'?catalog.definitions.get(name):{namespace:'agent',name,interfaceVersion:'1.0.0',annotations:{readOnlyHint:name!=='invoke'},inputSchema:{},outputSchema:{}};
   assert.ok(namespace==='codex'?[threadMethod,'turn/start'].includes(name):['inputs','status','operation','invoke','read'].includes(name),'Unbound driver tool '+namespace+'.'+name);
   assert.ok(definition);
   reply(id,{provider:{node:{serviceNodeId:'native-agent'}},items:[{qualifiedName:namespace+'.'+name,definition,definitionHash:toolDefinitionHash(definition),catalogHash:catalog.catalogHash}],nextCursor:null});
   return true;
  }
  if(method!=='tools.call')return false;
  assert.ok(catalog);
  const name=params.qualifiedName;
  if(name==='agent.inputs')reply(id,{epoch:'epoch',items:[],truncated:false});
  else if(name==='agent.status')reply(id,{serviceNodeId:'native-agent',hostId:'fixture-native-host',nativeVersion:catalog.catalog.version,nativeExecutableHash:catalog.catalog.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch:'epoch',pid:42,state:'ready',observedAt:'2026-01-02T03:04:05.006Z',code:null,initialized:{},pendingInputs:0,operations:{retained:operations.size,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]});
  else if(name==='agent.operation'){
   const operationId=params.arguments.operationId,operation=operations.get(operationId);
   if(!operation){
    const absence={kind:'agent_operation_absent',operationId,serviceNodeId:'native-agent',epoch:'epoch'};
    emit({kind:'absence',absence});fail('not_found','Exact isolated owner absence.','not_executed',absence);
   }else if(selected===1){
    emit({kind:'receipt-unavailable',operationId});
    fail('fixture_receipt_wait','The committed original receipt is unavailable until the replacement generation.');
   }else{
    emit({kind:'original-operation',operation});reply(id,operation);
   }
  }else if(name==='agent.invoke'){
   const a=params.arguments;
   assert.equal(a.operationId,params.operationId);
   assert.ok([threadMethod,'turn/start'].includes(a.method));
   assert.equal(a.nativeVersion,catalog.catalog.version);
   assert.equal(a.expectedDefinitionHash,toolDefinitionHash(catalog.definitions.get(a.method)));
   assert.ok(!operations.has(a.operationId),'A native operation was dispatched twice');
   if(a.method==='thread/resume'){assert.equal(a.params.threadId,'thread');assert.equal(a.params.excludeTurns,true);}
   if(a.method==='turn/start'){
    assert.equal(a.params.threadId,'thread');
    assert.deepEqual(a.params.input,[{type:'text',text:'retained input',text_elements:[]},{type:'text',text:'driver prompt\ud800',text_elements:[]}]);
   }
   const operation={schemaVersion:1,operationId:a.operationId,callerPrincipalId:'fixture-principal',serviceNodeId:'native-agent',nativeVersion:a.nativeVersion,nativeExecutableHash:catalog.catalog.nativeExecutableHash,method:a.method,params:a.params,requestHash:hashJson({method:a.method,params:a.params}),phase:'succeeded',createdAt:'2026-01-02T03:04:05.006Z',updatedAt:'2026-01-02T03:04:05.007Z',epoch:'epoch',requestId:operations.size+1,reply:{result:a.method==='turn/start'?{turn:{id:'turn'}}:{thread:{id:'thread',ephemeral:false}}},code:null};
   operations.set(a.operationId,operation);emit({kind:'committed-operation',operation});
   if(a.method==='turn/start'){assert.equal(selected,2);reply(id,operation);}
   else{assert.equal(selected,1);fail('outcome_unknown','The isolated owner committed; its invocation reply was lost.','unknown');}
  }else if(name==='agent.read'){
   const a=params.arguments;
   if(a.method==='thread/turns/list'){
    emit({kind:'inspection-boundary'});fail('fixture_driver_stop','Terminal inspection is outside this driver scenario.');
   }else{
    assert.equal(selected,2);assert.equal(a.method,'thread/read');assert.deepEqual(a.params,{threadId:'thread',includeTurns:false});
    const observation={schemaVersion:1,observationId:'driver-read-'+(++read),callerPrincipalId:'fixture-principal',serviceNodeId:'native-agent',nativeVersion:catalog.catalog.version,nativeExecutableHash:catalog.catalog.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch:'epoch',requestId:read,method:a.method,params:a.params,requestHash:hashJson({method:a.method,params:a.params}),observedAt:'2026-01-02T03:04:05.008Z',reply:{result:{thread:{id:'thread',ephemeral:false,canAcceptDirectInput:true,status:{type:'idle'}}}}};
    emit({kind:'fresh-primary',observation});reply(id,observation);
   }
  }else throw Error('Unbound isolated driver call '+name);
  return true;
 };
}
