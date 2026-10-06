import test from 'node:test';
import assert from 'node:assert/strict';
import { NativeOperations } from '../services/agent-manager/src/operations.js';
import { NativeJournal } from '../services/agent-manager/src/journal.js';
import type { LocalAgentState, SavedState } from '../services/agent-manager/src/hive-state.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';

test('local durable claims precede native effects, survive empty service memory and prevent competing dispatch', async () => {
  const rows=new Map<string,SavedState<unknown>>();let unavailable=false;
  const state:Pick<LocalAgentState,'serviceNodeId'|'read'|'write'>={serviceNodeId:'agent',
    read:async <T>(_kind:string,key:string)=>structuredClone(rows.get(key)??null) as SavedState<T>|null,
    write:async <T>(_kind:string,key:string,value:T,previous?:{objectId:string;revision:number})=>{
      if(unavailable)throw new IvyError('service_unavailable','Hive is unavailable.');
      const prior=rows.get(key);if(prior?.pin.revision!==previous?.revision)throw new IvyError('revision_conflict','Another owner claimed the operation.');
      const saved={value:structuredClone(value),pin:{objectId:key,revision:(prior?.pin.revision??0)+1}};rows.set(key,saved);return saved;
    }};
  const owner=()=>{const memory=new NativeJournal({hostId:'host',serviceNodeId:'agent',nativeVersion:'test',nativeExecutableHash:digest('native')},
    {maxOperations:100,maxJournalBytes:128*1024*1024,maxPendingInputs:16,maxNotificationBytes:1048576});memory.beginEpoch(digest(Math.random().toString()).slice(7));return new NativeOperations(state,memory);};
  let sends=0;const key={callerPrincipalId:'caller',operationId:'original'},first=owner();
  const execute=(manager:NativeOperations,identity=key)=>manager.run(identity,'thread/start',{},async()=>{
    assert.ok(rows.size);sends++;const id=manager.requestId(identity),epoch=manager.memory.epoch!;
    manager.memory.dispatch(identity,epoch,id);return manager.memory.finish(identity,epoch,id,{result:{thread:'original'}});
  });
  unavailable=true;await assert.rejects(execute(first),{code:'service_unavailable'});assert.equal(sends,0);assert.equal(first.memory.get(key),null);
  unavailable=false;const saved=await execute(first);first.memory.close();
  assert.deepEqual(await execute(owner()),saved);assert.equal(sends,1);
  const closed={...key,operationId:'closed-before-send'},refused=owner();
  const prevented=await refused.run(closed,'thread/start',{},async()=>{throw new IvyError('service_not_ready','Provider connection is closed.','not_executed');});
  assert.equal(prevented.phase,'failed');assert.equal(prevented.code,'service_not_ready');assert.equal(prevented.requestId,null);
  refused.memory.close();assert.deepEqual(await execute(owner(),closed),prevented);assert.equal(sends,1);
  const competing={...key,operationId:'competing'};await Promise.all([execute(owner(),competing),execute(owner(),competing)]);assert.equal(sends,2);
  const lost={...key,operationId:'lost-result'},dying=owner();
  await assert.rejects(dying.run(lost,'thread/start',{},async()=>{
    sends++;dying.memory.dispatch(lost,dying.memory.epoch!,dying.requestId(lost));throw new Error('Abrupt owner loss');
  }));dying.memory.close();
  assert.equal((await execute(owner(),lost)).phase,'outcome_unknown');assert.equal(sends,3);
  const pending={...key,operationId:'pending'},manager=owner();let release!:()=>void;
  const barrier=new Promise<void>(resolve=>{release=resolve;});
  const running=manager.run(pending,'thread/start',{},async()=>{manager.memory.dispatch(pending,manager.memory.epoch!,manager.requestId(pending));await barrier;return manager.memory.get(pending)!;});
  const duplicate=await manager.run(pending,'thread/start',{},async()=>{assert.fail('Duplicate reached native dispatch');});
  assert.ok(['accepted','dispatched'].includes(duplicate.phase));release();await running;
});
