import {Client,compact,collectorSeeds,now,at,instructions,summary,error,result} from './chat-collector-oracle.mjs';
import {bodies} from './chat-native-evidence-oracle.mjs';
import {mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,basename} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
import {buildInstructions} from '../../../tools/build/build-instructions.mjs';
const {ChatStore,mutation}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatOperations}=await import('../../../services/chat-bridge/src/operations.ts');
const {ChatQueue}=await import('../../../services/chat-bridge/src/queue.ts');
const {ChatCancellation}=await import('../../../services/chat-bridge/src/cancellation.ts');
const {ChatCollector}=await import('../../../services/chat-bridge/src/collector.ts');
const {ChatCancelWorker}=await import('../../../services/chat-bridge/src/cancel-worker.ts');
const {ChatNativeEvidence}=await import('../../../services/chat-bridge/src/native-evidence.ts');
const {ChatNotices}=await import('../../../services/chat-bridge/src/notices.ts');
const {checkedNativeContract}=await import('../../../packages/contracts/src/checked-native-contract.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {hashJson,digest}=await import('../../../packages/contracts/src/canonical.ts');
const {deriveOperationId}=await import('../../../packages/contracts/src/operation-id.ts');
const clone=v=>structuredClone(v),probe=Symbol.for('ivy.test.chat-native-evidence-gate');
const lookup=(r,p)=>p.startsWith('data:/')?p.slice(6).split('/').reduce((v,k)=>v?.[k],r.content.value):p.split('.').reduce((v,k)=>v?.[k],r);
function matches(r,p){switch(p.op){case'and':return p.args.every(p=>matches(r,p));case'or':return p.args.some(p=>matches(r,p));case'isNull':return lookup(r,p.field)===null;case'eq':return lookup(r,p.field)===p.value;default:throw Error('Unsupported worker predicate '+p.op);}}
class WorkerClient extends Client{
 async request(method,p){if(method!=='objects.query')return super.request(method,p);this.calls.push({method,params:clone(p)});const rows=this.records.filter(r=>r.object.contractKey===p.contractKey&&matches(r,p.where));return{items:rows.slice(0,p.limit).map(r=>({objectId:r.object.id,revision:r.revision.revision})),nextCursor:rows.length>p.limit?'more':null};}
}
const assetRoot=mkdtempSync(join(tmpdir(),'ivy-cancel-worker-assets-'));let noticeInstructions;
try{const files=buildInstructions({root:process.cwd(),outputRoot:assetRoot,component:'chat-bridge'});noticeInstructions=readFileSync(files.find(p=>basename(p)==='chat-notice-prompt.json')).toString('base64');}finally{rmSync(assetRoot,{recursive:true,force:true});}
const cases=[],step={action:'step'},settle={action:'settle'},reopen={action:'reopen'},rounds=n=>Array.from({length:n},()=>step),control=value=>({action:'control',value}),patch=value=>({action:'patch-input',value});
const add=(label,steps,extra={})=>cases.push({label,version:'0.154.0',steps,...extra});
add('prevent-original-turn-then-settle',[...rounds(3),settle,reopen,step,settle],{mode:'turn'});
add('prevent-both-originals-then-settle',[...rounds(4),settle],{mode:'both'});
add('already-succeeded-resume-can-settle',[...rounds(2),settle],{mode:'resume-succeeded'});
add('native-turn-won-prevention-race',[step,reopen,...rounds(3),settle],{mode:'won'});
for(const version of ['0.142.3','0.158.0','0.159.2'])add('exact-interrupt-'+version,[...rounds(3),reopen,step,settle],{version});
for(const prevent of ['unknown','pending','failure','failure-epoch','unknown-error'])add('original-prevention-'+prevent,[control({prevent}),...rounds(4),reopen,step,settle],{mode:'turn'});
for(const advance of ['unknown','failed','pending'])add('original-interrupt-'+advance,[step,control({advance}),...rounds(3),reopen,step,settle]);
add('saved-terminal-suppresses-interrupt',[{action:'collect'},step,reopen,step,settle]);
add('pre-turn-failure-without-intent-settles',[settle,reopen,settle],{skipCancel:true,mode:'failure'});
for(const value of [{state:'cancelled',finishedAt:at},{state:'failed',finishedAt:at},{result:{objectId:'retained-result',revision:1}},{cancellation:null}])add('inactive-input-'+cases.length,[patch(value),step,settle]);
add('no-head',[control({noHead:true}),step,settle]);
add('prevention-guard-revision-race',[control({guardChange:true}),step,settle],{mode:'turn'});
add('interrupt-guard-revision-race',[step,control({guardChange:true}),step,settle]);
add('owner-unavailable-before-state-write',[control({ownerUnavailable:true}),step,settle]);
add('failed-turn-blocked-by-unknown-resume',[control({prevent:'failure'}),step,control({prevent:'unknown'}),step,patch({state:'failed',finishedAt:at}),settle],{mode:'both'});
for(const target of ['pendingAction','publication'])add('settlement-preserves-'+target,[{action:'patch-main',value:{...(target==='publication'?{nextSequence:3}:{}),[target]:target==='pendingAction'?{objectId:'missing-operation',revision:1}:{inputId:'original-input',collection:{objectId:'missing-collection',revision:1},result:{objectId:'missing-result',revision:1},firstSequence:2,partCount:1,publishedParts:0,createdAt:at}}},settle],{skipCancel:true,mode:'failure'});
for(const value of [{method:'thread/start'},{operationId:'foreign-operation'}])add('original-call-identity-'+cases.length,[{action:'patch-call',slot:'turn',value},step,settle],{mode:'turn'});
add('pre-turn-cannot-settle-interrupt',[patch({turnId:null,epoch:null,state:'failed',finishedAt:at,nativeCalls:{resume:null,turn:null,interrupt:null}}),{action:'attach-interrupt'},settle],{skipCancel:true});

for(const c of cases){
 const seed=collectorSeeds[c.version],root=mkdtempSync(join(tmpdir(),'ivy-cancel-worker-')),file=join(root,'chat-bridge.sqlite');writeFileSync(file,Buffer.from(seed.database,'base64'));
 const client=new WorkerClient(seed.records),events=[],gates=[],controls={},aliases=new Map(),mutations=new Map();let store,admission,operations,queue,evidence,main,collector,worker,last=0,readIndex=0;
 c.expected=[];c.outcomes=[];c.observations=[];
 globalThis[probe]=(kind,input,fn)=>{const call={kind,input:(()=>{const v=clone(input);if(v.value)v.value=summary(v.value);if(v.params)v.params=summary(v.params);if(v.observation)v.observation=summary(v.observation);return v;})()};gates.push(call);try{const v=fn();if(v?.then)return v.then(value=>{call.output={value:value??null};return value;},e=>{call.output={error:error(e)};throw e;});call.output={value:v??null};return v;}catch(e){call.output={error:error(e)};throw e;}};
 const input=()=>store.read('chat-bridge/input','original-input');
 function patchInput(value){const record=client.records[0];Object.assign(record.content.value,clone(value));record.revision.revision++;client.versions.get('original-input').set(record.revision.revision,clone(record));}
 const update=async(key,doc,value)=>store.write(key,{...doc.value,...value},mutation(doc.pin.objectId,'fixture:'+doc.pin.revision),{objectId:doc.pin.objectId,expectedRevision:doc.pin.revision});
 const catalog=checkedNativeContract(c.version);
 async function prepare(id,operationId,request,slot='interrupt',log=true){
  if(log)events.push({method:'prepare',inputId:id,operationId,request:clone(request)});
  const reference=await evidence.saveRequest(id,request),key={'turn/start':'turnStart','thread/resume':'threadResume','turn/interrupt':'turnInterrupt'}[request.method];
  const value={schemaVersion:1,operationId,serviceNodeId:'owner',callerPrincipalId:'chat-service',request:reference,nativeVersion:c.version,catalogSourceHash:request.catalogSourceHash,method:request.method,predecessor:null,preparedEpoch:slot==='resume'?'original-epoch':null,requestHash:hashJson({method:request.method,params:request.params}),expectedDefinitionHash:seed.definition.nativePlan.definitions[key],createdAt:at,state:'prepared',epoch:null,evidence:null,code:null,updatedAt:at};
  const pin=await store.write('chat-bridge/native-call',value,mutation(id,'fixture-prepare:'+slot),{create:{parentId:id,name:'Fixture '+slot}});return store.read('chat-bridge/native-call',pin,id);
 }
 async function outcome(id,call,mode,record=true){
  const request=await evidence.request(id,call.value),phase=mode==='unknown'?'outcome_unknown':mode==='pending'?'dispatched':['failure','failure-epoch','failed','prevented'].includes(mode)?'failed':'succeeded';
  const epoch=mode==='prevented'||mode==='failure'?null:'original-epoch',code=mode==='prevented'?'request_prevented':mode==='unknown'?'native_epoch_lost':phase==='failed'?'native_error':null;
  const resumed={thread:{cliVersion:c.version,createdAt:1788690000,cwd:'/synthetic/project',ephemeral:false,id:'original-thread',modelProvider:'openai',preview:'Original resume',projectId:null,sessionId:'original-thread',source:'appServer',status:{type:'idle'},turns:[],updatedAt:1788690000,canAcceptDirectInput:true,name:'Original'},cwd:'/synthetic/project',model:'fixture-model',modelProvider:'openai',approvalPolicy:'on-request',approvalsReviewer:'user',sandbox:{type:'readOnly'},reasoningEffort:'high'};
  const operation={schemaVersion:1,operationId:call.value.operationId,callerPrincipalId:'chat-service',serviceNodeId:'owner',nativeVersion:c.version,nativeExecutableHash:catalog.catalog.nativeExecutableHash,method:request.method,params:request.params,requestHash:call.value.requestHash,phase,createdAt:at,updatedAt:at,epoch,requestId:epoch?'fixture-native-request':null,code,reply:phase==='succeeded'?{result:request.method==='turn/start'?{turn:{id:'original-turn',status:'inProgress',items:[]}}:request.method==='thread/resume'?resumed:{}}:phase==='failed'&&epoch?{error:{code:-32000,message:'Original native request failed.'}}:null};
  if(record)c.outcomes.push(clone(operation));
  const reference=await evidence.saveOperation(id,call.value,operation);
  const pin=await update('chat-bridge/native-call',call,{state:phase,epoch,evidence:reference,code,updatedAt:at});return store.read('chat-bridge/native-call',pin,id);
 }
 async function dispatch(kind,id,pin,guard){
  events.push({method:kind,inputId:id,pin});
  if(controls.guardChange){patchInput({state:'preparing'});controls.guardChange=false;}
  await guard();
  if(kind==='prevent'&&controls.prevent==='unknown-error')throw new IvyError('chat_native_prevention_unknown','Original prevention is unavailable.','unknown');
  const call=await store.read('chat-bridge/native-call',pin.objectId,id);
  if(call.value.state!=='prepared')return call;
  return outcome(id,call,controls[kind]??(kind==='prevent'?'prevented':'succeeded'));
 }
 function open(){
  store=new ChatStore(client,null,root);admission=new ChatAdmission(seed.definition);operations=new ChatOperations(store,admission);queue=new ChatQueue(operations);evidence=new ChatNativeEvidence(store,seed.definition);
  main={store,admission,operations,queue,async ensure(){throw Error('Unexpected binding creation');},async recoverPending(){events.push({method:'recover'});const current=await queue.main();if(current.value.pendingAction){const operation=await store.read('chat-bridge/operation',current.value.pendingAction);if(operation.value.request.action==='cancel')await new ChatCancellation(main).recoverPending();else if(operation.value.request.action==='notify')await new ChatNotices(main).recoverPending();else await queue.recoverPending();}},async fixtureHead(id){events.push({method:'head',inputId:id??null});if(controls.noHead||!(await queue.main()).value.queue.length)return null;return{input:await store.read('chat-bridge/input',id??'original-input'),binding:await store.read('chat-bridge/binding',seed.binding)};},native:{evidence,async verifyOwner(){events.push({method:'verify'});if(controls.ownerUnavailable)throw new IvyError('fixture_owner_unavailable','Original owner is unavailable.');},async get(id,pin){events.push({method:'call',inputId:id,pin});return store.read('chat-bridge/native-call',pin.objectId,id);},prepare,prevent:(id,pin,guard)=>dispatch('prevent',id,pin,guard),advance:(id,pin,guard)=>dispatch('advance',id,pin,guard),async ready(){events.push({method:'ready'});return{epoch:'original-epoch',observedAt:at};},async management(method,request){events.push({method:'read',request});const observation={schemaVersion:1,observationId:'cancel-observation-'+readIndex,serviceNodeId:'owner',callerPrincipalId:'chat-service',nativeVersion:c.version,epoch:'original-epoch',nativeExecutableHash:catalog.catalog.nativeExecutableHash,catalogHash:catalog.catalogHash,method:request.method,params:request.params,requestHash:hashJson({method:request.method,params:request.params}),requestId:'cancel-read-'+readIndex,observedAt:at,reply:{result:{data:[{id:'original-turn',status:'completed',items:[],itemsView:'notLoaded'}],nextCursor:null}}};readIndex++;c.observations.push(observation);return observation;}}};
  collector=new ChatCollector(main);worker=new ChatCancelWorker(collector);
 }
 function normalize(v){if(typeof v==='string'){if(aliases.has(v))return aliases.get(v);for(const[id,name]of aliases)if(v.includes(id))v=v.replaceAll(id,name);return v;}if(Array.isArray(v))return v.map(normalize);if(v&&typeof v==='object')return Object.fromEntries(Object.entries(v).map(([k,v])=>[k,normalize(v)]));return v;}
 function snapshot(output){
  const db=new DatabaseSync(file),rows=db.prepare('SELECT d.id,d.key,d.name,d.parent_id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON d.id=r.id AND d.current_revision=r.revision ORDER BY d.name,d.id').all(),blobs=db.prepare('SELECT id,key,name,parent_id,bytes FROM blobs ORDER BY name,id').all();
  for(const r of rows)aliases.set(r.id,r.key==='chat-bridge/collection'?'<collection>':r.key==='chat-bridge/result'?'<result>':'<local:'+r.name+'>');for(const b of blobs)aliases.set(b.id,'<blob:'+b.name+'>');
  for(const call of client.calls)if(call.params.mutationId){const id=call.params.mutationId;if(!mutations.has(id))mutations.set(id,'<mutation:'+mutations.size+'>');aliases.set(id,mutations.get(id));}
  const writes=db.prepare('SELECT rowid,key,id,revision,value_json FROM collector_test_writes WHERE rowid>? ORDER BY rowid').all(last);last=writes.at(-1)?.rowid??last;db.close();
  const answer=compact(normalize({output,documents:rows.map(r=>({id:r.id,key:r.key,name:r.name,parent:r.parent_id,revision:r.current_revision,value:JSON.parse(r.value_json)})),blobs:blobs.map(b=>({id:b.id,key:b.key,name:b.name,parent:b.parent_id,byteLength:b.bytes.length,contentHash:digest(b.bytes)})),writes:writes.map(w=>({key:w.key,id:w.id,revision:w.revision,value:JSON.parse(w.value_json)})),records:client.records,calls:client.calls,events,gates}));client.calls.length=0;events.length=0;gates.length=0;return answer;
 }
 open();try{
  if(c.mode){
   const current=await input();
   if(c.mode==='failure')patchInput({state:'failed',finishedAt:at,turnId:null,epoch:null,nativeCalls:{resume:null,turn:null,interrupt:null},error:{code:'chat_image_invalid',outcome:'not_executed',detail:'Original pre-turn image refusal.'}});
   else{
    patchInput({state:'preparing',turnId:null,epoch:null});
    const turn=await store.read('chat-bridge/native-call',current.value.nativeCalls.turn,'original-input');
    if(c.mode!=='won')await update('chat-bridge/native-call',turn,{state:'prepared',epoch:null,evidence:null,code:null});
    if(c.mode==='both'||c.mode==='resume-succeeded'){
     const origin={predecessor:null,preparedEpoch:'original-epoch'},id=deriveOperationId(current.value.operationId,'chat:native:resume:'+hashJson(origin)),{request}=await bodies(store,seed.definition,client.records[0],{method:'thread/resume'});
     let resume=await prepare('original-input',id,request,'resume',false);if(c.mode==='resume-succeeded')resume=await outcome('original-input',resume,'succeeded',false);
     patchInput({nativeCalls:{resume:resume.pin,turn:c.mode==='both'?(await input()).value.nativeCalls.turn:null,interrupt:null}});
    }
   }
  }
  if(!c.skipCancel){const current=await input();await new ChatCancellation(main).cancel('alice',{action:'cancel',operationId:`chat:${now}:worker-cancel`,inputId:'original-input',expectedRevision:current.pin.revision,expectedBridge:admission.expected('alice'),reason:'Original cancellation worker intent.'});}
  store.close();c.database=readFileSync(file).toString('base64');c.databaseHash=digest(Buffer.from(c.database,'base64'));c.records=clone(client.records);c.history=[...client.versions].map(([id,rows])=>[id,[...rows]]);c.definition=seed.definition;c.binding=seed.binding;open();client.calls.length=0;events.length=0;gates.length=0;c.outcomes.length=0;
  for(const s of c.steps){const output=await result(async()=>{switch(s.action){
   case'step':await worker.step();return;case'settle':await worker.settle();return;
   case'collect':{const value=await collector.step('original-input');return value?{pin:value.pin,value:value.value}:null;}
   case'reopen':store.close();open();return;case'control':Object.assign(controls,s.value);return;
   case'patch-input':patchInput(s.value);return;
   case'patch-call':{const current=await input();await update('chat-bridge/native-call',await store.read('chat-bridge/native-call',current.value.nativeCalls[s.slot],'original-input'),s.value);return;}
   case'patch-main':await update('chat-bridge/main',await queue.main(),s.value);return;
   case'attach-interrupt':{const current=await input(),request={nativeVersion:c.version,catalogSourceHash:seed.definition.nativePlan.catalogSourceHash,method:'turn/interrupt',params:{threadId:'original-thread',turnId:'original-turn'}},call=await prepare('original-input',deriveOperationId(current.value.operationId,'fixture-interrupt'),request);patchInput({nativeCalls:{...current.value.nativeCalls,interrupt:call.pin}});return;}
   default:throw Error('Unknown cancellation worker action '+s.action);
  }});c.expected.push(snapshot(output));}
  if(['prevent-original-turn-then-settle','prevent-both-originals-then-settle','already-succeeded-resume-can-settle','pre-turn-failure-without-intent-settles'].includes(c.label)){
   if(c.expected.some(s=>s.output.error)||(await queue.main()).value.queue.length)throw Error('Original settlement did not release '+c.label+': '+JSON.stringify(c.expected.map(s=>s.output)));
  }
  if(c.outcomes.filter(o=>o.method==='turn/interrupt').length>1)throw Error('Original interrupt was replaced '+c.label);
  if(c.label.includes('unknown')&&!(await queue.main()).value.queue.length)throw Error('Unknown original call released its ticket '+c.label);
  if(c.label==='saved-terminal-suppresses-interrupt'&&c.expected.flatMap(s=>s.events).some(e=>['prepare','advance'].includes(e.method)))throw Error('Saved terminal evidence did not suppress interrupt');
  const expectedErrors=c.label==='owner-unavailable-before-state-write'?['fixture_owner_unavailable']:c.label.startsWith('original-call-identity-')||c.label==='pre-turn-cannot-settle-interrupt'?['chat_native_mismatch']:[];
  if(JSON.stringify(c.expected.filter(s=>s.output.error).map(s=>s.output.error.code))!==JSON.stringify(expectedErrors))throw Error('Unexpected original error '+c.label+': '+JSON.stringify(c.expected.map(s=>s.output)));
 }finally{store.close();rmSync(root,{recursive:true,force:true});}
}
process.stdout.write(JSON.stringify({now,instructions,noticeInstructions,cases,boundaries:{native:'Synthetic prepare/prevent/advance outcomes and live-head transport; original evidence, cancellation, collector and SQLite execute unchanged.',shared:'Only the four outstanding native plan/compiler gates are observed for replay; no Rust qualification implied.'}}));
