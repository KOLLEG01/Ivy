import assert from 'node:assert/strict';
import {load,rootId,settings,owner,id,caller,scope,request,accepted,at} from './inbox-fixtures.mjs';
const {SecretaryEngine,operationName}=await load('services/secretary/src/engine.js');
const {IvyError,canonical,deriveOperationId}=await load('packages/sdk/src/node.js');
const config={...settings,identity:{...settings.identity,serviceNodeId:'fixture.secretary'},sources:[]};
const result=[];
const failure=e=>({code:e.code,message:e.message,outcome:e.outcome});
{
 const trace=[];let resolveEpoch;
 const pending=new Promise(resolve=>{resolveEpoch=resolve;});
 const client={request(method,p){trace.push({method,params:p});if(method==='system.status')return pending;if(method==='serviceNodes.get')return Promise.resolve(owner);if(method==='objects.stat')return Promise.resolve({effectivelyArchived:false});throw Error(method);}};
 const engine=new SecretaryEngine(client,config,{serviceNodeId:'fixture.secretary',generation:1},new AbortController().signal);
 const afterConstructor=structuredClone(trace);const initialize=engine.initialize(),afterInitializeCall=structuredClone(trace);await initialize;
 let settled=false;void engine.epoch.then(()=>{settled=true;});await Promise.resolve();assert.equal(settled,false);
 const initializedBeforeEpoch=true;resolveEpoch({runtimeEpoch:'fixture-epoch'});await engine.epoch;engine.store.close();
 assert.deepEqual(afterConstructor.map(r=>r.method),['system.status']);assert.deepEqual(afterInitializeCall.map(r=>r.method),['system.status','serviceNodes.get']);
 result.push({case:'constructor starts epoch; initialize completes independently',afterConstructor,afterInitializeCall,initializedBeforeEpoch,trace});
}
{
 const trace=[];let count=0;
 const client={request(method,p){trace.push({method,params:p});assert.equal(method,'system.status');return ++count===1?Promise.reject(new IvyError('fixture_epoch_failed','Original epoch failure.','unknown')):Promise.resolve({runtimeEpoch:'fixture-epoch'});}};
 const first=new SecretaryEngine(client,config,{serviceNodeId:'fixture.secretary',generation:1},new AbortController().signal);
 const firstResult=await first.epoch.catch(failure);
 const again=await first.validateOperationId(id).catch(failure);
 const second=new SecretaryEngine(client,config,{serviceNodeId:'fixture.secretary',generation:2},new AbortController().signal);const secondResult=await second.epoch;
 const failedStill=await first.validateOperationId(id).catch(failure);
 assert.deepEqual(again,firstResult);assert.deepEqual(failedStill,firstResult);assert.equal(secondResult,'fixture-epoch');assert.equal(count,2);
 first.store.close();second.store.close();result.push({case:'failed engine waiter stays failed; new constructor retries evicted cache',firstResult,again,secondResult,failedStill,trace});
}
{
 const trace=[];let resolveEpoch;
 const client={request(method,p){trace.push({method,params:p});assert.equal(method,'system.status');return new Promise(resolve=>{resolveEpoch=resolve;});}};
 const first=new SecretaryEngine(client,config,{serviceNodeId:'fixture.secretary',generation:1},new AbortController().signal);
 const second=new SecretaryEngine(client,config,{serviceNodeId:'fixture.secretary',generation:2},new AbortController().signal);
 const abandoned=first.epoch;first.store.close();resolveEpoch({runtimeEpoch:'fixture-epoch'});
 assert.equal(await second.epoch,'fixture-epoch');assert.equal(await abandoned,'fixture-epoch');assert.equal(trace.length,1);second.store.close();result.push({case:'two constructors share one started request; closing one journal leaves epoch work alive',trace});
}
{
 const trace=[];let rejectEpoch;
 const client={request(method,p){trace.push({method,params:p});assert.equal(method,'system.status');return new Promise((_,reject)=>{rejectEpoch=reject;});}};
 const engine=new SecretaryEngine(client,{...config,sources:settings.sources},{serviceNodeId:'fixture.secretary',generation:1},new AbortController().signal);
 const encoded=canonical(accepted);
 engine.store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run('retained','secretary/operation',operationName(caller,id),1,0,null,encoded);
 engine.store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run('retained',1,encoded);
 let settled=false;const work=engine.action(caller,request).catch(failure).finally(()=>{settled=true;});
 await Promise.resolve();await Promise.resolve();const pendingBeforeEpoch=!settled;
 rejectEpoch(new IvyError('fixture_epoch_failed','Original epoch failure.','unknown'));const outcome=await work;
 assert.equal(pendingBeforeEpoch,true);assert.equal(trace.length,1);engine.store.close();
 result.push({case:'replayed action waits for the original constructor epoch',pendingBeforeEpoch,outcome,trace});
}
{
 const trace=[];let resolveEpoch;
 const client={request(method,p){trace.push({method,params:p});if(method==='system.status')return new Promise(resolve=>{resolveEpoch=resolve;});if(method==='serviceNodes.get')return Promise.resolve(owner);throw Error(method);}};
 const engine=new SecretaryEngine(client,config,{serviceNodeId:'fixture.secretary',generation:1},new AbortController().signal,()=>new Date(at));
 const saved={operationId:id,callerPrincipalId:caller,action:'create_assignment',write:{mutationId:deriveOperationId(id,['secretary-workflow',caller])},phase:'succeeded',effect:{objectId:rootId,revision:7},errorCode:null};
 engine.store.technicalCreate('secretary/public-operation',operationName(caller,id),saved);
 const outcome=await engine.operationRead(caller,scope,id);
 const completedBeforeEpoch=true;resolveEpoch({runtimeEpoch:'fixture-epoch'});await engine.epoch;engine.store.close();
 result.push({case:'retained assignment operation read is independent of the constructor epoch',saved,completedBeforeEpoch,outcome,trace});
}
console.log(JSON.stringify({settings,config,owner,rootId,id,caller,scope,request,accepted,at,cases:result,scopeDescription:'Original constructor promise/request ordering reference for the Rust consumer.'}));
