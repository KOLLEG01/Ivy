import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { PassThrough } from 'node:stream';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { join, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { NativeRpc } from '../services/agent-manager/src/rpc.js';
import type { NativeRequest, NativeNotification } from '../services/agent-manager/src/rpc.js';
import { NativeJournal, nativeFrameBytes } from '../services/agent-manager/src/journal.js';
import { nativeRequestFrameBytes, nativeAnswerFrameBytes } from '../services/agent-manager/src/limits.js';
import { canonical, digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Agent } from '../packages/contracts/src/generated.js';

const catalog = JSON.parse(readFileSync('specs/native/codex-0.154.0/catalog.json', 'utf8')) as Agent.Catalog;
function fixture(t: TestContext, onClose: (code: string) => void = () => undefined) {
  const input = new PassThrough(), output = new PassThrough(), sent: Record<string, unknown>[] = [], requests: NativeRequest[] = [], notifications: NativeNotification[] = [];
  input.on('data', (value: Buffer) => sent.push(JSON.parse(value.toString('utf8')) as Record<string, unknown>));
  const rpc = new NativeRpc(catalog, input, output, { onClose, onRequest: value => { requests.push(value); }, onNotification: value => { notifications.push(value); } });
  t.after(() => { rpc.close(); input.destroy(); output.destroy(); });
  return { input, output, sent, requests, notifications, rpc, emit: (value: unknown) => output.write(JSON.stringify(value) + '\n') };
}

test('expired agent reads do not close active requests and accept their late response', async t => {
  const f=fixture(t);
  const background=f.rpc.request('thread/read',{threadId:'one',includeTurns:false}, {detachOnTimeout:true},10);
  const active=f.rpc.request('account/logout',null);
  await assert.rejects(background,{code:'native_observation_deadline'});
  assert.equal(f.rpc.connected,true);
  f.emit({id:f.sent[0]!['id'],result:{data:[],nextCursor:null}});
  assert.equal(f.rpc.connected,true);
  f.emit({id:f.sent[1]!['id'],result:{}});assert.deepEqual(await active,{result:{}});
});

test('foreground native interactions still dispatch when background observation slots are occupied',async t=>{
  const f=fixture(t),background=[];
  for(let i=0;i<16;i++)background.push(f.rpc.request('thread/loaded/list',{}, {detachOnTimeout:true}));
  await assert.rejects(f.rpc.request('thread/loaded/list',{}, {detachOnTimeout:true}),{code:'native_capacity'});
  const foreground=f.rpc.request('account/logout',null,{detachOnTimeout:true,priority:'interaction'});
  f.emit({id:f.sent.at(-1)!['id'],result:{}});assert.deepEqual(await foreground,{result:{}});
  for(const frame of f.sent.slice(0,16))f.emit({id:frame['id'],result:{data:[],nextCursor:null}});
  await Promise.all(background);
});

test('public native JSONL preserves null, fragmented UTF-8, exact server IDs and unmodified native replies', async t => {
  const f = fixture(t), order: string[] = [];
  const pending = f.rpc.request('account/logout', null, { beforeSend: () => { assert.equal(f.sent.length, 0); order.push('intent'); }, beforeResolve: () => { order.push('result'); } });
  assert.equal(f.sent.length, 1); assert.deepEqual(f.sent[0], { id: f.sent[0]!['id'], method: 'account/logout', params: null });
  const reply = { result: { untouched: ['Grüße', null, false] } };
  const line = Buffer.from(JSON.stringify({ id: f.sent[0]!['id'], ...reply }) + '\n');
  for (const byte of line) f.output.write(Buffer.from([byte]));
  assert.deepEqual(await pending, reply); assert.deepEqual(order, ['intent', 'result']);
  f.rpc.notify('initialized'); assert.deepEqual(f.sent[1], { method: 'initialized' });
  for (const id of [-2, '-2']) f.emit({ id, method: 'currentTime/read', params: { threadId: 'original-thread' } });
  assert.deepEqual(f.requests.map(value => value.id), [-2, '-2']);
  f.emit({ method: 'serverRequest/resolved', params: { requestId: -2, threadId: 'original-thread' } });
  assert.deepEqual(f.notifications, [{ method: 'serverRequest/resolved', params: { requestId: -2, threadId: 'original-thread' } }]);
  let answerIntents = 0;
  f.rpc.answer('currentTime/read', -2, { result: { currentTimeAt: 1788600000 } }, () => { answerIntents++; assert.equal(f.sent.length, 2); });
  assert.deepEqual(f.sent[2], { id: -2, result: { currentTimeAt: 1788600000 } }); assert.equal(answerIntents, 1);
  f.rpc.answer('currentTime/read', '-2', { result: {} }, () => { answerIntents++; }); assert.equal(answerIntents, 2);
  assert.deepEqual(f.sent[3], { id: '-2', result: {} });
  const error = f.rpc.request('account/logout', null);
  const original = { error: { code: -32003, message: 'Exact native failure.', data: { nativeDetail: [1, false] } } };
  f.emit({ id: f.sent[4]!['id'], ...original }); assert.deepEqual(await error, original);
});

test('native request frames carry the full two-image allowance and enforce exact UTF-8 bounds before intent', async t => {
  const f = fixture(t); let intents = 0;
  // Synthetic bytes exercise transport capacity; native image decoding has separate target tests.
  const image = 'data:image/png;base64,' + Buffer.alloc(2 * 1024 * 1024, 11).toString('base64');
  const params = { threadId: 'image-capacity-thread', input: [{ type: 'text', text: '🧪'.repeat(65536) },
    { type: 'image', url: image }, { type: 'image', url: image }] };
  const images = f.rpc.request('turn/start', params, { beforeSend: () => { intents++; } });
  assert.equal(f.sent.length, 1); assert.deepEqual(f.sent[0]!['params'], params);
  const bytes = Buffer.byteLength(canonical(f.sent[0]));
  assert.ok(bytes > nativeAnswerFrameBytes && bytes < nativeRequestFrameBytes);
  f.emit({ id: f.sent[0]!['id'], error: { code: -32000, message: 'Synthetic protocol response, not a decoding observation.' } });
  await images;
  const exact = { threadId: 'exact-bound', input: [{ type: 'text', text: '' }] };
  const overhead = Buffer.byteLength(canonical({ id: '00000000-0000-0000-0000-000000000000', method: 'turn/start', params: exact }));
  exact.input[0]!.text = 'a'.repeat(nativeRequestFrameBytes - overhead - 4) + '🧪';
  const boundary = f.rpc.request('turn/start', exact, { beforeSend: () => { intents++; } });
  assert.equal(f.sent.length, 2); assert.equal(Buffer.byteLength(canonical(f.sent[1])), nativeRequestFrameBytes);
  f.emit({ id: f.sent[1]!['id'], error: { code: -32000, message: 'Exact-bound fixture.' } }); await boundary;
  exact.input[0]!.text += 'x';
  await assert.rejects(f.rpc.request('turn/start', exact, { beforeSend: () => { intents++; } }),
    (error: unknown) => error instanceof IvyError && error.code === 'content_too_large');
  assert.equal(intents, 2); assert.equal(f.sent.length, 2); assert.equal(f.rpc.connected, true);
  assert.throws(() => f.rpc.answer('currentTime/read', 7, { error: { code: -32000, message: 'Oversized error data.', data: 'x'.repeat(nativeAnswerFrameBytes) } },
    () => { intents++; }), (error: unknown) => error instanceof IvyError && error.code === 'content_too_large');
  assert.equal(intents, 2); assert.equal(f.sent.length, 2);
});

test('unsupported calls and refused durable intent have no native bytes; invalid envelopes, UTF-8 and bounds close without replay', async t => {
  const before = fixture(t);
  await assert.rejects(before.rpc.request('absent/nativeMethod', {}));
  await assert.rejects(before.rpc.request('account/logout', null, { beforeSend: () => { throw new IvyError('native_journal_capacity', 'Fixture quota refusal.'); } }));
  assert.equal(before.sent.length, 0); assert.equal(before.rpc.connected, true);
  const opaque = before.rpc.request('account/logout', {});
  before.emit({ id: before.sent[0]!['id'], result: 'opaque result' });
  assert.deepEqual(await opaque, { result: 'opaque result' });
  for (const failure of ['mixed', 'unknown-id', 'unsafe-id', 'unknown-notification', 'utf8', 'oversize', 'truncated']) {
    const f = fixture(t); const result = f.rpc.request('account/logout', null); const id = f.sent[0]!['id'];
    if (failure === 'mixed') f.emit({ id, result: {}, error: { code: -1, message: 'mixed' } });
    if (failure === 'unknown-id') f.emit({ id: 'not-the-original-id', result: {} });
    if (failure === 'unsafe-id') f.output.write('{"id":9007199254740993,"method":"currentTime/read","params":{"threadId":"thread"}}\n');
    if (failure === 'unknown-notification') f.emit({ method: 'unmapped/newNativeMethod', params: {} });
    if (failure === 'utf8') f.output.write(Buffer.from([0xff, 10]));
    if (failure === 'oversize') f.output.write(Buffer.alloc(nativeFrameBytes + 1, 32));
    if (failure === 'truncated') { f.output.write('{'); f.output.end(); }
    await assert.rejects(result, (error: unknown) => error instanceof IvyError && error.outcome === 'unknown');
    await f.rpc.closed; assert.equal(f.sent.length, 1); assert.equal(f.rpc.connected, false);
  }
});

test('native image-bearing notifications and successful responses retain the full receive allowance with exact UTF-8 overflow detection', async t => {
  const f = fixture(t), image = 'data:image/png;base64,' + Buffer.alloc(2 * 1024 * 1024, 12).toString('base64');
  const item = { id: 'original-user-item', type: 'userMessage', content: [{ type: 'image', url: image }, { type: 'image', url: image }] };
  const notification = { method: 'item/started', params: { threadId: 'thread', turnId: 'turn', startedAtMs: 1, item } };
  assert.ok(Buffer.byteLength(canonical(notification)) > nativeAnswerFrameBytes);
  f.emit(notification); assert.deepEqual(f.notifications, [notification]); assert.equal(f.rpc.connected, true);
  const pending = f.rpc.request('turn/start', { threadId: 'thread', input: [] });
  const reply = { result: { turn: { id: 'turn', status: 'inProgress', items: [item] } } };
  f.emit({ id: f.sent[0]!['id'], ...reply }); assert.deepEqual(await pending, reply);
  const text = { type: 'text', text: '' }, exact = { ...notification, params: { ...notification.params, item: { ...item, content: [text] } } };
  text.text = 'a'.repeat(nativeFrameBytes - Buffer.byteLength(canonical(exact)) - 4) + '🧪';
  const line = Buffer.from(canonical(exact) + '\n'); assert.equal(line.length, nativeFrameBytes + 1);
  f.output.write(line.subarray(0, line.length - 3)); f.output.write(line.subarray(line.length - 3));
  assert.deepEqual(f.notifications[1], exact); assert.equal(f.rpc.connected, true);
  text.text += 'x'; f.emit(exact);
  assert.equal(await f.rpc.closed, 'native_frame_too_large'); assert.equal(f.notifications.length, 2); assert.equal(f.sent.length, 1);
});

test('a large complete native catalogue preserves every entry across fragments and adjacent response frames', async t => {
  const f = fixture(t); let committed = 0;
  const plugins = Array.from({ length: 4096 }, (_, i) => ({ id: 'plugin-' + i, name: 'Plugin ' + i, installed: false, enabled: false,
    source: { type: 'remote' }, installPolicy: 'AVAILABLE', authPolicy: 'ON_USE', interface: { capabilities: [], screenshotUrls: [], screenshots: [], longDescription: 'x'.repeat(3072) } }));
  const reply = { result: { marketplaces: [{ name: 'Complete native catalogue', plugins }], marketplaceLoadErrors: [] } };
  const expected = digest(canonical(reply));
  const pending = f.rpc.request('plugin/list', { forceRefetch: true }, { beforeResolve: (_id, value) => { assert.equal(digest(canonical(value)), expected); committed++; } });
  const adjacent = f.rpc.request('account/logout', null);
  const bytes = Buffer.from(JSON.stringify({ id: f.sent[0]!['id'], ...reply }) + '\n' + JSON.stringify({ id: f.sent[1]!['id'], result: {} }) + '\n');
  assert.ok(bytes.length > 8 * 1024 * 1024 && bytes.length < nativeFrameBytes);
  for (let offset = 0; offset < bytes.length; offset += 65536) f.output.write(bytes.subarray(offset, offset + 65536));
  assert.equal(digest(canonical(await pending)), expected); assert.deepEqual(await adjacent, { result: {} }); assert.equal(committed, 1); assert.equal(f.rpc.connected, true);
});

test('native replies commit before upstream resolution and lost storage or deadlines preserve journal uncertainty', { timeout: 10_000 }, async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-native-rpc-'));
  const journal = new NativeJournal({ hostId: 'host', serviceNodeId: 'native-rpc', nativeVersion: catalog.version, nativeExecutableHash: digest('fixture-native') },
    { maxOperations: 100, maxJournalBytes: 128 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 });
  t.after(() => { journal.close(); assert.ok(relative(tmpdir(), root).startsWith('ivy-native-rpc-')); rmSync(root, { recursive: true, force: true }); });
  const key = { callerPrincipalId: 'user', operationId: 'saved-native' }; journal.beginEpoch('connection');
  const f = fixture(t, code => journal.loseEpoch('connection', code));
  journal.accept(key, 'account/logout', null);
  const result = f.rpc.request('account/logout', null, { beforeSend: id => { journal.dispatch(key, 'connection', id); }, beforeResolve: (id, reply) => { journal.finish(key, 'connection', id, reply); } });
  f.emit({ id: f.sent[0]!['id'], result: { originalId: 'native-result' } }); await result;
  assert.equal(journal.get(key)!.phase, 'succeeded');
  const uncertainKey = { ...key, operationId: 'outcome-write-failure' }; journal.accept(uncertainKey, 'account/logout', null);
  const uncertain = f.rpc.request('account/logout', null, { beforeSend: id => { journal.dispatch(uncertainKey, 'connection', id); }, beforeResolve: () => { throw new Error('simulated storage failure after native reply'); } });
  f.emit({ id: f.sent[1]!['id'], result: {} });
  await assert.rejects(uncertain, (error: unknown) => error instanceof IvyError && error.code === 'native_outcome_commit_failed' && error.outcome === 'unknown');
  assert.equal(journal.get(uncertainKey)!.phase, 'outcome_unknown');
  assert.equal(journal.accept(uncertainKey, 'account/logout', null).created, false);
  journal.beginEpoch('timeout'); const next = fixture(t, code => journal.loseEpoch('timeout', code));
  const timeoutKey = { ...key, operationId: 'response-never-arrived' }; journal.accept(timeoutKey, 'account/logout', null);
  await assert.rejects(next.rpc.request('account/logout', null, { beforeSend: id => { journal.dispatch(timeoutKey, 'timeout', id); } }, 30));
  assert.equal(journal.get(timeoutKey)!.code, 'native_deadline_exceeded'); assert.equal(next.sent.length, 1);
});
