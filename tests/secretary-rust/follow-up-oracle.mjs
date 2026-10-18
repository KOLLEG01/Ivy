import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,settings,owner,rootId,itemId,item,source,sourceId,at} from './inbox-fixtures.mjs';
const {SecretaryFollowUp,followUpPrompt}=await load('services/secretary/src/follow-up.js');
const {SecretaryEngine,sourceName}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {IvyError,hashJson,canonical,deriveOperationId}=await load('packages/sdk/src/node.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const sample=name=>JSON.parse(schemaSamples(ajv,[schema]).find(r=>r.valid&&r.reference.endsWith('#/$defs/'+name)).input);
const configurationSample=sample('SecretaryConfiguration'),executionSample=sample('Execution');
const catalog=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const target={serviceNodeId:'fixture.agent',hostId:'fixture-host',nativeVersion:catalog.version,nativeExecutableHash:catalog.nativeExecutableHash,catalogHash:hashJson(catalog)};
const digest=c=>'sha256:'+c.repeat(64),key='secretary/follow-up',contextKey='secretary/follow-up-context';
const time=Date.parse(at),wall=1800000000000,id=`fixture-epoch:${wall}:follow-up`;
Date.now=()=>wall;
const principal=settings.identity.principalId;
const baseContext={schemaVersion:1,itemId,threadId:'context-thread',target,model:'original-model',effort:'medium',threadCwd:'/workspace',developerInstructions:'Original instructions',itemSnapshot:'Original \ud800 message é😀',retainedAt:at,expiresAt:time+90*86400000};
const baseStatus={...target,epoch:'epoch',pid:42,state:'ready',observedAt:at,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
const rows=[];
async function add(label,c={}){
 const caller=c.caller??principal,request={operationId:id,itemId,question:'What happened? \ud800 é😀',...c.request};
 const name='follow-up-'+caller+'-'+request.operationId;
 const config={...structuredClone(settings),...c.settings};
 const context={...structuredClone(baseContext),...c.context};
 const record={request,callerPrincipalId:caller,threadId:null,turnId:null,phase:'prepared',calls:null,answer:null,errorCode:null,expiresAt:time+90*86400000,...c.saved};
 const documents=[];
 if(!c.newContext)documents.push({id:'context',key:contextKey,name:c.namedContext?name:request.itemId,value:context});
 if(c.preparedContext){const method='thread/start',params={cwd:'/original',projectId:'original-project',permissions:':read-only',approvalPolicy:'never'};documents[0].value={...context,threadId:null,start:{operationId:deriveOperationId(request.operationId,'start-secretary-follow-up-context'),method,params,definitionHash:digest('b')},createdFor:name};}
 if(c.legacy)documents[0]={id:'context',key:contextKey,name,value:{...context,threadId:''}};
 if(c.saved)documents.push({id:'question',key,name,value:record});
 if(c.busy)documents.push({id:'competitor',key,name:'earlier',value:{...record,threadId:context.threadId,phase:c.busy}});
 if(c.extra)documents.push(...c.extra);
 const trace=[],steps=[],counts={},operations=new Map();let reads=0,store;
 const control=new AbortController();if(c.cancel)control.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const execution={serviceNodeId:target.serviceNodeId,threadCwd:'/workspace',model:'original-model',effort:'medium',permissions:':read-only',...c.execution};
 const configuration={...structuredClone(configurationSample),execution:c.noExecution?null:execution};
 const fullExecution={...structuredClone(executionSample),serviceNodeId:target.serviceNodeId};
 const objectId='00000000-0000-4000-8000-000000000006';
 const raw=(objectId,value,contractKey)=>({object:{id:objectId,parentId:rootId,effectivelyArchived:false,contractKey},revision:{revision:1,contractVersion:contractVersion(contractKey),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest(contractKey,value,{object:{objectId,revision:1}},'fixture:1800000000000:read').references,createdAt:at},content:{encoding:'json',value}});
 const retainedCall=operationId=>{
  for(const d of store.technicalList(contextKey)){if(d.value.start?.operationId===operationId)return d.value.start;}
  for(const d of store.technicalList(key)){for(const call of Object.values(d.value.calls??{}))if(call.operationId===operationId)return call;}
  assert.fail('Missing original retained call '+operationId);
 };
 const operation=call=>({schemaVersion:1,operationId:call.operationId,callerPrincipalId:principal,serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,method:call.method,params:call.params,requestHash:hashJson({method:call.method,params:call.params}),createdAt:at,updatedAt:at,phase:'succeeded',epoch:'epoch',requestId:42,reply:{result:c.replies?.[call.method]??(call.method==='turn/start'?{turn:{id:'original-turn'}}:{thread:{id:'context-thread'}})},code:null,...c.operation,...c.operations?.[call.method]});
 const client={async request(method,p,options){
  trace.push({method,params:structuredClone(p),timeout:options?.timeoutMs??35000});let result,error;
  const counter=method==='tools.call'?p.qualifiedName:method;counts[counter]=(counts[counter]??0)+1;
  const fault=(c.faults??[]).find(f=>f.method===counter&&f.n===counts[counter]);
  if(c.fail===counter||fault)error={code:fault?.code??'fixture_failure',message:'Fixture failure.',outcome:fault?.outcome??'unknown',...(fault?.details?{details:fault.details}:{})};
  else if(method==='serviceNodes.get'){
   result=p.serviceNodeId==='fixture.secretary'?{...owner,...c.owner}:p.serviceNodeId==='fixture.voice'?{serviceNodeId:'fixture.voice',principalId:'voice-principal',serviceName:'phone-bridge',connected:true,synced:true,...c.voiceNode}:{serviceNodeId:'fixture.chat',principalId:'chat-principal',serviceName:'chat-bridge',connected:true,synced:true,ready:true,...c.mainNode};
  }else if(method==='objects.stat')result={contractKey:c.contractKey??'secretary/item',parentId:rootId,...c.source};
  else if(method==='objects.query')result={items:c.noConfiguration?[]:[{objectId,revision:1}],nextCursor:null};
  else if(method==='objects.read')result=p.objectId===objectId?raw(objectId,configuration,'secretary/configuration'):raw(p.objectId,c.contractKey==='secretary/execution'?fullExecution:item,c.contractKey??'secretary/item');
  else if(method==='serviceNodes.list')result={items:[],nextCursor:null};
  else if(method==='tools.list')result={provider:{node:{serviceNodeId:p.serviceNodeId??'fixture.chat'}},items:[{qualifiedName:p.namespace+'.'+p.namePrefix,definition:{namespace:p.namespace,name:p.namePrefix,interfaceVersion:p.namespace==='codex'?target.nativeVersion:p.namespace==='chat'?'1.2.0':'1.0.0',...(p.namePrefix==='projects'?{annotations:{readOnlyHint:true}}:{}),inputSchema:{},outputSchema:{}},definitionHash:digest(p.namespace==='codex'?'b':'a'),catalogHash:target.catalogHash}],nextCursor:null};
  else if(method==='tools.call'){
   if(p.qualifiedName==='chat.workspace')result=c.workspace??{expectedBridge:{principalId:'chat-principal',callerPrincipalId:principal},nativeOwnerReady:true,channels:[{channel:{adapter:'whatsapp',channelId:'original'}}]};
   else if(p.qualifiedName==='agent.status')result={...baseStatus,...c.status};
   else if(p.qualifiedName==='agent.projects')result={source:'native',observedAt:at,projects:[{nativeId:'project',name:'project',paths:['/workspace'],source:'native'}],defaults:{projectRoot:'/projects',internalProjectRoot:'/internal'}};
   else if(p.qualifiedName==='agent.operation'){
    const call=retainedCall(p.arguments.operationId);
    if(c.dispatch&&!operations.has(call.operationId))error={code:'not_found',message:'Original operation absent.',outcome:'not_executed',details:{kind:'agent_operation_absent',operationId:call.operationId,serviceNodeId:target.serviceNodeId,epoch:'epoch'}};
    else result=operations.get(call.operationId)??operation(call);
   }else if(p.qualifiedName==='agent.invoke'){
    const call=retainedCall(p.arguments.operationId);result=operation(call);operations.set(call.operationId,result);
    if(c.lose===call.method)error={code:'outcome_unknown',message:'Dispatch reply lost.',outcome:'unknown'};
   }else if(p.qualifiedName==='agent.read'){
    const {method,params}=p.arguments,full=params.itemsView==='full';
    const items=c.items??[{id:'final',type:'agentMessage',phase:'final_answer',text:'Original answer \ud800 é😀'}];
    const turn={id:'original-turn',status:full?(c.fullStatus??c.turnStatus??'completed'):(c.turnStatus??'completed'),itemsView:params.itemsView,items:full?items:[],...c.row};
    const reply=method==='thread/items/list'?{error:{code:c.itemError??-32601,message:'Fixture full-turn fallback'}}:c.readReply??{result:{data:[turn]}};
    result={schemaVersion:1,observationId:'read-'+(++reads),callerPrincipalId:principal,serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,catalogHash:target.catalogHash,epoch:'epoch',requestId:42,observedAt:at,requestHash:hashJson({method,params}),method,params,reply,...c.observation};
   }else assert.fail('Unexpected tool '+p.qualifiedName);
  }else assert.fail('Unexpected RPC '+method);
  if(c.concurrentAt===trace.length){const current=store.technicalNamed(key,name);store.technicalAmend(key,current,{...current.value,errorCode:'concurrent-change'});}
  if(c.cancelAfter===trace.length)control.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
  if(error){steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome,error.details);}
  steps.push({method,value:structuredClone(result)});return structuredClone(result);
 }};
 store=new SecretaryStore(client,rootId);
 const locals=[{id:sourceId,key:'secretary/source',name:sourceName('mail'),value:source}];
 for(const d of locals){const value=canonical(d.value);store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(d.id,d.key,d.name,1,0,null,value);store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(d.id,1,value);}
 for(const d of documents){const value=canonical(d.value);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(d.id,d.key,d.name,1,value);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(d.id,1,value);}
 if(c.expiredBefore)store.db.prepare('INSERT INTO journal_state VALUES(?,?)').run('expiredBefore',String(c.expiredBefore));
 const issues=new Map(c.issues??[]);
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:config,owner:{serviceNodeId:'fixture.secretary'},signal:control.signal,now:()=>new Date(c.time??time),epoch:Promise.resolve('fixture-epoch'),recoveryIssues:issues});
 const worker=new SecretaryFollowUp(engine),actions=c.actions??[{method:'run'},{method:c.legacy?'importAndAdvance':'advance'},{method:'read'}];
 const results=[];
 for(const action of actions){
  try{
   let value;const args=action.request??request,who=action.caller??caller,operationId=action.operationId??args.operationId;
   if(action.method==='run')value=await worker.run(args,who);
   else if(action.method==='read')value=await worker.read(operationId,who);
   else if(action.method==='tick')value=await worker.tick();
   else if(action.method==='prompt')value=followUpPrompt(args.itemId,args.question);
   else if(action.method==='advance'||action.method==='importAndAdvance')value=await worker.advance(action.name??name);
   else assert.fail(action.method);
   results.push({value:value??null});
  }catch(cause){const e=IvyError.from(cause);results.push({code:e.code,message:e.message,outcome:e.outcome});}
 }
 const remaining=store.db.prepare('SELECT key,name,current_revision,value_json FROM technical_documents ORDER BY key,name').all().map(r=>({key:r.key,name:r.name,revision:r.current_revision,value:JSON.parse(r.value_json)}));
 const cleanup=store.db.prepare('SELECT d.key,d.name FROM technical_cleanup c JOIN technical_documents d ON d.id=c.root_id ORDER BY d.key,d.name').all();
 store.close();rows.push({input:{label,settings:config,caller,request,documents,locals,actions,steps,time:c.time??time,wall,expiredBefore:c.expiredBefore??0,cancel:c.cancel??false,cancelAfter:c.cancelAfter??null,concurrentAt:c.concurrentAt??null,issues:c.issues??[]},expected:{results,trace,remaining,cleanup,issues:[...issues],cursor:worker.cursor}});
}
await add('retained-context');
await add('new-item-context',{newContext:true});
await add('new-execution-context',{newContext:true,contractKey:'secretary/execution'});
await add('prepared-context',{preparedContext:true});
await add('explicit-legacy-import',{legacy:true});
await add('named-context',{namedContext:true});
for(const model of [null,'','original-model'])await add('model-'+model,{context:{model}});
for(const phase of ['prepared','running','outcome_unknown','succeeded','failed'])await add('saved-'+phase,{saved:{phase},actions:[{method:'run'},{method:'advance'},{method:'read'}]});
await add('replay-expired-id',{saved:{},request:{operationId:'other:1:expired'},expiredBefore:wall});
await add('fresh-expired-id',{request:{operationId:'other:1:expired'}});
await add('fresh-boundary',{expiredBefore:wall});
await add('fresh-future',{request:{operationId:`fixture-epoch:${wall+60001}:future`}});
await add('different-question',{saved:{request:{operationId:id,itemId,question:'Original'}},actions:[{method:'run'}]});
await add('different-caller',{saved:{callerPrincipalId:'other'},actions:[{method:'run'}]});
await add('read-missing',{actions:[{method:'read'}]});
await add('read-failed',{saved:{phase:'failed',errorCode:'original'},actions:[{method:'read'}]});
await add('read-self-after-owner-change',{saved:{},owner:{connected:false},actions:[{method:'read'}]});
await add('owner-changed',{owner:{principalId:'other'}});
await add('cancelled',{cancel:true});
for(const caller of ['chat-principal','voice-principal','intruder'])await add('authorize-'+caller,{caller,settings:{policy:{...settings.policy,voiceEscalation:{serviceNodeId:'fixture.voice',recipientId:'user',allowFromPrincipalIds:[]}}},actions:[{method:'run'}]});
await add('explicit-principal',{caller:'explicit',settings:{followUpPrincipalIds:['explicit']},actions:[{method:'run'}]});
await add('explicit-surrogate-principal',{caller:'explicit-\ud800',settings:{followUpPrincipalIds:['explicit-\ud800']},actions:[{method:'run'},{method:'read'},{method:'tick'}]});
for(const mainNode of [{connected:false},{synced:false},{ready:false},{serviceName:'other'},{principalId:'other'}])await add('main-node-'+rows.length,{caller:'chat-principal',mainNode,actions:[{method:'run'}]});
for(const fail of ['tools.list','chat.workspace','serviceNodes.list'])await add('auth-'+fail,{caller:'intruder',fail,actions:[{method:'run'}]});
await add('uncoded-main-boundary',{caller:'intruder',workspace:{expectedBridge:null},actions:[{method:'run'}]});
for(const busy of ['running','outcome_unknown','succeeded','failed'])await add('busy-'+busy,{busy});
for(const context of [{itemId:'other'},{expiresAt:time},{threadId:null},{threadId:'other'}])await add('context-'+rows.length,{context,saved:{threadId:'context-thread',turnId:'original-turn',phase:'running'}});
await add('missing-retained-no-replacement',{newContext:true,saved:{threadId:'context-thread',turnId:'original-turn',phase:'running'}});
await add('expired-new-context',{context:{expiresAt:time}});
for(const source of [{contractKey:'other'},{parentId:'other'}])await add('source-'+rows.length,{newContext:true,source});
await add('missing-configuration',{newContext:true,noConfiguration:true});
await add('missing-execution-target',{newContext:true,noExecution:true});
for(const status of [{epoch:null},{nativeVersion:null},{serviceNodeId:'other'}])await add('new-target-'+rows.length,{newContext:true,status});
await add('new-start-failed',{newContext:true,operations:{'thread/start':{phase:'failed',reply:{error:{code:1,message:'Failed'}},code:'failed'}}});
for(const method of ['thread/start','thread/resume','turn/start']){
 const context=method==='thread/start'?{newContext:true}:{};
 await add('native-failed-'+method,{...context,operations:{[method]:{phase:'failed',reply:{error:{code:1,message:'Failed'}},code:'failed'}}});
 await add('native-pending-'+method,{...context,operations:{[method]:{phase:'accepted',epoch:null,requestId:null,reply:null}}});
 await add('native-dispatch-'+method,{...context,dispatch:true});
 await add('native-lost-reply-'+method,{...context,dispatch:true,lose:method});
}
for(const reply of [{turn:{}},{turn:{id:null}},{turn:{id:0}},{turn:{id:'\ud800'}}])await add('turn-identity-'+rows.length,{replies:{'turn/start':reply}});
for(const reply of [{thread:{}},{thread:{id:null}},{thread:{id:0}},{thread:{id:'\ud800'}}])await add('thread-identity-'+rows.length,{newContext:true,replies:{'thread/start':reply}});
for(const turnStatus of ['inProgress','failed','interrupted'])await add('turn-'+turnStatus,{turnStatus});
await add('changed-turn',{row:{id:'other'}});
await add('missing-turn',{readReply:{result:{data:[]}}});
await add('native-status-stopped',{status:{epoch:null,state:'stopped',pid:null}});
await add('read-wrong-epoch',{observation:{epoch:'other'}});
await add('full-status-changed',{fullStatus:'failed'});
await add('item-read-failed',{itemError:1});
for(const items of [[],[{id:'one',type:'agentMessage',phase:'commentary',text:'Wrong'}],[{id:'one',type:'agentMessage',phase:'final_answer',text:1}],[{id:'one',type:'agentMessage',phase:'final_answer',text:'One'},{id:'two',type:'agentMessage',phase:'final_answer',text:'Two'}],[{id:'same'},{id:'same'}]])await add('answer-'+rows.length,{items});
for(const fail of ['agent.operation','agent.status','agent.read'])await add('failure-'+fail,{fail});
await add('retained-calls-survive-read-failure',{faults:[{method:'agent.operation',n:2}],actions:[{method:'run'},{method:'advance'},{method:'advance'},{method:'read'}]});
for(const n of [1,3,5,8,12])await add('cancel-at-'+n,{cancelAfter:n});
await add('concurrent-question-revision',{concurrentAt:4});
await add('tick-clears-old-issue',{issues:[['follow-up-follow-up','unrelated'],['follow-up:follow-up-'+principal+'-'+id,'old']],actions:[{method:'run'},{method:'tick'},{method:'read'}]});
await add('tick-reports-failure',{fail:'agent.operation',actions:[{method:'run'},{method:'tick'},{method:'read'}]});
await add('tick-wraps-cursor',{settings:{recordsPerTick:1},actions:[{method:'run'},{method:'tick'},{method:'tick'},{method:'read'}]});
await add('prompt-code-units',{actions:[{method:'prompt'}],request:{itemId:'id-\ud800',question:'Question \udfff'}});
console.log(JSON.stringify({rootId,epoch:'fixture-epoch',rows}));
