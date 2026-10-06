import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { join, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { PassThrough } from 'node:stream';
import { NativeJournal } from '../services/agent-manager/src/journal.js';
import { NativeRpc } from '../services/agent-manager/src/rpc.js';
import { answerNativeClock, nativeClockPrincipal } from '../services/agent-manager/src/clock.js';
import { hashJson } from '../packages/contracts/src/canonical.js';
import type { Agent, Wire } from '../packages/contracts/src/generated.js';
import type { TestContext } from 'node:test';

const catalog = JSON.parse(readFileSync('specs/native/codex-0.154.0/catalog.json', 'utf8')) as Agent.Catalog;
function fixture(t: TestContext, options: { failIntent?: boolean; maximumBytes?: number } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'ivy-native-clock-')), input = new PassThrough(), output = new PassThrough();
  const open = () => new NativeJournal({ serviceNodeId: 'clock-owner', hostId: 'clock-host', nativeVersion: catalog.version, nativeExecutableHash: catalog.nativeExecutableHash },
    { maxOperations: 100, maxJournalBytes: options.maximumBytes ?? 128 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 });
  const journal = open(); journal.beginEpoch('original');
  if(options.failIntent)t.mock.method(journal,'accept',()=>{throw new Error('clock fixture commit failure');});
  const frames: Record<string, Wire.Json>[] = [];
  const rpc = new NativeRpc(catalog, input, output, { onRequest: () => undefined, onNotification: () => undefined, onClose: code => journal.loseEpoch('original', code) });
  input.on('data', bytes => frames.push(JSON.parse(bytes.toString('utf8'))));
  t.after(() => { rpc.close('fixture_stop'); input.destroy(); output.destroy(); journal.close(); assert.ok(relative(tmpdir(), root).startsWith('ivy-native-clock-')); rmSync(root, { recursive: true, force: true }); });
  const observe = (requestId: Agent.RequestId) => journal.observeInput({ serviceNodeId: 'clock-owner', epoch: 'original', requestId }, 'currentTime/read', { threadId: 'clock-thread' });
  const key = (value: Agent.PendingInput) => ({ callerPrincipalId: nativeClockPrincipal, operationId: 'clock:' + hashJson(value.identity).slice(7) });
  return { root, journal, rpc, frames, observe, key, open };
}

test('host clock saves exact whole seconds before write, distinguishes typed IDs and never substitutes a repeated observation', t => {
  const f = fixture(t), first = f.observe(7), second = f.observe('7');
  const originalTime = first.observedAt;
  f.rpc.answer = ((original => (method, id, reply, beforeSend) => original.call(f.rpc, method, id, reply, () => {
    beforeSend(); const key = id === 7 ? f.key(first) : f.key(second), retained = f.journal.get(key)!;
    assert.equal(retained.phase, 'dispatched'); assert.equal(f.journal.input(id === 7 ? first.identity : second.identity)!.state, 'answering');
  }))(f.rpc.answer));
  answerNativeClock(f.journal, f.rpc, first); answerNativeClock(f.journal, f.rpc, second);
  answerNativeClock(f.journal, f.rpc, first);
  assert.deepEqual(f.frames.map(value => value['id']), [7, '7']);
  assert.deepEqual(f.frames[0]!['result'], { currentTimeAt: Math.floor(Date.parse(originalTime) / 1000) });
  assert.notEqual(f.key(first).operationId, f.key(second).operationId);
  assert.throws(() => answerNativeClock(f.journal, f.rpc, { ...first, observedAt: '2030-01-01T00:00:00.000Z' }), /different|match|original/i);
  assert.equal(f.frames.length, 2);
  f.journal.resolveInput('original', 7, 'clock-thread'); assert.equal(f.journal.get(f.key(first))!.phase, 'succeeded');
  answerNativeClock(f.journal, f.rpc, first); assert.equal(f.frames.length, 2);
});

test('host clock input and unknown answer survive owner loss and cannot replay in another epoch', t => {
  const f = fixture(t), original = f.observe('lost'); answerNativeClock(f.journal, f.rpc, original);
  f.rpc.close('fixture_connection_lost');
  assert.equal(f.journal.get(f.key(original))!.phase, 'outcome_unknown'); assert.equal(f.journal.input(original.identity)!.state, 'outcome_unknown');
  f.journal.close(); const reopened = f.open();
  try { reopened.beginEpoch('replacement'); assert.equal(reopened.get(f.key(original)), null);
    assert.throws(() => answerNativeClock(reopened, f.rpc, original)); assert.equal(f.frames.length, 1);
  } finally { reopened.close(); }
});

test('host clock cannot write when its original answer intent fails to commit, and other methods remain caller-owned', t => {
  const f = fixture(t, { failIntent: true }), original = f.observe('commit-failure');
  assert.throws(() => answerNativeClock(f.journal, f.rpc, original), /clock fixture commit failure/);
  assert.equal(f.journal.get(f.key(original)), null); assert.equal(f.journal.input(original.identity)!.state, 'pending'); assert.equal(f.frames.length, 0);
  assert.throws(() => answerNativeClock(f.journal, f.rpc, { ...original, method: 'item/tool/requestUserInput' })); assert.equal(f.frames.length, 0);
});

test('host clock input admission reserves its answer before accepting a new request', t => {
  const f = fixture(t, { maximumBytes: 40 * 1024 * 1024 });
  assert.throws(() => f.observe('capacity'), /cannot reserve/);
  assert.deepEqual(f.journal.inputs({}).items, []); assert.equal(f.frames.length, 0);
});
