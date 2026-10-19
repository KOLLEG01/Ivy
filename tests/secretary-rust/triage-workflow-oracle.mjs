import assert from 'node:assert/strict';
import {readFileSync,mkdtempSync,rmSync,realpathSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,dirname,basename} from 'node:path';
import {load,settings as base,owner,rootId,itemId,sourceId,item as baseItem,source as baseSource,assessment,definition} from './inbox-fixtures.mjs';
import {normalizeTriage} from './triage-state.mjs';
const {SecretaryTriage}=await load('services/secretary/src/triage.js');
const {TriageNative}=await load('services/secretary/src/triage-native.js');
const {TriageResults}=await load('services/secretary/src/triage-results.js');
const {triageName}=await load('services/secretary/src/triage-store.js');
const {triagePrompt,triageNativeParams}=await load('services/secretary/src/triage-prompt.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {SecretaryEngine,sourceName,identityHash,messageHash}=await load('services/secretary/src/engine.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {IvyError,canonical,digest,hashJson}=await load('packages/sdk/src/node.js');
const {secretaryNotificationPolicy}=await load('instructions/secretary-notification-policy.js');
const catalog=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const target={serviceNodeId:'fixture.agent',hostId:'fixture-host',nativeVersion:catalog.version,nativeExecutableHash:catalog.nativeExecutableHash,catalogHash:hashJson(catalog)};
const clone=structuredClone,at='2026-09-08T09:00:00.000Z',time=Date.parse(at),wall=1800000000000,hash=c=>'sha256:'+c.repeat(64);
Date.now=()=>wall;
const config={schemaVersion:1,target,model:'original-model',effort:'medium',instructions:'Original explicit rules.',threadCwd:process.platform==='win32'?'C:\\fixture\\triage':'/fixture/triage',maximumConcurrent:1};
const itemPin={objectId:itemId,revision:1},planPin={objectId:'plan',revision:1};
const originalId=slot=>'fixture-epoch:'+wall+':triage-'+slot;
const rows=[];
const set=(v,path,next)=>{const keys=path.split('.'),last=keys.pop();for(const k of keys)v=v[k];v[last]=clone(next);};
async function add(label,c={}){
 const input={label,mode:'begin',attempts:1,settings:clone(base),configuration:clone(config),core:[],documents:[],blobs:[],...clone(c)};
 for(const[p,v]of input.configChanges??[])set(input.configuration,p,v);
 const settings=input.settings,configuration=input.configuration;settings.identity.serviceNodeId??='fixture.secretary';
 const item=clone(baseItem);if(input.attachments)item.message.attachments='expected';for(const[p,v]of input.itemChanges??[])set(item,p,v);
 item.identityHash=identityHash(item.sourceId,item.message);item.messageHash=messageHash(item.message);
 input.item=item;input.itemRevision=input.itemRevision??1;
 const publicInput={item:clone(itemPin),source:clone(definition),message:clone(item.message),contentScope:'captured_text_or_caption',media:null,policy:clone(base.policy)};
 const prompt=triagePrompt(publicInput,config),params=triageNativeParams(config,prompt,[]);
 for(const[p,v]of input.paramsChanges??[])set(params,p,v);
 const blob=(kind,identity,value,id)=>{const bytes=Buffer.from(canonical(value,8*1024*1024));input.blobs.push({id,key:'secretary/triage-native-'+kind,name:triageName('native-'+kind,[identity,digest(bytes)]),bytes:bytes.toString('base64')});return{objectId:id,revision:1};};
 const parameters=blob('parameters',itemPin,{schemaVersion:1,item:itemPin,params},'parameters');
 const plan={schemaVersion:1,identity:{...clone(base.identity),serviceNodeId:'fixture.secretary'},settings:clone(config),input:publicInput,...prompt,nativeParams:parameters,bindings:['thread/start','thread/resume','turn/start','thread/unsubscribe'].map(method=>({method,definitionHash:hash('d'),outputSchema:{}})),createdAt:at};
 for(const[p,v]of input.planChanges??[])set(plan,p,v);
 if(!input.noPlan)input.documents.push({id:'plan',key:'secretary/triage-plan',name:triageName('plan',itemPin),revision:input.planRevision??1,value:plan});
 const work={schemaVersion:1,item:clone(itemPin),plan:clone(planPin),operationId:originalId('assessment'),phase:'pending',completion:null,assessment:null,errorCode:null,...clone(input.workChanges??{})};
 if(!input.noWork)input.documents.push({id:'work',key:'secretary/triage-work',name:triageName('work',itemPin),revision:1,value:work});
 input.core.push({id:sourceId,key:'secretary/source',name:sourceName('mail'),revision:1,value:clone(baseSource)});
 const invocation=slot=>{const method=slot==='thread'?(input.resumeThread?'thread/resume':'thread/start'):slot==='turn'?'turn/start':slot==='cleanup'?'thread/unsubscribe':'thread/resume';return{method,params:method==='thread/start'?clone(params.threadStart):method==='thread/resume'?{...clone(params.threadResume),threadId:'original-thread'}:method==='turn/start'?{...clone(params.turnStart),threadId:'original-thread'}:{threadId:'original-thread'}};};
 const phase=slot=>input.pendingSlot===slot?(input.pendingPhase??'accepted'):'succeeded';
 function operation(call,slot,selected=phase(slot)){
  const result=slot==='turn'?{turn:{id:'original-turn'}}:slot==='cleanup'?{status:input.cleanupStatus??'unsubscribed'}:{thread:{id:input.threadId??'original-thread',ephemeral:input.ephemeral??false},model:input.model??'original-model'};
  const value={schemaVersion:1,operationId:call.operationId,callerPrincipalId:base.identity.principalId,serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,method:call.method,params:call.params,requestHash:hashJson({method:call.method,params:call.params}),phase:selected,createdAt:at,updatedAt:at,epoch:selected==='accepted'?null:slot.startsWith('resume:')&&input.reattach?input.epoch:input.operationEpoch??'epoch',requestId:selected==='accepted'?null:42,reply:selected==='succeeded'?{result}:selected==='failed'?{error:{code:-1,message:'Original failed'}}:null,code:selected==='failed'?'native_failed':selected==='outcome_unknown'?'outcome_unknown':null};
  if(input.operationChanges?.slot===slot)for(const[p,v]of input.operationChanges.changes)set(value,p,v);return value;
 }
 for(const slot of input.calls??['thread','turn']){
  const call={schemaVersion:1,plan:clone(planPin),slot,...invocation(slot),operationId:originalId(slot),definitionHash:hash('d')};
  const request=blob('request',[planPin,slot],call,'request-'+slot),value={...call,request,preparedAt:at,observation:null};delete value.params;
  if(input.retained!==false)value.observation=blob('operation',[planPin,slot],operation(call,slot,input.previousPhase??phase(slot)),'observation-'+slot);
  if(input.callChanges?.slot===slot)for(const[p,v]of input.callChanges.changes)set(value,p,v);
  input.documents.push({id:'call-'+slot,key:'secretary/triage-call',name:triageName('call',[planPin,slot]),revision:1,value});
 }
 if(input.shared)input.documents.push({id:'shared',key:'secretary/triage-thread',name:triageName('shared-thread',[target.serviceNodeId,config.threadCwd]),revision:1,value:{schemaVersion:1,plan:clone(planPin),threadId:'original-thread',...input.shared}});
 if(input.completion){const value={schemaVersion:1,plan:clone(planPin),turn:{objectId:'call-turn',revision:1},threadId:'original-thread',turnId:'original-turn',full:{objectId:'full',revision:1},verification:{objectId:'verification',revision:1},result:clone(assessment),completedAt:at,...clone(input.completionChanges??{})};input.documents.push({id:'completion',key:'secretary/triage-completion',name:triageName('completion',planPin),revision:1,value});}
 if(input.corruptBlob){const row=input.blobs.find(b=>b.id===input.corruptBlob.id);row.bytes=Buffer.from(input.corruptBlob.text??'',input.corruptBlob.encoding??'utf8').toString('base64');}
 const folder=input.reopen?mkdtempSync(join(tmpdir(),'secretary-triage-reference-')):null,path=folder??undefined;
 let store,engine,worker,reads=0,clock=0,cut=false;const trace=[],steps=[],remote=new Map([[1,clone(item)]]),issues=new Map(input.issues??[]),invoked=new Set();
 let current=input.itemRevision;remote.set(current,clone(item));
 const raw=(revision,value)=>({object:{id:itemId,parentId:rootId,effectivelyArchived:false,contractKey:'secretary/item'},revision:{revision,contractVersion:contractVersion('secretary/item'),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest('secretary/item',value,{object:{objectId:itemId,revision}},'fixture:'+wall+':read').references,createdAt:at},content:{encoding:'json',value}});
 function retainedCall(id){const row=store.technicalList('secretary/triage-call').find(d=>d.value.operationId===id);assert(row);const request=store.technicalBlob('secretary/triage-native-request','',row.value.request);return JSON.parse(request.bytes.toString('utf8'));}
 const client={async request(method,p,options){
  trace.push({method,params:clone(p),timeout:options?.timeoutMs??35000});let value;
  try{
   if(method==='serviceNodes.get')value={...owner,...input.owner};
   else if(method==='system.status')value={runtimeEpoch:'fixture-epoch'};
   else if(method==='objects.read'){const revision=p.revision??current;if(!remote.has(revision))throw new IvyError('not_found','Original item revision not present.','not_executed');value=raw(revision,remote.get(revision));}
   else if(method==='objects.write'){
    assert.equal(p.objectId,itemId);if(input.writeConflict)throw new IvyError('revision_conflict','Original item changed.','not_executed');
    if(p.expectedRevision!==current)throw new IvyError('revision_conflict','Original item changed.','not_executed');remote.set(++current,clone(p.content.value));value=raw(current,remote.get(current));
    if(input.writeLost&&!cut){cut=true;throw new IvyError('fixture_write_lost','Original assessment receipt lost.','unknown');}
   }else if(method==='objects.query')value={items:input.queryEmpty?[]:[{objectId:itemId,revision:current}],nextCursor:input.queryCursor??null};
   else if(method==='tools.list')value={provider:{node:{serviceNodeId:target.serviceNodeId}},items:[{qualifiedName:p.namespace+'.'+p.namePrefix,definition:{namespace:p.namespace,name:p.namePrefix,interfaceVersion:p.namespace==='agent'?'1.0.0':target.nativeVersion,...(p.namePrefix==='projects'?{annotations:{readOnlyHint:true}}:{}),inputSchema:{},outputSchema:{}},definitionHash:hash(p.namespace==='agent'?'e':'d'),catalogHash:target.catalogHash}],nextCursor:null};
   else if(method==='tools.call'){
    if(p.qualifiedName==='agent.status')value={...target,state:'ready',epoch:input.epoch??'epoch',pid:42,observedAt:at,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:1000,bytes:0,maximumBytes:1048576},observedMethods:[],...input.status};
    else if(p.qualifiedName==='agent.catalog')value=catalog;
    else if(p.qualifiedName==='agent.projects')value={source:'native',observedAt:at,projects:[{nativeId:'project',name:'project',paths:[config.threadCwd],source:'native'}],defaults:{projectRoot:'/projects',internalProjectRoot:'/internal'}};
    else if(p.qualifiedName==='agent.operation'||p.qualifiedName==='agent.invoke'){
     const call=retainedCall(p.arguments.operationId),slot=call.slot;
     if(p.qualifiedName==='agent.operation'&&(input.absentSlot===slot||input.absentSlots?.includes(slot))&&!invoked.has(slot))throw new IvyError('not_found','Original native absent.','not_executed',{kind:'agent_operation_absent',serviceNodeId:target.serviceNodeId,operationId:call.operationId,epoch:input.absenceEpoch??input.epoch??'epoch'});
     if(p.qualifiedName==='agent.invoke'){assert.equal(p.arguments.expectedDefinitionHash,hash('d'));assert.deepEqual(p.arguments.params,call.params);invoked.add(slot);if(input.invokeLost&&!cut){cut=true;throw new IvyError('fixture_send_lost','Original native send uncertain.','unknown');}}
     value=operation(call,slot,input.observedPhase??phase(slot));
    }else if(p.qualifiedName==='agent.read'){
     const {method,params}=p.arguments;let reply;
     const finalItems=input.items??[{id:'user',type:'userMessage',text:'original'},{id:'final',type:'agentMessage',phase:'final_answer',text:JSON.stringify(assessment)}];
     if(method==='thread/read')reply={result:{thread:{id:input.readThread??'original-thread',ephemeral:false,status:{type:input.threadState??'idle'},canAcceptDirectInput:true}}};
     else if(method==='thread/items/list'){
      if(input.paged){const second=params.cursor!==null;reply={result:{data:(second?finalItems.slice(1):finalItems.slice(0,1)).map(item=>({turnId:'original-turn',item})),nextCursor:second?null:'page2'}};if(input.pageCycle)reply.result.nextCursor='page2';}
      else reply={error:{code:input.itemReadError??-32601,message:'Original unsupported item-page method'}};
     }else {const full=params.itemsView==='full';reply={result:{data:[{id:input.turnId??'original-turn',status:input.turnState??'completed',itemsView:params.itemsView,items:full?finalItems:[],error:input.turnError??null}]}};}
     value={schemaVersion:1,observationId:input.sameObservation?'same':'read-'+(++reads),callerPrincipalId:base.identity.principalId,...target,epoch:input.readEpoch??input.epoch??'epoch',requestId:42,observedAt:input.verificationBefore&&reads>2?'2026-09-08T08:59:59.000Z':at,requestHash:hashJson({method,params}),method,params,reply};delete value.hostId;
     if(input.readChanges)for(const[p,v]of input.readChanges)set(value,p,v);
    }else assert.fail('Unexpected tool '+p.qualifiedName);
   }else assert.fail('Unexpected RPC '+method);
   if(input.cutAfter===trace.length&&!cut){cut=true;throw new IvyError('fixture_cut','Original reply lost.','unknown');}
   steps.push({method,value:clone(value)});return clone(value);
  }catch(e){const error=IvyError.from(e);steps.push({method,error:{code:error.code,message:error.message,outcome:error.outcome,...(error.details===undefined?{}:{details:error.details})}});throw e;}
 }};
 function open(){store=new SecretaryStore(client,rootId,path);engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,epoch:Promise.resolve('fixture-epoch'),owner:{serviceNodeId:'fixture.secretary'},signal:new AbortController().signal,now:()=>new Date(time+(clock++)*(input.clockStep??0)),recoveryIssues:issues});worker=null;}
 open();
 for(const d of input.core){const bytes=canonical(d.value);store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(d.id,d.key,d.name,d.revision,0,null,bytes);store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(d.id,d.revision,bytes);}
 for(const d of input.documents){const bytes=canonical(d.value);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(d.id,d.key,d.name,d.revision,bytes);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(d.id,d.revision,bytes);}
 for(const b of input.blobs)store.db.prepare('INSERT INTO technical_blobs VALUES(?,?,?,?)').run(b.id,b.key,b.name,Buffer.from(b.bytes,'base64'));
 const results=[];
 for(let attempt=0;attempt<input.attempts;attempt++){
  if(attempt&&input.reopen){store.close();open();}
  try{worker??=new SecretaryTriage(engine,configuration);const native=worker.native;let value;
   if(input.mode==='constructor')value=null;
   else if(input.mode==='work')value=await native.work(itemPin);
   else if(input.mode==='prepare')value=await native.prepare(itemPin);
   else {const work=await native.work(itemPin),plan=await native.plan(work);
    if(input.mode==='plan')value=plan;
    else if(input.mode==='begin'){const result=await native.beginTurn(plan);value=result?{turn:result.turn,threadId:result.threadId,epoch:result.epoch}:null;}
    else if(input.mode==='release')value=await native.releaseClosed(plan);
    else if(input.mode==='completion'){const result=new TriageResults(native,plan);const call=await native.call(plan,'turn','turn/start','original-thread',false);value=await result.collect(call,'original-thread',input.epoch??'epoch');}
    else if(input.mode==='step')value=await worker.step(itemPin);
    else if(input.mode==='tick')value=input.concurrent?await Promise.all([worker.tick(),worker.tick()]):await worker.tick();
    else if(input.mode==='advance'){const call=await native.call(plan,'thread','thread/start',undefined,false);value=await native.advance(plan,call);}
    else assert.fail(input.mode);
   }results.push({value:value??null});
  }catch(e){const x=IvyError.from(e);results.push({code:x.code,message:x.message,outcome:x.outcome,...(x.details===undefined?{}:{details:x.details})});}
 }
 const docs=store.db.prepare('SELECT id,key,name,current_revision,value_json FROM technical_documents ORDER BY key,name').all(),blobs=store.db.prepare('SELECT * FROM technical_blobs ORDER BY key,name').all();
 const state={results,trace,issues:[...issues],retained:{documents:docs.map(d=>({id:d.id,key:d.key,name:d.name,revision:d.current_revision,value:JSON.parse(d.value_json)})),revisions:store.db.prepare('SELECT * FROM technical_revisions').all().map(r=>({id:r.id,revision:r.revision,value:JSON.parse(r.value_json)})),blobs:blobs.map(b=>({id:b.id,key:b.key,name:b.name,bytes:Buffer.from(b.bytes).toString('base64')}))},core:store.db.prepare('SELECT * FROM documents').all().map(d=>({id:d.id,key:d.key,name:d.name,revision:d.current_revision,value:JSON.parse(d.value_json),completedAt:d.completed_at})),cleanup:store.db.prepare('SELECT * FROM technical_cleanup').all().map(r=>({id:r.root_id,deleteAfter:r.delete_after}))};
 state.clockBounds=[wall,wall];input.steps=steps;store.close();if(folder){const saved=realpathSync(folder);assert.equal(dirname(saved),realpathSync(tmpdir()));assert(basename(saved).startsWith('secretary-triage-reference-'));rmSync(saved,{recursive:true,force:true});}rows.push({input,expected:normalizeTriage(state,input)});
}
await add('original retained start and turn');await add('retained replay',{attempts:2});await add('retained work reopens',{attempts:3,reopen:true});
await add('new original calls retain complete intent',{calls:[]});await add('fresh original plan and work',{mode:'prepare',noPlan:true,noWork:true,calls:[]});
await add('foreign unstarted shared reservation waits',{calls:[],shared:{plan:{objectId:'other-plan',revision:1},threadId:null}});
await add('original shared thread reattaches after restart',{epoch:'new-epoch',reattach:true,attempts:2,reopen:true});
await add('load original plan',{mode:'plan'});await add('read original work',{mode:'work'});await add('reuse prior original plan',{mode:'prepare'});await add('admit work from retained plan',{mode:'prepare',noWork:true});
await add('original pending start',{pendingSlot:'thread'});await add('original pending turn',{pendingSlot:'turn'});
for(const pendingPhase of ['failed','outcome_unknown'])for(const pendingSlot of ['thread','turn'])await add(pendingPhase+' '+pendingSlot,{pendingPhase,pendingSlot});
await add('poll original pending call',{retained:false});await add('retain progress then physical reopen',{retained:false,attempts:2,reopen:true});
await add('absent old call cannot erase observation',{mode:'advance',previousPhase:'dispatched',absentSlot:'thread'});
await add('confirmed original absence dispatches same ID',{retained:false,absentSlot:'thread'});await add('original send reply lost',{retained:false,absentSlot:'thread',invokeLost:true,attempts:2,reopen:true});
await add('turn dispatch checks original idle thread',{retained:false,absentSlot:'turn'});
await add('turn dispatch waits for original busy thread',{retained:false,absentSlot:'turn',threadState:'active'});
await add('turn dispatch checks exact thread',{retained:false,absentSlot:'turn',readThread:'foreign'});
await add('both original absent calls retain ordered intent',{retained:false,absentSlots:['thread','turn']});
await add('absence epoch changed',{retained:false,absentSlot:'thread',absenceEpoch:'old'});
for(const model of ['different',''])await add('model changed '+model,{model});await add('ephemeral thread refused',{ephemeral:true});await add('missing original thread',{threadId:''});
await add('foreign native target',{retained:false,operationChanges:{slot:'thread',changes:[['serviceNodeId','foreign']]}});
await add('foreign request hash',{retained:false,operationChanges:{slot:'turn',changes:[['requestHash',hash('f')]]}});
await add('foreign operation original ID',{operationChanges:{slot:'thread',changes:[['operationId','foreign:1:operation']]}});
await add('owner changed',{owner:{principalId:'foreign'}});
await add('original call has no observation',{retained:false,pendingSlot:'turn'});
await add('no replacement start for closed absent work',{mode:'release',retained:false,absentSlot:'thread'});
await add('closed original failed start releases reservation',{mode:'release',pendingSlot:'thread',pendingPhase:'failed'});
await add('closed original pending turn retains reservation',{mode:'release',pendingSlot:'turn'});
await add('closed unknown turn retains reservation',{mode:'release',pendingSlot:'turn',pendingPhase:'outcome_unknown'});
await add('closed work confirms original cleanup',{mode:'release',calls:['thread','turn','cleanup']});
await add('new cleanup dispatch preserves closed original reservation',{mode:'release',absentSlot:'cleanup'});
for(const cleanupStatus of ['notLoaded','notSubscribed','foreign'])await add('cleanup '+cleanupStatus,{mode:'release',calls:['thread','turn','cleanup'],cleanupStatus});
await add('closed work waits for idle native thread',{mode:'release',threadState:'active'});
await add('closed work validates exact native thread',{mode:'release',readThread:'foreign'});
await add('retained shared thread resumes',{resumeThread:true,shared:{threadId:'original-thread'}});
await add('original full turn fallback',{mode:'completion'});await add('paginated original turn',{mode:'completion',paged:true});
await add('saved completion bytes physically reopen',{mode:'completion',attempts:2,reopen:true,paged:true});
await add('pending original turn',{mode:'completion',turnState:'inProgress'});
for(const turnState of ['failed','interrupted','foreign'])await add('turn state '+turnState,{mode:'completion',turnState});
await add('turn changed',{mode:'completion',turnId:'foreign'});await add('verification cannot reuse observation',{mode:'completion',sameObservation:true});await add('verification cannot predate full read',{mode:'completion',verificationBefore:true});
await add('pagination cannot cycle',{mode:'completion',paged:true,pageCycle:true});await add('first page arbitrary error is not fallback',{mode:'completion',itemReadError:-32000});
for(const type of ['commandExecution','webSearch','mcpToolCall'])await add('original evidence admission '+type,{mode:'completion',items:[{id:'action',type}]});
await add('two final answers',{mode:'completion',items:[{id:'a',type:'agentMessage',phase:'final_answer',text:JSON.stringify(assessment)},{id:'b',type:'agentMessage',phase:'final_answer',text:JSON.stringify(assessment)}]});
await add('model cannot invent task authority',{mode:'completion',items:[{id:'a',type:'agentMessage',phase:'final_answer',text:JSON.stringify({...assessment,task:{objectId:itemId,revision:1},disposition:'task'})}]});
await add('invalid result JSON',{mode:'completion',items:[{id:'a',type:'agentMessage',phase:'final_answer',text:'not json'}]});
await add('assessment from retained completion',{mode:'step',completion:true});await add('assess once after physical reopen',{mode:'step',completion:true,attempts:2,reopen:true});await add('recover accepted assessment write receipt',{mode:'step',completion:true,writeLost:true,attempts:2,reopen:true});
await add('collect then assess original result',{mode:'step'});
await add('bounded admission defers pending old work',{mode:'tick',pendingSlot:'turn',attempts:2});
await add('concurrent ticks await one original run',{mode:'tick',pendingSlot:'turn',concurrent:true});
await add('terminal native failure waits for cleanup',{mode:'tick',pendingSlot:'turn',pendingPhase:'failed',calls:['thread','turn','cleanup']});
await add('terminal result failure retains cleanup failure',{mode:'tick',items:[{id:'a',type:'commandExecution'}],calls:['thread','turn','cleanup'],cleanupStatus:'foreign'});
await add('completed work keeps follow-up thread',{mode:'tick',completion:true,queryEmpty:true});
await add('new item with missing media not admitted',{mode:'prepare',noPlan:true,noWork:true,attachments:true});
await add('different current item revision',{mode:'prepare',noWork:true,itemRevision:2});
for(const phase of ['pending','assessed','failed','superseded'])await add('work invariants '+phase,{mode:'work',workChanges:{phase}});
await add('original immutable plan revision',{mode:'plan',planRevision:2});
await add('invalid original parameter bytes',{mode:'plan',corruptBlob:{id:'parameters',text:'{'}});
await add('UTF8 parameter evidence',{mode:'plan',corruptBlob:{id:'parameters',text:'/w==',encoding:'base64'}});
await add('absolute triage cwd required',{mode:'constructor',configChanges:[['threadCwd','relative']]});
for(const code of ['never','on-request'])await add('execution policy '+code,{mode:'begin',retained:false,absentSlot:'thread',paramsChanges:[['threadStart.approvalPolicy',code]]});
for(const changes of [[['sources',[]]],[['sources.0.ownSenderIds',['sender']]]]){const settings=clone(base);for(const[p,v]of changes)set(settings,p,v);await add('current source gate '+rows.length,{retained:false,absentSlot:'thread',settings});}
for(const label of ['assessment from retained completion','original full turn fallback','paginated original turn']){const r=rows.find(r=>r.input.label===label);assert(label==='assessment from retained completion'?r.expected.results[0].value==='assessed':r.expected.results[0].value?.value.result,JSON.stringify({label,result:r.expected.results,trace:r.expected.trace.slice(-2)}));}
assert.equal(rows.find(r=>r.input.label==='fresh original plan and work').expected.results[0].value?.value.phase,'pending');
assert.equal(rows.find(r=>r.input.label==='new original calls retain complete intent').expected.results[0].value?.threadId,'original-thread');
process.stdout.write(JSON.stringify({rows,rootId,nodeId:'fixture.secretary',time,wall,notificationPolicy:secretaryNotificationPolicy}));
