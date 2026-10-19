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
const load=n=>readFileSync('services/task-board/src/runtime/'+n+'.ts','utf8');
const mutation=new Function('hashJson',stripTypeScriptTypes(load('store').match(/export const mutation = [\s\S]*?;/)[0].replace('export ',''))+'\nreturn mutation;')(hashJson);
const source=load('native-intent').slice(load('native-intent').indexOf('export type NativeSlot')).replaceAll('export ','');
const originalDriver=load('native-driver');
const driverSource=originalDriver.slice(originalDriver.indexOf('export class TaskBoardNativeDriver'),originalDriver.indexOf('  private async management')).replace('export class','class')+'\n}';
const same=(a,b)=>a===undefined||b===undefined?a===b:canonical(a)===canonical(b);
// finishedRun has a type annotation in the original source.
const terminal=new Function(stripTypeScriptTypes(load('native-publication').match(/export const finishedRun[^;]+;/)[0].replace('export ',''))+'\nreturn finishedRun;')();
const bases=JSON.parse(execFileSync(process.execPath,['--experimental-transform-types','--disable-warning=ExperimentalWarning','tests/fixtures/task-board-native-preparation-oracle.mjs'],{encoding:'utf8'}));
const id=n=>'00000000-0000-4000-8000-'+String(n).padStart(12,'0'),pin=(n,r=1)=>({objectId:id(n),revision:r});
const time='2026-01-02T03:04:05.006Z',groups=[];
for(const previous of bases){
 const run={...previous.base.run,phase:'running',operationId:'run-operation',turnId:null,calls:{thread:null,turn:null,steer:null,interrupt:null}};
 const task={...previous.base.initial,claim:{run:pin(200,2)},publication:null};
 const initial={...task,claim:null};
 const base={run,task,initial,runPin:pin(200,2),taskPin:pin(100,2),callPin:pin(300)};
 const rows=[],prepare={kind:'prepare',slot:'thread'},add=(name,input={})=>rows.push({name,input:{steps:[prepare],...input}});
 add('new retained call');add('same owner replay',{steps:[prepare,prepare]});add('existing named call',{existing:true});
 add('already attached returns current call',{existing:true,attached:true});
 for(const phase of ['completed','failed','cancelled'])add('terminal '+phase,{run:{phase}});
 add('pending publication',{task:{publication:{operationId:'publishing'}}});
 add('missing active claim',{task:{claim:null}});add('another claim',{task:{claim:{run:pin(201,2)}}});
 add('stale active pin',{task:{claim:{run:pin(200)}}});add('changed active pin fields',{task:{claim:{run:{...pin(200,2),extra:true}}}});
 add('cancelled claim cannot prepare input',{run:{cancellation:{reason:'cancel'}}});
 add('inactive retained cancellation may interrupt',{run:{cancellation:{reason:'cancel'},turnId:'turn'},task:{claim:null},steps:[{kind:'prepare',slot:'interrupt'}]});
 add('interruption requires cancellation',{steps:[{kind:'prepare',slot:'interrupt'}]});
 add('owner refusal precedes reads',{fault:{owner:'fixture_failed'}});add('scope refusal precedes lookup',{fault:{scope:'fixture_failed'}});
 add('lookup refusal',{fault:{named:'fixture_failed'}});
 for(const code of ['fixture_failed','outcome_unknown','revision_conflict','mutation_conflict'])add('write before commit '+code,{fault:{write:{code,commit:false}}});
 for(const code of ['outcome_unknown','revision_conflict','mutation_conflict'])add('write after commit '+code,{fault:{write:{code,commit:true}},steps:[prepare,prepare]});
 for(const code of ['fixture_failed','not_found','revision_conflict','mutation_conflict'])add('saved read '+code,{fault:{read:code},steps:[prepare,prepare]});
 add('conflicting write and unavailable competing lookup',{fault:{write:{code:'revision_conflict',commit:true},namedAfter:'fixture_failed'}});
 add('update failure retains original call',{fault:{update:'fixture_failed'},steps:[prepare,prepare]});
 add('unknown update retains original call',{fault:{update:'outcome_unknown'},steps:[prepare,prepare]});
 add('attached after lost update skips repeat update',{fault:{update:'outcome_unknown'},steps:[prepare,{kind:'attach'},prepare]});
 add('default initial validates only on creation',{steps:[{...prepare,origin:{kind:'initial'}}]});
 add('invalid origin on create',{steps:[{...prepare,origin:null}]});
 add('named replay does not regenerate intent',{existing:true,steps:[{...prepare,origin:{kind:'invalid'}}]});
 add('reattach transition',{steps:[{...prepare,origin:{kind:'reattach',predecessor:pin(299),preparedEpoch:'new-epoch'}}]});
 const steer={kind:'steer',commentIds:['comment'],workRevision:1,peersHash:'sha256:'+'0'.repeat(64),predecessor:null};
 add('steer retains update input',{run:{turnId:'turn'},steps:[{kind:'prepare',slot:'steer',origin:steer}]});
 add('turn uses immutable original Task',{steps:[{kind:'prepare',slot:'turn'}]});
 add('UTF16 scoped identity',{operationId:'native\ud800',steps:[prepare,prepare]});
 add('fresh owner identity is a different durable call',{steps:[prepare,{kind:'resetScope'},prepare]});
 for(const row of rows){
  const i=row.input,trace=[],docs=[],scopes=new Map(),fault=structuredClone(i.fault??{});let identity=0,writes=0,named=0;
  const activeRun={...run,...i.run},activeTask={...task,...i.task};
  if(i.attached)activeRun.calls={...run.calls,thread:pin(300)};
  const sqliteText=s=>Buffer.from(s,'utf8').toString('utf8');
  const doc=(key,n,r,value,name=id(n))=>({key,name:sqliteText(name),pin:pin(n,r),value:structuredClone(value),metadata:{parentId:'root'},revision:{contentHash:hashJson(value)}});
  docs.push(doc('task-board/run',200,1,activeRun),doc('task-board/run',200,2,activeRun),doc('task-board/task',100,1,initial),doc('task-board/task',100,2,activeTask));
  const latest=(key,address)=>{const list=docs.filter(d=>d.key===key&&d.pin.objectId===(typeof address==='string'?address:address.objectId));return typeof address==='string'?list.at(-1):list.find(d=>d.pin.revision===address.revision);};
  const fail=(code,message='Fixture driver unavailable.')=>{throw new IvyError(code,message,code==='outcome_unknown'?'unknown':'not_executed');};
  const validate=(name,value)=>{trace.push(['validate',name]);validateTaskBoard(name,value);};
  const prompt=(kind,value,owner,flag)=>{trace.push(['prompt',kind,value,owner,flag===undefined?{undefined:true}:flag]);return 'driver prompt\ud800';};
  const intent=new Function('taskPrompt','taskUpdatePrompt','hashJson','requireThat','validateTaskBoard','checkedNativeContract','validateNativeInvocation','saveNativeParameters','readNativeParameters','mutation',stripTypeScriptTypes(source)+'\nreturn {intendedCall,retainNativeRequest};')((...a)=>prompt('task',...a),(...a)=>prompt('update',...a),hashJson,requireThat,validate,checkedNativeContract,validateNativeInvocation,saveNativeParameters,readNativeParameters,mutation);
  const store={rootObjectId:'root',client:{request(){throw Error('Unexpected original driver native/Hive call');}},
   async read(key,address,upgrade=true){trace.push(['read',key,address,upgrade]);if(key==='task-board/native-call'&&typeof address!=='string'&&fault.read){const code=fault.read;delete fault.read;fail(code);}const d=latest(key,address);if(!d)fail('not_found');return structuredClone(d);},
   async named(key,name,parent){trace.push(['named',key,name,parent??null]);named++;if(fault.named){const code=fault.named;delete fault.named;fail(code);}if(named>1&&fault.namedAfter){const code=fault.namedAfter;delete fault.namedAfter;fail(code);}return structuredClone(docs.filter(d=>d.key===key&&d.name===sqliteText(name)).at(-1)??null);},
   async write(key,value,mutation,destination){trace.push(['write',key,value,mutation,destination]);validateTaskBoard('NativeCall',value);const f=fault.write;delete fault.write;if(f&&!f.commit)fail(f.code);const d=doc(key,300+writes++,1,value,destination.create.name);docs.push(d);if(f)fail(f.code);return d.pin;},
  };
  const scoped=async(_client,scope)=>{trace.push(['scope',scope]);if(fault.scope)fail(fault.scope);const key=canonical(scope);if(!scopes.has(key))scopes.set(key,i.operationId??'native-operation-'+(++identity));return scopes.get(key);};
  const op=i.operationId??'native-operation-1';
  if(i.existing){const prepared=await intent.intendedCall(store,activeRun,'thread');const request=await intent.retainNativeRequest(store,prepared.request,op);docs.push(doc('task-board/native-call',300,1,{...prepared,request,origin:{kind:'initial'},operationId:op,state:'prepared',preparedAt:time,observedAt:null,ownerPhase:null,epoch:null,evidence:null,code:null},'Native call '+op));writes++;}
  const engine={store,async verifyOwner(){trace.push(['owner']);if(fault.owner)fail(fault.owner);},async nativeUpdate(request){trace.push(['update',request]);if(fault.update){const code=fault.update;delete fault.update;fail(code);}return null;}};
  const FixedDate=class extends Date{constructor(){super(time);trace.push(['now']);}};
  const Driver=new Function('hashJson','IvyError','requireThat','same','finishedRun','mutation','scopedOperationId','intendedCall','retainNativeRequest','Date',stripTypeScriptTypes(driverSource,{mode:'transform'})+'\nreturn TaskBoardNativeDriver;')(hashJson,IvyError,requireThat,same,terminal,mutation,scoped,intent.intendedCall,intent.retainNativeRequest,FixedDate);
  trace.length=0;const driver=new Driver(engine),outcomes=[];
  for(const step of i.steps){
   if(step.kind==='resetScope'){scopes.clear();trace.push(['resetScope']);continue;}
   if(step.kind==='attach'){const r=latest('task-board/run',id(200)),t=latest('task-board/task',id(100));const next={...r.value,calls:{...r.value.calls,thread:pin(300)}};docs.push(doc('task-board/run',200,r.pin.revision+1,next));docs.push(doc('task-board/task',100,t.pin.revision+1,{...t.value,claim:{run:pin(200,r.pin.revision+1)}}));trace.push(['attach']);continue;}
   try{outcomes.push({ok:await driver.prepare(id(200),step.slot,step.origin)});}catch(e){if(!(e instanceof IvyError))throw new Error('Original driver fixture '+row.name,{cause:e});outcomes.push({error:{code:e.code,message:e.message,outcome:e.outcome}});}
  }
  const calls=docs.filter(d=>d.key==='task-board/native-call').map(d=>({pin:d.pin,value:d.value,name:d.name}));
  if(['new retained call','same owner replay','existing named call','already attached returns current call','reattach transition','steer retains update input','turn uses immutable original Task','UTF16 scoped identity'].includes(row.name))assert.ok(outcomes.every(o=>o.ok),row.name+JSON.stringify(outcomes));
  row.expectedHash=hashJson({outcomes,trace,calls});
 }
 groups.push({path:previous.path,base,rows});
}
process.stdout.write(JSON.stringify(groups));
