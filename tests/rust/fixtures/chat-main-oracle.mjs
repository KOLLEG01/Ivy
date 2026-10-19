import {now,instructions,seeds,error,result} from './chat-native-evidence-oracle.mjs';
import {mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {join,basename} from 'node:path';
import {tmpdir} from 'node:os';
import {pathToFileURL} from 'node:url';
import {DatabaseSync} from 'node:sqlite';
import {buildInstructions} from '../../../tools/build/build-instructions.mjs';
const {ChatStore,mutation}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatOperations}=await import('../../../services/chat-bridge/src/operations.ts');
const {ChatNativeDriver}=await import('../../../services/chat-bridge/src/native-driver.ts');
const {ChatMain}=await import('../../../services/chat-bridge/src/chat-main.ts');
const {ChatNotices}=await import('../../../services/chat-bridge/src/notices.ts');
const {ChatCancellation}=await import('../../../services/chat-bridge/src/cancellation.ts');
const {mainName,conversationName}=await import('../../../services/chat-bridge/src/queue.ts');
const {checkedNativeContract}=await import('../../../packages/contracts/src/checked-native-contract.ts');
const {canonical,hashJson,digest}=await import('../../../packages/contracts/src/canonical.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {parseOperationId}=await import('../../../packages/contracts/src/operation-id.ts');
const {deriveOperationId}=await import('../../../packages/contracts/src/operation-id.ts');
globalThis[Symbol.for('ivy.test.chat-native-evidence-gate')]=(_kind,_value,run)=>run();
const clone=v=>structuredClone(v),at=new Date(now).toISOString(),cases=[],starting={};
const assetRoot=mkdtempSync(join(tmpdir(),'ivy-main-assets-'));let noticeInstructions;
try{const files=buildInstructions({root:process.cwd(),outputRoot:assetRoot,component:'chat-bridge'});noticeInstructions=readFileSync(files.find(p=>basename(p)==='chat-notice-prompt.json')).toString('base64');}finally{rmSync(assetRoot,{recursive:true,force:true});}
for(const[version,seed]of Object.entries(seeds)){const definition=clone(seed.definition);definition.nativePlan.threadStart.params.cwd='/synthetic/project';starting[version]={database:seed.database,definition,records:[],journal:[],controls:{},starts:0};}
const operation=label=>`chat:${now}:${label}`;
function aliasesFor(file){const db=new DatabaseSync(file),rows=db.prepare('SELECT id,key,name FROM documents').all(),blobs=db.prepare('SELECT id,name FROM blobs').all();db.close();return new Map([...rows.map(r=>[r.id,'<local:'+r.name+'>']),...blobs.map(r=>[r.id,'<blob:'+r.name+'>'])]);}
function normalizer(aliases){const string=v=>{for(let n=0;n<8;n++){let next=v;for(const[id,alias]of aliases)next=next.replaceAll(id,alias);if(next===v)break;v=next;}return v;};return function normalize(v){return typeof v==='string'?string(v):Array.isArray(v)?v.map(normalize):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,v])=>[k,normalize(v)])):v;};}
function matches(record,predicate){
 if(predicate.op==='and')return predicate.args.every(p=>matches(record,p));
 if(predicate.op==='or')return predicate.args.some(p=>matches(record,p));
 let value;if(predicate.field.startsWith('object.'))value=record.object[predicate.field.slice(7)];
 else if(predicate.field.startsWith('data:/')){value=record.content.value;for(const part of predicate.field.slice(6).split('/'))value=value?.[part.replaceAll('~1','/').replaceAll('~0','~')];}
 return predicate.op==='isNull'?value==null:JSON.stringify(value)===JSON.stringify(predicate.value);
}
export function environment(seed,approvedAutoSend=false,extension=null){
 const root=mkdtempSync(join(tmpdir(),'ivy-main-original-')),file=join(root,'chat-bridge.sqlite');writeFileSync(file,Buffer.from(seed.database,'base64'));
 let definition=clone(seed.definition),controls={epoch:'original-epoch',phase:'succeeded',...clone(seed.controls)},starts=seed.starts??0,store,main,ops,crash=null,closed=false;
 const records=clone(seed.records),journal=new Map(clone(seed.journal)),events=[],mutations=new Map(),receipts=new Map(),aliases=new Map(),selections=[];
 const source=checkedNativeContract(definition.nativePlan.nativeVersion),node={principalId:'chat-service',serviceName:'chat-bridge',connected:true,synced:true};
 const status=()=>({serviceNodeId:'owner',hostId:'synthetic-native',nativeVersion:source.catalog.version,nativeExecutableHash:source.catalog.nativeExecutableHash,catalogHash:source.catalogHash,epoch:controls.epoch,pid:42,state:'ready',observedAt:at,code:null,initialized:{},pendingInputs:0,operations:{retained:journal.size,maximum:100,bytes:0,maximumBytes:33554432},observedMethods:[]});
 const baseRpc=async(method,p)=>{
  if(method==='system.status')return{runtimeEpoch:'chat'};
  if(method==='serviceNodes.get'){
   if(controls.ownerReadBudget!==undefined&&--controls.ownerReadBudget<0)throw new IvyError('fixture_recursion_budget','Bounded synthetic owner outage after original recovery recursion.','unknown');
   if(controls.bumpMainOnOwnerRead!==undefined&&--controls.bumpMainOnOwnerRead===0){
    const current=await main.find();await store.write('chat-bridge/main',current.value,'fixture-competing-main-revision',{objectId:current.pin.objectId,expectedRevision:current.pin.revision});
    events.at(-1).competingMainRevision=true;
   }
   return{...node,...controls.node};
  }
  if(method==='objects.query')return{items:records.filter(r=>r.object.contractKey===p.contractKey&&(p.includeArchived||!r.object.effectivelyArchived)&&matches(r,p.where)).map(r=>({objectId:r.object.id,revision:r.revision.revision})),nextCursor:null};
  if(method==='objects.read'||method==='objects.stat'){if(p.objectId==='synthetic-source')return{object:{id:'synthetic-source',effectivelyArchived:false},revision:{revision:1},content:{encoding:'json',value:{synthetic:true}}};const record=records.find(r=>r.object.id===p.objectId);if(!record)throw new IvyError('object_missing','Original public record is unavailable.');return clone(method==='objects.stat'?record.object:record);}
  if(method==='objects.write'){
   if(parseOperationId(p.mutationId).runtimeEpoch!=='chat')throw Error('Wrong mutation epoch');
   if(receipts.has(p.mutationId))return clone(receipts.get(p.mutationId));
   if(controls.publicBefore){controls.publicBefore=false;throw new IvyError('fixture_unavailable','Original public write not executed.');}
   let record;if(p.create){if(records.some(r=>r.object.contractKey===p.create.contractKey&&r.object.parentId===p.create.parentId&&r.object.name===p.create.name))throw new IvyError('revision_conflict','Original public name exists.');
    record={object:{id:'main-public-'+(records.length+1),parentId:p.create.parentId,ownerObjectId:p.create.ownerObjectId,name:p.create.name,contractKey:p.create.contractKey,effectivelyArchived:false},revision:{revision:1,contractVersion:p.contractVersion,mediaType:'application/json'},content:clone(p.content)};records.push(record);
   }else{record=records.find(r=>r.object.id===p.objectId);if(!record)throw new IvyError('object_missing','Original public record is unavailable.');if(record.revision.revision!==p.expectedRevision)throw new IvyError('revision_conflict','Original public revision changed.');record.revision.revision++;record.content=clone(p.content);}
   receipts.set(p.mutationId,clone(record));if(controls.publicAfter){controls.publicAfter=false;throw new IvyError('outcome_unknown','Original public write reply lost.','unknown');}return clone(record);
  }
  if(method==='tools.list'){const namespace=p.namespace,name=p.namePrefix,definition=namespace==='codex'?source.definitions.get(name):{namespace:'agent',name,interfaceVersion:'1.0.0',inputSchema:{},outputSchema:{}};
   return{provider:{node:{serviceNodeId:'owner'}},items:[{qualifiedName:namespace+'.'+name,definition,definitionHash:hashJson(definition),catalogHash:source.catalogHash}],nextCursor:null};}
  if(method==='tools.call'){
   const name=p.name??p.qualifiedName?.split('.').slice(1).join('.'),args=p.arguments;
   if(name==='status')return status();
   if(name==='projects')return{source:'native',observedAt:at,projects:[{nativeId:definition.project.nativeId,source:'native',name:'Synthetic project',paths:controls.paths??['/synthetic']}]};
   if(name==='operation'){
    if(controls.genericAbsence)throw new IvyError('not_found','Generic fixture absence.');
    const value=journal.get(args.operationId);if(value&&!controls.ownerAbsence)return clone(value);
    if(controls.changeProjectBeforeGuard)controls.paths=['/elsewhere'];
    throw new IvyError('not_found','Exact synthetic owner absence.','not_executed',{kind:'agent_operation_absent',operationId:args.operationId,serviceNodeId:'owner',epoch:controls.epoch});
   }
   if(name==='invoke'){
    if(!journal.has(args.operationId)){
     const threadId=args.method==='thread/resume'?args.params.threadId:'native-main-'+(++starts),cwd=controls.replyCwd??'/synthetic/project';
     const thread={cliVersion:source.catalog.version,createdAt:1788690000,cwd,ephemeral:controls.ephemeral??false,id:controls.replyThreadId??threadId,modelProvider:'openai',preview:'Synthetic Main',projectId:null,sessionId:threadId,source:'appServer',status:{type:'idle'},turns:[],updatedAt:1788690000,canAcceptDirectInput:true,name:'Synthetic'};
     let value={schemaVersion:1,operationId:args.operationId,callerPrincipalId:'chat-service',serviceNodeId:'owner',nativeVersion:source.catalog.version,nativeExecutableHash:source.catalog.nativeExecutableHash,method:args.method,params:clone(args.params),requestHash:hashJson({method:args.method,params:args.params}),phase:controls.phase,createdAt:at,updatedAt:at,epoch:controls.epoch,requestId:'original-request-'+(journal.size+1),code:null,
      reply:{result:{thread,cwd,model:'fixture-model',modelProvider:'openai',approvalPolicy:'on-request',approvalsReviewer:'user',sandbox:{type:'readOnly'},reasoningEffort:'high'}}};
     if(controls.phase==='dispatched')Object.assign(value,{reply:null});
     if(controls.phase==='failed')Object.assign(value,{code:'native_error',reply:{error:{code:-1,message:'Original native refusal.'}}});
     journal.set(args.operationId,value);
    }
    if(controls.loseReply){controls.loseReply=false;throw new IvyError('outcome_unknown','Original successful reply lost.','unknown');}
    return clone(journal.get(args.operationId));
   }
  }
  throw Error('Unexpected synthetic Main RPC '+method+' '+JSON.stringify(p));
 };
 function context(){return{root,file,definition,controls,store,main,ops,records,journal,source,events};}
 const rpc=(method,p)=>extension?.rpc?extension.rpc(method,p,context(),()=>baseRpc(method,p)):baseRpc(method,p);
 const client={async request(method,params){const event={method,params:clone(params)};events.push(event);
  const handled=await extension?.beforeRequest?.(method,params,event,context());
  if(!handled&&method==='tools.call'&&params.qualifiedName==='agent.invoke'){
   const selected=await store.named('chat-bridge/main',mainName(definition));if(!selected?.value.pendingAction)throw Error('No retained Main claim before invoke');
   const op=await store.read('chat-bridge/operation',selected.value.pendingAction.objectId),call=await store.read('chat-bridge/native-call',op.value.nativeCall,op.pin.objectId);
   if(op.value.phase!=='applying'||call.value.operationId!==params.arguments.operationId||selected.value.queue.length)throw Error('Native invoke preceded its durable attachment');event.assertClaim=true;
  }
  try{const value=await rpc(method,params);event.result={value:clone(value)};return value;}catch(e){const err=IvyError.from(e);event.result={error:{...error(err),...(err.details===undefined?{}:{details:err.details})}};throw e;}finally{if(method==='objects.write')event.records=clone(records);}}};
 function open(){store=new ChatStore(client,null,root);closed=false;const write=store.write.bind(store);store.write=async(...args)=>{const saved=await write(...args);if(crash&&crash(args)){crash=null;throw new IvyError('fixture_crash','Original checkpoint committed.','unknown');}return saved;};
  const admission=new ChatAdmission(definition);ops=new ChatOperations(store,admission);main=new ChatMain(ops,new ChatNativeDriver(store,admission,{serviceNodeId:'chat-provider',generation:7}));
  if(approvedAutoSend){
   // Explicit approved difference, not an unmodified-original oracle. Only this
   // test adapter selects the held binding; all subsequent admission, input,
   // queue/CAS and receipt writes execute the unchanged original implementation.
   const originalEnsure=main.ensure.bind(main);
   main.ensure=async(caller,parentOperationId,role='reader')=>{
    const prior=await ops.find(caller,deriveOperationId(parentOperationId,{action:'chat:auto-main',caller}));
    if(!prior){const current=await main.find();if(current?.value.binding&&current.value.pendingAction){
     const accepted=await store.read('chat-bridge/operation',current.value.pendingAction);ops.check(accepted.value);const saved=accepted.value;
     if(saved.phase==='accepted'&&saved.nativeCall===null&&saved.callerPrincipalId===caller&&saved.operationId===parentOperationId&&saved.request.action==='send'&&saved.request.expectedBinding===null){
      await main.native.verifyOwner();const binding=await store.read('chat-bridge/binding',current.value.binding);
      selections.push({caller,parentOperationId});return{object:binding.pin,data:binding.value};
     }
    }}
    return originalEnsure(caller,parentOperationId,role);
   };
  }
  extension?.opened?.(context());
 }
 open();
 const createRequest=(label='create',caller='alice',expectedMainRevision=null)=>({action:'createMain',operationId:operation(label),expectedBridge:main.admission.expected(caller),expectedMainRevision,reason:'Explicit synthetic Main creation.'});
 async function snapshot(output){
  const db=new DatabaseSync(file),documents=db.prepare('SELECT id,key,name,parent_id,current_revision FROM documents').all(),revisions=db.prepare('SELECT d.name,d.key,r.id,r.revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id').all(),blobs=db.prepare('SELECT id,key,name,parent_id,bytes FROM blobs').all();db.close();
  for(const[id,alias]of aliasesFor(file))aliases.set(id,alias);
  for(const event of events)if(event.params.mutationId){const id=event.params.mutationId;if(!mutations.has(id))mutations.set(id,'<mutation:'+mutations.size+'>');aliases.set(id,mutations.get(id));}
  const normalize=normalizer(aliases),sort=rows=>normalize(rows).sort((a,b)=>Buffer.compare(Buffer.from(canonical(a)),Buffer.from(canonical(b))));
  const value={output:normalize(output),documents:sort(documents.map(d=>({id:d.id,key:d.key,name:d.name,parentId:d.parent_id,revision:d.current_revision}))),revisions:sort(revisions.map(r=>({id:r.id,key:r.key,name:r.name,revision:r.revision,value:JSON.parse(r.value_json)}))),blobs:sort(blobs.map(b=>({id:b.id,key:b.key,name:b.name,parentId:b.parent_id,byteLength:b.bytes.length,contentHash:digest(b.bytes)}))),records:normalize(records),transport:normalize(events)};events.length=0;return value;
 }
 async function run(s){
  const caller=s.caller??'alice';let request;
  switch(s.action){
   case'create':request=createRequest(s.label??'create',caller,s.current?(await main.find())?.pin.revision??null:s.revision??null);if(s.resume)request.resumeMain=clone(s.resume);
    if(s.patch)Object.assign(request,clone(s.patch));return main.create(caller,request);
   case'ensure':return main.ensure(caller,operation(s.label??'incoming'));
   case'find':{const found=await main.find();return found?{pin:found.pin,value:found.value}:null;}
   case'recover':return main.recoverPending();
   case'reopen':store.close();open();return;
   case'state':Object.assign(controls,clone(s.value));return;
   case'phase':for(const value of journal.values()){value.phase=s.phase;value.updatedAt=new Date(now+(s.tick??1)).toISOString();if(s.phase==='outcome_unknown')Object.assign(value,{code:'native_epoch_lost',reply:null});}return;
   case'reconfigure':definition.channels[0].displayName='Changed configuration';if(s.value)Object.assign(definition,clone(s.value));openWithoutClosing();return;
   case'resume':{const db=new DatabaseSync(file),rows=db.prepare("SELECT d.id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/main'").all();db.close();const source=rows.find(r=>JSON.parse(r.value_json).definitionHash!==main.admission.definitionHash);request=createRequest(s.label??'resume',caller);request.resumeMain={objectId:source.id,revision:s.stale?source.current_revision-1:source.current_revision};return main.create(caller,request);}
   case'send':{const binding=(await main.find())?.value.binding;return main.queue.send(caller,{action:'send',operationId:operation(s.label??'main-send'),expectedBridge:main.admission.expected(caller),channel:definition.channels[0].channel,messageId:s.label??'main-send',expectedBinding:s.auto?null:binding,payload:{text:'Original queued input.',images:[]}});}
   case'notify':return new ChatNotices(main).notify(caller,{action:'notify',operationId:operation(s.label??'main-notice'),source:{objectId:'synthetic-source',revision:1},text:'Original private context.'});
   case'cancel':{const input=records.find(r=>r.object.contractKey==='chat-bridge/input'),request=s.replay?clone((await ops.find(caller,operation('main-cancel'))).value.request):{action:'cancel',operationId:operation('main-cancel'),expectedBridge:main.admission.expected(caller),inputId:input.object.id,expectedRevision:input.revision.revision,reason:'Explicit synthetic cancellation.'};return new ChatCancellation(main).cancel(caller,request);}
   case'patch-main':{const selected=await main.find();await store.write('chat-bridge/main',{...selected.value,...clone(s.value)},'fixture-main-patch',{objectId:selected.pin.objectId,expectedRevision:selected.pin.revision});return;}
   case'claim-send':{
    const selected=await main.find(),label=s.label??'main-send',request={action:'send',operationId:operation(label),expectedBridge:main.admission.expected(caller),channel:definition.channels[0].channel,messageId:label,expectedBinding:s.auto?null:selected.value.binding,payload:{text:'Original queued input.',images:[]}};
    const accepted=await ops.begin(caller,request);await store.write('chat-bridge/main',{...selected.value,pendingAction:accepted.pin,updatedAt:accepted.value.createdAt},'fixture-claim-send',{objectId:selected.pin.objectId,expectedRevision:selected.pin.revision});return;
   }
   case'patch-pending':{
    const selected=await main.find(),saved=await store.read('chat-bridge/operation',selected.value.pendingAction),value=clone(saved.value);
    if(s.value)Object.assign(value,clone(s.value));if(s.requestPatch)Object.assign(value.request,clone(s.requestPatch));
    if(s.useBindingAsNativeCall)value.nativeCall=selected.value.binding;if(s.rehash)value.requestHash=hashJson(value.request);
    const pin=await store.write('chat-bridge/operation',value,'fixture-pending-patch',{objectId:saved.pin.objectId,expectedRevision:saved.pin.revision});
    await store.write('chat-bridge/main',{...selected.value,pendingAction:pin},'fixture-pending-pin',{objectId:selected.pin.objectId,expectedRevision:selected.pin.revision});return;
   }
   case'patch-binding':{
    const selected=await main.find(),saved=await store.read('chat-bridge/binding',selected.value.binding),value=clone(saved.value);
    if(s.field==='project')value.project.nativeId='foreign-native-project';
    else if(s.field==='nativePlan')value.nativePlan.threadStart.params.cwd='/another-project';
    else if(s.field==='primary')value.primary.serviceNodeId='foreign-native-owner';
    else throw Error('Unexpected controlled binding rewrite');
    const pin=await store.write('chat-bridge/binding',value,'fixture-binding-patch',{objectId:saved.pin.objectId,expectedRevision:saved.pin.revision});
    await store.write('chat-bridge/main',{...selected.value,binding:pin},'fixture-binding-pin',{objectId:selected.pin.objectId,expectedRevision:selected.pin.revision});return;
   }
   case'constructor-store':{const other=new ChatStore(client,null);try{return new ChatMain(new ChatOperations(other,main.admission),main.native);}finally{other.close();}}
   case'constructor-definition':{const changed=new ChatAdmission({...definition,workspaceId:'foreign'});return new ChatMain(new ChatOperations(store,changed),main.native);}
   default:if(extension?.run)return extension.run(s,context());throw Error('Unknown Main step '+s.action);
  }
 }
 function openWithoutClosing(){store.close();open();}
 return{root,file,run,snapshot,createRequest,selections,get main(){return main;},get store(){return store;},get ops(){return ops;},get definition(){return definition;},setCrash(fn){crash=fn;},exportSeed(){store.close();closed=true;return{database:readFileSync(file).toString('base64'),definition:clone(definition),records:clone(records),journal:clone([...journal]),controls:clone(controls),starts};},close(){if(!closed)store.close();rmSync(root,{recursive:true,force:true});}};
}
export {now,instructions,noticeInstructions,starting,operation,clone,at,error,result};

// Re-open exact original states rather than re-enacting a crash with different
// writes. These copies retain the original IDs and protected aggregate revisions.
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href){
for(const version of Object.keys(starting)){
 const e=environment(starting[version]);try{const created=await e.run({action:'create'});if(created.phase!=='succeeded')throw Error('Original seed Main did not complete');starting['completed-'+version]=e.exportSeed();}finally{e.close();}
}
for(const checkpoint of ['accept','claim','native-prepare','native-attach','native-observe','binding','publish','finish','release']){
 const e=environment(starting['0.154.0']);e.setCrash(args=>{const[key,value,,destination]=args;return checkpoint==='accept'?key==='chat-bridge/operation'&&value.phase==='accepted':checkpoint==='claim'?key==='chat-bridge/main'&&value.pendingAction&&value.binding===null:checkpoint==='native-prepare'?key==='chat-bridge/native-call'&&value.state==='prepared':checkpoint==='native-attach'?key==='chat-bridge/operation'&&value.phase==='applying':checkpoint==='native-observe'?key==='chat-bridge/native-call'&&value.state==='succeeded':checkpoint==='binding'?key==='chat-bridge/binding':checkpoint==='publish'?key==='chat-bridge/main'&&value.binding&&value.pendingAction:checkpoint==='finish'?key==='chat-bridge/operation'&&value.phase==='succeeded':key==='chat-bridge/main'&&value.binding&&!value.pendingAction;});
 try{const outcome=await result(()=>e.run({action:'create'}));if(outcome.error?.code!=='fixture_crash')throw Error('Original checkpoint not reached '+checkpoint+JSON.stringify(outcome));starting['checkpoint-'+checkpoint]=e.exportSeed();}finally{e.close();}
}
for(const action of ['send','notify','cancel']){
 const e=environment(starting['completed-0.154.0']);try{
  if(action==='cancel')await e.run({action:'send'});
  e.setCrash(([key,value])=>key==='chat-bridge/main'&&value.pendingAction);
  const outcome=await result(()=>e.run({action}));if(outcome.error?.code!=='fixture_crash')throw Error('Original pending action checkpoint not reached '+action+JSON.stringify(outcome));
  starting['pending-'+action]=e.exportSeed();
 }finally{e.close();}
}
{
 const e=environment(starting['completed-0.154.0']);try{
  e.setCrash(([key,value])=>key==='chat-bridge/main'&&value.pendingAction);
  const outcome=await result(()=>e.run({action:'send',auto:true}));if(outcome.error?.code!=='fixture_crash')throw Error('Original automatic send claim not reached');
  starting['pending-auto-send']=e.exportSeed();
 }finally{e.close();}
}
const add=(label,steps,seed='0.154.0')=>cases.push({label,steps,seed});
for(const version of Object.keys(seeds)){
 add('create-replay-'+version,[{action:'find'},{action:'create'},{action:'find'},{action:'reopen'},{action:'create'},{action:'ensure'},{action:'find'}],version);
 add('original-main-replacement-'+version,[{action:'create',label:'replacement',caller:'bob',current:true},{action:'find'},{action:'create',label:'create'}],'completed-'+version);
}
for(const checkpoint of ['accept','claim','native-prepare','native-attach','native-observe','binding','publish','finish','release'])add('cross-open-'+checkpoint,[{action:'reopen'},{action:'recover'},{action:'create'},{action:'find'}],'checkpoint-'+checkpoint);
for(const action of ['send','notify','cancel'])add('cross-open-pending-'+action,[{action:'reopen'},{action:'recover'},{action:'find'}],'pending-'+action);
add('automatic-first-main',[{action:'ensure'},{action:'ensure'},{action:'reopen'},{action:'ensure',caller:'bob',label:'other'},{action:'find'}]);
add('stale-main-before-admission',[{action:'create',label:'stale'},{action:'find'}],'completed-0.154.0');
add('same-identity-different-reason',[{action:'create',patch:{reason:'Changed original.'}},{action:'find'}],'completed-0.154.0');
add('project-before-admission',[{action:'state',value:{paths:['/elsewhere']}},{action:'create'},{action:'find'}]);
add('project-rechecked-after-attachment',[{action:'state',value:{changeProjectBeforeGuard:true}},{action:'create'},{action:'find'},{action:'state',value:{paths:['/synthetic'],changeProjectBeforeGuard:false}},{action:'recover'},{action:'find'}]);
add('owner-before-historical-replay',[{action:'state',value:{node:{connected:false}}},{action:'create'}],'completed-0.154.0');
add('lost-invoke-reply-keeps-original',[{action:'state',value:{loseReply:true}},{action:'create'},{action:'reopen'},{action:'create'},{action:'find'}]);
add('held-original-no-replacement',[{action:'state',value:{phase:'dispatched'}},{action:'create'},{action:'create'},{action:'create',label:'replacement',current:true},{action:'phase',phase:'outcome_unknown'},{action:'create'},{action:'find'}]);
add('observed-owner-absence-no-replay',[{action:'state',value:{phase:'dispatched'}},{action:'create'},{action:'state',value:{ownerAbsence:true}},{action:'recover'},{action:'find'}]);
add('generic-absence-keeps-attachment',[{action:'state',value:{genericAbsence:true}},{action:'create'},{action:'find'},{action:'state',value:{genericAbsence:false}},{action:'create'}]);
add('original-failure-releases-claim',[{action:'state',value:{phase:'failed'}},{action:'create'},{action:'find'},{action:'create'},{action:'create',label:'second',current:true},{action:'find'}]);
add('native-reply-outside-project',[{action:'state',value:{replyCwd:'/elsewhere'}},{action:'create'},{action:'find'}]);
add('native-reply-ephemeral',[{action:'state',value:{ephemeral:true}},{action:'create'},{action:'find'}]);
add('public-write-before-commit',[{action:'state',value:{publicBefore:true}},{action:'create'},{action:'reopen'},{action:'create'},{action:'find'}]);
add('public-write-lost-reply',[{action:'state',value:{publicAfter:true}},{action:'create'},{action:'reopen'},{action:'create'},{action:'find'}]);
add('resumed-native-primary',[{action:'reconfigure'},{action:'resume'},{action:'find'},{action:'reopen'},{action:'resume'},{action:'find'}],'completed-0.154.0');
add('resumed-native-lost-reply',[{action:'reconfigure'},{action:'state',value:{loseReply:true}},{action:'resume'},{action:'find'}],'completed-0.154.0');
add('stale-resumption-refused',[{action:'reconfigure'},{action:'resume',stale:true},{action:'find'}],'completed-0.154.0');
add('foreign-workspace-resumption',[{action:'reconfigure',value:{workspaceId:'foreign'}},{action:'resume'},{action:'find'}],'completed-0.154.0');
add('busy-queue-before-replacement',[{action:'send'},{action:'create',label:'busy',current:true},{action:'find'}],'completed-0.154.0');
add('preserved-auto-send-recovery-recursion',[{action:'state',value:{ownerReadBudget:12}},{action:'send',auto:true},{action:'find'}],'completed-0.154.0');
cases.at(-1).referenceOnly=true;
add('real-main-notice-selection',[{action:'notify',caller:'producer'},{action:'find'}],'completed-0.154.0');
add('constructor-store-scope',[{action:'constructor-store'}]);
add('constructor-definition-scope',[{action:'constructor-definition'}]);

const approved=(label,steps,seed='pending-auto-send',selection=true)=>{add(label,steps,seed);Object.assign(cases.at(-1),{approvedAutoSend:true,selection,oneInput:selection});};
for(const version of Object.keys(seeds))approved('approved-auto-send-replay-'+version,[{action:'send',auto:true},{action:'send',auto:true},{action:'reopen'},{action:'send',auto:true},{action:'find'}],'completed-'+version);
approved('approved-auto-send-cross-open',[{action:'reopen'},{action:'recover'},{action:'find'},{action:'send',auto:true},{action:'find'}]);
approved('approved-auto-send-owner-refused',[{action:'state',value:{node:{connected:false}}},{action:'ensure',label:'main-send'},{action:'find'}],'pending-auto-send',false);
for(const [label,action]of [['wrong-caller',{action:'ensure',caller:'bob',label:'main-send'}],['wrong-id',{action:'ensure',label:'other'}]])approved('approved-auto-send-'+label,[{action:'state',value:{ownerReadBudget:1}},action,{action:'find'}],'pending-auto-send',false);
for(const phase of ['applying','outcome_unknown'])approved('approved-auto-send-nonaccepted-'+phase,[{action:'patch-pending',value:{phase}},{action:'state',value:{ownerReadBudget:1}},{action:'ensure',label:'main-send'},{action:'find'}],'pending-auto-send',false);
approved('approved-auto-send-attached-refused',[{action:'patch-pending',useBindingAsNativeCall:true},{action:'state',value:{ownerReadBudget:1}},{action:'ensure',label:'main-send'},{action:'find'}],'pending-auto-send',false);
approved('approved-auto-send-corrupt-request',[{action:'patch-pending',requestPatch:{payload:{text:'Corrupt retained content.',images:[]}}},{action:'ensure',label:'main-send'},{action:'find'}],'pending-auto-send',false);
approved('approved-auto-send-mismatched-request-id',[{action:'patch-pending',requestPatch:{operationId:operation('foreign')}},{action:'ensure',label:'main-send'},{action:'find'}],'pending-auto-send',false);
approved('approved-auto-send-explicit-binding',[{action:'ensure',label:'main-send'},{action:'find'}],'pending-send',false);
approved('approved-auto-send-receipt-before-newer-binding',[{action:'ensure',label:'main-send'},{action:'create',label:'replacement',caller:'bob',current:true},{action:'claim-send',auto:true},{action:'ensure',label:'main-send'},{action:'recover'},{action:'find'}],'0.154.0',false);
approved('approved-auto-send-cas-retry',[{action:'state',value:{bumpMainOnOwnerRead:2}},{action:'recover'},{action:'find'},{action:'send',auto:true},{action:'find'}]);
approved('approved-auto-send-cancellation',[{action:'send',auto:true},{action:'cancel'},{action:'cancel',replay:true},{action:'reopen'},{action:'recover'},{action:'find'}],'completed-0.154.0');
for(const action of ['notify','cancel'])approved('approved-auto-send-unrelated-'+action,[{action:'ensure',label:'main-send'},{action:'find'}],'pending-'+action,false);
for(const field of ['project','nativePlan','primary']){
 approved('approved-auto-send-binding-'+field,[{action:'patch-binding',field},{action:'recover'},{action:'find'}]);
 Object.assign(cases.at(-1),{oneInput:false,refusal:'chat_binding_mismatch'});
}
approved('approved-auto-send-sequence-limit',[{action:'patch-main',value:{nextSequence:Number.MAX_SAFE_INTEGER}},{action:'recover'},{action:'find'}]);
Object.assign(cases.at(-1),{oneInput:false,refusal:'chat_queue_full'});

for(const c of cases){const e=environment(starting[c.seed],c.approvedAutoSend);c.expected=[];try{for(const s of c.steps)c.expected.push(await e.snapshot(await result(async()=>await e.run(s)??null)));c.selections=clone(e.selections);}finally{e.close();}
 if(c.approvedAutoSend&&Boolean(c.selections.length)!==c.selection)throw Error('Wrong approved selection boundary '+c.label+JSON.stringify(c));
 if(c.oneInput){const final=c.expected.at(-1),inputs=final.records.filter(r=>r.object.contractKey==='chat-bridge/input'),bindings=final.documents.filter(d=>d.key==='chat-bridge/binding');if(inputs.length!==1||bindings.length!==1||c.expected.some(s=>s.output.error))throw Error('Approved automatic send duplicated or failed '+c.label);}
 if(c.refusal){const final=c.expected.at(-1);if(final.records.some(r=>r.object.contractKey==='chat-bridge/input')||!c.expected.some(s=>s.output.error?.code===c.refusal||s.revisions.some(r=>r.key==='chat-bridge/operation'&&r.value.phase==='failed'&&r.value.error?.code===c.refusal)))throw Error('Later admission refusal bypassed '+c.label);}
}
for(const c of cases.filter(c=>/^(create-replay-|original-main-replacement-|cross-open-|automatic-first|lost-invoke-reply|resumed-native-|real-main-)/.test(c.label)))if(c.expected.some(s=>s.output.error))throw Error('Successful original Main path failed '+c.label+JSON.stringify(c.expected.map(s=>s.output)));
process.stdout.write(JSON.stringify({now,instructions,noticeInstructions,seeds:starting,cases,approvedDifference:{proposal:'6af88af7-cd82-49c2-b43f-1f87e36cf1ce@1',decision:'task0072-chat-main-root-answer-197-v1',referenceOnly:'preserved-auto-send-recovery-recursion',adapter:'Only approvedAutoSend cases wrap the original Main.ensure; unchanged original code performs all later Queue, cancellation, SQLite CAS and receipt transitions.'}}));
}
