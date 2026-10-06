import test from 'node:test';
import assert from 'node:assert/strict';
import { triageFixture } from './secretary-triage-fixture.mjs';
import { principal, assessment } from './secretary-fixture.mjs';
import { SecretaryStore } from '../dist/services/secretary/src/store.js';

test('terminal technical cleanup keeps its first completion deadline',()=>{
  const store=new SecretaryStore({},'root');
  try{
    const first=Date.parse('2026-09-01T00:00:00.000Z');store.completeTechnicalWorkflow('work',first);store.completeTechnicalWorkflow('work',first+23*60*60*1000);
    assert.equal(Number(store.db.prepare('SELECT delete_after FROM technical_cleanup WHERE root_id=?').get('work').delete_after),first+24*60*60*1000);
  }finally{store.close();}
});

async function supersede(f, item, worker, expected = 'pending') {
  const other = assessment({ disposition: 'record', notification: 'none', summary: 'An admitted agent has already assessed this message.' });
  await f.f.engine.action(principal, f.f.assessRequest(item, other));
  assert.equal(await worker.step(item), expected);
  return other;
}

function completeOriginal(f, operation) {
  const threadId = [...f.state.threads.keys()][0];
  const result = operation.method === 'thread/start'
    ? { thread: f.state.threads.get(threadId), model: operation.params.model }
    : { turn: { id: f.state.turns.get(threadId), status: 'inProgress', items: [] } };
  f.state.operations.set(operation.operationId, { ...operation, phase: 'succeeded', code: null,
    updatedAt: new Date(Date.parse(operation.updatedAt) + 1000).toISOString(), reply: { result } });
  f.state.phase = 'succeeded';
}

test('superseded triage recovers a late original start receipt and releases only its own idle thread', { timeout: 90000 }, async t => {
  for (const slot of ['thread', 'turn']) {
    const f = await triageFixture(t), item = await f.capture(), worker = f.worker;
    if (slot === 'turn') {
      const work = await worker.native.prepare(item), plan = await worker.native.plan(work);
      await worker.native.advance(plan, await worker.native.call(plan, 'thread', 'thread/start'));
    }
    f.state.phase = 'dispatched';
    assert.equal(await worker.step(item), 'pending');
    const original = structuredClone(f.state.dispatches.at(-1));
    assert.equal(original.method, slot === 'thread' ? 'thread/start' : 'turn/start');
    const decision = await supersede(f, item, worker), starts = f.state.dispatches.length;
    await f.restart();
    await f.worker.tick();
    assert.equal(f.state.dispatches.length, starts, 'A pending original operation cannot be replaced or guessed idle.');
    assert.equal((await f.worker.native.work(item)).value.phase, 'pending', 'The original native work retains its reservation while its receipt is pending.');

    f.state.absent = true; await f.worker.tick();
    assert.ok([...f.f.engine.recoveryIssues.values()].includes('secretary_triage_absence_conflict'));
    assert.equal(f.state.dispatches.length, starts, 'A previously observed start cannot later authorize dispatch by absence.');
    f.state.absent = false;
    completeOriginal(f, original); f.state.terminal = 'inProgress';
    await f.worker.tick();
    if (slot === 'turn') {
      assert.equal(f.state.dispatches.length, starts, 'A completed turn/start receipt is not a completed model turn.');
      f.state.terminal = 'completed'; await f.worker.tick();
    }
    assert.deepEqual(f.state.dispatches.map(call => call.method), slot === 'thread'
      ? ['thread/start', 'thread/unsubscribe'] : ['thread/start', 'turn/start', 'thread/unsubscribe']);
    assert.equal(f.state.dispatches.at(-1).params.threadId, [...f.state.threads.keys()][0]);
    assert.equal((await f.worker.native.work(item)).value.phase, 'superseded');
    assert.equal(f.f.engine.recoveryIssues.size, 0, JSON.stringify([...f.f.engine.recoveryIssues]));
    assert.deepEqual((await f.f.engine.item(item.objectId)).value.decision.assessment, decision);
    assert.equal((await f.f.engine.item(item.objectId)).pin.revision, 2);
    await f.restart(); f.state.offline = true; await f.worker.tick();
    assert.equal(f.state.dispatches.length, starts + 1, 'Saved terminal cleanup survives restart without another native request.');
    assert.equal(f.f.engine.recoveryIssues.size, 0, JSON.stringify([...f.f.engine.recoveryIssues]));
  }
});

test('superseded work never dispatches a prepared thread or turn whose original operation is absent', { timeout: 60000 }, async t => {
  for (const slot of ['thread', 'turn']) {
    const f = await triageFixture(t), item = await f.capture(), worker = f.worker;
    const work = await worker.native.prepare(item), plan = await worker.native.plan(work);
    const thread = await worker.native.call(plan, 'thread', 'thread/start');
    if (slot === 'turn') {
      const started = await worker.native.advance(plan, thread);
      await worker.native.call(plan, 'turn', 'turn/start', worker.native.threadId(started));
    }
    const decision = await supersede(f, item, worker, 'superseded');
    await f.restart(); await f.worker.tick();
    assert.deepEqual(f.state.dispatches.map(call => call.method), slot === 'thread' ? [] : ['thread/start', 'thread/unsubscribe']);
    assert.deepEqual((await f.f.engine.item(item.objectId)).value.decision.assessment, decision);
    assert.equal(f.f.engine.recoveryIssues.size, 0, JSON.stringify([...f.f.engine.recoveryIssues]));
  }
});

test('a competing assessment during native recovery clears the resolved warning while preserving the winning decision', { timeout: 60000 }, async t => {
  const f = await triageFixture(t), item = await f.capture(), worker = f.worker;
  f.state.terminal = 'inProgress'; assert.equal(await worker.step(item), 'pending');
  f.state.offline = true; await worker.tick();
  assert.ok([...f.f.engine.recoveryIssues.values()].includes('service_not_ready'));
  f.state.offline = false; f.state.terminal = 'completed';
  const other = assessment({ disposition: 'record', notification: 'none', summary: 'The explicit assessment won during native recovery.' });
  let raced = false;
  f.state.after = async (method, args) => {
    if (!raced && method === 'tools.call' && args.qualifiedName === 'agent.status') {
      raced = true; await f.f.engine.action(principal, f.f.assessRequest(item, other));
    }
  };
  await worker.tick(); assert.equal(raced, true);
  assert.equal((await worker.native.work(item)).value.phase, 'pending', 'The racing decision cannot release a native reservation before cleanup.');
  assert.deepEqual((await f.f.engine.item(item.objectId)).value.decision.assessment, other);
  assert.equal((await f.f.engine.item(item.objectId)).pin.revision, 2);
  assert.equal(f.f.engine.recoveryIssues.size, 0, JSON.stringify([...f.f.engine.recoveryIssues]));
  await worker.tick();
  assert.equal((await worker.native.work(item)).value.phase, 'superseded');
  assert.deepEqual(f.state.dispatches.map(call => call.method), ['thread/start', 'turn/start', 'thread/unsubscribe']);
  assert.equal(f.f.engine.recoveryIssues.size, 0, JSON.stringify([...f.f.engine.recoveryIssues]));
});

test('a competing assessment keeps the native concurrency reservation until original cleanup is confirmed', { timeout: 60000 }, async t => {
  const f = await triageFixture(t); f.settings.maximumConcurrent = 1;
  const item = await f.capture(), worker = f.worker;
  f.state.terminal = 'inProgress'; assert.equal(await worker.step(item), 'pending');
  const waiting = await f.capture(), decision = await supersede(f, item, worker);
  await f.restart(); await f.worker.tick();
  assert.equal(f.state.dispatches.filter(call => call.method === 'turn/start').length, 1);
  assert.equal((await f.worker.native.work(item)).value.phase, 'pending');
  assert.equal(await f.worker.native.work(waiting), null);
  assert.equal((await f.f.engine.item(waiting)).value.decision, null);
  assert.deepEqual((await f.f.engine.item(item.objectId)).value.decision.assessment, decision);
  f.state.terminal = 'completed';
  for (let i = 0; i < 3 && !(await f.f.engine.item(waiting.objectId)).value.decision; i++) await f.worker.tick();
  assert.equal((await f.worker.native.work(item)).value.phase, 'superseded');
  assert.ok((await f.f.engine.item(waiting.objectId)).value.decision);
  assert.deepEqual(f.state.dispatches.map(call => call.method), ['thread/start', 'turn/start', 'thread/unsubscribe', 'thread/resume', 'turn/start']);
});
