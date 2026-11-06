import {registerHooks} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {NativeOwner}=await import('../../packages/sdk/src/native-owner.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const base=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const catalog={...base,clientRequests:[{...base.clientRequests[0],inputSchema:true,outputSchema:true}],serverRequests:[],serverNotifications:[],clientNotifications:[],derivedLegacyTypes:[]};
const digest=letter=>'sha256:'+letter.repeat(64), time='2026-01-02T03:04:05.006Z';
const target={serviceNodeId:'native-agent',hostId:'fixture',nativeVersion:catalog.version,nativeExecutableHash:catalog.nativeExecutableHash,catalogHash:hashJson(catalog)};
const status={...target,epoch:'epoch',pid:42,state:'ready',observedAt:time,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
const call={operationId:'original',method:'thread/read',params:{threadId:'thread'},definitionHash:digest('b')};
const operation={schemaVersion:1,operationId:call.operationId,callerPrincipalId:'caller',serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,method:call.method,params:call.params,requestHash:hashJson({method:call.method,params:call.params}),createdAt:time,updatedAt:time,phase:'accepted',epoch:null,requestId:null,reply:null,code:null};
const observation={schemaVersion:1,observationId:'read',callerPrincipalId:'caller',serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,catalogHash:target.catalogHash,epoch:'epoch',requestId:42,observedAt:time,requestHash:operation.requestHash,method:call.method,params:call.params,reply:{result:{thread:{id:'thread'}}}};
const interaction={operationId:'original',epoch:'epoch',reply:{result:null}};
const frame={...target,epoch:'epoch',requestFrameBytes:6291456,receivedFrameBytes:25165824,answerFrameBytes:4194304,managementFrameBytes:33554432};delete frame.hostId;
const absence={kind:'agent_operation_absent',operationId:'original',serviceNodeId:target.serviceNodeId,epoch:'epoch'};
const err=(code='fixture_failure',outcome='not_executed',details)=>({code,message:code==='tool_definition_changed'?'Changed.':'Fixture failure.',outcome,...(details===undefined?{}:{details})});
const changed=()=>({error:err('tool_definition_changed')});
const rows=[];
async function add(label,action,steps,edit=()=>{}){
 const input=structuredClone({target,call,action,steps});edit(input);
 const trace=[],lists=new Map(),controller=new AbortController();let calls=0,guards=0;
 const failure=e=>new IvyError(e.code,e.message,e.outcome,e.details);
 const cancel=new IvyError('fixture_cancelled','Fixture cancellation.');
 if(input.cancelBefore)controller.abort(cancel);
 const client={async request(method,params){
  trace.push({method,params:structuredClone(params)});
  if(method==='tools.list'){
   const name=params.namespace+'.'+params.namePrefix,index=lists.get(name)??0;lists.set(name,index+1);
   if(input.listErrors?.[name]?.[index])throw failure(input.listErrors[name][index]);
   const hash=input.hashes?.[name]?.[index]??digest(params.namespace==='agent'?'a':'b');
   return {provider:{node:{serviceNodeId:input.target.serviceNodeId}},items:[{qualifiedName:name,definition:{namespace:params.namespace,name:params.namePrefix,interfaceVersion:params.namespace==='agent'?'1.0.0':target.nativeVersion,inputSchema:{},outputSchema:{}},definitionHash:hash,catalogHash:digest('c')}],nextCursor:null};
  }
  assert.equal(method,'tools.call');const step=input.steps[calls++];assert(step,'unexpected call');
  if(input.cancelAfter===calls)controller.abort(cancel);
  if(step.error)throw failure(step.error);return structuredClone(step.value);
 }};
 const owner=new NativeOwner(client,'caller',input.target,{signal:controller.signal,timeoutMs:1234});
 const guard=async()=>{trace.push({guard:++guards});if(input.cancelGuard===guards)controller.abort(cancel);if(input.guardErrors?.[guards-1])throw failure(input.guardErrors[guards-1]);};
 async function run(){
  if(action==='management')return owner.management('custom',{x:'\ud800'},'original');
  if(action==='managementConflict')return owner.management('custom',{backend:input.otherBackend},'original');
  if(action==='dispatch')return (await owner.dispatch(input.call,guard))??null;
  if(action==='interact')return owner.interact(input.call,guard);
  if(action==='interaction')return owner.interaction('original');
  if(action==='operation')return owner.operation(input.call);
  if(action==='originalOperation'){
   const original=structuredClone(input.originalOwner),retained=owner.originalOperations(original);
   original.serviceNodeId='changed-after-selection';
   assert.equal('dispatch' in retained,false);
   return retained.operation(input.call);
  }
  if(action==='status')return owner.status();
  if(action==='frame')return owner.frameLimits();
  if(action==='read')return owner.read(input.call.method,input.call.params,input.epoch,input.allowError);
  if(action==='project')return owner.threadStartParams(input.call.params);
  if(action==='contract'){
   const first=await owner.selectedContract(),second=await new NativeOwner(client,'other-caller',input.target).selectedContract();
   assert.equal(first,second);return {catalogHash:first.catalogHash};
  }
  if(action==='contractRetry'){
   try{await owner.selectedContract();}catch{}
   return {catalogHash:(await owner.selectedContract()).catalogHash};
  }
  if(action==='contractConcurrent'){
   const other=new NativeOwner(client,'other-caller',input.target);
   const [first,second]=await Promise.all([owner.selectedContract(),other.selectedContract()]);
   assert.equal(first,second);return {catalogHash:first.catalogHash};
  }
  if(action==='contractCache'){
   for(const index of [...Array(16).keys(),0,16,0])await new NativeOwner(client,'caller',{...input.target,hostId:'host'+index}).selectedContract();
   return null;
  }
  if(action==='contractAccounts'){
   const first=await owner.selectedContract();
   const second=await new NativeOwner(client,'caller',{...input.target,backend:input.otherBackend}).selectedContract();
   assert.notEqual(first,second);
   const method=catalog.clientRequests[0].method;
   assert.equal(first.catalog,second.catalog);
   assert.equal(first.inputSchema(method),second.inputSchema(method));
   return {sameCatalog:first.catalogHash===second.catalogHash,firstDefinitionHash:first.definitionHash(method),secondDefinitionHash:second.definitionHash(method)};
  }
  throw Error(action);
 }
 let expected;try{expected={ok:true,value:await run()};}catch(e){expected={ok:false,error:IvyError.from(e).toWire()};}
 rows.push({label,input:JSON.stringify(input),expected:JSON.stringify(expected),trace:JSON.stringify(trace)});
}
await add('management','management',[{value:{ok:true}}]);
for(const action of ['management','dispatch','interact']){
 const name=action==='management'?'custom':action==='dispatch'?'invoke':'interact',value=action==='interact'?interaction:operation;
 await add(action+'-ok',action,[{value}]);
 for(const outcome of ['not_executed','unknown','completed'])for(const code of ['fixture_failure','tool_definition_changed'])await add(action+'-'+code+'-'+outcome,action,[{error:err(code,outcome)}]);
 await add(action+'-same-envelope',action,[changed()]);
 const steps=action==='management'?[changed(),{value}]:[changed(),{value:status},{value}];
 await add(action+'-refresh',action,steps,i=>i.hashes={['agent.'+name]:[digest('a'),digest('d')]});
 await add(action+'-refresh-fails',action,[changed()],i=>{i.hashes={['agent.'+name]:[digest('a'),digest('d')]};i.listErrors={['agent.'+name]:[null,err('fixture_discovery','unknown')]};});
 await add(action+'-second-send-fails',action,[...steps.slice(0,-1),changed()],i=>i.hashes={['agent.'+name]:[digest('a'),digest('d')]});
 await add(action+'-cancel-before',action,[],i=>i.cancelBefore=true);
 if(action!=='management'){
  await add(action+'-guard-fails',action,[],i=>i.guardErrors=[err('fixture_guard','unknown')]);
  await add(action+'-guard-fails-after-refresh',action,steps,i=>{i.hashes={['agent.'+name]:[digest('a'),digest('d')]};i.guardErrors=[null,err('fixture_guard','unknown')];});
  await add(action+'-cancel-guard',action,[],i=>i.cancelGuard=1);
  await add(action+'-native-changed',action,steps,i=>i.hashes={['agent.'+name]:[digest('a'),digest('d')],'codex.thread/read':[digest('b'),digest('e')]});
  await add(action+'-wrong-initial-native',action,[],i=>i.call.definitionHash=digest('f'));
  await add(action+'-status-fails',action,[changed(),{value:{...status,state:'stopped'}}],i=>i.hashes={['agent.'+name]:[digest('a'),digest('d')]});
 }
}
await add('operation-valid','operation',[{value:operation}]);
await add('operation-wrong-caller','operation',[{value:{...operation,callerPrincipalId:'other'}}]);
for(const details of [absence,{...absence,operationId:'other'},{...absence,serviceNodeId:'other'},null,{}])await add('operation-absence','operation',[{error:err('not_found','not_executed',details)}]);
for(const action of ['interact','interaction'])for(const value of [{...interaction,operationId:'other'},null,{}])await add(action+'-invalid',action,[{value}]);
for(const value of [status,{...status,epoch:null},{...status,hostId:'other'},{}])await add('status','status',[{value}]);
for(const value of [frame,{...frame,epoch:null},{...frame,catalogHash:digest('f')},{}])await add('frame','frame',[{value}]);
for(const value of [observation,{...observation,reply:{error:{code:-1,message:'failed'}}},{...observation,epoch:'other'},{...observation,params:{}},null,{}])for(const allowError of [false,true])await add('read','read',[{value}],i=>{i.epoch='epoch';i.allowError=allowError;});
await add('read-observed-epoch','read',[{value:observation}]);
await add('read-absent-observed-epoch','read',[{value:{}}]);
await add('project-explicit','project',[],i=>i.call.params={cwd:'C:/workspace'});
await add('project-cancel','project',[],i=>{i.call.params={cwd:'C:/workspace'};i.cancelBefore=true;});
const discovery=[{value:status},{value:catalog},{value:status}];
await add('contract-shared','contract',discovery);
await add('contract-optional-host','contract',[{value:status},...discovery],i=>delete i.target.hostId);
await add('contract-retry','contractRetry',[{error:err()},...discovery]);
await add('contract-cancel-observer','contract',discovery,i=>i.cancelAfter=3);
await add('contract-concurrent','contractConcurrent',discovery);
await add('contract-fifo-eviction','contractCache',[...Array(17).keys(),0].flatMap(index=>[{value:{...status,hostId:'host'+index}},{value:catalog},{value:{...status,hostId:'host'+index}}]));
const backendA={backendId:'codex',provider:'codex',accountId:'work'};
const backendB={...backendA,accountId:'personal'};
for(const action of ['management','dispatch','interact','interaction','operation','status','frame','read']){
 const value={management:{ok:true},dispatch:operation,interact:interaction,interaction,operation,status,frame,read:observation}[action];
 await add('backend-'+action,action,[{value:action==='management'?value:{...value,backend:backendA}}],i=>i.target.backend=backendA);
 if(action!=='management'&&action!=='dispatch')await add('backend-wrong-account-'+action,action,[{value:{...value,backend:backendB}}],i=>i.target.backend=backendA);
}
await add('backend-before-send-conflict','managementConflict',[],i=>{i.target.backend=backendA;i.otherBackend=backendB;});
await add('backend-unscoped-before-send-conflict','managementConflict',[],i=>i.otherBackend=backendA);
for(const backend of [{...backendA,backendId:'worker'}, {backendId:'claude',provider:'claude',accountId:'personal'}]){
 for(const action of ['dispatch','interact']) {
  const value=action==='dispatch'?operation:interaction;
  await add('named-backend-'+backend.backendId+'-'+action,action,[{value:{...value,backend}}],i=>i.target.backend=backend);
  assert.equal(JSON.parse(rows.at(-1).expected).ok,true);
  await add('named-backend-refresh-'+backend.backendId+'-'+action,action,
   [changed(),{value:{...status,backend}},{value:{...value,backend}}],i=>{
    i.target.backend=backend;i.hashes={['agent.'+(action==='dispatch'?'invoke':'interact')]:[digest('a'),digest('d')]};
   });
  assert.equal(JSON.parse(rows.at(-1).expected).ok,true);
 }
}
await add('backend-catalog-cache-accounts','contractAccounts',[
 {value:{...status,backend:backendA}},{value:catalog},{value:{...status,backend:backendA}},
 {value:{...status,backend:backendB}},{value:catalog},{value:{...status,backend:backendB}},
],i=>{i.target.backend=backendA;i.otherBackend=backendB;});
for(const [label,second] of [
 ['foreign-account',[{value:{...status,backend:backendA}}]],
 ['changed-catalog',[{value:{...status,backend:backendB}},{value:{...catalog,sourceHash:digest('f')}}]],
 ['changed-epoch',[{value:{...status,backend:backendB}},{value:catalog},{value:{...status,backend:backendB,epoch:'new-epoch'}}]],
]){
 await add('shared-catalog-rejects-'+label,'contractAccounts',[
  {value:{...status,backend:backendA}},{value:catalog},{value:{...status,backend:backendA}},...second,
 ],i=>{i.target.backend=backendA;i.otherBackend=backendB;});
 assert.equal(JSON.parse(rows.at(-1).expected).ok,false);
}
assert(rows.some(r=>JSON.parse(r.expected).ok));assert(rows.length>=80);
const relocated=i=>{i.originalOwner=structuredClone(i.target);i.target={...i.target,serviceNodeId:'shared-agent',backend:backendA};};
for(const phase of ['accepted','dispatched','outcome_unknown','succeeded','failed']){
 const value={...operation,phase,epoch:phase==='accepted'?null:'old-epoch',requestId:phase==='accepted'?null:'old-request',
  code:['outcome_unknown','failed'].includes(phase)?'original_failure':null,reply:phase==='succeeded'?{result:{original:'\ud800'}}:null};
 await add('relocated-original-'+phase,'originalOperation',[{value}],relocated);
 assert.equal(JSON.parse(rows.at(-1).expected).ok,true);
}
for(const change of [{serviceNodeId:'other'}, {callerPrincipalId:'other'}, {backend:backendA}, {params:{changed:true}}, {requestHash:digest('f')}]){
 await add('relocated-original-mismatch','originalOperation',[{value:{...operation,...change}}],relocated);
 assert.equal(JSON.parse(rows.at(-1).expected).error.data.code,'native_evidence_mismatch');
}
await add('relocated-current-absence-is-not-original-absence','originalOperation',[
 {error:err('not_found','not_executed',{...absence,serviceNodeId:'shared-agent',backend:backendA})},
],relocated);
assert.equal(JSON.parse(rows.at(-1).expected).error.data.code,'native_absence_mismatch');
await add('relocated-original-after-native-upgrade','originalOperation',[{value:operation}],i=>{
 relocated(i);i.target.nativeVersion='0.163.0';i.target.nativeExecutableHash=digest('e');i.target.catalogHash=digest('f');
});
assert.equal(JSON.parse(rows.at(-1).expected).ok,true);
await add('relocated-original-current-check-rejects','operation',[{value:operation}],i=>{i.target.serviceNodeId='shared-agent';i.target.backend=backendA;});
assert.equal(JSON.parse(rows.at(-1).expected).error.data.code,'native_evidence_mismatch');
process.stdout.write(JSON.stringify(rows));
