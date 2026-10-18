import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
registerHooks({resolve(specifier,context,next){
  if(specifier.endsWith('.js')&&specifier.startsWith('.')&&context.parentURL){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const {runtimeEpoch}=await import('../../packages/sdk/src/client.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const epoch='epoch-\ud800';
const failure=outcome=>new IvyError('service_unavailable','x'.repeat(2100)+'\udfff',outcome,{text:'\ud800',values:[null,'e\u0301']});
const error=value=>({code:value.code,message:value.message,outcome:value.outcome,details:value.details});
function deferred(){let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};}
function client(steps){
  const calls=[];let completed=0;
  return {calls,get completed(){return completed;},async request(method){
    calls.push(method);
    if(method==='initialize')return true;
    assert.equal(method,'system.status');assert.ok(steps.length,'unexpected epoch request');
    try{return await steps.shift()();}finally{completed++;}
  }};
}
const flush=()=>new Promise(resolve=>setImmediate(resolve));
const name=process.argv[2];let result;
if(name==='pending-success'){
  const gate=deferred(),c=client([()=>gate.promise]);
  const first=runtimeEpoch(c),second=runtimeEpoch(c);let settled=false;void first.then(()=>{settled=true;});
  const afterStart=[...c.calls];assert.deepEqual(afterStart,['system.status']);
  await c.request('initialize',{});const afterInitialize=[...c.calls],pendingBeforeRelease=!settled;
  assert.equal(pendingBeforeRelease,true);gate.resolve({runtimeEpoch:epoch});
  result={afterStart,afterInitialize,pendingBeforeRelease,values:await Promise.all([first,second]),cached:await runtimeEpoch(c),legacy:await runtimeEpoch(c),calls:c.calls};
}else if(name?.startsWith('failure-')){
  const outcome=name.slice('failure-'.length);assert.ok(['not_executed','unknown','completed'].includes(outcome));
  const gate=deferred(),original=failure(outcome),c=client([()=>gate.promise,async()=>({runtimeEpoch:'second'})]);
  const first=runtimeEpoch(c),second=runtimeEpoch(c),afterStart=[...c.calls];
  const rejected=Promise.allSettled([first,second]);gate.reject(original);
  const failed=await rejected;assert.ok(failed.every(value=>value.status==='rejected'&&value.reason===original));
  const recovered=await runtimeEpoch(c),retained=await Promise.allSettled([first,second]);
  result={afterStart,failures:failed.map(value=>error(value.reason)),recovered,retained:retained.map(value=>error(value.reason)),calls:c.calls};
}else if(name==='ready-failure'){
  const original=failure('unknown'),c=client([async()=>{throw original;},async()=>({runtimeEpoch:'second'})]);
  const first=runtimeEpoch(c),second=runtimeEpoch(c),afterStart=[...c.calls];
  assert.deepEqual(afterStart,['system.status']);
  const failed=await Promise.allSettled([first,second]);
  assert.ok(failed.every(value=>value.status==='rejected'&&value.reason===original));
  result={afterStart,failures:failed.map(value=>error(value.reason)),recovered:await runtimeEpoch(c),retained:error(await first.catch(value=>value)),calls:c.calls};
}else if(name==='abandoned-success'||name==='abandoned-failure'){
  const gate=deferred(),failed=name==='abandoned-failure',c=client([()=>gate.promise,async()=>({runtimeEpoch:'second'})]);
  // A rejection observer keeps this test process handled; no consumer awaits
  // or uses the result. SDK cache eviction must still run on its own.
  void runtimeEpoch(c).catch(()=>{});const afterStart=[...c.calls];
  if(failed)gate.reject(failure('unknown'));else gate.resolve({runtimeEpoch:epoch});
  await flush();assert.equal(c.completed,1);
  result={afterStart,completedBeforeRetry:c.completed,recovered:await runtimeEpoch(c),calls:c.calls,completed:c.completed};
}else throw Error('Unknown epoch oracle case');
process.stdout.write(JSON.stringify(result));
