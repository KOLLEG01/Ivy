import{existsSync,readFileSync,readdirSync}from'node:fs';
import{registerHooks,stripTypeScriptTypes}from'node:module';import{fileURLToPath}from'node:url';import assert from'node:assert/strict';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const{IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const{hashJson,canonical}=await import('../../packages/contracts/src/canonical.ts');
const{validateAgent}=await import('../../packages/contracts/src/agent-validation.ts');
const{checkedNativeContract}=await import('../../packages/contracts/src/checked-native-contract.ts');
const{saveNativePlan,readNativePlan}=await import('../../packages/sdk/src/native-plan.ts');
const clean=n=>readFileSync('services/task-board/src/runtime/'+n+'.ts','utf8').replace(/^import .*;\r?\n/gm,'').replaceAll('export ','');
const readPlan=new Function('checkedNativeContract','readNativePlan',stripTypeScriptTypes(clean('native-plan'))+'\nreturn readPlan;')(checkedNativeContract,readNativePlan);
const recovery=clean('native-recovery');
const missingNativeThread=new Function(stripTypeScriptTypes(recovery.slice(recovery.indexOf('const missingNativeThread'),recovery.indexOf('async function contextRecoveryHistory')))+'\nreturn missingNativeThread;')();
const{NativeOwner}=await import('../../packages/sdk/src/native-owner.ts');
const{reconcileNativeOperation}=await import('../../packages/sdk/src/native-operation.ts');
const{serviceTools}=await import('../../packages/sdk/src/client.ts');
const nativeRecord=v=>v!==null&&typeof v==='object'&&!Array.isArray(v)?v:{},nativeString=v=>typeof v==='string'?v:'';
const same=(a,b)=>a===undefined||b===undefined?a===b:canonical(a)===canonical(b);
const source=readFileSync('services/task-board/src/runtime/native-lifecycle.ts','utf8').replace(/^import .*;\r?\n/gm,'').replaceAll('export ','');
const id=n=>'00000000-0000-4000-8000-'+String(n).padStart(12,'0'),pin=(n,r=1)=>({objectId:id(n),revision:r}),time='2026-01-02T03:04:05.006Z',zero='sha256:'+'0'.repeat(64);
const primary={namespace:'codex',kind:'thread',serviceNodeId:'native-agent',nativeId:'thread'};
const baseTask={taskKey:'TASK-0123',fields:{control:'agent',nextReviewAt:null},workflowState:'waiting',primaryResourceRef:primary,claim:null,publication:null,waiting:{reason:'user',detail:'retained',since:time},blocker:null,lastRun:pin(200),lastHistory:null,acceptedReview:{retained:true},commentDeliveries:[{commentId:'queued',state:'queued'}],comments:[],updatedAt:time};
const baseMeta={id:id(100),parentId:'root',currentRevision:2,archivedAt:null,effectivelyArchived:false};
const rows=[],add=(name,input={})=>rows.push({name,input});
add('archive original durable thread',{action:'archive',archived:true});add('unarchive original durable thread',{action:'archive',archived:false,path:'/x/archived_sessions/a'});
add('explicit new native context',{action:'newThread'});add('recover saved unstarted thread',{action:'recoverThread',proof:true});add('missing unstarted thread queues fresh Run',{action:'recoverThread',proof:true,phase:'failed',message:'thread thread not found'});
add('restore before continuation',{entry:'restore',path:'/x/archived_sessions/a'});add('archive missing context is idempotent',{action:'archive',archived:true,readError:'unknown thread: thread'});
add('archive already archived',{action:'archive',archived:true,path:'/x/archived_sessions/a'});add('unarchive already live',{action:'archive',archived:false,path:'/x/sessions/a'});
const standardCount=rows.length;
add('restore live active context does not archive',{entry:'restore',path:'/x/sessions/a',threadStatus:'active'});
add('restore missing original has no replacement',{entry:'restore',readError:'unknown thread: thread'});add('unarchive missing original creates replacement',{action:'archive',archived:false,readError:'unknown thread: thread'});
add('unarchive failed original creates replacement',{action:'archive',archived:false,path:'/x/archived_sessions/a',phase:'failed'});
add('restore failed original refuses replacement',{entry:'restore',path:'/x/archived_sessions/a',phase:'failed'});
add('original active context blocks archive',{action:'archive',archived:true,threadStatus:'active'});
add('only archived check precedes active check',{entry:'restore',threadStatus:'active'});
add('thread path is segment and case sensitive',{action:'archive',archived:true,path:'/x/ARCHIVED_sessions/a'});
add('Windows archive segment',{action:'archive',archived:true,path:'C:\\x\\archived_sessions\\a'});
add('no primary still archives membership',{action:'archive',archived:true,task:{primaryResourceRef:null}});
add('contexts include complete runs and historical primaries',{action:'archive',archived:true,runs:[{phase:'completed',primaryResourceRef:primary},{phase:'failed',primaryResourceRef:{...primary,nativeId:'second'}},{phase:'cancelled',primaryResourceRef:null}],histories:[null,pin(100,1)],oldPrimary:{...primary,nativeId:'historical'}});
add('context page cursor is retained',{action:'archive',archived:true,pageSize:1,runs:[{phase:'completed',primaryResourceRef:primary},{phase:'completed',primaryResourceRef:{...primary,nativeId:'second'}}]});
add('history revision pruned is permitted',{action:'archive',archived:true,histories:[pin(100,1)],oldReadError:'revision_pruned'});
add('unrelated history is rejected',{action:'archive',archived:true,histories:[pin(999)]});add('other history error remains original',{action:'archive',archived:true,histories:[pin(100,1)],oldReadError:'service_unavailable'});
add('unfinished previous run prevents context changes',{action:'newThread',runs:[{phase:'running',primaryResourceRef:primary}]});
add('invalid current primary prevents context changes',{action:'archive',archived:true,task:{primaryResourceRef:{...primary,namespace:'other'}}});
add('new work requires scheduler',{action:'newThread',enabled:false});add('new thread must be distinct',{action:'newThread',resultId:'thread'});add('new thread must be durable',{action:'newThread',ephemeral:true});add('new thread error preserves UTF16',{action:'newThread',phase:'failed',message:'failure\ud800'});
add('restore must retain native owner',{entry:'restore',request:{target:{serviceNodeId:'other'}}});add('restore read identity is exact',{entry:'restore',path:'/x/archived_sessions/a',readId:'other'});
add('restore reply identity is exact',{entry:'restore',path:'/x/archived_sessions/a',resultId:'other'});
add('failed archive missing error is accepted',{action:'archive',archived:true,phase:'failed',message:'unknown thread: thread'});add('failed archive refuses unrelated error',{action:'archive',archived:true,phase:'failed'});
add('read failure is preserved',{action:'archive',archived:true,readError:'other native error'});
add('unknown-thread matching trims ECMAScript whitespace',{action:'archive',archived:true,readError:'\ufeff UNKNOWN THREAD: THREAD \u00a0'});
add('unknown-thread matching uses Unicode lowercase',{action:'archive',archived:true,task:{primaryResourceRef:{...primary,nativeId:'Σ'}},readError:'unknown thread: σ'});
for(const state of ['accepted','dispatched','outcome_unknown'])add('pending original '+state,{action:'newThread',phase:state,passes:2});
add('confirmed absence dispatches exact retained call',{action:'newThread',absence:true});add('archive absence dispatches exact original',{action:'archive',archived:true,absence:true});
add('absence epoch mismatch forbids dispatch',{action:'newThread',absence:true,absenceEpoch:'replacement'});
add('dispatch guard rereads task',{action:'newThread',absence:true,currentTask:{claim:{run:pin(200)}}});
add('dispatch guard rejects changed revision',{action:'newThread',absence:true,currentRevision:3});
add('dispatch guard verifies logical owner',{action:'newThread',absence:true,fault:{where:'owner',code:'service_unavailable'}});
add('retained terminal call avoids preparation and inspection',{action:'newThread',retained:'succeeded',fault:{where:'allocate',code:'fixture_failed'}});
add('retained observed work cannot become absent',{action:'newThread',retained:'accepted',absence:true});
add('retained receipt is checked against immutable identity',{action:'newThread',retained:'succeeded',observation:{operationId:'wrong'}});
add('retained call skips current native preflight',{action:'archive',archived:true,retained:'succeeded',readError:'must not read'});
add('membership permits original publication reservation',{action:'archive',archived:true,currentRevision:5,currentTask:{publication:pin(300)}});
add('membership follows latest retained Task write',{action:'archive',archived:true,currentRevision:5,writes:[{contractKey:'task-board/task',outcome:pin(100,5)}]});
add('membership rejects unrelated current write',{action:'archive',archived:true,currentRevision:3});
add('membership already archived remains idempotent',{action:'archive',archived:true,meta:{archivedAt:time,effectivelyArchived:true}});
add('membership checks effective inherited archive',{action:'archive',archived:false,meta:{archivedAt:null,effectivelyArchived:true},task:{primaryResourceRef:null}});
add('archive result scope is verified',{action:'archive',archived:true,archive:{parentId:'other'}});
add('archive keeps existing outcome task',{action:'archive',archived:true,outcome:{task:pin(700)}});
for(const patch of [{callerPrincipalId:'other'},{actor:{source:'user'}}])add('automatic recovery actor '+JSON.stringify(patch),{action:'recoverThread',proof:true,operation:patch});
for(const patch of [{fields:{control:'manual'}},{workflowState:'review'},{blocker:{taskId:id(999),until:'done'}},{waiting:{reason:'host'}},{lastRun:pin(201)}])add('automatic recovery task '+JSON.stringify(patch),{action:'recoverThread',proof:true,task:patch});
add('automatic recovery requires actual proof',{action:'recoverThread',proof:false});add('admitted recovery survives expired proof',{action:'recoverThread',proof:false,retained:'succeeded'});
add('automatic recovery retains owner',{action:'recoverThread',proof:true,allocation:{target:{serviceNodeId:'other',hostId:'fixture'}}});
add('automatic recovery retains workspace',{action:'recoverThread',proof:true,allocation:{workspace:{canonicalCwd:'/other',intendedPath:'/workspace'}}});
add('failed resume requires exact missing proof',{action:'recoverThread',proof:true,phase:'failed',message:'unknown thread: thread'});
add('failed resume with confirmed rollout absence',{action:'recoverThread',proof:true,phase:'failed',message:'no rollout found for thread'});
add('recovered original thread identity is exact',{action:'recoverThread',proof:true,resultId:'other'});
add('owner registration must match status',{action:'newThread',node:{nativeVersion:'different'}});
add('owner registration must be agent service',{action:'newThread',node:{serviceName:'other'}});
add('owner must be ready',{action:'newThread',status:{state:'unavailable',epoch:null,pid:null}});
add('binding must use selected version',{action:'newThread',bindingVersion:'different'});
for(const code of ['service_not_ready','service_unavailable','task_board_native_unavailable','task_board_target_unavailable','task_board_native_outcome_unresolved','task_board_native_epoch_changed','fixture_failed'])add('publish preserves failure '+code,{action:'newThread',fault:{where:'allocate',code}});
add('aggregate failure stays outside uncertainty catch',{action:'newThread',fault:{where:'aggregate',code:'service_unavailable'}});
add('original revision guard',{action:'newThread',meta:{currentRevision:3}});add('original claim guard',{action:'newThread',task:{claim:{run:pin(200)}}});add('original publication guard',{action:'newThread',task:{publication:pin(300)}});add('archived ticket cannot start work',{action:'newThread',meta:{effectivelyArchived:true}});
add('unsupported lifecycle action',{action:'continue'});
add('archive calls finish before publication membership',{action:'archive',archived:true,runs:[{phase:'completed',primaryResourceRef:{...primary,nativeId:'second'}}],fault:{where:'operation:second',code:'service_unavailable'}});
add('partial task publication uses retained operation on retry',{action:'newThread',fault:{where:'step:2',code:'fixture_failed'},passes:2});
const groups=[];
for(const directory of readdirSync('specs/native').filter(n=>/^codex-/.test(n))){
 const path='specs/native/'+directory+'/catalog.json';if(!existsSync(path))continue;const c=JSON.parse(readFileSync(path)),catalog=checkedNativeContract(c.version,{sourceHash:c.sourceHash});
 const definitions=Object.fromEntries(['thread/start','thread/resume','thread/archive','thread/unarchive'].map(m=>[m,catalog.definitions.get(m)]));
 const draft={nativeVersion:c.version,catalogSourceHash:c.sourceHash,definitions:Object.fromEntries(Object.entries({threadStart:'thread/start',threadResume:'thread/resume',turnStart:'turn/start',turnInterrupt:'turn/interrupt'}).map(([k,m])=>[k,hashJson(catalog.definitions.get(m))])),threadStart:{cwd:'/workspace'},threadResume:{cwd:'/workspace'},turnStart:{input:[{type:'text',text:'retained input',text_elements:[]}]}};
 const plan=await saveNativePlan({request(){throw Error('Small plans must stay inline')}},catalog.contract,{draft,kind:'task-board',parentId:'root',mutationId:'plan'});
 const status={serviceNodeId:'native-agent',hostId:'fixture',nativeVersion:c.version,nativeExecutableHash:c.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch:'epoch',pid:42,state:'ready',observedAt:time,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
 const base={plan,status,node:{serviceName:'agent-manager',hostId:'fixture',nativeVersion:c.version},allocation:{target:{serviceNodeId:'native-agent',hostId:'fixture'},workspace:{canonicalCwd:'/workspace',intendedPath:'/workspace'},intent:pin(400)},task:baseTask,metadata:baseMeta};
 const selected=structuredClone(c.version==='0.159.2'?rows:rows.slice(0,standardCount));
 for(const row of selected){
  const i=row.input,trace=[],calls=new Map(),documents=new Map();let prepared=0,observed=0,invokes=0,opReads=0,step=0,consumed=false;
  const fail=where=>{if(!consumed&&i.fault?.where===where){consumed=true;throw new IvyError(i.fault.code,'Fixture failure.');}};
  const scope=async s=>{trace.push(['scope',s]);return 'scoped-'+hashJson(s).slice(7);};
  const task={pin:pin(100,2),value:{...structuredClone(baseTask),...i.task},metadata:{...baseMeta,...i.meta}};
  const request={action:i.entry==='restore'?'continue':i.action,taskId:id(100),expectedRevision:2,...(i.action==='archive'?{archived:i.archived}:{}),...(i.action==='newThread'?{reason:'requested\ud800'}:{}),...(i.action==='recoverThread'?{run:pin(200)}:{}),...(i.entry==='restore'?{target:{serviceNodeId:'native-agent'}}:{}),...i.request};
  const operation={pin:pin(300),value:{request,operationId:'action',callerPrincipalId:'caller',actor:{source:i.action==='recoverThread'?'scheduler':'user'},createdAt:time,writes:i.writes??[],nativeCalls:[],...i.operation}};
  const liveOp=structuredClone(operation),owner={serviceNodeId:'native-agent',hostId:'fixture',nativeVersion:c.version,nativeExecutableHash:c.nativeExecutableHash,catalogHash:catalog.catalogHash};
  const observation=(call,phase)=>({schemaVersion:1,operationId:call.operationId,callerPrincipalId:'caller',serviceNodeId:'native-agent',nativeVersion:c.version,nativeExecutableHash:c.nativeExecutableHash,method:call.method,params:call.params,requestHash:hashJson({method:call.method,params:call.params}),phase,createdAt:time,updatedAt:time,epoch:phase==='accepted'?null:'epoch',requestId:phase==='accepted'?null:42,reply:['succeeded','failed'].includes(phase)?phase==='failed'?{error:{code:-32000,message:i.message??'native failed'}}:{result:{thread:{id:i.resultId??(call.method==='thread/start'?'new-thread':call.params.threadId),ephemeral:i.ephemeral??false}}}:null,code:phase==='failed'?'native_failed':phase==='outcome_unknown'?'outcome_unknown':null,...i.observation});
  if(i.retained){const method=i.action==='archive'?(i.archived?'thread/archive':'thread/unarchive'):i.action==='recoverThread'?'thread/resume':'thread/start';const p=task.value.primaryResourceRef,key=method==='thread/start'?'new-thread':method+':'+hashJson(p).slice(7),call={key,owner,operationId:'retained',method,params:method==='thread/start'?draft.threadStart:method==='thread/resume'?{...draft.threadResume,threadId:p.nativeId,excludeTurns:true}:{threadId:p.nativeId},definitionHash:hashJson(definitions[method]),observation:null};call.observation=observation(call,i.retained);liveOp.value.nativeCalls.push(call);operation.value.nativeCalls.push(structuredClone(call));calls.set(call.operationId,call);}
  const read=async(key,address,primary=true,archived=false)=>{
   trace.push(['read',key,address,primary,archived]);fail('read:'+key);
   if(key==='task-board/operation'){opReads++;return structuredClone(liveOp);}
   if(key==='task-board/native-plan')return{pin:pin(400),value:plan};
   if(key==='task-board/task'){
    if(typeof address!=='string'&&address.revision===1){if(i.oldReadError)throw new IvyError(i.oldReadError,'Fixture old task unavailable.');return{...task,pin:pin(100),value:{...task.value,primaryResourceRef:i.oldPrimary??primary}};}
    if(typeof address==='string')return{...structuredClone(task),pin:pin(100,i.currentRevision??2),value:{...structuredClone(task.value),...i.currentTask}};
    return structuredClone(task);
   }
   if(key==='task-board/run'&&address.objectId===id(200))return{pin:pin(200),value:{target:base.allocation.target,workspace:base.allocation.workspace}};
   const d=documents.get(address.objectId);assert.ok(d,'Unknown '+key+JSON.stringify(address));return d;
  };
  const store={rootObjectId:'root',read,async page(key,options){trace.push(['page',key,options]);const list=key==='task-board/run'?(i.runs??[]):(i.histories??[]).map(previousTask=>({previousTask}));const offset=Number(options.cursor??0),size=i.pageSize??100;const data=list.slice(offset,offset+size).map((value,j)=>{const n=(key==='task-board/run'?500:600)+offset+j;documents.set(id(n),{pin:pin(n),value});return{objectId:id(n),revision:1};});return{items:data,nextCursor:offset+size<list.length?String(offset+size):null};},completeLocalWorkflow(id,at){trace.push(['complete',id,at]);},releaseWorkspace(id){trace.push(['release',id]);}};
  const rpcReplies=[];
  const rawClient={async request(method,args){trace.push(['rpc',method,args]);fail(method);
   if(method==='serviceNodes.get')return{...base.node,...i.node};
   if(method==='tools.list'){const namespace=args.namespace,name=args.namePrefix,definition=namespace==='codex'?{...definitions[name],...(i.bindingVersion?{interfaceVersion:i.bindingVersion}:{})}:{namespace:'agent',name,interfaceVersion:'1.0.0',annotations:{readOnlyHint:true},inputSchema:{},outputSchema:{}};return{provider:{node:{serviceNodeId:'native-agent'}},items:[{qualifiedName:namespace+'.'+name,definition,definitionHash:namespace==='codex'?hashJson(definition):'sha256:'+'a'.repeat(64),catalogHash:zero}],nextCursor:null};}
   if(method==='objects.archive')return{...baseMeta,...i.meta,archivedAt:args.archived?time:null,effectivelyArchived:args.archived,...i.archive};
   if(method==='objects.stat')return{...baseMeta,archivedAt:i.archived?time:null,effectivelyArchived:i.archived??false};
   assert.equal(method,'tools.call');const a=args.arguments,name=args.qualifiedName;
   if(name==='agent.status')return{...status,...i.status};
   if(name==='agent.read'){const thread=a.params.threadId;return{schemaVersion:1,observationId:'read',callerPrincipalId:'caller',serviceNodeId:'native-agent',nativeVersion:c.version,nativeExecutableHash:c.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch:'epoch',requestId:1,observedAt:time,method:a.method,params:a.params,requestHash:hashJson({method:a.method,params:a.params}),reply:i.readError?{error:{code:-32000,message:i.readError}}:{result:{thread:{id:i.readId??thread,ephemeral:false,path:i.path??'/x/rollout/a',status:{type:i.threadStatus??'idle'}}}}};}
   const call=calls.get(a.operationId);assert.ok(call,name+JSON.stringify(a));
   if(name==='agent.operation'){fail('operation:'+call.params.threadId);if(i.absence)throw new IvyError('not_found','Original operation absent.','not_executed',{kind:'agent_operation_absent',operationId:call.operationId,serviceNodeId:'native-agent',epoch:i.absenceEpoch??'epoch'});return observation(call,i.phase??'succeeded');}
   assert.equal(name,'agent.invoke');invokes++;return observation(call,i.phase??'succeeded');
  }};
  const client={async request(method,args){try{const value=await rawClient.request(method,args);rpcReplies.push({method,args,value:structuredClone(value)});return value;}catch(e){if(!(e instanceof IvyError))throw e;rpcReplies.push({method,args,error:{code:e.code,message:e.message,outcome:e.outcome,details:e.details??null}});throw e;}}};store.client=client;
  const engine={store,settings:{principalId:'caller',scheduler:{enabled:i.enabled??true}},checks:{async aggregate(value,id){trace.push(['aggregate',value,id]);fail('aggregate');}},async verifyOwner(){trace.push(['owner']);fail('owner');},operations:{async prepareNative(id,call){trace.push(['prepare-native',id,call]);prepared++;liveOp.value.nativeCalls.push(structuredClone(call));calls.set(call.operationId,structuredClone(call));return call;},async observeNative(id,key,value){trace.push(['observe-native',id,key,value]);observed++;const call=liveOp.value.nativeCalls.find(c=>c.key===key);assert.ok(call);call.observation=value;return call;},async archiveIdentity(id){trace.push(['archive-identity',id]);return 'archive-id';}}};
  class Scheduler{async allocate(task,id){trace.push(['allocate',task,id]);fail('allocate');return{...base.allocation,...i.allocation};}}
  const Lifecycle=new Function('hashJson','IvyError','requireThat','validateAgent','scopedOperationId','serviceTools','NativeOwner','reconcileNativeOperation','nativeRecord','nativeString','missingNativeThread','unstartedMissingContext','same','readPlan','TaskBoardScheduler',stripTypeScriptTypes(source,{mode:'transform'})+'\nreturn TaskBoardNativeLifecycle;')(hashJson,IvyError,requireThat,validateAgent,(_client,s)=>scope(s),serviceTools,NativeOwner,reconcileNativeOperation,nativeRecord,nativeString,missingNativeThread,async(_e,t)=>{trace.push(['proof',t]);return i.proof??false;},same,readPlan,Scheduler);
  const steps={pin:pin(300),async write(key,value,destination){trace.push(['step-write',key,value,destination]);step++;fail('step:'+step);return{objectId:destination.objectId,revision:destination.expectedRevision+1};},async create(key,value,label){trace.push(['step-create',key,value,label]);step++;fail('step:'+step);return pin(800);}};
  const initial={task:structuredClone(task),operation:structuredClone(operation),liveOperation:structuredClone(liveOp)},outcomes=[];
  for(let pass=0;pass<(i.passes??1);pass++)try{const value=i.entry==='restore'?await new Lifecycle(engine).restoreForExecution(operation,task):await new Lifecycle(engine).publish(operation,steps,{task:null,history:null,archive:null,...i.outcome});outcomes.push({ok:value??null});}catch(e){if(!(e instanceof IvyError))throw new Error(row.name,{cause:e});outcomes.push({error:{code:e.code,message:e.message,outcome:e.outcome,details:e.details??null}});}
  if(selected.indexOf(row)<standardCount)assert.ok(outcomes.every(o=>'ok'in o),row.name+JSON.stringify({outcomes,last:trace.slice(-4)}));
  row.initial=initial;row.rpcReplies=rpcReplies;row.expectedHash=hashJson({outcomes,trace,nativeCalls:liveOp.value.nativeCalls});row.counts={prepared,observed,invokes,opReads};
 }
 groups.push({path,base,rows:selected});
}
process.stdout.write(JSON.stringify(groups));
