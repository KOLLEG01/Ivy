import assert from 'node:assert/strict';
import {registerHooks} from 'node:module';
import {mkdtemp,rmdir} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {load,oracleRoot,settings as base} from './inbox-fixtures.mjs';
const {IvyError}=await load('packages/sdk/src/node.js');
const actualSdk=pathToFileURL(join(oracleRoot,'packages/sdk/src/node.js')).href;
const mainUrl=pathToFileURL(join(oracleRoot,'services/secretary/src/main.js')).href;
const directory=await mkdtemp(join(tmpdir(),'secretary-reconcile-'));
const settings={...base,identity:{...base.identity,serviceNodeId:'fixture.secretary'}};
const config={componentId:'secretary',credential:'fixture',serviceNodeId:'fixture.secretary',hostId:settings.identity.hostId,settings,dataRoot:join(directory,'data'),publicBaseUrl:'http://127.0.0.1:1',version:'test',buildId:'sha256:'+'a'.repeat(64)};
const at='2027-01-15T08:00:00.000Z',NativeDate=Date;
globalThis.Date=class extends NativeDate{constructor(...args){super(...(args.length?args:[at]));}static now(){return NativeDate.parse(at);}};
let state;
function fail(phase,engine){
 if(phase!==state.phase||engine.id>state.failures)return;
 const error=new IvyError(state.code,'Fixture initialization failure.','unknown');
 if(state.abort)state.cancel.abort(error);
 throw error;
}
class Engine{
 constructor(client,settings,owner,signal){
  Object.assign(this,{id:++state.attempts,client,settings,owner,signal,recoveryIssues:new Map()});
  this.store={close:()=>state.closed.push(this.id)};
  fail('constructor',this);
 }
 async initialize(){fail('initialize',this);if(state.gate)await state.gate;}
 async tick(){fail('tick',this);}
}
class Scheduler{constructor(engine){this.engine=engine;}async initialize(){fail('scheduler',this.engine);}async tick(){}}
class Runner{async tick(){}async drain(){}}
class FollowUp{async tick(){}}
class Client{constructor(options){state.options=options;}start(){}async stop(){}}
class Health{async write(ready,detail,generation=null){state.health.push({ready,detail,generation});}async control(){return null;}}
globalThis.__secretaryReconcileMocks={Engine,Scheduler,Runner,FollowUp,Client,Health,config,
 delay:async(ms,_value,{signal})=>{state.delays.push(ms);signal.throwIfAborted();await Promise.resolve();signal.throwIfAborted();}};
const replacements={
 '../../../packages/sdk/src/node.js':`export * from ${JSON.stringify(actualSdk)};export const ServiceClient=globalThis.__secretaryReconcileMocks.Client;`,
 '../../../packages/sdk/src/host.js':'export const instanceConfig=async()=>globalThis.__secretaryReconcileMocks.config;export const configurationPath=()=>"fixture-config";export const HealthFile=globalThis.__secretaryReconcileMocks.Health;',
 'node:timers/promises':'export const setTimeout=globalThis.__secretaryReconcileMocks.delay;',
 './engine.js':'export const SecretaryEngine=globalThis.__secretaryReconcileMocks.Engine;',
 './assignment-scheduler.js':'export const AssignmentScheduler=globalThis.__secretaryReconcileMocks.Scheduler;',
 './assignment-runner.js':'export const AssignmentRunner=globalThis.__secretaryReconcileMocks.Runner;',
 './follow-up.js':'export const SecretaryFollowUp=globalThis.__secretaryReconcileMocks.FollowUp;',
};
const hooks=registerHooks({
 resolve(specifier,context,next){if(context.parentURL===mainUrl&&specifier in replacements)return {url:'secretary-reconcile:'+encodeURIComponent(specifier),shortCircuit:true};return next(specifier,context);},
 load(url,context,next){if(url.startsWith('secretary-reconcile:'))return {format:'module',source:replacements[decodeURIComponent(url.slice('secretary-reconcile:'.length))],shortCircuit:true};return next(url,context);},
});
const rows=[],retryRows=[];
let running,release;
const flush=()=>new Promise(resolve=>setImmediate(resolve));
async function waitFor(done){for(let i=0;i<128;i++){if(done()){await flush();return;}await flush();}assert.fail('Original reconciliation did not settle');}
function fresh(options={}){
 state={attempts:0,closed:[],delays:[],health:[],failures:0,phase:'initialize',code:'limit_exceeded',abort:false,...options,cancel:new AbortController(),connectionError:null};
 state.connection={generation:7,signal:state.cancel.signal,close(error){state.connectionError=error?{code:error.code,message:error.message,outcome:error.outcome}:{};}};
}
async function observe(label,generation=7,{recovering=false,failure=null}={}){
 const heartbeat=await state.options.readiness({...state.connection,generation});
 const health=state.health.at(-1);
 rows.push({label,input:{generation,engineGeneration:running.engine?.owner.generation??null,recovering,failure,issue:running.engine?.recoveryIssues.values().next().value??null,serviceNodeId:config.serviceNodeId,at},expected:{ready:health.ready,detail:health.detail,heartbeat}});
}
try{
 const {startSecretary}=await import(mainUrl);
 fresh({gate:new Promise(resolve=>{release=resolve;})});
 running=await startSecretary('fixture-config');
 await observe('before reconciliation');
 await state.options.reconcile(state.connection);
 await observe('initialization pending',7,{recovering:true});
 release();await waitFor(()=>!!running.engine);
 await observe('reconciled');
 running.engine.recoveryIssues.set('first','first_issue');running.engine.recoveryIssues.set('second','second_issue');
 await observe('first independent warning');await observe('stale generation',8);
 running.engine.recoveryIssues.delete('first');await observe('next independent warning');
  running.engine.recoveryIssues.clear();await observe('warnings cleared');
  running.engine.recoveryIssues.set('first','');await observe('empty warning code');
  running.engine.recoveryIssues.set('first','warning-\ud800');await observe('UTF-16 warning code');
 await running.close();running=null;
 for(const input of [
  {failures:0},{failures:1},{failures:5},{failures:6},
  {failures:6,code:'fixture_invalid'},
  {failures:1,abort:true},
  {failures:1,phase:'constructor'},
  {failures:1,phase:'tick'},
  {failures:1,phase:'scheduler'},
 ]){
  fresh(input);running=await startSecretary('fixture-config');
  await state.options.reconcile(state.connection);
  await waitFor(()=>!!running.engine||state.connectionError!==null||state.cancel.signal.aborted);
  const expected={attempts:state.attempts,delays:state.delays,closed:[...state.closed],reconciled:!!running.engine,connectionError:state.connectionError};
  retryRows.push({input:{failures:state.failures,code:state.code,phase:state.phase,abort:state.abort},expected});
  if(state.connectionError?.code)await observe('initialization failure: '+state.code,7,{failure:state.code});
  await running.close();running=null;
 }
 process.stdout.write(JSON.stringify({rows,retryRows})+'\n');
}finally{
 release?.();await running?.close();hooks.deregister();delete globalThis.__secretaryReconcileMocks;globalThis.Date=NativeDate;
 await rmdir(directory);
}
