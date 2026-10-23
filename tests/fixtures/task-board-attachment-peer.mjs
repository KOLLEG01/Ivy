import assert from 'node:assert/strict';
import {checkedNativeContract} from '../../packages/contracts/src/checked-native-contract.ts';
import {hashJson} from '../../packages/contracts/src/canonical.ts';
import {toolDefinitionHash} from '../../packages/contracts/src/tool-definition.ts';

// A retained cancelled turn remains visible across two native owner epochs.
// Only thread/resume is dispatchable; no replacement turn or input is exposed.
export function attachmentBoundary(){
 let row=null,catalog=null,settled=false,reads=0;
 const operations=new Map();
 return ({method,params,id,selected,send,reply,emit})=>{
  const fail=(code,message,details={})=>send({jsonrpc:'2.0',id,error:{code:-32000,message,data:{code,outcome:'not_executed',details}}});
  if(method==='test.nativeObservations'&&params.row.input.liveAttachment===true){
   assert.equal(row,null);row=structuredClone(params.row);
   catalog=checkedNativeContract(row.run.value.plan.nativeVersion,{sourceHash:row.run.value.plan.catalogSourceHash});
   reply(id,{});return true;
  }
  if(!row)return false;
  const epoch=row.input.staleAttachment?(selected===1?'first-replacement':'replacement-epoch'):'epoch';
  if(method==='tools.list'){
   const namespace=params.namespace,name=params.namePrefix;
   if(name==='resolveWorkspace'){fail('service_unavailable','Finite saved workspace boundary.');return true;}
   assert.ok(namespace==='codex'?name==='thread/resume':['inputs','status','read','operation','invoke','notifications'].includes(name),'Unexpected attachment tool '+namespace+'.'+name);
   const definition=namespace==='codex'?catalog.definitions.get(name):{namespace:'agent',name,interfaceVersion:'1.0.0',annotations:{readOnlyHint:name!=='invoke'},inputSchema:{},outputSchema:{}};
   reply(id,{provider:{node:{serviceNodeId:'native-agent'}},items:[{qualifiedName:namespace+'.'+name,definition,definitionHash:toolDefinitionHash(definition),catalogHash:catalog.catalogHash}],nextCursor:null});return true;
  }
  if(method!=='tools.call')return false;
  const name=params.qualifiedName,a=params.arguments;
  if(name==='agent.inputs')reply(id,{epoch,items:[],truncated:false});
  else if(name==='agent.notifications'){
   assert.deepEqual(a,{afterSequence:0,limit:100});
   reply(id,{epoch,firstAvailableSequence:1,throughSequence:0,gap:false,hasMore:false,items:[]});
  }
  else if(name==='agent.status'){
   const status={serviceNodeId:'native-agent',hostId:'fixture-native-host',nativeVersion:catalog.catalog.version,nativeExecutableHash:catalog.catalog.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch,pid:42,state:'ready',observedAt:row.metadata.observedAt,code:null,initialized:{},pendingInputs:0,operations:{retained:operations.size,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
   emit({kind:'attachment-status',status});reply(id,status);
  }else if(name==='agent.operation'){
   const operation=operations.get(a.operationId);
   if(operation){
    if(row.input.staleAttachment){
     assert.equal(selected,1,'The uncertain old resume must be reattached, never replayed in a new epoch');
     emit({kind:'attachment-retained-unknown',operation});reply(id,operation);
    }else if(selected===1){emit({kind:'attachment-receipt-unavailable',operationId:a.operationId});fail('fixture_receipt_wait','The original resume receipt is unavailable until reconnection.');}
    else {assert.equal(selected,2);settled=true;emit({kind:'attachment-recovered',operation});reply(id,operation);}
   }else{
    assert.ok(selected<=2);
    const absence={kind:'agent_operation_absent',operationId:a.operationId,serviceNodeId:'native-agent',epoch};
    emit({kind:'attachment-absence',absence});fail('not_found','Exact retained resume absence.',absence);
   }
  }else if(name==='agent.invoke'){
   assert.ok(selected<=2);assert.equal(a.operationId,params.operationId);
   assert.equal(a.method,'thread/resume');assert.equal(a.nativeVersion,catalog.catalog.version);
   assert.equal(a.expectedDefinitionHash,toolDefinitionHash(catalog.definitions.get('thread/resume')));
   assert.deepEqual(a.params,{threadId:'thread',excludeTurns:true});
   assert.equal(operations.size,selected-1,'Each owner epoch gets exactly one distinct attachment');
   assert.ok(!operations.has(a.operationId),'An uncertain resume may not be dispatched again');
   const uncertain=selected===1&&row.input.staleAttachment;
   const operation={schemaVersion:1,operationId:a.operationId,callerPrincipalId:'fixture-principal',serviceNodeId:'native-agent',nativeVersion:a.nativeVersion,nativeExecutableHash:catalog.catalog.nativeExecutableHash,method:a.method,params:a.params,requestHash:hashJson({method:a.method,params:a.params}),phase:uncertain?'outcome_unknown':'succeeded',createdAt:'2026-01-02T03:04:05.006Z',updatedAt:'2026-01-02T03:04:05.007Z',epoch,requestId:selected,reply:uncertain?null:{result:{thread:{id:'thread',ephemeral:false}}},code:uncertain?'native_connection_lost':null};
   operations.set(a.operationId,operation);settled=selected===2;
   emit({kind:'attachment-invoke',operation});
   if(!row.input.staleAttachment){send({jsonrpc:'2.0',id,error:{code:-32000,message:'The original attachment committed but its reply was lost.',data:{code:'outcome_unknown',outcome:'unknown',details:{}}}});}
   else reply(id,operation);
  }else if(name==='agent.read'){
   assert.equal(a.nativeVersion,catalog.catalog.version);
   let result;
   if(a.method==='thread/read'){
    assert.deepEqual(a.params,{threadId:'thread',includeTurns:false});
    result={thread:{id:'thread',ephemeral:false,status:{type:'active',activeFlags:[]}}};
   }else if(a.method==='thread/turns/list'){
    assert.deepEqual(a.params,{threadId:'thread',cursor:null,limit:100,itemsView:'notLoaded',sortDirection:'desc'});
    result={data:[{id:'turn',status:settled?'interrupted':'inProgress'}],nextCursor:null};
   }else{
    assert.equal(a.method,'thread/items/list');assert.ok(settled);
    assert.deepEqual(a.params,{threadId:'thread',turnId:'turn',cursor:null,limit:1,sortDirection:'desc'});
    result={data:[{turnId:'turn',item:{id:'answer',type:'agentMessage',phase:'final_answer',text:'answer'}}],nextCursor:null};
   }
   const count=++reads,observation={...row.metadata,observationId:'attachment-read-'+count,callerPrincipalId:'fixture-principal',epoch,requestId:count,method:a.method,params:a.params,requestHash:hashJson({method:a.method,params:a.params}),observedAt:'2026-01-02T03:04:05.'+String(10+count).padStart(3,'0')+'Z',reply:{result}};
   emit({kind:'attachment-observation',observation});reply(id,observation);
  }else throw Error('Unexpected retained attachment call '+name);
  return true;
 };
}
