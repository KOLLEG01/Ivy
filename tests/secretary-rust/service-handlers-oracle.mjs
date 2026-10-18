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
const settings={...base,identity:{...base.identity,serviceNodeId:'fixture.secretary'}};
const NativeDate=Date,wall=1800000000000;
globalThis.Date=class extends NativeDate {constructor(...a){super(...(a.length?a:[wall]));}static now(){return wall;}};
const state={calls:[],wakes:0,error:null,ownerError:null,options:null};
const callback=(method,args)=>{
 state.calls.push({method,arguments:structuredClone(args)});
 if(state.error)throw new IvyError(state.error.code,state.error.message,state.error.outcome);
 return {method,arguments:structuredClone(args)};
};
class Engine {
 constructor(client,config,owner,signal){Object.assign(this,{client,settings:config,owner,signal,recoveryIssues:new Map(),store:{close(){}}});}
 async initialize(){} async tick(){}
 authorize(...args){state.calls.push({method:'authorize',arguments:structuredClone(args)});}
 async verifyOwner(){state.calls.push({method:'verifyOwner',arguments:[]});if(state.ownerError)throw new IvyError(...state.ownerError);}
}
for(const method of ['listAssignments','getAssignment','saveConfiguration','saveAssignment','operation','operationRead','status','action'])Engine.prototype[method]=async function(...args){return callback(method,args);};
class FollowUp {constructor(engine){this.engine=engine;}async run(...args){return callback('followUp',args);}async read(...args){return callback('followUpRead',args);}}
class Scheduler {async initialize(){}async tick(){}}
class Runner {async drain(){}async tick(){}}
class Client {constructor(options){state.options=options;}start(){}async stop(){}}
class Health {async write(){}async control(){return null;}}
const directory=await mkdtemp(join(tmpdir(),'secretary-handlers-'));
const config={componentId:'secretary',credential:'fixture',serviceNodeId:'fixture.secretary',hostId:settings.identity.hostId,settings,dataRoot:join(directory,'data'),publicBaseUrl:'http://127.0.0.1:1',version:'test',buildId:'sha256:'+'a'.repeat(64)};
globalThis.__secretaryHandlerMocks={Engine,FollowUp,Scheduler,Runner,Client,Health,config};
const replacements={
 '../../../packages/sdk/src/node.js':`export * from ${JSON.stringify(actualSdk)};export const ServiceClient=globalThis.__secretaryHandlerMocks.Client;`,
 '../../../packages/sdk/src/host.js':'export const instanceConfig=async()=>globalThis.__secretaryHandlerMocks.config;export const configurationPath=()=>"fixture-config";export const HealthFile=globalThis.__secretaryHandlerMocks.Health;',
 './engine.js':'export const SecretaryEngine=globalThis.__secretaryHandlerMocks.Engine;',
 './follow-up.js':'export const SecretaryFollowUp=globalThis.__secretaryHandlerMocks.FollowUp;',
 './assignment-scheduler.js':'export const AssignmentScheduler=globalThis.__secretaryHandlerMocks.Scheduler;',
 './assignment-runner.js':'export const AssignmentRunner=globalThis.__secretaryHandlerMocks.Runner;',
};
const hooks=registerHooks({
 resolve(specifier,context,next){if(context.parentURL===mainUrl&&specifier in replacements)return {url:'secretary-handler:'+encodeURIComponent(specifier),shortCircuit:true};return next(specifier,context);},
 load(url,context,next){if(url.startsWith('secretary-handler:'))return {format:'module',source:replacements[decodeURIComponent(url.slice('secretary-handler:'.length))],shortCircuit:true};return next(url,context);},
});
const rows=[];let running;
try {
 const {startSecretary}=await import(mainUrl);
 running=await startSecretary('fixture-config');
 const handlers=state.options.handlers,names=Object.keys(handlers).map(k=>k.slice('secretary.'.length));
 async function add(method,label,c={}){
  state.calls=[];state.wakes=0;state.error=c.error??null;state.ownerError=c.ownerError??null;
  if(running.engine)running.engine.owner.generation=c.engineGeneration??7;
  const args='args' in c?structuredClone(c.args):{action:method,operationId:'op',expectedScope:settings.identity.scope,assignment:method==='createAssignment'?null:{objectId:'assignment',revision:3},value:{retained:true},itemId:'item',question:'Question'};
  const cancel=new AbortController();if(c.cancelled)cancel.abort(new IvyError('fixture_cancelled','Fixture cancelled.','unknown'));
  const context={callerPrincipalId:c.caller??'caller',generation:c.generation??7,signal:cancel.signal,...(c.noRoutedId?{}:{operationId:c.operationId??'op'})};
  let result;
  try{result={value:await handlers['secretary.'+method](args,context)};}catch(cause){const e=IvyError.from(cause);result={code:e.code,message:e.message,outcome:e.outcome};}
  rows.push({input:{method,label,args,engineGeneration:running.engine?running.engine.owner.generation:null,context:{caller:context.callerPrincipalId,generation:context.generation,operationId:context.operationId??null,cancelled:!!c.cancelled},error:state.error,ownerError:state.ownerError},expected:{result,calls:state.calls,wakes:state.wakes}});
 }
 for(const name of names)await add(name,'no current generation');
 const connection={generation:7,signal:new AbortController().signal,close(){}};
 await state.options.reconcile(connection);
 for(let i=0;i<32&&!running.engine;i++)await Promise.resolve();
 assert.ok(running.engine,'Original reconciliation must finish');
 // Count the real kick() boundary while keeping its independent background
 // workers outside this handler-only fixture. Service/engine work is separately
 // qualified; no copied handler body is evaluated here.
 running.engine.signal={get aborted(){state.wakes++;return true;}};
 for(const name of names){
  await add(name,'valid');await add(name,'stale generation',{generation:8,cancelled:true});
  await add(name,'cancelled caller',{cancelled:true});
  await add(name,'domain failure',{error:{code:'fixture_domain_failure',message:'Fixture domain failure.',outcome:'unknown'}});
  await add(name,'null arguments',{args:null});
 }
 for(const name of ['saveConfiguration','saveAssignment','createAssignment','updateAssignment','followUp','capture','assess','attach','checkpoint'])for(const op of [null,1,'different','op']){
  const args={action:name,operationId:op,assignment:null};await add(name,'routed identity '+JSON.stringify(op),{args});
 }
 for(const name of ['createAssignment','updateAssignment','saveAssignment'])for(const assignment of [undefined,null,{},[],false,{objectId:'original',revision:2}]){
  const args={operationId:'op',...(assignment===undefined?{}:{assignment})};await add(name,'assignment null gate '+JSON.stringify(assignment),{args});
 }
 for(const name of ['operation','operationRead','followUpRead'])for(const value of [undefined,null,true,42,[],[1,null,'two'],{}, {toString:null},'\ud800é😀']){
  const args={expectedScope:settings.identity.scope,...(value===undefined?{}:{operationId:value})};await add(name,'string coercion '+JSON.stringify(value),{args,noRoutedId:true});
 }
 for(const name of names)await add(name,'lossless caller',{caller:'caller-\ud800é😀'});
 await add('binding','binding owner rejected',{ownerError:['fixture_owner','Fixture owner failure.','unknown']});
 const valid=rows.filter(r=>r.input.label==='valid');assert.equal(valid.length,16);assert.ok(valid.every(r=>'value' in r.expected.result));
 assert.equal(valid.find(r=>r.input.method==='createAssignment').expected.calls[0].method,'saveAssignment');
 assert.equal(valid.find(r=>r.input.method==='followUpRead').expected.wakes,0);
 process.stdout.write(JSON.stringify({settings,wall,handlerNames:Object.keys(handlers),rows}));
}finally{if(running)await running.close();hooks.deregister();delete globalThis.__secretaryHandlerMocks;globalThis.Date=NativeDate;await rmdir(directory);}
