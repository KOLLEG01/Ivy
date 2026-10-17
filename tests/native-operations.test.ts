import test from 'node:test';
import assert from 'node:assert/strict';
import { NativeOperations } from '../services/agent-manager/src/operations.js';
import { NativeJournal } from '../services/agent-manager/src/journal.js';
import { LocalAgentState } from '../services/agent-manager/src/hive-state.js';
import type { SavedState } from '../services/agent-manager/src/hive-state.js';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { operationId } from '../packages/contracts/src/operation-id.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';

test('local receipt history evicts a completed original beyond the configured count without a time guarantee', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-native-capacity-')), path = join(root, 'journal.sqlite');
  const state = new LocalAgentState(path, 'agent', { maxOperations: 10_002, maxJournalBytes: 64 * 1024 * 1024 });
  t.after(() => { state.close(); rmSync(root, { recursive: true, force: true }); });
  state.setRuntimeEpoch('runtime');
  const db = new DatabaseSync(path);
  db.prepare(`WITH RECURSIVE rows(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM rows WHERE i<10000)
    INSERT INTO state SELECT 'operation','retained-'||i,1,'{}',2,?,? FROM rows`).run(Date.now(), Date.now());
  db.close();
  const original = { operationId: operationId('runtime'), phase: 'succeeded', reply: { result: {} } };
  await state.write('operation', 'original', original);
  await state.write('operation', 'last', { ...original, operationId: operationId('runtime') });
  await state.write('operation', 'over-limit', { ...original, operationId: operationId('runtime') });
  assert.equal(await state.read('operation', 'original'), null);
  assert.ok(await state.read('operation', 'over-limit'));
  await assert.rejects(state.write('operation', 'original', original), { code: 'operation_expired' });
});

test('completion ownership retains the exact native turn caller after restart and settles an immediate completion', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-native-notice-')), path = join(root, 'journal.sqlite');
  const limits = { maxOperations: 100, maxJournalBytes: 64 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 };
  let state = new LocalAgentState(path, 'agent', limits);
  t.after(() => { state.close(); rmSync(root, { recursive: true, force: true }); });
  state.setRuntimeEpoch('runtime');
  const memory = new NativeJournal({ hostId: 'host', serviceNodeId: 'agent', nativeVersion: 'test', nativeExecutableHash: digest('native') }, limits);
  memory.beginEpoch('native-epoch');
  const manager = new NativeOperations(state, memory), key = { callerPrincipalId: 'workflow-caller', operationId: operationId('runtime') };
  let release!: () => void;
  const barrier = new Promise<void>(resolve => { release = resolve; });
  const run = manager.run(key, 'turn/start', { threadId: 'managed-thread' }, async () => {
    memory.dispatch(key, 'native-epoch', manager.requestId(key)); await barrier;
    return memory.finish(key, 'native-epoch', manager.requestId(key), { result: { turn: { id: 'managed-turn' } } });
  });
  let settled = false;
  const completion = manager.settleTurn('managed-thread').then(() => { settled = true; });
  await Promise.resolve(); assert.equal(settled, false);
  release(); await run; await completion;
  assert.equal(state.completionCaller('managed-thread', 'managed-turn'), 'workflow-caller');
  assert.equal(state.completionCaller('managed-thread', 'another-turn'), null);
  assert.equal(state.completionCaller('another-thread', 'managed-turn'), null);
  state.close(); state = new LocalAgentState(path, 'agent', limits);
  assert.equal(state.completionCaller('managed-thread', 'managed-turn'), 'workflow-caller');
});

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
