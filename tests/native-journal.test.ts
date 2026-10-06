import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';
import { NativeJournal, nativeFrameBytes } from '../services/agent-manager/src/journal.js';
import { nativeRequestFrameBytes, nativeAnswerFrameBytes, managementFrameBytes } from '../services/agent-manager/src/limits.js';
import { canonical, digest, hashJson } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Agent } from '../packages/contracts/src/generated.js';

const owner = { hostId: 'fixture-host', serviceNodeId: 'native-owner', nativeVersion: '0.154.0', nativeExecutableHash: digest('fixture-native') };
const defaults: Agent.Settings['limits'] = { maxOperations: 100, maxJournalBytes: 128 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 };
const code = (expected: string) => (error: unknown) => error instanceof IvyError && error.code === expected;

function fixture(t: TestContext, limits = defaults) {
  const root = mkdtempSync(join(tmpdir(), 'ivy-native-journal-')), handles: NativeJournal[] = [];
  const open = (identity = owner) => { const journal = new NativeJournal(identity, limits); handles.push(journal); return journal; };
  t.after(() => { for (const journal of handles) journal.close(); assert.ok(relative(tmpdir(), root).startsWith('ivy-native-journal-')); rmSync(root, { recursive: true, force: true }); });
  return { root, open };
}

test('in-memory prevention preserves every existing phase and does not reserve a native reply', t => {
  const f = fixture(t), journal = f.open(), key = { callerPrincipalId: 'user', operationId: 'prevented' };
  journal.beginEpoch('first'); const prevented = journal.prevent(key, 'turn/start', { threadId: 'thread', input: [] });
  assert.equal(prevented.phase, 'failed'); assert.equal(prevented.code, 'request_prevented'); assert.equal(prevented.requestId, null); assert.equal(prevented.epoch, null);
  assert.ok(journal.status().bytes < 4096); assert.deepEqual(journal.accept(key, prevented.method, prevented.params), { created: false, operation: prevented });
  assert.throws(() => journal.prevent(key, 'thread/start', {}), code('mutation_conflict'));
  assert.throws(() => journal.dispatch(key, 'first', 1), code('native_state_conflict'));
  for (const phase of ['accepted', 'dispatched', 'succeeded', 'failed', 'outcome_unknown'] as const) {
    const identity = { ...key, operationId: phase }; journal.accept(identity, 'turn/start', {});
    if (phase !== 'accepted' && phase !== 'failed') journal.dispatch(identity, 'first', phase);
    if (phase === 'succeeded') journal.finish(identity, 'first', phase, { result: {} });
    if (phase === 'failed') journal.rejectBeforeDispatch(identity, 'other-reason');
    if (phase === 'outcome_unknown') journal.loseEpoch('first', 'test_loss');
    const before = journal.get(identity)!; assert.equal(before.phase, phase);
    assert.deepEqual(journal.prevent(identity, 'turn/start', {}), before);
  }
  journal.close(); assert.equal(f.open().get(key), null); assert.deepEqual(readdirSync(f.root), []);
});

test('native intents preserve original replies during the connection and caller-local replay never dispatches twice', t => {
  const f = fixture(t), journal = f.open(), key = { callerPrincipalId: 'user', operationId: 'create-once' };
  journal.beginEpoch('epoch-1');
  const first = journal.accept(key, 'thread/start', { cwd: '/fixture/project', model: 'actual-selected-model' });
  assert.equal(first.created, true); assert.equal(first.operation.requestId, null);
  assert.deepEqual(journal.accept(key, first.operation.method, first.operation.params), { operation: first.operation, created: false });
  assert.throws(() => journal.accept(key, 'thread/start', { cwd: '/other' }), code('mutation_conflict'));
  journal.dispatch(key, 'epoch-1', 1);
  assert.throws(() => journal.dispatch(key, 'epoch-1', 2), code('native_state_conflict'));
  assert.throws(() => journal.finish(key, 'epoch-1', '1', { result: {} }), code('native_state_conflict'));
  assert.throws(() => journal.rejectBeforeDispatch(key, 'not_executed'), code('native_state_conflict'));
  const saved = journal.finish(key, 'epoch-1', 1, { result: { thread: { id: 'the-original-native-thread' }, untouched: [null, false] } });
  assert.equal(saved.createdAt, first.operation.createdAt); assert.equal(saved.phase, 'succeeded');
  assert.equal(journal.get({ ...key, callerPrincipalId: 'other' }), null);
  assert.equal(journal.accept({ ...key, callerPrincipalId: 'other' }, first.operation.method, first.operation.params).created, true);
  assert.deepEqual(journal.get(key), saved);
  journal.close(); assert.equal(f.open().get(key), null); assert.deepEqual(readdirSync(f.root), []);
});

test('native epoch loss distinguishes unsent from unknown and cannot accept late replies', t => {
  const f = fixture(t), journal = f.open(), sent = { callerPrincipalId: 'user', operationId: 'sent' }, unsent = { ...sent, operationId: 'unsent' };
  journal.beginEpoch('old'); journal.accept(sent, 'turn/start', { threadId: 'thread', input: [] }); journal.accept(unsent, 'thread/start', {});
  journal.dispatch(sent, 'old', -7); journal.loseEpoch('irrelevant-old-epoch', 'connection_lost');
  assert.equal(journal.get(sent)!.phase, 'dispatched');
  journal.loseEpoch('old', 'connection_lost');
  const unknown = journal.get(sent)!; assert.equal(unknown.phase, 'outcome_unknown'); assert.equal(unknown.requestId, -7); assert.equal(unknown.reply, null);
  assert.equal(journal.get(unsent)!.code, 'request_not_dispatched'); assert.equal(journal.epoch, null);
  journal.beginEpoch('new');
  assert.throws(() => journal.finish(sent, 'old', -7, { result: {} }), code('native_state_conflict'));
  assert.deepEqual(journal.accept(sent, unknown.method, unknown.params), { operation: unknown, created: false });
  journal.close(); assert.equal(f.open().get(sent), null);
});

test('journal reserves bounded outcomes before admitting effects and preserves replays when count or bytes are exhausted', t => {
  const f = fixture(t, { ...defaults, maxJournalBytes: 2 * nativeFrameBytes + 1024 * 1024 }), journal = f.open(), key = { callerPrincipalId: 'caller', operationId: 'one' };
  journal.beginEpoch('current'); const first = journal.accept(key, 'model/list', {});
  assert.throws(() => journal.accept({ ...key, operationId: 'two' }, 'model/list', {}), code('native_journal_capacity'));
  assert.equal(journal.status().retained, 1); assert.deepEqual(journal.accept(key, 'model/list', {}).operation, first.operation);
  journal.dispatch(key, 'current', 1);
  const saved = journal.finish(key, 'current', 1, { error: { code: -32602, message: 'Original native error.', data: { detail: 'preserved' } } });
  assert.equal(saved.phase, 'failed'); assert.equal(saved.code, 'native_error');
  journal.retainedInHive(key);
  for (let i = 1; i < 100; i++) { const identity = { ...key, operationId: 'retained-' + i }; journal.accept(identity, 'model/list', {}); journal.rejectBeforeDispatch(identity, 'fixture_refused'); journal.retainedInHive(identity); }
  assert.ok(journal.status().retained < 100);
  assert.equal(journal.accept({ ...key, operationId: 'over-count' }, 'model/list', {}).created, true);
  assert.equal(journal.get(key), null);
  assert.throws(() => journal.accept({ ...key, operationId: 'oversized' }, 'model/list', 'x'.repeat(nativeRequestFrameBytes)), code('content_too_large'));
});

test('a six-MiB native request reserves its full receive allowance and remains recoverable inside management capacity', t => {
  const f = fixture(t), journal = f.open(), key = { callerPrincipalId: 'caller', operationId: 'full-request-and-reply' };
  const requestId = '00000000-0000-0000-0000-000000000000', params = { payload: '' };
  const overhead = Buffer.byteLength(canonical({ id: requestId, method: 'turn/start', params }));
  params.payload = 'q'.repeat(nativeRequestFrameBytes - overhead);
  journal.beginEpoch('capacity');
  const accepted = journal.accept(key, 'turn/start', params);
  const reservation = journal.status().bytes;
  assert.ok(reservation > nativeRequestFrameBytes + nativeFrameBytes);
  const limited = fixture(t, { ...defaults, maxJournalBytes: 10 * 1024 * 1024 }).open();
  assert.throws(() => limited.accept(key, 'turn/start', params), code('native_journal_capacity'));
  assert.equal(limited.status().retained, 0);
  assert.throws(() => journal.accept({ ...key, operationId: 'one-byte-over' }, 'turn/start', { payload: params.payload + 'x' }), code('content_too_large'));
  assert.equal(journal.status().retained, 1);
  journal.dispatch(key, 'capacity', requestId);
  assert.throws(() => journal.finish(key, 'capacity', requestId, { result: 'r'.repeat(nativeFrameBytes) }), code('content_too_large'));
  assert.equal(journal.get(key)!.phase, 'dispatched');
  const reply = { result: { payload: '' } };
  reply.result.payload = 'r'.repeat(nativeFrameBytes - Buffer.byteLength(canonical({ id: requestId, ...reply })));
  const saved = journal.finish(key, 'capacity', requestId, reply), savedHash = hashJson(saved);
  assert.ok(Buffer.byteLength(canonical(saved, managementFrameBytes)) < managementFrameBytes);
  assert.ok(journal.status().bytes < reservation); assert.equal(saved.createdAt, accepted.operation.createdAt);
  journal.close(); const reopened = f.open();
  assert.equal(reopened.get(key), null);
});

test('pending native inputs survive page reads and require a caller-bound answer intent plus exact native resolution', t => {
  const f = fixture(t), journal = f.open(); journal.beginEpoch('live');
  const identity = { serviceNodeId: owner.serviceNodeId, epoch: 'live', requestId: 1 };
  const pending = journal.observeInput(identity, 'item/tool/requestUserInput', { threadId: 'thread', turnId: 'turn', questions: [{ id: 'q', question: 'Choose?' }] });
  assert.deepEqual(journal.observeInput(identity, pending.method, pending.params), pending);
  assert.deepEqual(journal.inputs({}), journal.inputs({})); assert.equal(journal.input({ ...identity, requestId: '1' }), null);
  assert.throws(() => journal.observeInput(identity, pending.method, {}), code('native_input_conflict'));
  const answer = { callerPrincipalId: 'user', operationId: 'original-answer' }, reply = { result: { answers: { q: { answers: ['first'] } } } };
  journal.accept(answer, 'agent.answer', { identity, reply }); const sending = journal.dispatchAnswer(answer, identity, reply);
  assert.equal(sending.state, 'answering'); assert.equal(sending.answerCallerPrincipalId, 'user'); assert.equal(journal.get(answer)!.phase, 'dispatched');
  assert.equal(journal.accept(answer, 'agent.answer', { identity, reply }).created, false);
  assert.throws(() => journal.dispatchAnswer(answer, identity, reply), code('native_input_expired'));
  const rival = { ...answer, callerPrincipalId: 'other' }; journal.accept(rival, 'agent.answer', { identity, reply });
  assert.throws(() => journal.dispatchAnswer(rival, identity, reply), code('native_input_expired')); journal.rejectBeforeDispatch(rival, 'native_input_expired');
  assert.throws(() => journal.resolveInput('live', 1, 'different-thread'), code('native_input_conflict'));
  assert.equal(journal.get(answer)!.phase, 'dispatched');
  const resolved = journal.resolveInput('live', 1, 'thread')!; assert.equal(resolved.state, 'answered');
  assert.equal(journal.get(answer)!.phase, 'succeeded'); assert.deepEqual(journal.get(answer)!.reply, { result: { nativeRequestResolved: true } });
  assert.deepEqual(journal.resolveInput('live', 1, 'thread'), resolved);
  assert.equal(journal.inputs({}).items.length, 0); assert.deepEqual(journal.inputs({ identity }).items, [resolved]);
  journal.close(); const reopened = f.open(); reopened.beginEpoch('after-restart');
  assert.equal(reopened.input(identity), null); assert.equal(reopened.get(answer), null);
});

test('large pending native input and its answer remain visible in pages until owner restart', t => {
  const f = fixture(t, { ...defaults, maxJournalBytes: 2 * nativeFrameBytes + 16 * 1024 * 1024 }), journal = f.open(); journal.beginEpoch('large-input');
  const identity = { serviceNodeId: owner.serviceNodeId, epoch: 'large-input', requestId: 'question' };
  const params = { threadId: 'thread', payload: 'q'.repeat(7 * 1024 * 1024) };
  const pending = journal.observeInput(identity, 'item/tool/requestUserInput', params);
  assert.deepEqual(journal.inputs({}).items, [pending]);
  const answer = { callerPrincipalId: 'caller', operationId: 'large-answer' }, reply = { error: { code: -32000, message: 'Synthetic reply.', data: 'r'.repeat(3 * 1024 * 1024) } };
  journal.accept(answer, 'agent.answer', { identity, reply });
  const answering = journal.dispatchAnswer(answer, identity, reply);
  assert.ok(Buffer.byteLength(canonical(answering)) > 9 * 1024 * 1024);
  const page = journal.inputs({}); assert.deepEqual(page.items, [answering]); assert.equal(page.truncated, false);
  journal.close(); const replacement = f.open(); replacement.beginEpoch('replacement');
  assert.deepEqual(replacement.inputs({ includeExpired: true }).items, []);
});

test('a maximum received native request and maximum answer remain discoverable in ordinary pages before restart', t => {
  const f = fixture(t), journal = f.open(); journal.beginEpoch('maximum-input');
  const identity = { serviceNodeId: owner.serviceNodeId, epoch: 'maximum-input', requestId: 'question' }, method = 'item/tool/requestUserInput';
  const params = { threadId: 'thread', payload: '' };
  params.payload = 'q'.repeat(nativeFrameBytes - Buffer.byteLength(canonical({ id: identity.requestId, method, params })));
  const pending = journal.observeInput(identity, method, params);
  assert.deepEqual(journal.inputs({}).items, [pending]); assert.equal(journal.inputs({}).truncated, false);
  assert.throws(() => journal.observeInput({ ...identity, requestId: 'overflow' }, method, { ...params, payload: params.payload + 'xx' }), code('content_too_large'));
  const answer = { callerPrincipalId: 'caller', operationId: 'maximum-answer' }, reply = { error: { code: -32000, message: 'Synthetic boundary.', data: '' } };
  reply.error.data = 'r'.repeat(nativeAnswerFrameBytes - Buffer.byteLength(canonical({ id: identity.requestId, ...reply })));
  assert.throws(() => journal.accept({ ...answer, operationId: 'oversized-answer' }, 'agent.answer', { identity, reply: { error: { ...reply.error, data: reply.error.data + 'x' } } }), code('content_too_large'));
  journal.accept(answer, 'agent.answer', { identity, reply }); journal.dispatchAnswer(answer, identity, reply);
  const page = journal.inputs({}); assert.equal(page.truncated, false); assert.equal(page.items.length, 1);
  assert.ok(Buffer.byteLength(canonical(page)) > 27 * 1024 * 1024); assert.ok(Buffer.byteLength(canonical(page)) < managementFrameBytes);
  assert.deepEqual(page.items[0]!.params, params); assert.deepEqual(page.items[0]!.reply, reply);
  journal.close(); const recovered = f.open(); recovered.beginEpoch('replacement');
  assert.deepEqual(recovered.inputs({ includeExpired: true }).items, []);
});

test('epoch loss expires unanswered input, retains uncertain answers and cannot rebind old IDs onto a new process', t => {
  const f = fixture(t, { ...defaults, maxPendingInputs: 2 }), journal = f.open(); journal.beginEpoch('first');
  const identity = { serviceNodeId: owner.serviceNodeId, epoch: 'first', requestId: 7 }, other = { ...identity, requestId: '7' };
  journal.observeInput(identity, 'currentTime/read', { threadId: 'thread' }); journal.observeInput(other, 'currentTime/read', { threadId: 'thread' });
  assert.throws(() => journal.observeInput({ ...identity, requestId: 8 }, 'currentTime/read', { threadId: 'thread' }), code('native_input_capacity'));
  const answer = { callerPrincipalId: 'user', operationId: 'lost-answer' }, reply = { result: { currentTimeAt: 1788600000 } };
  journal.accept(answer, 'agent.answer', { identity, reply }); journal.dispatchAnswer(answer, identity, reply); journal.loseEpoch('first', 'connection_lost');
  assert.equal(journal.input(identity)!.state, 'outcome_unknown'); assert.equal(journal.input(other)!.state, 'expired'); assert.equal(journal.get(answer)!.phase, 'outcome_unknown');
  journal.beginEpoch('replacement'); const fresh = { ...identity, epoch: 'replacement' }; journal.observeInput(fresh, 'currentTime/read', { threadId: 'different-native-thread' });
  assert.deepEqual(journal.inputs({}).items.map(value => value.identity), [fresh]);
  const history = journal.inputs({ includeExpired: true, limit: 2 }); assert.equal(history.items[0]!.identity.epoch, 'replacement'); assert.equal(history.truncated, false);
  assert.throws(() => journal.resolveInput('first', 7, 'thread'), code('native_state_conflict'));
  assert.equal(journal.input(fresh)!.state, 'pending');
});
