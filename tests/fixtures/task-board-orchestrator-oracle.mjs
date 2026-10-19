import {existsSync,readFileSync,readdirSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {canonical,digest,hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {managementFrameBytes}=await import('../../packages/contracts/src/limits.ts');
const {validateAgent}=await import('../../packages/contracts/src/agent-validation.ts');
const {validateTaskBoard}=await import('../../packages/contracts/src/task-board-validation.ts');
const {checkedNativeContract:catalogFor}=await import('../../packages/contracts/src/checked-native-contract.ts');
const {validateNativeRead}=await import('../../packages/sdk/src/native-evidence.ts');
const {nativeTurnItems}=await import('../../packages/sdk/src/native-observations.ts');
const load=n=>readFileSync('services/task-board/src/runtime/'+n+'.ts','utf8').replace(/^import .*;\r?\n/gm,'').replaceAll('export ','');
const mutation=new Function('hashJson',stripTypeScriptTypes(load('store').match(/const mutation = [\s\S]*?;/)[0])+'\nreturn mutation;')(hashJson);
const bytes=new Function('canonical','digest','requireThat','IvyError','managementFrameBytes',stripTypeScriptTypes(load('evidence-bytes'))+'\nreturn {saveEvidenceChunks,readEvidenceChunks,evidenceMaximumBytes};')(canonical,digest,requireThat,IvyError,managementFrameBytes);
const Reads=new Function('canonical','digest','hashJson','requireThat','validateNativeRead','validateTaskBoard','catalogFor','evidenceMaximumBytes','saveEvidenceChunks','readEvidenceChunks','mutation',stripTypeScriptTypes(load('read-evidence'),{mode:'transform'})+'\nreturn TaskBoardReadEvidence;')(canonical,digest,hashJson,requireThat,validateNativeRead,validateTaskBoard,catalogFor,bytes.evidenceMaximumBytes,bytes.saveEvidenceChunks,bytes.readEvidenceChunks,mutation);
const same=(a,b)=>a===undefined||b===undefined?a===b:canonical(a)===canonical(b);
const nativeRecord=v=>v!==null&&typeof v==='object'&&!Array.isArray(v)?v:{},nativeArray=v=>Array.isArray(v)?v:[],nativeString=v=>typeof v==='string'?v:'';
const Turns=new Function('canonical','hashJson','requireThat','validateAgent','validateTaskBoard','nativeTurnItems','catalogFor','same','nativeArray','nativeRecord','nativeString','TaskBoardReadEvidence','evidenceMaximumBytes','mutation',stripTypeScriptTypes(load('turn-evidence'),{mode:'transform'})+'\nreturn TaskBoardTurnEvidence;')(canonical,hashJson,requireThat,validateAgent,validateTaskBoard,nativeTurnItems,catalogFor,same,nativeArray,nativeRecord,nativeString,Reads,bytes.evidenceMaximumBytes,mutation);
const {TaskBoardFullTurnEvidence:Full,fullTurnParams,firstItemsParams}=new Function('canonical','requireThat','validateTaskBoard','same','evidenceMaximumBytes','nativeArray','nativeRecord','nativeString','TaskBoardTurnEvidence',stripTypeScriptTypes(load('full-turn-evidence'),{mode:'transform'})+'\nreturn {TaskBoardFullTurnEvidence,fullTurnParams,firstItemsParams};')(canonical,requireThat,validateTaskBoard,same,bytes.evidenceMaximumBytes,nativeArray,nativeRecord,nativeString,Turns);
const nativeSource=load('native-publication'),projectionSource=nativeSource.slice(nativeSource.indexOf('const finishedRun'),nativeSource.indexOf('// The ticket keeps'));
const {finishedRun,projectNativeState}=new Function('requireThat','nativeRecord','nativeArray',stripTypeScriptTypes(projectionSource)+'\nreturn {finishedRun,projectNativeState};')(requireThat,nativeRecord,nativeArray);
const condition=load('condition'),claimedWorkflowState=new Function(stripTypeScriptTypes(condition.slice(condition.indexOf('function claimedWorkflowState'),condition.indexOf('/** One derived')))+'\nreturn claimedWorkflowState;')();
const runHandoff=new Function(stripTypeScriptTypes(load('conversation'))+'\nreturn runHandoff;')();
const source=load('turn-evidence'),helpers=source.slice(source.indexOf('const pageCursor'),source.indexOf('/** Verifies read artifacts'));
const {finalOutputText,outputParams,pageCursor}=new Function('requireThat','nativeString','nativeRecord',stripTypeScriptTypes(helpers)+'\nreturn {finalOutputText,outputParams,pageCursor};')(requireThat,nativeString,nativeRecord);
class Driver {constructor(e){return e.driver;}}class Inspector{constructor(e){return e.inspector;}}class Evidence{constructor(s){return s.evidence;}}
const id=n=>'00000000-0000-4000-8000-'+String(n).padStart(12,'0'),pin=(n,r=2)=>({objectId:id(n),revision:r}),time='2026-01-02T03:04:05.006Z';
const artifact={object:pin(400,1),label:'Snapshot',contentHash:'sha256:'+'a'.repeat(64),mediaType:'application/json'},output={...artifact,object:pin(401,1),label:'Output'};
const baseRun={phase:'running',taskId:id(100),target:{serviceNodeId:'native-agent'},turnId:'turn',nativeEpoch:'epoch',primaryResourceRef:{namespace:'codex',kind:'thread',serviceNodeId:'native-agent',nativeId:'thread'},calls:{thread:null,turn:null,steer:null,interrupt:null},cancellation:null,notificationCursor:7,externalOutcome:{state:'active',observedAt:time,evidence:null,code:null},originalRequest:{expectedRevision:1,commentIds:[]}};
const baseTask={workflowState:'in_progress',claim:{run:pin(200),phase:'running'},publication:null,waiting:null,workRevision:1,comments:[],commentDeliveries:[]};
const note=(thread='thread',node='native-agent',method='turn/completed')=>({params:{serviceNodeId:node,payload:{method,params:{threadId:thread}}}});
const fault=(where,code='invalid_arguments',message='Fixture unavailable.')=>({where,code,message});
const rows=[],add=(name,input={},actions=[['step']])=>rows.push({name,input,actions});
for(const phase of ['completed','failed','cancelled'])add('finished '+phase,{run:{phase}});
add('owner failure outside reconciliation catch',{fault:fault('owner','service_unavailable')});
add('publication guard',{task:{publication:{pending:true}}});add('claimed revision guard',{task:{claim:{run:pin(200,1),phase:'running'}}});
add('prepare original thread',{run:{turnId:null,phase:'starting'}});add('prepare turn only after thread receipt',{run:{turnId:null,phase:'starting'},calls:{thread:{ownerPhase:'succeeded'}}});
add('resume recovery wins',{run:{turnId:null},calls:{thread:{ownerPhase:'outcome_unknown'}},recovered:true});
add('pending original thread advances',{run:{turnId:null},calls:{thread:{ownerPhase:'accepted'}}});
add('new saved conclusive thread observation advances',{run:{turnId:null},calls:{thread:{ownerPhase:'succeeded',revision:3}}});
add('reattach only original active turn',{run:{turnId:null},calls:{thread:{ownerPhase:'succeeded'},turn:{ownerPhase:'accepted'}},fault:fault('advance:turn','task_board_reattach_required')});
for(const [name,input]of Object.entries({inactive:{task:{claim:null}},cancelled:{run:{cancellation:{reason:'cancel'}}},threadless:{calls:{thread:null}},differentError:{fault:fault('advance:turn','invalid_arguments')}}))add('reattach refuses '+name,{run:{turnId:null,...input.run},calls:{thread:{ownerPhase:'succeeded'},turn:{ownerPhase:'accepted'},...input.calls},fault:fault('advance:turn','task_board_reattach_required'),...input});
add('cancel unstarted',{run:{turnId:null,cancellation:{reason:'cancel'}}});add('retire unstarted',{run:{turnId:null},task:{claim:null}});
add('unstarted conclusive turn is not closed twice',{run:{turnId:null,cancellation:{reason:'cancel'}},calls:{turn:{ownerPhase:'failed'}}});
add('steering is reconciled before inspection',{calls:{steer:{ownerPhase:'outcome_unknown'}},advanceResult:null});
add('inspection pending',{inspection:'pending'});add('missing original turn becomes unavailable',{inspection:'absent'});
add('native unsuccessful read becomes unavailable',{metadataError:true});
add('lost original process',{epoch:'replacement',stopped:true});add('unconfirmed original owner projects unknown',{epoch:'replacement'});
add('prepare cancellation interrupt',{run:{cancellation:{reason:'cancel'}}});
add('pending interrupt advances',{run:{cancellation:{reason:'cancel'}},calls:{interrupt:{ownerPhase:'accepted'}}});
add('false interrupt result continues observation',{run:{cancellation:{reason:'cancel'}},calls:{interrupt:{ownerPhase:'accepted'}},advanceResult:false});
add('conclusive interrupt still needs terminal read',{run:{cancellation:{reason:'cancel'}},calls:{interrupt:{ownerPhase:'succeeded'}}});
add('unchanged active poll writes no evidence');add('unchanged nonclaim poll ignores task waiting',{task:{claim:null,waiting:{reason:'user',detail:'Keep this.',since:time}}});
add('work revision prepares steer',{task:{workRevision:2}});add('peer state prepares steer',{peersHash:hashJson(['changed'])});
add('queued comment prepares steer',{task:{commentDeliveries:[{commentId:'new',state:'queued'}]}});
add('initial queued comment is not resent',{run:{originalRequest:{expectedRevision:1,commentIds:['old']}},task:{commentDeliveries:[{commentId:'old',state:'queued'}]}});
add('previous steer baseline is retained',{calls:{steer:{ownerPhase:'succeeded',origin:{kind:'steer',workRevision:1,peersHash:hashJson([]),commentIds:['old']}}},task:{commentDeliveries:[{commentId:'old',state:'queued'}]}});
add('new steering keeps predecessor and original queue',{calls:{steer:{ownerPhase:'succeeded',origin:{kind:'steer',workRevision:1,peersHash:hashJson([]),commentIds:['old']}}},task:{commentDeliveries:[{commentId:'old',state:'queued'},{commentId:'new',state:'queued'},{commentId:'done',state:'delivered'}]}});
add('native user input projects waiting',{thread:{status:{type:'active',activeFlags:['waitingOnUserInput']}}});
add('idle thread projects unknown',{thread:{status:{type:'idle'}}});
add('first terminal observation saves snapshot',{status:'completed'});
add('different retained epoch gets a fresh snapshot',{status:'completed',prior:true,retainedEpoch:'old'});
add('different retained status gets a fresh snapshot',{status:'completed',prior:true,retainedStatus:'failed'});
add('another artifact type is not a snapshot',{status:'completed',prior:true,localKey:'task-board/native-evidence'});
add('confirmed terminal handoff skips history',{status:'completed',prior:true,handoff:true});
add('late native answer handoff uses fresh task pin',{status:'completed',prior:true,syncHandoff:true});
add('inactive completion does not mirror native inputs',{status:'completed',prior:true,task:{claim:null}});
add('failed completion does not mirror native inputs',{status:'failed',prior:true});
add('interrupted completion',{status:'interrupted',prior:true});
add('unsupported output uses confirmed status',{status:'completed',prior:true,outputs:[{error:-32601}]});
add('failed output read becomes unavailable',{status:'completed',prior:true,outputs:[{error:-32000}]});
add('latest final assistant output',{status:'completed',prior:true});
add('find final output after trailing item',{status:'completed',prior:true,outputs:[{item:{id:'reason',type:'reasoning'},nextCursor:'next'},{item:{id:'answer',type:'agentMessage',phase:'analysis',text:'answer'}}]});
add('commentary is not a final output',{status:'completed',prior:true,outputs:[{item:{id:'progress',type:'agentMessage',phase:'commentary',text:'progress'}}]});
add('output search stops at three pages',{status:'completed',prior:true,outputs:Array.from({length:4},(_,i)=>({item:{id:'r'+i,type:'reasoning'},nextCursor:'cursor'+i}))});
add('output cursor loop is fatal',{status:'completed',prior:true,outputs:[{item:{id:'r1',type:'reasoning'},nextCursor:'same'},{item:{id:'r2',type:'reasoning'},nextCursor:'same'}]});
add('unsupported cursor representation is fatal',{status:'completed',prior:true,outputs:[{item:{id:'r',type:'reasoning'},nextCursor:7}]});
add('output one-item limit is fatal',{status:'completed',prior:true,outputs:[{many:true}]});
for(const code of ['not_found','service_not_ready','service_unavailable','native_unavailable','task_board_native_unavailable','task_board_native_outcome_unavailable','task_board_turn_unavailable','task_board_native_read_failed','task_board_native_epoch_changed','task_board_native_search_limit','task_board_native_collection_limit','invalid_arguments','revision_conflict'])add('original error '+code,{fault:fault('find',code)});
add('error detail clips UTF16 units',{fault:fault('find','service_unavailable','a'.repeat(2047)+'😄tail')});
add('terminal nonclaim outcome is never overwritten',{task:{claim:null},run:{externalOutcome:{state:'succeeded',code:null,evidence:null,observedAt:time}},fault:fault('find','service_unavailable')});
add('unchanged recovering claim is ephemeral',{run:{externalOutcome:{state:'unknown',code:'service_unavailable',evidence:null,observedAt:time}},task:{claim:{run:pin(200),phase:'outcome_unknown'}},fault:fault('find','service_unavailable')});
add('unchanged nonclaim recovery is ephemeral',{run:{externalOutcome:{state:'unknown',code:'service_unavailable',evidence:null,observedAt:time}},task:{claim:null},fault:fault('find','service_unavailable')});
add('unchanged retained waiting is ephemeral',{task:{waiting:{reason:'user',detail:'Native reconciliation is waiting: service_unavailable. Fixture unavailable.',since:time},workflowState:'waiting'},fault:fault('find','service_unavailable')});
add('completion failure is not treated as success',{status:'completed',prior:true,fault:fault('update','revision_conflict')});
add('drain immediate progress bound',{run:{turnId:null,phase:'starting'},progress:true},[['drain']]);
add('drain finished clears advisory tracking',{run:{turnId:null,phase:'starting'},progress:true,finishAt:2},[['drain'],['notification',note()]]);
add('drain transient host wait postpones',{run:{turnId:null,phase:'starting'},task:{waiting:{reason:'host',detail:'Reconnecting.',since:time}}},[['drain'],['drain'],['clock',30000],['drain']]);
add('retry growth and cap',{fault:fault('owner')},[['drain'],['drain'],['clock',29999],['drain'],['clock',30000],['drain'],['clock',89999],['drain'],['clock',90000],['drain'],['clock',210000],['drain'],['clock',450000],['drain'],['clock',750000],['drain'],['clock',1049999],['drain'],['clock',1050000],['drain']]);
add('notifications are scoped advisory retry hints',{},[['drain'],['fault',fault('owner')],['drain'],['notification',note('other')],['notification',note('thread','other')],['notification',note('thread','native-agent','ignored')],['drain'],['notification',note()],['drain']]);
for(const n of [1,2,4,5])add('drain read failure boundary '+n,{run:{turnId:null,phase:'starting'},readFaultAt:n},[['drain'],['drain']]);
add('bounded maps preserve insertion and refresh eviction',{run:{turnId:null,phase:'outcome_unknown'}},[['many',1024],['notification',note('thread-'+id(1224))],['drain',1224],['notification',note('thread-'+id(201))],['drain',1225],['drain',201]]);

const catalog=catalogFor('0.159.2');
const projection=[];
for(const status of ['inProgress','completed','failed','interrupted'])for(const input of [null,{reason:'cancel'}])projection.push({run:{...baseRun,cancellation:input},status,epoch:'epoch',thread:{status:{type:'active',activeFlags:[]}}});
for(const thread of [{},{status:{type:'idle'}},{status:{type:'active',activeFlags:['waitingOnApproval']}},{status:{type:'active',activeFlags:[['waitingOnUserInput']]}},{status:{type:'active',activeFlags:[null,{},17]}}])projection.push({run:baseRun,status:'inProgress',epoch:'epoch',thread});
projection.push({run:baseRun,status:'inProgress',epoch:'replacement',thread:{status:{type:'active'}}});
for(const state of ['succeeded','failed','cancelled'])for(const status of ['inProgress','completed','failed','interrupted'])projection.push({run:{...baseRun,cancellation:{reason:'cancel'},externalOutcome:{...baseRun.externalOutcome,state,code:state==='succeeded'?null:state==='failed'?'native_failed':'native_interrupted'}},status,epoch:'epoch',thread:{status:{type:'active'}}});
projection.push({run:{...baseRun,cancellation:{reason:'cancel'}},status:'inProgress',epoch:'epoch',thread:{status:{type:'active',activeFlags:[{toString:1}]}}});
for(const row of projection){try{row.expected={ok:projectNativeState(row.run,row.status,row.epoch,row.thread,time,artifact)};}catch(e){row.expected={error:{code:e.code,message:e.message,outcome:e.outcome}};}row.finished=finishedRun(row.run);}

for(const row of rows){
 const i=row.input,trace=[],runDocs=new Map(),taskDocs=new Map(),versions=new Map();let now=0,reads=0,progress=0,readOutput=0,activeFault=i.fault??null;
 const error=f=>{throw new IvyError(f.code,f.message);};
 const check=where=>{if(activeFault?.where===where)error(activeFault);};
 const runDoc=(objectId=id(200))=>{if(!runDocs.has(objectId)){const n=Number(objectId.slice(-12)),value={...structuredClone(baseRun),...structuredClone(i.run??{})};if(n!==200){value.taskId=id(10000+n);value.primaryResourceRef.nativeId='thread-'+objectId;}if(i.prior)value.externalOutcome.evidence=artifact;value.plan={nativeVersion:catalog.catalog.version,catalogSourceHash:catalog.catalog.sourceHash};const d={pin:{objectId,revision:2},value};runDocs.set(objectId,d);}return runDocs.get(objectId);};
 const taskDoc=(objectId=id(100))=>{if(!taskDocs.has(objectId)){const n=Number(objectId.slice(-12)),r=runDoc(n===100?id(200):id(n-10000)),initial={...structuredClone(baseTask),claim:{run:r.pin,phase:'running'}};const d={pin:{objectId,revision:2},value:{...initial,...structuredClone(i.task??{})}};if(i.handoff)d.value.comments=[{purpose:'handoff',run:r.pin}];taskDocs.set(objectId,d);versions.set(objectId,{pin:{objectId,revision:1},value:{...initial,workRevision:1}});}return taskDocs.get(objectId);};
 const run=runDoc();const task=taskDoc();
 const calls={};for(const [slot,c]of Object.entries(i.calls??{})){if(!c)continue;const n={thread:300,turn:301,steer:302,interrupt:303}[slot];run.value.calls[slot]=pin(n,2);calls[id(n)]={pin:pin(n,c.revision??2),value:{ownerPhase:c.ownerPhase,origin:c.origin??null}};}
 const copy=d=>({pin:structuredClone(d.pin),value:structuredClone(d.value),revision:{contentHash:hashJson(d.value)}});
 const effect=r=>{if(i.progress){progress++;r.pin={...r.pin,revision:r.pin.revision+1};r.value.phase=progress===i.finishAt?'completed':'starting';const t=taskDoc(r.value.taskId);t.pin={...t.pin,revision:t.pin.revision+1};t.value.claim={...t.value.claim,run:r.pin};}};
 const observed=(method,params,reply)=>({schemaVersion:1,observationId:'read',callerPrincipalId:'caller',serviceNodeId:'native-agent',nativeVersion:catalog.catalog.version,nativeExecutableHash:catalog.catalog.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch:i.epoch??'epoch',requestId:1,observedAt:time,method,params,requestHash:hashJson({method,params}),reply});
 const metadata=observed('thread/read',{threadId:'thread',includeTurns:false},i.metadataError?{error:{code:-32000,message:'failed'}}:{result:{thread:{id:'thread',ephemeral:false,status:{type:'active',activeFlags:[]},...i.thread}}});
 const turns=observed('thread/turns/list',{threadId:'thread',cursor:null,limit:100,itemsView:'notLoaded',sortDirection:'desc'},{result:{data:[{id:'turn',status:i.status??'inProgress'}],nextCursor:null}});
 const context={run:pin(200),nativeVersion:catalog.catalog.version,catalogSourceHash:catalog.catalog.sourceHash,serviceNodeId:'native-agent',epoch:i.epoch??'epoch',threadId:'thread',turnId:'turn'};
 const store={async read(key,address,primary=true){trace.push(['read',key,structuredClone(address),primary]);if(++reads===i.readFaultAt)error(fault('read','service_unavailable'));const objectId=typeof address==='string'?address:address.objectId;
   return copy(key==='task-board/run'?runDoc(objectId):key==='task-board/task'?((taskDoc(objectId),typeof address!=='string'&&address.revision===1)?versions.get(objectId):taskDoc(objectId)):calls[objectId]);},
   localKey(objectId){trace.push(['local-key',objectId]);return i.localKey??'task-board/native-turn-snapshot';},async withReadCache(work){trace.push(['cache-enter']);try{return await work();}finally{trace.push(['cache-exit']);}}};
 store.evidence={async snapshot(r,a){trace.push(['snapshot',r,a]);check('snapshot');return{document:{value:{status:i.retainedStatus??i.status??'completed',epoch:i.retainedEpoch??i.epoch??'epoch'}}};},
  async saveSnapshot(r,m,t){trace.push(['save-snapshot',r,m,t]);check('save-snapshot');return artifact;},async saveOutput(r,s,c,o){trace.push(['save-output',r,s,c,o]);check('save-output');return output;}};
 const engine={store,settings:{principalId:'caller'},async verifyOwner(){trace.push(['owner']);check('owner');},async nativeUpdate(r){trace.push(['update',r]);check('update');return null;}};
 engine.driver={async prepare(r,s,o){trace.push(['prepare',r,s,o??null]);check('prepare:'+s);effect(runDoc(r));return null;},async advance(r,s){trace.push(['advance',r,s]);check('advance:'+s);effect(runDoc(r));return i.advanceResult===undefined?{advanced:true}:i.advanceResult;},async recoverUncertainResume(r){trace.push(['recover',r]);check('recover');return i.recovered??false;},async status(r,c){trace.push(['status',r,c]);check('status');return{epoch:'replacement'};}};
 engine.inspector={async find(r){trace.push(['find',r]);check('find');return i.inspection==='pending'?{kind:'pending',search:pin(900,1)}:i.inspection==='absent'?{kind:'absent'}:{kind:'found',metadata,turns};},async ownerStopped(r){trace.push(['stopped',r]);check('stopped');return i.stopped??false;},async context(r){trace.push(['context',r]);check('context');return context;},async read(c,m,p,a){trace.push(['output-read',c,m,p,a]);check('output-read');const v=(i.outputs??[{item:{id:'a',type:'agentMessage',phase:'final_answer',text:'answer'}}])[readOutput++]??{};let reply;
   if(v.error)reply={error:{code:v.error,message:'unavailable'}};else{const raw=v.item??{id:'a',type:'agentMessage',phase:'final_answer',text:'answer'},data=v.many?[raw,{...raw,id:'b'}]:v.item===null?[]:[raw];reply={result:{data:data.map(item=>({turnId:'turn',item})),nextCursor:v.nextCursor??null}};}return observed(m,p,reply);}};
 let ClockDate=class extends Date{static now(){return now;}};
 const Orchestrator=new Function('hashJson','IvyError','requireThat','nativeTurnItems','same','TaskBoardNativeDriver','TaskBoardNativeInspector','catalogFor','runHandoff','finalOutputText','outputParams','pageCursor','TaskBoardTurnEvidence','nativeArray','nativeRecord','nativeString','finishedRun','projectNativeState','claimedWorkflowState','mutation','syncNativeComments','peerState','Date',stripTypeScriptTypes(load('native-orchestrator'),{mode:'transform'})+'\nreturn TaskBoardNativeOrchestrator;')
 (hashJson,IvyError,requireThat,nativeTurnItems,same,Driver,Inspector,catalogFor,runHandoff,finalOutputText,outputParams,pageCursor,Evidence,nativeArray,nativeRecord,nativeString,finishedRun,projectNativeState,claimedWorkflowState,mutation,
  async(e,r)=>{trace.push(['sync',r]);check('sync');if(i.syncHandoff){const t=taskDoc(runDoc(r).value.taskId);t.pin={...t.pin,revision:t.pin.revision+1};t.value.comments.push({purpose:'handoff',run:runDoc(r).pin});}},
  async(s,t,id)=>{trace.push(['peers',t,id]);check('peers');return i.peersHash??hashJson([]);},ClockDate);
 const subject=new Orchestrator(engine),results=[];
 for(const action of row.actions){try{let value=null;const r=id(action[1]??200);
   if(action[0]==='step')await subject.step(r);else if(action[0]==='drain')value=await subject.drain(r);else if(action[0]==='notification')value=subject.notification(action[1]);else if(action[0]==='clock')now=action[1];else if(action[0]==='fault')activeFault=action[1];else if(action[0]==='many'){for(let n=201;n<201+action[1];n++)await subject.drain(id(n));}else throw Error('Unknown action');results.push({ok:value});
  }catch(e){if(!(e instanceof IvyError))throw new Error(row.name,{cause:e});results.push({error:{code:e.code,message:e.message,outcome:e.outcome}});}}
 row.run=runDocs.get(id(200));row.task=taskDocs.get(id(100));row.initial=versions.get(id(100));row.calls=calls;row.context=context;row.metadata=metadata;row.turns=turns;row.expectedHash=hashJson({results,trace});
 // Preserve initial fixtures; runtime effects are reconstructed from input below.
 row.run=copy({pin:pin(200),value:{...structuredClone(baseRun),...structuredClone(i.run??{}),plan:{nativeVersion:catalog.catalog.version,catalogSourceHash:catalog.catalog.sourceHash}}});
 if(i.prior)row.run.value.externalOutcome.evidence=artifact;row.run.value.calls={...baseRun.calls};for(const[slot,c]of Object.entries(i.calls??{}))if(c)row.run.value.calls[slot]=pin({thread:300,turn:301,steer:302,interrupt:303}[slot],2);
 row.task=copy({pin:pin(100),value:{...structuredClone(baseTask),...structuredClone(i.task??{})}});if(i.handoff)row.task.value.comments=[{purpose:'handoff',run:pin(200)}];row.initial=copy({pin:pin(100,1),value:{...structuredClone(baseTask),workRevision:1}});
}
process.stdout.write(JSON.stringify({time,artifact,output,catalog:{version:catalog.catalog.version,sourceHash:catalog.catalog.sourceHash,catalogHash:catalog.catalogHash},projection,rows}));
