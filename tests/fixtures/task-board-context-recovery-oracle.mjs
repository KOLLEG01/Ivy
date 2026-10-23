import {existsSync, readFileSync} from 'node:fs';
import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s,c,n) {
  if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL) {
    const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);
  }
  return n(s,c);
}});
const {canonical,digest,hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {managementFrameBytes}=await import('../../packages/contracts/src/limits.ts');
const {validateNativeOperation}=await import('../../packages/sdk/src/native-evidence.ts');
const {checkedNativeContract}=await import('../../packages/contracts/src/checked-native-contract.ts');
const {validateTaskBoard}=await import('../../packages/contracts/src/task-board-validation.ts');
const {validateNativeInvocation}=await import('../../packages/sdk/src/native-plan.ts');
const {readNativeParameters,saveNativeParameters}=await import('../../packages/sdk/src/native-parameters.ts');
const load=name=>readFileSync(new URL('../../services/task-board/src/runtime/'+name+'.ts',import.meta.url),'utf8')
  .replace(/^import .*;\r?\n/gm,'').replace(/^export \{.*\} from .*;\r?\n/gm,'').replaceAll('export ','');
const mutationSource=readFileSync(new URL('../../services/task-board/src/runtime/store.ts',import.meta.url),'utf8')
  .match(/export const mutation = [\s\S]*?;/)[0].replace('export ','');
const mutation=new Function('hashJson',stripTypeScriptTypes(mutationSource)+'\nreturn mutation;')(hashJson);
const bytes=new Function('canonical','digest','requireThat','IvyError','managementFrameBytes',stripTypeScriptTypes(load('evidence-bytes'))+'\nreturn {saveEvidenceChunks,readEvidenceChunks,evidenceMaximumBytes,evidenceChunkBytes};')(canonical,digest,requireThat,IvyError,managementFrameBytes);
const intentSource=load('native-intent');
const {resolveNativeRequest,retainNativeRequest,evidenceIdentity}=new Function('checkedNativeContract','validateNativeInvocation','readNativeParameters','saveNativeParameters','mutation','validateTaskBoard',stripTypeScriptTypes(intentSource.slice(intentSource.indexOf('async function retainNativeRequest')))+'\nreturn {resolveNativeRequest,retainNativeRequest,evidenceIdentity};')(checkedNativeContract,validateNativeInvocation,readNativeParameters,saveNativeParameters,mutation,validateTaskBoard);
const Evidence=new Function('canonical','digest','hashJson','requireThat','validateNativeOperation','validateTaskBoard','checkedNativeContract','resolveNativeRequest',...Object.keys(bytes),'mutation',stripTypeScriptTypes(load('evidence'),{mode:'transform'})+'\nreturn TaskBoardEvidence;')(canonical,digest,hashJson,requireThat,validateNativeOperation,validateTaskBoard,checkedNativeContract,resolveNativeRequest,...Object.values(bytes),mutation);
// Keep the finite legacy receipt/lineage comparison, with the current Result
// semantics: recovery does not depend on the retired Result.kind field. The
// actual current question resolver and producer qualification are checked separately.
const same=(a,b)=>a===undefined||b===undefined?a===b:canonical(a)===canonical(b);
const makeRecovery=guard=>new Function('IvyError','requireThat','same','unansweredQuestion','TaskBoardEvidence','evidenceIdentity','resolveNativeRequest',stripTypeScriptTypes(load('native-recovery').replace(" && result.value.kind === 'final'",''))+'\nreturn {archived:unstartedArchivedContext,missing:unstartedMissingContext};')(IvyError,requireThat,same,guard,Evidence,evidenceIdentity,resolveNativeRequest);
const id=n=>'00000000-0000-4000-8000-'+String(n).padStart(12,'0'),pin=(n,revision=1)=>({objectId:id(n),revision});
const primary={namespace:'codex',kind:'thread',serviceNodeId:'native-agent',nativeId:'thread'};
const task={pin:pin(100,4),value:{primaryResourceRef:primary,lastRun:pin(200,2),lastHistory:null,comments:[]}};
const time='2026-01-02T03:04:05.006Z',catalog=checkedNativeContract('0.159.2');
const records=[],blobs=[];
const doc=(key,n,revision,value,parentId='root')=>({key,pin:pin(n,revision),value,parentId});
const stub={rootObjectId:'root',client:{request(){throw Error('Unexpected native or Hive RPC');}},
  async read(key,address){const matches=records.filter(d=>d.key===key&&d.pin.objectId===(typeof address==='string'?address:address.objectId));const found=typeof address==='string'?matches.at(-1):matches.find(d=>d.pin.revision===address.revision);if(!found)throw new IvyError('not_found','Fixture unavailable.');return {...found,metadata:{parentId:found.parentId},revision:{contentHash:hashJson(found.value)}};},
  saveLocalBlob(key,name,parent,body){const object=pin(500+blobs.length);blobs.push({key,name,parent,object,body:body.toString('hex')});return object;},
  localBlob(key,object,parent){const b=blobs.find(b=>same(b.object,object)&&b.key===key&&b.parent===parent);if(!b)throw new IvyError('task_board_evidence_mismatch','The local evidence blob is unavailable.');return Buffer.from(b.body,'hex');},
  async write(key,value,mutation,destination){const d=doc(key,600+records.filter(d=>d.key==='task-board/native-evidence').length,1,value,destination.create.parentId);records.push(d);return d.pin;}
};
const makeCall=async(n,method,params,code='native_failed',message='thread is archived')=>{
  const request=await retainNativeRequest(stub,{nativeVersion:'0.159.2',catalogSourceHash:catalog.catalog.sourceHash,method,params},'operation-'+n);
  const call={operationId:'operation-'+n,request,expectedDefinitionHash:hashJson(catalog.definitions.get(method)),ownerPhase:'failed',state:'observed',code,epoch:code==='request_prevented'?null:'epoch',evidence:null};
  records.push(doc('task-board/native-call',n,1,{...call,state:'prepared',ownerPhase:null}));
  const current=doc('task-board/native-call',n,2,call);records.push(current);
  const observation={schemaVersion:1,operationId:call.operationId,callerPrincipalId:'caller',serviceNodeId:'native-agent',nativeVersion:'0.159.2',nativeExecutableHash:catalog.catalog.nativeExecutableHash,method,params,requestHash:hashJson({method,params}),phase:'failed',createdAt:time,updatedAt:time,code,epoch:call.epoch,requestId:code==='request_prevented'?null:42,reply:code==='request_prevented'?null:{error:{code:-32600,message}}};
  call.evidence=await new Evidence(stub).save(current.pin,observation,await evidenceIdentity(stub,{target:{serviceNodeId:'native-agent'}},call,'caller'));
};
await makeCall(300,'thread/resume',{threadId:'thread',excludeTurns:true});
await makeCall(301,'turn/start',{threadId:'thread',input:[]},'request_prevented');
const run={taskId:id(100),primaryResourceRef:primary,phase:'failed',externalOutcome:{state:'failed'},turnId:null,cancellation:null,result:pin(400),calls:{thread:pin(300),turn:null,steer:null,interrupt:null},target:{serviceNodeId:'native-agent'},plan:{nativeVersion:'0.159.2',catalogSourceHash:catalog.catalog.sourceHash},originalRequest:{expectedRevision:1}};
records.push(doc('task-board/run',200,1,{...run,phase:'running',result:null}),doc('task-board/run',200,2,run));
records.push(doc('task-board/result',400,1,{taskId:id(100),kind:'final',run:pin(200)}));
records.push(doc('task-board/task',100,1,{...task.value,lastRun:null}),doc('task-board/task',100,2,{...task.value,lastRun:pin(201,2)}));
records.push(doc('task-board/run',201,1,{...run,phase:'running',result:null}),doc('task-board/run',201,2,{...run,result:pin(401)}));
records.push(doc('task-board/result',401,1,{taskId:id(100),kind:'final',run:pin(201)}));
records.push(doc('task-board/history',700,1,{taskId:id(100),kind:'recovered',priorRun:pin(200),actor:{source:'scheduler'},previousHistory:null}));
const base={task,records,blobs};
const rows=[],change=(n,revision,patch)=>({objectId:id(n),revision,patch});
const add=(name,options={})=>rows.push({name,mode:'archived',...options});
const both=(name,options={})=>{add('archived: '+name,options);add('missing: '+name,{...options,mode:'missing',message:'thread not found',...options});};
both('exact terminal proof');both('prevented turn is safe',{changes:[change(200,1,{calls:{...run.calls,turn:pin(301)}})]});
for(const value of [null,{}, {...primary,namespace:'other'},{...primary,kind:'other'}])both('primary '+JSON.stringify(value),{task:{primaryResourceRef:value}});
both('no last Run',{task:{lastRun:null}});
both('question blocks before reads',{guard:'blocked',fail:{key:'task-board/run',code:'fixture_failed'}});
both('question error precedes read catch',{guard:'failed'});both('question not_found is outside recovery catch',{guard:'not_found'});
for(const patch of [{taskId:'foreign'},{primaryResourceRef:{...primary,nativeId:'other'}},{phase:'running'},{phase:'succeeded'},{externalOutcome:{state:'unknown'}},{turnId:'turn'},{cancellation:{}},{result:null}])both('terminal '+JSON.stringify(patch),{changes:[change(200,2,patch)]});
for(const patch of [{taskId:'foreign'},{kind:'intermediate'},{run:pin(201)},{run:pin(200,2)},{run:pin(200,3)},{run:null}])both('Result '+JSON.stringify(patch),{changes:[change(400,1,patch)]});
for(const patch of [{turnId:'turn'},{calls:{...run.calls,steer:pin(301)}},{calls:{...run.calls,interrupt:pin(301)}},{calls:{...run.calls,thread:null}}])both('saved '+JSON.stringify(patch),{changes:[change(200,1,patch)]});
for(const patch of [{evidence:null},{ownerPhase:'running'},{state:'prepared'},{code:'other'},{epoch:'other'},{expectedDefinitionHash:'sha256:'+'0'.repeat(64)}])both('current call '+JSON.stringify(patch),{changes:[change(300,2,patch)]});
for(const patch of [{requestId:42},{epoch:'epoch'},{reply:{result:{}}},{code:'native_failed'},{phase:'accepted'},{method:'turn/interrupt',params:{threadId:'thread',turnId:'turn'}}])both('turn receipt '+JSON.stringify(patch),{changes:[change(200,1,{calls:{...run.calls,turn:pin(301)}})],turnObservation:patch});
both('actual prevented thread is not evidence of archive or loss',{threadObservation:{code:'request_prevented',epoch:null,requestId:null,reply:null},changes:[change(300,2,{code:'request_prevented',epoch:null})]});
for(const patch of [{callerPrincipalId:'other'},{serviceNodeId:'other'},{operationId:'other'},{nativeExecutableHash:'sha256:'+'0'.repeat(64)}])both('receipt identity '+JSON.stringify(patch),{threadObservation:patch});
both('valid receipt for other context',{requestThread:'other'});
both('valid original receipt for other method',{requestMethod:'turn/interrupt'});
both('source mismatch',{requestSource:'sha256:'+'0'.repeat(64)});
both('saved prepared pin must read current call',{changes:[change(300,1,{evidence:null,ownerPhase:'running',state:'prepared'})]});
for(const message of ['thread IS ARCHIVED','thread is archived.','thread (is archived)','thread \nis archived','thread this is archived','thread is archivedx','thread thisis archived','thread _is archived','thread is archived_','other is archived','thread is\narchived','thread is  archived','thread İS ARCHIVED','thread ıS ARCHIVED','thread iſ archived','thread is archived\ud800','\ud800thread is archived','thread is archived\u212a'])add('archive message '+JSON.stringify(message),{message});
for(const message of ['thread not found','not found thread','no rollout found thread','thread does not exist','THREAD NOT FOUND thread','thread\nnot found','thread\rnot found','thread\u2028not found','thread\u2029not found','other not found','thread is archived','thread rollout does not exist','thread no rollout found','thread NO ROLLOUT FOUND','thread not found\ud800'])add('missing message '+JSON.stringify(message),{mode:'missing',message});
add('empty native ID follows includes empty',{task:{primaryResourceRef:{...primary,nativeId:''}},changes:[change(200,1,{primaryResourceRef:{...primary,nativeId:''}}),change(200,2,{primaryResourceRef:{...primary,nativeId:''}})],requestThread:'',message:'is archived'});
for(const target of [pin(200,2),pin(400),pin(200),pin(300,2),pin(600)])for(const code of ['not_found','fixture_failed'])both('read failure '+JSON.stringify(target)+' '+code,{fail:{address:target,code}});
both('unreadable current call',{fail:{key:'task-board/native-call',address:id(300),code:'not_found'}});
both('unavailable blob',{fail:{key:'blob',code:'fixture_failed'}});
add('restoration History forbids missing replacement',{mode:'missing',message:'thread not found',task:{lastHistory:pin(700)}});
add('user History does not forbid proof',{mode:'missing',message:'thread not found',task:{lastHistory:pin(700)},changes:[change(700,1,{actor:{source:'user'}})]});
add('foreign History refuses',{mode:'missing',task:{lastHistory:pin(700)},changes:[change(700,1,{taskId:'other'})]});
const lineage=[change(200,2,{originalRequest:{expectedRevision:2}})];
add('same context previous unstarted Run',{mode:'missing',message:'thread not found',changes:lineage});
add('previous used Run forbids replacement',{mode:'missing',message:'thread not found',changes:[...lineage,change(201,1,{turnId:'turn'})]});
add('previous uncertain Run forbids replacement',{mode:'missing',message:'thread not found',changes:[...lineage,change(201,2,{externalOutcome:{state:'unknown'}})]});
add('previous foreign context ends lineage',{mode:'missing',message:'thread not found',changes:[...lineage,change(201,2,{primaryResourceRef:{...primary,nativeId:'prior'}})]});
add('initial different context ends lineage',{mode:'missing',message:'thread not found',changes:[change(100,1,{primaryResourceRef:null})]});
add('initial previous Run cycle',{mode:'missing',message:'thread not found',changes:[change(100,1,{lastRun:pin(200)})]});
add('cycle uses Run identity despite revision',{mode:'missing',message:'thread not found',changes:[change(100,1,{lastRun:pin(200,99)})]});
add('two Run cycle',{mode:'missing',message:'thread not found',changes:[...lineage,change(100,1,{lastRun:pin(200,2)})]});
add('initial snapshot unavailable',{mode:'missing',message:'thread not found',fail:{key:'task-board/task',code:'not_found'}});
for(const row of rows) {
  const docs=structuredClone(records),localBlobs=structuredClone(blobs),trace=[];
  const subject={...stub,async read(key,address,upgrade=true) {
    trace.push(['read',key,address,upgrade]);
    if(row.fail&&(!row.fail.key||row.fail.key===key)&&(!row.fail.address||same(address,row.fail.address)))throw new IvyError(row.fail.code,'Fixture unavailable.');
    const matches=docs.filter(d=>d.key===key&&d.pin.objectId===(typeof address==='string'?address:address.objectId));
    const d=typeof address==='string'?matches.at(-1):matches.find(d=>d.pin.revision===address.revision);if(!d)throw new IvyError('not_found','Fixture unavailable.');
    return {...d,metadata:{parentId:d.parentId},revision:{contentHash:hashJson(d.value)}};
  },localBlob(key,object,parent) {
    trace.push(['blob-read',key,object,parent]);if(row.fail?.key==='blob')throw new IvyError(row.fail.code,'Fixture unavailable.');
    return Buffer.from(localBlobs.find(b=>same(b.object,object)&&b.key===key&&b.parent===parent).body,'hex');
  },async write(){throw Error('Recovery wrote a document');},saveLocalBlob(){throw Error('Recovery wrote a blob');}};
  for(const c of row.changes??[])Object.assign(docs.find(d=>same(d.pin,{objectId:c.objectId,revision:c.revision})).value,c.patch);
  const rewrite=(n,patch)=>{
    const call=docs.find(d=>d.pin.objectId===id(n)&&d.pin.revision===2),original=records.find(d=>same(d.pin,call.pin)),manifest=docs.find(d=>same(d.pin,original.value.evidence.object)),b=localBlobs.find(b=>same(b.object,manifest.value.chunks[0].object));
    let observation=JSON.parse(Buffer.from(b.body,'hex'));Object.assign(observation,patch);
    const body=Buffer.from(canonical(observation));b.body=body.toString('hex');
    Object.assign(manifest.value,{contentHash:digest(body),byteLength:body.length,chunks:[{object:b.object,contentHash:digest(body),byteLength:body.length}]});
    for(const key of ['operationId','callerPrincipalId','serviceNodeId','nativeVersion','nativeExecutableHash','method','requestHash','phase','epoch'])manifest.value[key]=observation[key];
    if(call.value.evidence)call.value.evidence.contentHash=hashJson(manifest.value);
  };
  if(row.message!==undefined)rewrite(300,{reply:{error:{code:-32600,message:row.message}}});
  if(row.threadObservation)rewrite(300,row.threadObservation);if(row.turnObservation)rewrite(301,row.turnObservation);
  if(row.requestThread!==undefined||row.requestMethod) {
    const call=docs.find(d=>d.pin.objectId===id(300)&&d.pin.revision===2).value,method=row.requestMethod??'thread/resume',params=method==='turn/interrupt'?{threadId:'thread',turnId:'turn'}:{threadId:row.requestThread,excludeTurns:true};
    call.request=await retainNativeRequest(stub,{nativeVersion:'0.159.2',catalogSourceHash:catalog.catalog.sourceHash,method,params},call.operationId);call.expectedDefinitionHash=hashJson(catalog.definitions.get(method));
    rewrite(300,{method,params,requestHash:hashJson({method,params})});
  }
  if(row.requestSource)docs.find(d=>d.pin.objectId===id(300)&&d.pin.revision===2).value.request.catalogSourceHash=row.requestSource;
  // Keep a compact shared base; only export exact changed document/blob values.
  row.documents=docs.filter(d=>!same(d,records.find(b=>same(b.pin,d.pin))));
  row.blobs=localBlobs.filter(b=>!same(b,blobs.find(a=>same(a.object,b.object))));
  const current={...task,value:{...task.value,...row.task}};
  const guard=(value,runId)=>{trace.push(['question',value,runId]);if(row.guard==='failed'||row.guard==='not_found')throw new IvyError(row.guard==='failed'?'fixture_failed':'not_found','Question guard unavailable.');return row.guard==='blocked';};
  let result;try{result={ok:await makeRecovery(guard)[row.mode]({store:subject,settings:{principalId:'caller'}},current)};}catch(e){if(!(e instanceof IvyError))throw new Error('Original recovery fixture '+row.name,{cause:e});result={error:{code:e.code,message:e.message}};}
  if(['archived: exact terminal proof','missing: exact terminal proof','archived: prevented turn is safe','missing: prevented turn is safe','same context previous unstarted Run','previous foreign context ends lineage','initial different context ends lineage'].includes(row.name)&&result.ok!==true)throw Error('Original positive recovery failed '+row.name+JSON.stringify(result));
  row.expectedHash=hashJson({result,trace});delete row.changes;delete row.threadObservation;delete row.turnObservation;
}
if(rows.length<120)throw Error('Insufficient recovery coverage');
process.stdout.write(JSON.stringify({base,rows}));
