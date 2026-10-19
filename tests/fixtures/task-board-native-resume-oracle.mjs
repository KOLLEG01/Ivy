import {readFileSync,existsSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {canonical,hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {validateTaskBoard}=await import('../../packages/contracts/src/task-board-validation.ts');
const {checkedNativeContract}=await import('../../packages/contracts/src/checked-native-contract.ts');
const {validateNativeInvocation}=await import('../../packages/sdk/src/native-plan.ts');
const {saveNativeParameters,readNativeParameters}=await import('../../packages/sdk/src/native-parameters.ts');
const {validateNativeStatus}=await import('../../packages/sdk/src/native-evidence.ts');
const {NativeOwner}=await import('../../packages/sdk/src/native-owner.ts');
const {serviceTools}=await import('../../packages/sdk/src/client.ts');
const load=n=>readFileSync('services/task-board/src/runtime/'+n+'.ts','utf8');
const mutation=new Function('hashJson',stripTypeScriptTypes(load('store').match(/export const mutation = [\s\S]*?;/)[0].replace('export ',''))+'\nreturn mutation;')(hashJson);
const intentSource=load('native-intent').slice(load('native-intent').indexOf('export type NativeSlot')).replaceAll('export ','');
const original=load('native-driver');
const driverSource=original.slice(original.indexOf('export class TaskBoardNativeDriver'),original.indexOf('  async advance')).replace('export class','class')+'\n}';
const terminal=new Function(stripTypeScriptTypes(load('native-publication').match(/export const finishedRun[^;]+;/)[0].replace('export ',''))+'\nreturn finishedRun;')();
const same=(a,b)=>a===undefined||b===undefined?a===b:canonical(a)===canonical(b);
const bases=JSON.parse(execFileSync(process.execPath,['--experimental-transform-types','--disable-warning=ExperimentalWarning','tests/fixtures/task-board-native-driver-oracle.mjs'],{encoding:'utf8'}));
const id=n=>'00000000-0000-4000-8000-'+String(n).padStart(12,'0'),pin=(n,r=1)=>({objectId:id(n),revision:r});
const time='2026-01-02T03:04:05.006Z',zero='sha256:'+'0'.repeat(64),groups=[];
const artifact={object:pin(900),label:'Original operation evidence',mediaType:'application/json',contentHash:zero,byteLength:1};
for(const previous of bases){
 const trace=[],catalog=checkedNativeContract(previous.base.run.plan.nativeVersion,{sourceHash:previous.base.run.plan.catalogSourceHash});
 const validate=(n,v)=>{trace.push(['validate',n]);validateTaskBoard(n,v);};
 const unexpected=()=>{throw Error('Recovery unexpectedly generated user input');};
 const intent=new Function('taskPrompt','taskUpdatePrompt','hashJson','requireThat','validateTaskBoard','checkedNativeContract','validateNativeInvocation','saveNativeParameters','readNativeParameters','mutation',stripTypeScriptTypes(intentSource)+'\nreturn {intendedCall,retainNativeRequest,resolveNativeRequest};')(unexpected,unexpected,hashJson,requireThat,validate,checkedNativeContract,validateNativeInvocation,saveNativeParameters,readNativeParameters,mutation);
 const run={...previous.base.run,target:{serviceNodeId:'native-agent',hostId:'fixture'},calls:{...previous.base.run.calls,thread:pin(300)}};
 const task=previous.base.task;
 const draft=await intent.intendedCall({async read(){return {value:previous.base.initial};}},run,'thread');
 const request=await intent.retainNativeRequest({client:{},rootObjectId:'root'},draft.request,'old-resume');
 const call={...draft,request,origin:{kind:'initial'},operationId:'old-resume',state:'outcome_unknown',preparedAt:time,observedAt:time,ownerPhase:'outcome_unknown',epoch:'old-epoch',evidence:artifact,code:'outcome_unknown'};
 const turn={...call,state:'prepared',ownerPhase:null,epoch:null,evidence:null,observedAt:null,code:null};
 const status={serviceNodeId:'native-agent',hostId:'fixture',nativeVersion:request.nativeVersion,nativeExecutableHash:catalog.catalog.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch:'new-epoch',pid:42,state:'ready',observedAt:time,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
 const base={...previous.base,run,task,call,turn,status};
 const rows=[],add=(name,input={})=>rows.push({name,input});
 add('exact replacement resumes saved thread');add('same epoch does not reattach',{status:{epoch:'old-epoch'}});
 add('repeated recovery reuses retained identity',{passes:2});
 for(const phase of ['completed','failed','cancelled'])add('terminal '+phase,{run:{phase}});
 add('pending publication',{task:{publication:{operationId:'publishing'}}});
 add('inactive claim',{task:{claim:null}});add('stale claim pin',{task:{claim:{run:pin(200)}}});
 add('another claimed run',{task:{claim:{run:pin(201)}}});
 add('cancellation blocks recovery',{run:{cancellation:{reason:'stop'}}});
 add('known turn blocks recovery',{run:{turnId:'turn'}});
 add('missing retained thread',{run:{calls:{...run.calls,thread:null}}});
 add('missing primary',{run:{primaryResourceRef:null}});
 add('untouched attached turn permits recovery',{attachedTurn:true});
 add('newer turn revision blocks recovery',{attachedTurn:true,turnNext:true});
 add('turn observation blocks recovery',{attachedTurn:true,turn:{state:'observed',ownerPhase:'accepted',evidence:artifact,observedAt:time}});
 add('missing turn revision propagates',{attachedTurn:true,missingTurn:true});
 add('missing thread revision propagates',{missingCall:true});
 add('reads exact older thread receipt',{callNext:true});
 add('different method has no recovery authority',{call:{request:{...request,method:'turn/start'}}});
 for(const ownerPhase of ['accepted','dispatched','succeeded','failed',null])add('receipt phase '+ownerPhase,{call:{ownerPhase}});
 add('receipt without evidence',{call:{evidence:null}});add('receipt without epoch',{call:{epoch:null}});
 for(const [k,v]of [['namespace','other'],['kind','other'],['serviceNodeId','other'],['nativeId','other']])add('primary identity '+k,{run:{primaryResourceRef:{...run.primaryResourceRef,[k]:v}}});
 add('primary refusal precedes request materialization',{run:{primaryResourceRef:{...run.primaryResourceRef,kind:'other'}},call:{request:{...request,catalogSourceHash:zero}}});
 add('unavailable retained catalog',{call:{request:{...request,catalogSourceHash:zero}}});
 add('mismatched retained parameters',{call:{request:{...request,params:{...request.params,method:'turn/start'}}}});
 add('status still read before bad catalog selection',{action:'status',call:{request:{...request,catalogSourceHash:zero}}});
 for(const [k,v]of [['serviceNodeId','other'],['hostId','other'],['nativeVersion','0.158.0'],['nativeExecutableHash',zero],['catalogHash',zero],['state','stopped'],['epoch',null]])add('status identity '+k,{status:{[k]:v}});
 add('status failure propagates',{fault:{status:'fixture_failed'}});add('read failure propagates',{fault:{nativeRead:'not_found'}});
 add('owner refusal precedes journal reads',{fault:{owner:'fixture_failed'}});
 add('owner refused after exact native read',{failOwner:2});
 add('unknown update keeps retained reattachment',{fault:{update:'outcome_unknown'},passes:2});
 for(const [k,v]of [['serviceNodeId','other'],['callerPrincipalId','other'],['nativeVersion','0.158.0'],['nativeExecutableHash',zero],['catalogHash',zero],['epoch','other'],['method','thread/list'],['params',{}],['requestHash',zero]])add('read evidence '+k,{read:{[k]:v}});
 add('native error is not a context',{read:{reply:{error:{code:-32000,message:'thread unavailable'}}}});
 for(const thread of [{id:'other',ephemeral:false},{id:'thread',ephemeral:true},{id:'thread'},{},null])add('read thread '+JSON.stringify(thread),{read:{reply:{result:{thread}}}});
 add('null result is not a saved thread',{read:{reply:{result:null}}});
 for(const row of rows){
  const i=row.input,docs=[],scopes=new Map(),fault=structuredClone(i.fault??{});let writes=0,owners=0;
  const activeRun={...run,...i.run};if(i.attachedTurn)activeRun.calls={...activeRun.calls,turn:pin(301)};
  const activeTask={...task,...i.task},activeCall={...call,...i.call},activeTurn={...turn,...i.turn};
  const doc=(key,n,r,value,name=id(n))=>({key,name,pin:pin(n,r),value:structuredClone(value),metadata:{parentId:'root'},revision:{contentHash:hashJson(value)}});
  docs.push(doc('task-board/run',200,1,activeRun),doc('task-board/run',200,2,activeRun),doc('task-board/task',100,2,activeTask));
  if(!i.missingCall)docs.push(doc('task-board/native-call',300,1,activeCall));
  if(i.callNext)docs.push(doc('task-board/native-call',300,2,{...activeCall,ownerPhase:'failed'}));
  if(i.attachedTurn&&!i.missingTurn)docs.push(doc('task-board/native-call',301,1,activeTurn));
  if(i.turnNext)docs.push(doc('task-board/native-call',301,2,{...activeTurn}));
  const latest=(key,address)=>{const list=docs.filter(d=>d.key===key&&d.pin.objectId===(typeof address==='string'?address:address.objectId));return typeof address==='string'?list.at(-1):list.find(d=>d.pin.revision===address.revision);};
  const fail=(code,message='Fixture driver unavailable.')=>{throw new IvyError(code,message,code==='outcome_unknown'?'unknown':'not_executed');};
  const client={async request(method,params){
   trace.push(['rpc',method,structuredClone(params)]);
   if(method==='tools.list')return {provider:{node:{serviceNodeId:'native-agent'}},items:[{qualifiedName:'agent.'+params.namePrefix,definition:{namespace:'agent',name:params.namePrefix,interfaceVersion:'1.0.0',annotations:{readOnlyHint:true},inputSchema:{},outputSchema:{}},definitionHash:'sha256:'+'a'.repeat(64),catalogHash:'sha256:'+'b'.repeat(64)}],nextCursor:null};
   assert.equal(method,'tools.call');
   if(params.qualifiedName==='agent.status'){if(fault.status)fail(fault.status);return {...status,...i.status};}
   assert.equal(params.qualifiedName,'agent.read');if(fault.nativeRead)fail(fault.nativeRead);
   const {method:m,params:p}=params.arguments;
   return {schemaVersion:1,observationId:'read',callerPrincipalId:'workflow-owner',serviceNodeId:'native-agent',nativeVersion:request.nativeVersion,nativeExecutableHash:status.nativeExecutableHash,catalogHash:status.catalogHash,epoch:'new-epoch',requestId:1,observedAt:time,method:m,params:p,requestHash:hashJson({method:m,params:p}),reply:{result:{thread:{id:'thread',ephemeral:false}}},...i.read};
  }};
  const store={client,rootObjectId:'root',
   async read(key,address,upgrade=true){trace.push(['read',key,address,upgrade]);const d=latest(key,address);if(!d)fail('task_board_evidence_mismatch','The local workflow revision is unavailable.');if(key==='task-board/native-call')validateTaskBoard('NativeCall',d.value);return structuredClone(d);},
   async named(key,name,parent){trace.push(['named',key,name,parent??null]);return structuredClone(docs.filter(d=>d.key===key&&d.name===name).at(-1)??null);},
   async write(key,value,m,destination){trace.push(['write',key,value,m,destination]);validateTaskBoard('NativeCall',value);const d=doc(key,400+writes++,1,value,destination.create.name);docs.push(d);return d.pin;},
  };
  const scoped=async(_client,scope)=>{trace.push(['scope',scope]);const k=canonical(scope);if(!scopes.has(k))scopes.set(k,'native-operation-'+(scopes.size+1));return scopes.get(k);};
  const engine={store,settings:{principalId:'workflow-owner'},async verifyOwner(){trace.push(['owner']);if(fault.owner||++owners===i.failOwner)fail(fault.owner??'fixture_failed');},async nativeUpdate(v){trace.push(['update',v]);if(fault.update){const code=fault.update;delete fault.update;fail(code);}return null;}};
  const FixedDate=class extends Date{constructor(){super(time);trace.push(['now']);}};
  const Driver=new Function('hashJson','IvyError','requireThat','same','finishedRun','mutation','scopedOperationId','intendedCall','retainNativeRequest','resolveNativeRequest','serviceTools','NativeOwner','catalogFor','validateNativeStatus','Date',stripTypeScriptTypes(driverSource,{mode:'transform'})+'\nreturn TaskBoardNativeDriver;')(hashJson,IvyError,requireThat,same,terminal,mutation,scoped,intent.intendedCall,intent.retainNativeRequest,intent.resolveNativeRequest,serviceTools,NativeOwner,(version,identity)=>checkedNativeContract(version,identity),validateNativeStatus,FixedDate);
  trace.length=0;const driver=new Driver(engine),outcomes=[];
  for(let pass=0;pass<(i.passes??1);pass++)try{outcomes.push({ok:i.action==='status'?await driver.status(activeRun,activeCall):await driver.recoverUncertainResume(id(200))});}catch(e){if(!(e instanceof IvyError))throw new Error(row.name,{cause:e});outcomes.push({error:{code:e.code,message:e.message,outcome:e.outcome}});}
  const calls=docs.filter(d=>d.key==='task-board/native-call'&&d.pin.objectId!==id(300)&&d.pin.objectId!==id(301)).map(d=>({pin:d.pin,value:d.value,name:d.name}));
  if(['exact replacement resumes saved thread','untouched attached turn permits recovery','reads exact older thread receipt','repeated recovery reuses retained identity'].includes(row.name))assert.ok(outcomes.every(o=>o.ok===true),row.name+JSON.stringify(outcomes));
  row.expectedHash=hashJson({outcomes,trace,calls});
 }
 groups.push({path:previous.path,base,rows});
}
process.stdout.write(JSON.stringify(groups));
