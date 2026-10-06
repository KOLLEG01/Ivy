import { assignmentFor } from './secretary-fixture.mjs';
import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { resolve } from 'node:path';
import { hashJson, IvyError, defaultNativeThreadProject } from '../dist/packages/sdk/src/node.js';
import { AssignmentRunner } from '../dist/services/secretary/src/assignment-runner.js';
import { defaultConfiguration } from '../dist/services/secretary/src/assignment-schema.js';
import { SecretaryStore } from '../dist/services/secretary/src/store.js';
import { validate } from '../dist/services/secretary/src/schema.js';

const at = '2026-09-22T08:00:00.000Z';

function fixture(t) {
  let now = new Date(at);
  const epoch = randomUUID(), rows = new Map(), operations = new Map(), calls = [], reads = [];
  const settings = { identity: { principalId: 'secretary', hostId: 'fixture' }, recordsPerTick: 2,
     policy: { silentTime: null, minimumMainUrgency: 'normal', researchMaxMinutes: 5, voiceEscalation: null } };
  const matches = (row, filter) => {
    if (filter.op === 'and') return filter.args.every(value => matches(row, value));
    if (filter.op === 'or') return filter.args.some(value => matches(row, value));
    if (filter.op === 'not') return !matches(row, filter.arg);
    const value = filter.field === 'object.parentId' ? 'root' : filter.field.slice('data:/'.length).split('/').reduce((value, key) => value?.[key], row.value);
    if (filter.op === 'isNull') return value == null;
    return filter.op === 'eq' ? value === filter.value : filter.op === 'ne' ? value !== filter.value : filter.op === 'in' ? filter.value.includes(value) : value <= filter.value;
  };
  const client = { async request(method, args) {
    if (method === 'system.status') return { runtimeEpoch: epoch };
    assert.equal(method, 'objects.query');
    const items = [...rows.values()].filter(row => matches(row, args.where) && (!args.cursor || row.pin.objectId > args.cursor))
      .sort((a, b) => a.pin.objectId.localeCompare(b.pin.objectId));
    const page = items.slice(0, args.limit);
    return { items: page.map(row => ({ objectId: row.pin.objectId, revision: row.pin.revision })), nextCursor: items.length > page.length ? page.at(-1).pin.objectId : null };
  } };
  const store = new SecretaryStore(client, 'root'); t.after(() => store.close());
  store.read = async (_key, pin) => {
    const row = rows.get(typeof pin === 'string' ? pin : pin.objectId); assert.ok(row); return structuredClone(row);
  };
  store.amend = async (current, key, value) => {
    assert.equal(key, 'secretary/execution'); validate('Execution', value);
    assert.equal(rows.get(current.pin.objectId).pin.revision, current.pin.revision);
    const next = { pin: { ...current.pin, revision: current.pin.revision + 1 }, value: structuredClone(value) };
    rows.set(next.pin.objectId, next); return structuredClone(next);
  };
  const state = { answer: JSON.stringify({ schemaVersion: 1, urgency: 'normal', notification: 'none', text: 'Done.', reason: 'Journal only.' }), status: 'completed', missingAnswer: false, unavailable: false, foreignReply: false };
  const native = {
    async threadStartParams(params) { return defaultNativeThreadProject(params, { source: 'native', observedAt: at,
      defaults: { projectRoot: resolve('secretary-test-project'), internalProjectRoot: resolve('secretary-test-project') },
      projects: [{ nativeId: state.projectId ?? 'internal-project', source: 'native', name: 'Internal', paths: [resolve('secretary-test-project')] }] }); },
    async binding(method) { return { definitionHash: hashJson(method) }; },
    checkOperation(observed, call) {
      assert.equal(observed.operationId, call.operationId, 'The returned operation must belong to the original call.');
      assert.equal(observed.method, call.method); assert.deepEqual(observed.params, call.params);
    },
    async operation(call) {
      if (state.unavailable) throw new IvyError('outcome_unknown', 'Response lost.', 'unknown');
      const found = operations.get(call.operationId);
      if (found) { this.checkOperation(found, call); return structuredClone(found); }
      return { operationId: call.operationId };
    },
    async dispatch(call, guard) {
      await guard(); calls.push(structuredClone(call));
      const result = call.method === 'thread/start' ? { thread: { id: 'thread-' + calls.length } }
        : call.method === 'turn/start' ? { turn: { id: 'turn-' + calls.length } } : {};
      const observed = { ...structuredClone(call), phase: 'succeeded', reply: { result } };
      operations.set(call.operationId, observed);
      return state.foreignReply ? { ...observed, operationId: 'another-operation' } : structuredClone(observed);
    },
    async status() { return { epoch: 'native-epoch' }; },
    checkRead(read, method, params, epoch) { assert.equal(read.method, method); assert.deepEqual(read.params, params); assert.equal(read.epoch, epoch); },
    async read(method, params) {
      reads.push({ method, params });
      if (method === 'thread/items/list') return { method, params, epoch: 'native-epoch', reply: { error: { code: -32601, message: 'Fixture item pagination unavailable.' } } };
      const row = [...rows.values()].find(row => row.value.threadId === params.threadId && row.value.phase === 'running');
      return { method, params, epoch: 'native-epoch', reply: { result: { data: [{ id: row.value.turnId, status: state.status, itemsView: params.itemsView,
        items: params.itemsView !== 'full' || state.missingAnswer ? [] : [{ id: 'answer', type: 'agentMessage', phase: 'final_answer', text: state.answer }] }] } } };
    },
  };
  const engine = { client, store, settings, signal: new AbortController().signal, now: () => now,
    recoveryIssues: new Map(), async verifyOwner() {}, issue(key, code) { this.recoveryIssues.set(key, code); } };
  const runner = () => { const value = new AssignmentRunner(engine); value.owner = () => native; value.delivery.step = async () => true; return value; };
  const add = (id, changes = {}) => {
    const assignment = { ...assignmentFor(at), assignmentId: id, builtInKey: null, enabled: true,
      execution: { model: null, effort: null, reuse: 'new' } };
    const executionTarget = { serviceNodeId: 'agent', threadCwd: resolve('secretary-test-project'), model: null, effort: 'medium', permissions: ':read-only' };
    const value = { schemaVersion: 1, executionId: id, assignment: { objectId: 'assignment-' + id, revision: 1 }, assignmentSnapshot: assignment,
      trigger: { kind: 'event', key: 'event:' + id, occurredAt: at, payload: {} }, effectiveRules: defaultConfiguration(settings, at).rules,
      executionTarget, phase: 'queued', serviceNodeId: 'agent',
      nativeTarget: { serviceNodeId: 'agent', hostId: 'fixture', nativeVersion: 'test', nativeExecutableHash: hashJson('native'), catalogHash: hashJson('catalog') },
      threadId: null, turnId: null, nativeOperations: { threadStart: null, turnStart: null, archive: null, delete: null }, preflightOutput: null,
      result: null, errorCode: null, createdAt: at, startedAt: null, completedAt: null, archivedAt: null, deleteAfter: null, deletedAt: null, updatedAt: at, ...changes };
    const row = { pin: { objectId: id, revision: 1 }, value }; validate('Execution', value); rows.set(id, row); return row;
  };
  return { engine, native, runner, add, rows, calls, reads, state, setNow(value) { now = new Date(value); } };
}

test('assignment execution starts once, archives its terminal task and deletes it after retention', async t => {
  const f = fixture(t), runner = f.runner(); f.add('one');
  await runner.tick(); assert.equal(f.rows.get('one').value.phase, 'running');
  assert.equal(f.calls[0].params.projectId, 'internal-project');
  await runner.tick(); assert.equal(f.rows.get('one').value.phase, 'archived');
  assert.deepEqual(f.calls.map(call => call.method), ['thread/start', 'turn/start', 'thread/archive']);
  f.setNow('2026-09-29T08:00:00.000Z');
  await f.runner().tick(); assert.equal(f.rows.get('one').value.phase, 'deleted');
  assert.equal(f.calls.at(-1).method, 'thread/delete');
  assert.equal(f.engine.store.technicalList('secretary/assignment-native-call').length, 0, 'Confirmed progress releases prepared call payloads.');
});

test('starting an execution preserves its admitted contact rules', async t => {
  const f = fixture(t), row = f.add('one'); row.value.assignmentSnapshot.builtInKey = 'retained-provenance';
  const rules = structuredClone(row.value.effectiveRules);
  await f.runner().tick();
  assert.equal(f.rows.get('one').value.phase, 'running');
  assert.deepEqual(f.rows.get('one').value.effectiveRules, rules);
});

test('running assignments poll metadata without loading contents until completion', async t => {
  const f = fixture(t), runner = f.runner(); f.add('one'); f.state.status = 'inProgress';
  await runner.tick(); for (let count = 0; count < 4; count++) await runner.tick();
  assert.ok(f.reads.length); assert.ok(f.reads.every(read => read.method === 'thread/turns/list' && read.params.itemsView === 'notLoaded'));
  f.state.status = 'completed'; await runner.tick(); assert.equal(f.rows.get('one').value.phase, 'archived');
  assert.equal(f.reads.filter(read => read.params.itemsView === 'full').length, 1);
});

test('preflight finishes before task creation and an uncertain earlier command is never replayed', async t => {
  const f = fixture(t), runner = f.runner();
  const row = f.add('one');
  row.value.executionTarget.threadCwd = process.cwd();
  row.value.assignmentSnapshot.preflight = { executable: process.execPath, args: ['-e', 'process.stdout.write("prepared")'], timeoutMs: 10000 };
  await runner.tick();
  assert.equal(f.calls.length, 0);
  await runner.drain();
  assert.equal(f.rows.get('one').value.phase, 'starting');
  assert.equal(f.rows.get('one').value.preflightOutput, 'prepared');
  await runner.tick();
  assert.equal(f.rows.get('one').value.phase, 'running');
  const uncertain = f.add('two', { phase: 'preflight', preflightOutput: '' });
  uncertain.value.assignmentSnapshot.preflight = row.value.assignmentSnapshot.preflight;
  await f.runner().tick();
  assert.equal(f.rows.get('two').value.phase, 'failed');
  assert.equal(f.engine.recoveryIssues.get('execution:two'), 'secretary_preflight_outcome_unknown');
});

test('failed turns and invalid terminal answers settle and release reused task leases', async t => {
  for (const scenario of [
    { status: 'failed', code: 'secretary_execution_turn_failed' },
    { answer: 'invalid JSON', code: 'secretary_assignment_result_invalid' },
    { missingAnswer: true, code: 'secretary_execution_answer_missing' },
    { answer: 'x'.repeat(131073), code: 'secretary_execution_answer_invalid' },
  ]) {
    for (const reuse of ['new', 'assignment']) {
      const f = fixture(t), row = f.add('one', { phase: 'running', threadId: 'thread-one', turnId: 'turn-one' });
      row.value.assignmentSnapshot.execution.reuse = reuse;
      Object.assign(f.state, scenario);
      if (reuse === 'assignment') f.engine.store.technicalCreate('secretary/thread-lease', 'assignment-one', {
        serviceNodeId: 'agent', threadCwd: row.value.executionTarget.threadCwd, createdByExecutionId: 'one', activeExecutionObjectId: 'one', threadId: 'thread-one',
      });
      await f.runner().tick();
      assert.equal(f.rows.get('one').value.phase, reuse === 'new' ? 'archived' : 'failed');
      assert.equal(f.rows.get('one').value.errorCode, scenario.code);
      if (reuse === 'assignment') assert.equal(f.engine.store.technicalNamed('secretary/thread-lease', 'assignment-one').value.activeExecutionObjectId, null);
    }
  }
});

test('reused tasks recover the prior task identity after completion was saved before its lease', async t => {
  const f = fixture(t), previous = f.add('previous', { phase: 'completed', threadId: 'original-thread', turnId: 'original-turn', result: 'Done.', completedAt: at });
  previous.value.assignmentSnapshot.execution.reuse = 'main';
  const next = f.add('next'); next.value.assignmentSnapshot.execution.reuse = 'main';
  f.engine.store.technicalCreate('secretary/thread-lease', 'main', { serviceNodeId: 'agent', threadCwd: next.value.executionTarget.threadCwd,
    createdByExecutionId: 'previous', activeExecutionObjectId: 'previous', threadId: null });
  await f.runner().tick();
  assert.equal(f.calls[0].method, 'thread/resume');
  assert.equal(f.calls[0].params.threadId, 'original-thread');
  assert.equal(f.calls[0].params.excludeTurns, true);
  assert.equal(f.rows.get('next').value.threadId, 'original-thread');
});

test('an unresolved old deletion cannot starve later expired tasks', async t => {
  const f = fixture(t), runner = f.runner();
  for (const id of ['a', 'b', 'c']) f.add(id, { phase: 'archived', threadId: 'thread-' + id, completedAt: at, archivedAt: at, deleteAfter: at });
  // Already-failed delete operations leave these rows in the due set.
  runner.remove = async current => {
    if (current.value.executionId !== 'c') throw new IvyError('secretary_execution_native_failed', 'Deletion failed.');
    return f.engine.store.amend(current, 'secretary/execution', { ...current.value, phase: 'deleted', deletedAt: at });
  };
  // Keep the first two archived, as happens when admission itself cannot be persisted.
  const update = runner.update.bind(runner);
  runner.update = async (current, value) => {
    if (['a', 'b'].includes(value.executionId) && value.phase === 'deleting') throw new IvyError('limit_exceeded', 'Temporary per-record failure.');
    return update(current, value);
  };
  await runner.tick(); await runner.tick();
  assert.equal(f.rows.get('c').value.phase, 'deleted');
});

test('completed history and pending deliveries do not delay active executions', async t => {
  const f = fixture(t), runner = f.runner();
  for (const id of ['a', 'b', 'c', 'd', 'e', 'f']) {
    const row = f.add(id, { phase: id < 'd' ? 'settled' : 'completed', threadId: 'thread-' + id, turnId: 'turn-' + id, result: 'Done.', completedAt: at });
    row.value.assignmentSnapshot.execution.reuse = 'assignment';
    if (id >= 'e') row.value.assignmentSnapshot.builtInKey = 'retained-provenance';
  }
  const delivered = [];
  runner.delivery.step = async row => { delivered.push(row.value.executionId); return row.value.executionId === 'new'; };
  f.add('abandoned', { phase: 'archived', result: 'Done.', completedAt: at,
    delivery: { executionId: 'abandoned', resultHash: hashJson('Done.'), operationId: null, request: null,
      state: 'outcome_unknown', reason: 'not_found', evidence: null, expiresAt: null } });
  f.add('new');
  await runner.tick();
  assert.equal(f.rows.get('new').value.phase, 'running');
  assert.deepEqual(delivered, ['d', 'e']);
  await runner.tick();
  assert.equal(f.rows.get('new').value.phase, 'archived');
  assert.deepEqual(delivered, ['d', 'e', 'new', 'f']);
});

test('native recovery retains the original prepared parameters across prompt changes', async t => {
  const f = fixture(t), runner = f.runner(), operationId = randomUUID();
  const dispatch = f.native.dispatch.bind(f.native);
  f.native.dispatch = async (...args) => { await dispatch(...args); f.state.unavailable = true; return undefined; };
  await assert.rejects(runner.completed(f.native, operationId, 'thread/start', { developerInstructions: 'Original instructions' }), { code: 'outcome_unknown' });
  f.state.unavailable = false; f.state.projectId = 'changed-default-project';
  const result = await f.runner().completed(f.native, operationId, 'thread/start', { developerInstructions: 'Updated instructions' });
  assert.equal(result.params.developerInstructions, 'Original instructions');
  assert.equal(result.params.projectId, 'internal-project');
  assert.equal(f.calls.length, 1);
});

test('failed native starts release their prepared calls once the failure is saved', async t => {
  for (const method of ['thread/start', 'turn/start']) {
    const f = fixture(t), runner = f.runner(); f.add('one');
    const dispatch = f.native.dispatch.bind(f.native);
    f.native.dispatch = async (...args) => {
      const result = await dispatch(...args);
      return result.method === method ? { ...result, phase: 'failed', reply: { error: { code: -1, message: 'Start failed.' } } } : result;
    };
    await runner.tick();
    assert.equal(f.rows.get('one').value.phase, method === 'thread/start' ? 'failed' : 'archiving');
    assert.equal(f.rows.get('one').value.errorCode, 'secretary_execution_native_failed');
    assert.equal(f.engine.store.technicalList('secretary/assignment-native-call').length, 0, 'A failed start is no longer needed for recovery after its terminal state is saved.');
    if (method === 'turn/start') {
      await runner.tick();
      assert.equal(f.rows.get('one').value.phase, 'archived');
    }
  }
});

test('native calls remain recoverable until their progress is saved', async t => {
  for (const method of ['thread/start', 'turn/start', 'thread/archive', 'thread/delete']) {
    const f = fixture(t), runner = f.runner();
    f.add('one', method === 'thread/delete' ? { phase: 'archived', threadId: 'thread-one', archivedAt: at, completedAt: at, deleteAfter: at } : {});
    const amend = f.engine.store.amend.bind(f.engine.store);
    f.engine.store.amend = async (current, key, value) => {
      if (f.calls.some(call => call.method === method)) throw new IvyError('outcome_unknown', 'Progress could not be saved.', 'unknown');
      return amend(current, key, value);
    };
    await runner.tick();
    if (method === 'thread/archive') await runner.tick();
    assert.equal(f.engine.store.technicalList('secretary/assignment-native-call').length, 1);
    f.engine.store.amend = amend;
    f.setNow('2026-09-22T08:00:31.000Z');
    await f.runner().tick();
    assert.equal(f.calls.filter(call => call.method === method).length, 1, 'Recovery must reuse the original native result.');
    assert.equal(f.engine.store.technicalList('secretary/assignment-native-call').length, 0);
  }
});

test('a dispatch receipt must identify the original operation before saving its task', async t => {
  const f = fixture(t); f.state.foreignReply = true;
  await assert.rejects(f.runner().completed(f.native, randomUUID(), 'thread/start', {}), /original call/);
});
