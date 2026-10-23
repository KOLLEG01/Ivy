import assert from 'node:assert/strict';
import {checkedNativeContract} from '../../packages/contracts/src/checked-native-contract.ts';
import {hashJson} from '../../packages/contracts/src/canonical.ts';
import {toolDefinitionHash} from '../../packages/contracts/src/tool-definition.ts';

// Finite provider for retained cancellation and steering source cases. It
// advertises only the management/read methods this control boundary consumes.
export function controlBoundary(){
 let catalog=null,replacement=false,steering=false,race=false,raceEpoch=false,reads=0;
 const operations=new Map();
 return ({method,params,id,selected,send,reply,emit})=>{
  const epoch=raceEpoch||selected>1&&replacement?'replacement-epoch':'epoch';
  const fail=(code,message,outcome='not_executed',details={})=>send({jsonrpc:'2.0',id,error:{code:-32000,message,data:{code,outcome,details}}});
  if(method==='test.control'){
   assert.equal(catalog,null);
   catalog=checkedNativeContract(params.plan.nativeVersion,{sourceHash:params.plan.catalogSourceHash});
   replacement=params.replacement;
   steering=params.steering===true;
   race=params.interruptRace===true;
   assert.ok(!race||!steering);
   reply(id,{});return true;
  }
  if(method==='tools.list'){
   assert.ok(catalog,'Configure the control provider before releasing the Run lane');
   const name=params.namePrefix,namespace=params.namespace;
   assert.ok(namespace==='codex'?name===(steering?'turn/steer':'turn/interrupt'):['inputs','status','read','operation','invoke',...(steering?['prevent']:[]),...(steering||race?['notifications']:[])].includes(name),'Unbound control tool '+namespace+'.'+name);
   const definition=namespace==='codex'?catalog.definitions.get(name):{namespace:'agent',name,interfaceVersion:'1.0.0',annotations:{readOnlyHint:!['invoke','prevent'].includes(name)},inputSchema:{},outputSchema:{}};
   reply(id,{provider:{node:{serviceNodeId:'native-agent'}},items:[{qualifiedName:namespace+'.'+name,definition,definitionHash:toolDefinitionHash(definition),catalogHash:catalog.catalogHash}],nextCursor:null});
   return true;
  }
  if(method!=='tools.call')return false;
  assert.ok(catalog);
  const name=params.qualifiedName,a=params.arguments;
  if(name==='agent.inputs')reply(id,{epoch,items:[],truncated:false});
  else if(name==='agent.notifications'){
   assert.ok(steering||race);assert.deepEqual(a,{afterSequence:0,limit:100});
   reply(id,{epoch,firstAvailableSequence:1,throughSequence:0,gap:false,hasMore:false,items:[]});
  }
  else if(name==='agent.status'){
   const status={serviceNodeId:'native-agent',hostId:'fixture-native-host',nativeVersion:catalog.catalog.version,nativeExecutableHash:catalog.catalog.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch,pid:42,state:'ready',observedAt:'2026-01-02T03:04:05.006Z',code:null,initialized:{},pendingInputs:0,operations:{retained:operations.size,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
   emit({kind:'control-status',status});reply(id,status);
  }else if(name==='agent.operation'){
   const operation=operations.get(a.operationId);
   if(!operation){
    if(race){
     assert.equal(selected,1);assert.equal(raceEpoch,false,'The original interrupt must not be queried again after the guard refuses it');
     raceEpoch=true;
     emit({kind:'epoch-switch',operationId:a.operationId,previousEpoch:epoch,epoch:'replacement-epoch'});
    }
    if(steering&&replacement&&selected===1){
     emit({kind:'owner-unavailable',operationId:a.operationId});
     fail('fixture_owner_wait','Original owner evidence is unavailable until reconnection.');return true;
    }
    assert.equal(selected,steering&&replacement?2:1,'Unexpected original operation absence');
    const absence={kind:'agent_operation_absent',operationId:a.operationId,serviceNodeId:'native-agent',epoch:race?'replacement-epoch':epoch};
    emit({kind:'absence',absence});fail('not_found','Exact isolated original operation absence.','not_executed',absence);
   }else if(selected===1){
    emit({kind:'receipt-unavailable',operationId:a.operationId});
    fail('fixture_receipt_wait','The committed interrupt receipt is unavailable until reconnection.');
   }else{
    assert.equal(epoch,'epoch','The replacement-epoch orchestrator must retain its uncertainty');
    emit({kind:'original-operation',operation});reply(id,operation);
   }
  }else if(name==='agent.invoke'){
   assert.equal(race,false,'An old turn must receive no input after its native owner epoch changes');
   assert.equal(selected,1);assert.equal(a.operationId,params.operationId);
   assert.equal(a.nativeVersion,catalog.catalog.version);
   assert.equal(a.method,steering?'turn/steer':'turn/interrupt');
   assert.equal(a.expectedDefinitionHash,toolDefinitionHash(catalog.definitions.get(a.method)));
   assert.deepEqual(a.params,steering?{threadId:'thread',expectedTurnId:'turn',input:[{type:'text',text:'advance prompt',text_elements:[]}]}:{threadId:'thread',turnId:'turn'});
   assert.equal(operations.size,0,'A retained interruption may be dispatched once');
   const operation={schemaVersion:1,operationId:a.operationId,callerPrincipalId:'fixture-principal',serviceNodeId:'native-agent',nativeVersion:a.nativeVersion,nativeExecutableHash:catalog.catalog.nativeExecutableHash,method:a.method,params:a.params,requestHash:hashJson({method:a.method,params:a.params}),phase:'succeeded',createdAt:'2026-01-02T03:04:05.006Z',updatedAt:'2026-01-02T03:04:05.007Z',epoch,requestId:1,reply:{result:steering?{turnId:'turn'}:{}},code:null};
   emit({kind:'native-invoke',operationId:a.operationId});
   operations.set(a.operationId,operation);emit({kind:'committed-operation',operation});
   fail('outcome_unknown','The original interruption committed but its reply was lost.','unknown');
  }else if(name==='agent.prevent'){
   assert.ok(steering&&replacement);assert.equal(selected,2);
   assert.equal(a.operationId,params.operationId);assert.equal(operations.size,0);
   assert.equal(a.nativeVersion,catalog.catalog.version);assert.equal(a.method,'turn/steer');
   assert.equal(a.expectedDefinitionHash,toolDefinitionHash(catalog.definitions.get(a.method)));
   assert.deepEqual(a.params,{threadId:'thread',expectedTurnId:'turn',input:[{type:'text',text:'advance prompt',text_elements:[]}]});
   const operation={schemaVersion:1,operationId:a.operationId,callerPrincipalId:'fixture-principal',serviceNodeId:'native-agent',nativeVersion:a.nativeVersion,nativeExecutableHash:catalog.catalog.nativeExecutableHash,method:a.method,params:a.params,requestHash:hashJson({method:a.method,params:a.params}),phase:'failed',createdAt:'2026-01-02T03:04:05.006Z',updatedAt:'2026-01-02T03:04:05.007Z',epoch:null,requestId:null,reply:null,code:'request_prevented'};
   operations.set(a.operationId,operation);emit({kind:'prevented-operation',operation});emit({kind:'committed-operation',operation});reply(id,operation);
  }else if(name==='agent.read'){
   assert.equal(a.nativeVersion,catalog.catalog.version);
   let result;
   if(a.method==='thread/read'){
    assert.deepEqual(a.params,{threadId:'thread',includeTurns:false});
    result={thread:{id:'thread',ephemeral:false,status:{type:'active',activeFlags:[]}}};
   }else{
    assert.equal(a.method,'thread/turns/list');
    assert.deepEqual(a.params,{threadId:'thread',cursor:null,limit:100,itemsView:'notLoaded',sortDirection:'desc'});
    result={data:[{id:'turn',status:'inProgress',items:[]}],nextCursor:null};
   }
   const observation={schemaVersion:1,observationId:'control-read-'+(++reads),callerPrincipalId:'fixture-principal',serviceNodeId:'native-agent',nativeVersion:catalog.catalog.version,nativeExecutableHash:catalog.catalog.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch,requestId:reads,method:a.method,params:a.params,requestHash:hashJson({method:a.method,params:a.params}),observedAt:'2026-01-02T03:04:05.008Z',reply:{result}};
   emit({kind:'control-observation',observation});reply(id,observation);
  }else throw Error('Unbound isolated control call '+name);
  return true;
 };
}
