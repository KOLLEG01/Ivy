import {readFileSync,existsSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {canonical,digest,hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {managementFrameBytes}=await import('../../packages/contracts/src/limits.ts');
const {validateTaskBoard}=await import('../../packages/contracts/src/task-board-validation.ts');
const {validateAgent}=await import('../../packages/contracts/src/agent-validation.ts');
const {checkedNativeContract}=await import('../../packages/contracts/src/checked-native-contract.ts');
const {validateNativeInvocation}=await import('../../packages/sdk/src/native-plan.ts');
const {saveNativeParameters,readNativeParameters}=await import('../../packages/sdk/src/native-parameters.ts');
const {validateNativeStatus,validateNativeRead,validateNativeOperation}=await import('../../packages/sdk/src/native-evidence.ts');
const {nativeThreadState}=await import('../../packages/sdk/src/native-observations.ts');
const {reconcileNativeOperation}=await import('../../packages/sdk/src/native-operation.ts');
const {NativeOwner}=await import('../../packages/sdk/src/native-owner.ts');
const {serviceTools}=await import('../../packages/sdk/src/client.ts');
const load=n=>readFileSync('services/task-board/src/runtime/'+n+'.ts','utf8');
const code=n=>load(n).replace(/^import .*;\r?\n/gm,'').replace(/^export \{.*\} from .*;\r?\n/gm,'').replaceAll('export ','');
const mutation=new Function('hashJson',stripTypeScriptTypes(load('store').match(/export const mutation = [\s\S]*?;/)[0].replace('export ',''))+'\nreturn mutation;')(hashJson);
const localStart=load('store').indexOf('  private localDocument<');
const localCode=load('store').slice(localStart,load('store').indexOf('\n  async read<',localStart));
const localDocument=new Function('requireThat','hashJson','contractVersion',stripTypeScriptTypes('class OriginalLocal {\n'+localCode+'\n}',{mode:'transform'})+'\nreturn OriginalLocal.prototype.localDocument;')(requireThat,hashJson,()=> '1.0.0');
const terminal=new Function(stripTypeScriptTypes(load('native-publication').match(/export const finishedRun[^;]+;/)[0].replace('export ',''))+'\nreturn finishedRun;')();
const same=(a,b)=>a===undefined||b===undefined?a===b:canonical(a)===canonical(b);
let trace=[];
const validate=(name,value)=>{trace.push(['validate',name]);validateTaskBoard(name,value);};
const intentCode=load('native-intent').slice(load('native-intent').indexOf('export type NativeSlot')).replaceAll('export ','');
const intent=new Function('taskPrompt','taskUpdatePrompt','hashJson','requireThat','validateTaskBoard','checkedNativeContract','validateNativeInvocation','saveNativeParameters','readNativeParameters','mutation',stripTypeScriptTypes(intentCode)+'\nreturn {intendedCall,retainNativeRequest,resolveNativeRequest,evidenceIdentity};')(()=> 'advance prompt',()=> 'advance prompt',hashJson,requireThat,validate,checkedNativeContract,validateNativeInvocation,saveNativeParameters,readNativeParameters,mutation);
const bytes=new Function('canonical','digest','requireThat','IvyError','managementFrameBytes',stripTypeScriptTypes(code('evidence-bytes'))+'\nreturn {saveEvidenceChunks,readEvidenceChunks,evidenceMaximumBytes,evidenceChunkBytes};')(canonical,digest,requireThat,IvyError,managementFrameBytes);
const Evidence=new Function('canonical','digest','hashJson','requireThat','validateNativeOperation','validateTaskBoard','checkedNativeContract','resolveNativeRequest','evidenceMaximumBytes','evidenceChunkBytes','saveEvidenceChunks','readEvidenceChunks','mutation',stripTypeScriptTypes(code('evidence'),{mode:'transform'})+'\nreturn TaskBoardEvidence;')(canonical,digest,hashJson,requireThat,validateNativeOperation,validate,checkedNativeContract,intent.resolveNativeRequest,bytes.evidenceMaximumBytes,bytes.evidenceChunkBytes,bytes.saveEvidenceChunks,bytes.readEvidenceChunks,mutation);
const Driver=new Function('hashJson','IvyError','requireThat','validateAgent','serviceTools','NativeOwner','reconcileNativeOperation','validateNativeRead','validateNativeStatus','nativeThreadState','same','TaskBoardEvidence','catalogFor','evidenceMaximumBytes','scopedOperationId','intendedCall','retainNativeRequest','resolveNativeRequest','evidenceIdentity','finishedRun','mutation',stripTypeScriptTypes(load('native-driver').slice(load('native-driver').indexOf('export class')).replace('export class','class'),{mode:'transform'})+'\nreturn TaskBoardNativeDriver;')(hashJson,IvyError,requireThat,validateAgent,serviceTools,NativeOwner,reconcileNativeOperation,validateNativeRead,validateNativeStatus,nativeThreadState,same,Evidence,checkedNativeContract,bytes.evidenceMaximumBytes,()=>{throw Error('Advance regenerated an operation identity');},intent.intendedCall,intent.retainNativeRequest,intent.resolveNativeRequest,intent.evidenceIdentity,terminal,mutation);
const bases=JSON.parse(execFileSync(process.execPath,['--experimental-transform-types','--disable-warning=ExperimentalWarning','tests/fixtures/task-board-native-preparation-oracle.mjs'],{encoding:'utf8'}));
const id=n=>'00000000-0000-4000-8000-'+String(n).padStart(12,'0'),pin=(n,r=1)=>({objectId:id(n),revision:r});
const time='2026-01-02T03:04:05.006Z',later='2026-01-02T03:04:05.007Z',zero='sha256:'+'0'.repeat(64),groups=[];
const fakeArtifact={object:pin(999),label:'Prior attachment receipt',mediaType:'application/json',contentHash:zero};
for(const previous of bases){
 const catalog=checkedNativeContract(previous.base.run.plan.nativeVersion,{sourceHash:previous.base.run.plan.catalogSourceHash});
 const common={...previous.base.run,phase:'running',operationId:'run-operation',turnId:null,cancellation:null,target:{serviceNodeId:'native-agent',hostId:'fixture'},calls:{thread:null,turn:null,steer:null,interrupt:null}};
 const task={...previous.base.initial,claim:{run:pin(200,2)},publication:null};
 const calls={},origins={thread:{kind:'initial'},turn:{kind:'initial'},interrupt:{kind:'initial'},steer:{kind:'steer',commentIds:['comment'],workRevision:1,peersHash:zero,predecessor:null}};
 for(const slot of Object.keys(origins)){
  const run={...common,turnId:['steer','interrupt'].includes(slot)?'turn':null,cancellation:slot==='interrupt'?{reason:'stop'}:null};
  const draft=await intent.intendedCall({async read(){return {value:previous.base.initial};}},run,slot,origins[slot]);
  const request=await intent.retainNativeRequest({client:{},rootObjectId:'root'},draft.request,'original');
  calls[slot]={...draft,request,origin:origins[slot],operationId:'original',state:'prepared',preparedAt:time,observedAt:null,ownerPhase:null,epoch:null,evidence:null,code:null};
 }
 const threadCall={...calls.thread,state:'observed',ownerPhase:'succeeded',epoch:'epoch',evidence:fakeArtifact,observedAt:time};
 const status={serviceNodeId:'native-agent',hostId:'fixture',nativeVersion:catalog.catalog.version,nativeExecutableHash:catalog.catalog.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch:'epoch',pid:42,state:'ready',observedAt:time,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
 const base={run:common,task,calls,threadCall,status};
 const rows=[],add=(name,input={})=>rows.push({name,input:{slot:'thread',...input}});
 add('dispatch original retained thread');add('retain already accepted operation',{operations:['accepted']});
 for(const phase of ['dispatched','outcome_unknown','succeeded','failed'])add('retain owner '+phase,{operations:[phase]});
 add('saved observation published before any owner query',{operations:['accepted'],passes:2});
 add('lost update publishes exact committed call',{operations:['accepted'],passes:2,fault:{update:'outcome_unknown'}});
 add('lost call write publishes committed revision',{operations:['accepted'],passes:2,fault:{write:{key:'task-board/native-call',code:'outcome_unknown',commit:true}}});
 add('failed call write reuses immutable evidence',{operations:['accepted'],passes:2,fault:{write:{key:'task-board/native-call',code:'fixture_failed',commit:false}}});
 add('manifest write failure cannot attach a receipt',{operations:['accepted'],fault:{write:{key:'task-board/native-evidence',code:'fixture_failed',commit:false}}});
 add('blob failure cannot attach a receipt',{operations:['accepted'],fault:{blobSave:'fixture_failed'}});
 add('owner absence cannot erase compact observed marker',{call:{state:'outcome_unknown',observedAt:time,ownerPhase:'outcome_unknown',epoch:'epoch',code:'outcome_unknown'}});
 add('owner absence cannot erase saved evidence',{previous:'accepted'});
 add('unchanged original receipt makes no new write',{previous:'accepted',operations:['accepted'],updatedAt:time});
 add('original receipt advances without dispatch',{previous:'accepted',operations:['dispatched']});
 add('regressed observation cannot be retained',{previous:'dispatched',operations:['accepted']});
 add('corrupt saved evidence precedes owner lookup',{previous:'accepted',corruptPrevious:true,operations:['accepted']});
 add('conclusive attached call refuses reconciliation',{call:{state:'observed',ownerPhase:'succeeded',observedAt:time,epoch:'epoch',evidence:fakeArtifact}});
 add('newer conclusive call publishes first',{latest:{state:'observed',ownerPhase:'failed',observedAt:time,epoch:'epoch',evidence:fakeArtifact}});
 add('request resolution precedes newer call shortcut',{latest:{request:{...calls.thread.request,catalogSourceHash:zero}}});
 add('missing attached pin refuses lookup',{run:{calls:{...common.calls}}});
 add('inactive claim prevents input',{task:{claim:null}});add('cancellation prevents input',{run:{cancellation:{reason:'stop'}}});
 add('different absence epoch refuses any send',{absence:{epoch:'other'}});
 add('absence belongs to original owner',{absence:{serviceNodeId:'other'}});add('absence belongs to original operation',{absence:{operationId:'other'}});
 add('generic not found is not absence',{fault:{operation:'not_found'}});
 add('unavailable operation does not dispatch',{fault:{operation:'service_unavailable'}});
 add('uncertain dispatch queries original identity',{fault:{invoke:'outcome_unknown'},operations:['absent','accepted']});
 add('uncertain dispatch without receipt stays unresolved',{fault:{invoke:'outcome_unknown'},operations:['absent','absent']});
 add('rejected dispatch propagates',{fault:{invoke:'fixture_failed'}});
 add('owner refused before any read',{fault:{owner:'fixture_failed'}});
 add('owner refused before send',{failOwner:2});add('owner refused after pin checks',{failOwner:3});
 add('changed Task refuses send',{bumpTask:2});add('changed Run refuses send',{bumpRun:2});
 add('envelope retry reruns domain guards',{retry:true});add('changed Task before envelope retry refuses send',{retry:true,bumpTask:4});
 add('changed original definition refuses dispatch',{nativeDefinitionChanged:true});
 for(const [key,value]of [['callerPrincipalId','other'],['operationId','other'],['requestHash',zero]])add('observed original identity '+key,{operations:['accepted'],observation:{[key]:value}});
 add('turn dispatch needs same epoch',{slot:'turn',status:{epoch:'other'},absence:{epoch:'other'}});
 add('turn dispatch needs retained thread',{slot:'turn',missingThread:true});
 add('failed thread prevents turn',{slot:'turn',thread:{ownerPhase:'failed'}});
 add('thread attachment must succeed',{slot:'turn',thread:{ownerPhase:'accepted'}});
 add('thread attachment must have current epoch',{slot:'turn',thread:{epoch:'other'}});
 add('turn dispatch reads exact idle primary',{slot:'turn'});add('system error primary may accept turn',{slot:'turn',threadRead:{status:{type:'systemError'}}});
 for(const [name,patch]of [['active',{status:{type:'active'}}],['ephemeral',{ephemeral:true}],['wrong identity',{id:'other'}],['cannot accept input',{canAcceptDirectInput:false}]])add('turn primary '+name,{slot:'turn',threadRead:patch});
 add('fresh primary read error refuses turn',{slot:'turn',read:{reply:{error:{code:-32000,message:'unavailable'}}}});
 add('fresh primary read must retain caller',{slot:'turn',read:{callerPrincipalId:'other'}});
 add('steer dispatch preserves turn identity',{slot:'steer'});add('steer replacement epoch prevents input',{slot:'steer',status:{epoch:'other'},absence:{epoch:'other'}});
 add('interrupt retained cancellation',{slot:'interrupt'});add('inactive cancellation can interrupt',{slot:'interrupt',task:{claim:null}});
 add('interrupt refuses replacement epoch',{slot:'interrupt',status:{epoch:'other'},absence:{epoch:'other'}});
 for(const row of rows){
  const i=row.input,slot=i.slot,docs=[],blobs=[],fault=structuredClone(i.fault??{}),lists=new Map();let next=400,owners=0,operationIndex=0,invokes=0;
  const selectedCalls={...common.calls,[slot]:pin(300)};if(slot==='turn'&&!i.missingThread)selectedCalls.thread=pin(301);
  const run={...common,turnId:['steer','interrupt'].includes(slot)?'turn':null,cancellation:slot==='interrupt'?{reason:'stop'}:null,calls:selectedCalls,...i.run};
  const activeTask={...task,...i.task},call={...calls[slot],...i.call},primaryCall={...threadCall,...i.thread};
  const put=(key,n,r,value,name=id(n),parent='root')=>{const d={key,name,pin:pin(n,r),value:structuredClone(value),metadata:{parentId:parent},revision:{contentHash:hashJson(value)}};docs.push(d);return d;};
  const latest=(key,address)=>{const a=docs.filter(d=>d.key===key&&d.pin.objectId===(typeof address==='string'?address:address.objectId));return typeof address==='string'?a.at(-1):a.find(d=>d.pin.revision===address.revision);};
  const local={prepare(sql){return {get(objectId,k){if(sql.includes('FROM documents')){const d=latest(k,objectId);return d?{parent_id:d.metadata.parentId,current_revision:d.pin.revision,created_at:time,updated_at:time,name:d.name}:undefined;}const d=docs.find(d=>d.pin.objectId===objectId&&d.pin.revision===k);return d?{value_json:JSON.stringify(d.value),created_at:time}:undefined;}}}};
  const fail=(code)=>{throw new IvyError(code,'Fixture driver unavailable.',code==='outcome_unknown'?'unknown':'not_executed');};
  const params=(await intent.resolveNativeRequest({client:{},rootObjectId:'root'},calls[slot].request)).params;
  const observed=(phase,at=later)=>({schemaVersion:1,operationId:'original',callerPrincipalId:'workflow-owner',serviceNodeId:'native-agent',nativeVersion:status.nativeVersion,nativeExecutableHash:status.nativeExecutableHash,method:calls[slot].request.method,params,requestHash:hashJson({method:calls[slot].request.method,params}),phase:phase==='prevented'?'failed':phase,createdAt:time,updatedAt:at,epoch:['accepted','prevented'].includes(phase)?null:'epoch',requestId:['accepted','prevented'].includes(phase)?null:42,reply:phase==='succeeded'?{result:{}}:phase==='failed'?{error:{code:-32000,message:'native failed'}}:null,code:phase==='failed'?'native_failed':phase==='outcome_unknown'?'outcome_unknown':phase==='prevented'?'request_prevented':null});
  const client={async request(method,args){
   trace.push(['rpc',method,structuredClone(args)]);
   if(method==='tools.list'){
    const name=args.namespace+'.'+args.namePrefix,count=lists.get(name)??0;lists.set(name,count+1);
    const definition=args.namespace==='codex'?catalog.definitions.get(args.namePrefix):{namespace:'agent',name:args.namePrefix,interfaceVersion:'1.0.0',annotations:{readOnlyHint:true},inputSchema:{},outputSchema:{}};
    const h=args.namespace==='codex'?(i.nativeDefinitionChanged?zero:hashJson(definition)):'sha256:'+(i.retry&&name==='agent.invoke'&&count>0?'b':'a').repeat(64);
    return {provider:{node:{serviceNodeId:'native-agent'}},items:[{qualifiedName:name,definition,definitionHash:h,catalogHash:zero}],nextCursor:null};
   }
   assert.equal(method,'tools.call');
   const name=args.qualifiedName;
   if(name==='agent.operation'){
    if(fault.operation)fail(fault.operation);const phases=i.operations??['absent'],phase=phases[Math.min(operationIndex++,phases.length-1)];
    if(phase==='absent')throw new IvyError('not_found','Original operation is absent.','not_executed',{kind:'agent_operation_absent',operationId:'original',serviceNodeId:'native-agent',epoch:'epoch',...i.absence});
    return {...observed(phase,i.updatedAt??later),...i.observation};
   }
   if(name==='agent.status')return {...status,...i.status};
   if(name==='agent.prevent')return observed('prevented');
   if(name==='agent.invoke'){
    invokes++;if(i.retry&&invokes===1)throw new IvyError('tool_definition_changed','Changed.');
    if(fault.invoke){const code=fault.invoke;delete fault.invoke;fail(code);}
    return {...observed('accepted'),...i.observation};
   }
   assert.equal(name,'agent.read');const p=args.arguments.params,m=args.arguments.method;
   return {schemaVersion:1,observationId:'read',callerPrincipalId:'workflow-owner',serviceNodeId:'native-agent',nativeVersion:status.nativeVersion,nativeExecutableHash:status.nativeExecutableHash,catalogHash:status.catalogHash,epoch:'epoch',requestId:1,observedAt:time,method:m,params:p,requestHash:hashJson({method:m,params:p}),reply:{result:{thread:{id:'thread',ephemeral:false,status:{type:'idle'},canAcceptDirectInput:true,...i.threadRead}}},...i.read};
  }};
  const store={client,rootObjectId:'root',
   async read(key,address,primary=true){trace.push(['read',key,address,primary]);const d=localDocument.call({local,rootObjectId:'root'},key,address,primary);if(key==='task-board/native-call')validateTaskBoard('NativeCall',d.value);return d;},
   async write(key,value,m,d){
    trace.push(['write',key,value,m,d]);validateTaskBoard(key==='task-board/native-call'?'NativeCall':'NativeEvidence',value);
    const f=fault.write&&(!fault.write.key||fault.write.key===key)?fault.write:null;if(f)delete fault.write;
    if(f&&!f.commit)fail(f.code);
    let saved;
    if(d.create){const old=docs.filter(x=>x.key===key&&x.name===d.create.name&&x.metadata.parentId===d.create.parentId).at(-1);if(old){assert.equal(hashJson(old.value),hashJson(value));saved=old.pin;}else saved=put(key,next++,1,value,d.create.name,d.create.parentId).pin;}
    else {const old=latest(key,d.objectId);assert.equal(old.pin.revision,d.expectedRevision);saved=put(key,Number(d.objectId.slice(-12)),old.pin.revision+1,value,old.name,old.metadata.parentId).pin;}
    if(f)fail(f.code);return saved;
   },
   saveLocalBlob(key,name,parent,body){trace.push(['blob-save',key,name,parent,digest(body),body.length]);if(fault.blobSave){const code=fault.blobSave;delete fault.blobSave;fail(code);}let b=blobs.find(b=>b.key===key&&b.name===name&&b.parent===parent);if(b){assert.ok(b.body.equals(body));}else{b={key,name,parent,pin:pin(900+blobs.length),body:Buffer.from(body),initial:Buffer.from(body)};blobs.push(b);}return b.pin;},
   localBlob(key,p,parent){trace.push(['blob-read',key,p,parent]);const b=blobs.find(b=>b.key===key&&b.pin.objectId===p.objectId&&b.parent===parent);assert.ok(b);return b.body;},
  };
  put('task-board/native-call',300,1,call);if(slot==='turn'&&!i.missingThread)put('task-board/native-call',301,1,primaryCall);
  if(i.previous){const expected=await intent.evidenceIdentity(store,run,call,'workflow-owner'),evidence=await new Evidence(store).save(pin(300),observed(i.previous,time),expected);const value={...call,state:i.previous==='outcome_unknown'?'outcome_unknown':'observed',ownerPhase:i.previous,observedAt:time,epoch:i.previous==='accepted'?null:'epoch',evidence,code:i.previous==='outcome_unknown'?'outcome_unknown':null};put('task-board/native-call',300,2,value);run.calls={...run.calls,[slot]:pin(300,2)};}
  if(i.latest)put('task-board/native-call',300,2,{...call,...i.latest});
  if(i.corruptPrevious)blobs[0].body[0]^=1;
  put('task-board/run',200,1,run);put('task-board/run',200,2,run);put('task-board/task',100,1,activeTask);put('task-board/task',100,2,activeTask);
  const engine={store,settings:{principalId:'workflow-owner'},async verifyOwner(){trace.push(['owner']);if(fault.owner||++owners===i.failOwner)fail(fault.owner??'fixture_failed');for(const [flag,key,n]of[['bumpTask','task-board/task',100],['bumpRun','task-board/run',200]])if(i[flag]===owners){const old=latest(key,id(n));const p=put(key,n,old.pin.revision+1,old.value).pin;if(flag==='bumpRun'){const t=latest('task-board/task',id(100));put('task-board/task',100,t.pin.revision+1,{...t.value,claim:{run:p}});}trace.push(['change',key,p]);}},async nativeUpdate(v){trace.push(['update',v]);if(fault.update){const code=fault.update;delete fault.update;fail(code);}return null;}};
  trace=[];const subject=new Driver(engine),outcomes=[];
  for(let pass=0;pass<(i.passes??1);pass++)try{outcomes.push({ok:await subject.advance(id(200),slot)});}catch(e){if(!(e instanceof IvyError))throw new Error(row.name,{cause:e});outcomes.push({error:{code:e.code,message:e.message,outcome:e.outcome}});}
  const documents=docs.filter((d,index)=>['task-board/native-call','task-board/native-evidence'].includes(d.key)&&!docs.slice(index+1).some(x=>x.key===d.key&&x.pin.objectId===d.pin.objectId)).map(d=>({key:d.key,pin:d.pin,value:d.value,name:d.name,parent:d.metadata.parentId})).sort((a,b)=>a.key===b.key?(a.pin.objectId<b.pin.objectId?-1:a.pin.objectId===b.pin.objectId?0:1):a.key<b.key?-1:1);
  row.expectedHash=hashJson({outcomes,trace,documents});
  row.blobs=blobs.map(b=>({key:b.key,name:b.name,parent:b.parent,pin:b.pin,hex:b.initial.toString('hex')}));
  if(['dispatch original retained thread','retain already accepted operation','unchanged original receipt makes no new write','original receipt advances without dispatch','envelope retry reruns domain guards','turn dispatch reads exact idle primary','steer dispatch preserves turn identity','interrupt retained cancellation'].includes(row.name))assert.ok(outcomes.every(o=>'ok'in o),row.name+JSON.stringify(outcomes));
 }
 groups.push({path:previous.path,base,rows});
}
process.stdout.write(JSON.stringify(groups));
