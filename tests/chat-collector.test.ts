import test from 'node:test';
import assert from 'node:assert/strict';
import { digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Chat, Wire } from '../packages/contracts/src/generated.js';
import { ChatDelivery } from '../services/chat-bridge/src/delivery.js';
import { ChatCollector } from '../services/chat-bridge/src/collector.js';
import { ChatWorker } from '../services/chat-bridge/src/worker.js';
import { ChatOutbox } from '../services/chat-bridge/src/outbox.js';
import { readChatResultText, chatReplyParts } from '../services/chat-bridge/src/result-text.js';
import { chatNativeFixture as fixture } from './fixtures/chat-native.js';

const message = (id: string, text = id) => ({ type: 'agentMessage', id, text });

test('generic chat drains terminal collection and publication without polling pauses and stops polling active turns',async t=>{
  const f=await running(t),worker=new ChatWorker(f.first);let completed=false;
  f.reply(frame=>frame['method']==='thread/turns/list'?{result:{data:[f.turn({status:completed?'completed':'inProgress'})],nextCursor:null}}:
    frame['method']==='thread/items/list'?{result:{data:[{turnId:f.input.value.turnId!,item:message('answer','Immediate generic reply')}],nextCursor:null}}:undefined);
  for(let slice=0;slice<16;slice++){await worker.drain();if(!worker.runnable)break;}
  const before=f.frames.length;await worker.drain();assert.equal(worker.runnable,false);
  assert.equal(worker.turnId,f.input.value.turnId);
  assert.equal(f.frames.length-before,1,'An active turn is polled once, not repeatedly in a busy loop.');
  completed=true;let slices=0;
  do{await worker.drain();slices++;assert.ok(slices<=16,'All ready stages must finish with bounded immediate continuations.');}while(worker.runnable);
  assert.equal(worker.idle,true);assert.equal((await f.first.store.read('chat-bridge/input',f.inputId)).value.state,'completed');
  assert.equal(worker.turnId,null);
  const history=await new ChatOutbox(f.first.operations).history('alice',{expectedBridge:f.admission.expected('alice'),channel:f.channel,afterSequence:0,limit:8});
  assert.ok(history.entries.some(entry=>'text' in entry.data&&entry.data.text==='Immediate generic reply'));
  assert.equal(f.frames.filter(frame=>frame['method']==='turn/start').length,1);
  const idleBefore=f.frames.length;await worker.drain();assert.equal(worker.runnable,false);assert.equal(f.frames.length,idleBefore);
});

test('an explicit failed-read retry preserves the original turn and terminal evidence without another native mutation', async t => {
  const f = await running(t); let failItems = true;
  f.reply(frame => {
    if (frame['method'] === 'thread/turns/list') return { result: { data: [f.turn()], nextCursor: null } };
    if (frame['method'] === 'thread/items/list') return failItems ? { error: { code: -32000, message: 'Transient original read failure' } }
      : { result: { data: [{ turnId: f.input.value.turnId!, item: message('original', 'Recovered original answer') }], nextCursor: null } };
    return undefined;
  });
  await f.collector.step(f.inputId); const failed = await f.collector.step(f.inputId);
  assert.equal(failed?.value.state, 'failed'); failItems = false;
  const request = { inputId: f.inputId, expectedCollection: failed!.pin, expectedBridge: f.admission.expected('alice'), operationId: 'retry-original-read', reason: 'Retry the failed read' };
  const retried = await f.collector.retry('alice', request); assert.deepEqual(retried.previousAttempt, failed!.pin);
  assert.equal(await f.collector.observedTerminal(f.inputId), true);
  assert.deepEqual(await f.collector.retry('alice', request), retried);
  const complete = await finish(f); assert.equal(complete.value.turnId, f.input.value.turnId);
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
});

test('a turn missing during native materialization is rediscovered without starting another turn', async t => {
  const f = await running(t); let materialized = false;
  f.reply(frame => {
    if (frame['method'] === 'thread/turns/list') return materialized
      ? { result: { data: [f.turn()], nextCursor: null } }
      : { result: { data: [{ id: 'older-turn', status: 'completed', items: [], itemsView: 'notLoaded' }], nextCursor: null } };
    if (frame['method'] === 'thread/items/list') return { result: { data: [{ turnId: f.input.value.turnId!, item: message('answer', 'Materialized answer') }], nextCursor: null } };
    return undefined;
  });
  const failed = await f.collector.step(f.inputId); assert.equal(failed?.value.error?.code, 'chat_collection_gap');
  const aged = { ...failed!.value, updatedAt: '2000-01-01T00:00:00.000Z' };
  await f.first.store.write('chat-bridge/collection', aged, 'age-transient-gap', { objectId: failed!.pin.objectId, expectedRevision: failed!.pin.revision });
  materialized = true; const complete = await finish(f, new ChatCollector(f.first));
  assert.equal(complete.value.attempt, 2); assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
  const result = await f.first.store.read('chat-bridge/result', complete.value.result!, f.inputId);
  assert.equal(await text(f, result.value), 'Materialized answer');
});
async function running(t: Parameters<typeof fixture>[0], version: Chat.NativePlan['nativeVersion'] = '0.154.0') {
  const f = await fixture(t, version), created = await f.first.create('alice', f.request('main')); if (created.outcome?.action !== 'createMain') throw Error('Expected Main');
  const sent = await f.first.queue.send('alice', { action: 'send', operationId: f.operation('original-message'), messageId: 'original-message',
    channel: f.channel, payload: { text: 'Original question', images: [] }, expectedBridge: f.admission.expected('alice'), expectedBinding: created.outcome.binding.object });
  if (sent.outcome?.action !== 'send') throw Error('Expected Input');
  const inputId = sent.outcome.input.object.objectId, delivery = new ChatDelivery(f.first);
  for (let i = 0; i < 4; i++) await delivery.step();
  const input = await f.first.store.read('chat-bridge/input', inputId); assert.equal(input.value.state, 'running');
  return { ...f, inputId, input, primary: created.outcome.binding.data.primary.nativeId, collector: new ChatCollector(f.first),
    turn: (extra: Record<string, Wire.Json> = {}) => ({ id: input.value.turnId!, status: 'completed', items: [], itemsView: 'notLoaded', ...extra }) };
}
async function finish(f: Awaited<ReturnType<typeof running>>, collector = f.collector) {
  for (let i = 0; i < 12; i++) {
    const value = await collector.step(f.inputId);
    if (value?.value.state === 'complete') return value;
    assert.notEqual(value?.value.state, 'failed', value?.value.error?.detail);
  }
  throw Error('Collection did not finish');
}
async function text(f: Awaited<ReturnType<typeof running>>, result: Chat.Result): Promise<string> {
  const chunks: string[] = [];
  for (const artifact of result.textParts) {
    const saved = await f.first.store.client.request('objects.read', artifact.object); assert.equal(saved.content.encoding, 'text');
    if (saved.content.encoding !== 'text') throw Error('Expected text');
    const bytes = Buffer.from(saved.content.value, 'utf8'); assert.equal(digest(bytes), artifact.contentHash); assert.equal(bytes.length, artifact.byteLength);
    assert.ok(bytes.length <= 1048576); assert.equal(saved.object.parentId, f.inputId); chunks.push(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
  }
  return chunks.join('');
}

for (const version of ['0.154.0'] as const) test('Chat ' + version + ' saves complete paged reply text with concurrent collection and retains the queue head', async t => {
  const f = await running(t, version), large = 'x'.repeat(1048575) + '😀end';
  const item = (id: string, text: string) => ({ turnId: f.input.value.turnId!, item: message(id, text) });
  f.reply(frame => {
    const params = frame['params'] as Record<string, Wire.Json>;
    if (frame['method'] === 'thread/turns/list') return { result: { data: [f.turn()], nextCursor: null } };
    if (frame['method'] === 'thread/items/list') return { result: params['cursor'] === null ? { data: [item('part1', large)], nextCursor: 'rest' }
      : { data: [item('part2', 'Last original reply')], nextCursor: null } };
    return undefined;
  });
  const second = new ChatCollector(f.second);
  for (let i = 0; i < 5; i++) await Promise.all([f.collector.step(f.inputId), second.step(f.inputId)]);
  const complete = await finish(f), result = await f.first.store.read('chat-bridge/result', complete.value.result!, f.inputId);
  assert.equal(result.pin.revision, 1); assert.equal(result.value.turnId, f.input.value.turnId); assert.equal(result.value.evidence.length, 4);
  assert.equal(await text(f, result.value), large + '\n\nLast original reply'); assert.equal(result.value.textParts.length, 2);
  assert.equal(await readChatResultText(f.first.store, f.inputId, result.value), large + '\n\nLast original reply');
  const badPart = { ...result.value.textParts[0]!, contentHash: digest('different') };
  await assert.rejects(readChatResultText(f.first.store, f.inputId, { ...result.value, textParts: [badPart] }), error => error instanceof IvyError && error.code === 'chat_evidence_mismatch');
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
  assert.equal((await f.first.queue.main()).value.queue[0]?.inputId, f.inputId); assert.equal((await f.first.store.read('chat-bridge/input', f.inputId)).value.state, 'running');
  const frameCount = f.frames.length; f.unavailable(true); f.intercept(() => { throw Error('Completed result recovery must be read-only'); });
  const recovered = await new ChatCollector(f.second).step(f.inputId);
  assert.deepEqual(recovered?.pin, complete.pin); assert.equal(f.frames.length, frameCount);
});

test('Chat presentation parts retain every Unicode character and all original result artifacts within reply limits', () => {
  const text = '😀'.repeat(16385) + 'last', textParts: Chat.Artifact[] = Array.from({ length: 64 }, (_, i) => ({ object: { objectId: 'part-' + i, revision: 1 },
    contentHash: digest('text-' + i), byteLength: 1, mediaType: 'text/plain', label: 'Part ' + i }));
  const result: Chat.Result = { schemaVersion: 1, inputId: 'input', binding: { objectId: 'binding', revision: 1 }, turnId: 'turn', nativeState: 'completed',
    evidence: [{ objectId: 'original-evidence', revision: 1 }], textParts, createdAt: '2026-09-07T04:00:00.000Z' };
  const parts = chatReplyParts(result, text);
  assert.equal(parts.length, 2); assert.equal(parts.map(part => part.text).join(''), text); assert.deepEqual(parts.flatMap(part => part.artifacts), textParts);
  for (const part of parts) { assert.ok([...part.text].length <= 16384); assert.ok(part.artifacts.length <= 32); }
  assert.equal(chatReplyParts({ ...result, textParts: [], nativeState: 'failed' }, '')[0]?.text, 'Native turn failed.');
});

test('Chat full-turn collection preserves the old epoch checkpoint and completes the original turn after a native restart', async t => {
  const f = await running(t); let pending = true;
  f.reply(frame => {
    const params = frame['params'] as Record<string, Wire.Json>;
    if (frame['method'] === 'thread/items/list') return { error: { code: -32601, message: 'Native items method unsupported' } };
    if (frame['method'] === 'thread/turns/list') return { result: { data: [f.turn({ status: pending ? 'inProgress' : 'completed',
      ...(params['itemsView'] === 'full' ? { itemsView: 'full', items: [message('answer', 'Saved after native restart')] } : {}) })], nextCursor: null } };
    return undefined;
  });
  const waiting = await f.collector.step(f.inputId); assert.equal(waiting?.value.reads.length, 0);
  pending = false; const old = await f.collector.step(f.inputId); assert.equal(old?.value.reads.length, 1);
  f.restart(); const complete = await finish(f, new ChatCollector(f.second));
  assert.equal(complete.value.attempt, 2); assert.deepEqual(complete.value.previousAttempt, old!.pin); assert.notEqual(complete.value.epoch, old!.value.epoch);
  const original = await f.first.store.read('chat-bridge/collection', old!.pin, f.inputId); assert.equal(original.value.reads.length, 1);
  const result = await f.first.store.read('chat-bridge/result', complete.value.result!, f.inputId);
  assert.equal(result.value.evidence.length, 4); assert.equal(await text(f, result.value), 'Saved after native restart');
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1); assert.equal(f.frames.filter(frame => frame['method'] === 'thread/resume').length, 0);
});

test('Chat result publication recovers retained text and Result response losses with the same original native turn', async t => {
  const f = await running(t);
  f.reply(frame => frame['method'] === 'thread/turns/list' ? { result: { data: [f.turn()], nextCursor: null } }
    : frame['method'] === 'thread/items/list' ? { result: { data: [{ turnId: f.input.value.turnId!, item: message('answer', 'Retained complete answer') }], nextCursor: null } } : undefined);
  const boundaries = ['text', 'result'], lost: string[] = [];
  f.intercept((params, saved) => {
    const value = params.content.encoding === 'json' ? params.content.value as Record<string, Wire.Json> : {};
    const key = saved.object.contractKey, target = boundaries[lost.length];
    const matches = target === 'text' ? key === 'chat-bridge/result-text' : key === 'chat-bridge/result';
    if (matches) { lost.push(target!); throw new IvyError('outcome_unknown', 'Lost committed ' + target + ' response.', 'unknown'); }
  });
  for (let i = 0; i < 18 && lost.length < boundaries.length; i++) {
    try { await new ChatCollector(i % 2 ? f.first : f.second).step(f.inputId); }
    catch (error) { assert.ok(error instanceof IvyError && error.code === 'outcome_unknown'); }
  }
  assert.deepEqual(lost, boundaries); f.intercept(null); f.unavailable(true);
  const complete = await f.collector.step(f.inputId); assert.equal(complete?.value.state, 'complete');
  const result = await f.first.store.read('chat-bridge/result', complete!.value.result!, f.inputId); assert.equal(await text(f, result.value), 'Retained complete answer');
  const saved = await f.first.store.client.request('objects.query', { contractKey: 'chat-bridge/result', where: { op: 'eq', field: 'object.parentId', value: f.inputId }, limit: 10 });
  assert.equal(saved.items.length, 1); assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
});

test('Chat retains the exact native collection error and keeps the original input blocked without a fabricated Result', async t => {
  const f = await running(t);
  f.reply(frame => frame['method'] === 'thread/turns/list' ? { result: { data: [f.turn()], nextCursor: null } }
    : frame['method'] === 'thread/items/list' ? { error: { code: -32000, message: 'Original native read refused' } } : undefined);
  await f.collector.step(f.inputId);
  let lost = false;
  f.intercept((_params, saved) => {
    if (!lost && saved.object.contractKey === 'chat-bridge/input') { lost = true; throw new IvyError('outcome_unknown', 'Lost committed input failure observation.', 'unknown'); }
  });
  await assert.rejects(f.collector.step(f.inputId), (error: unknown) => error instanceof IvyError && error.code === 'outcome_unknown');
  assert.equal(lost, true); f.intercept(null);
  const before = await f.first.store.read('chat-bridge/input', f.inputId);
  const failed = await f.collector.step(f.inputId);
  const visible = await f.first.store.read('chat-bridge/input', f.inputId);
  assert.deepEqual(visible.pin, before.pin); assert.equal(visible.value.state, 'collecting'); assert.deepEqual(visible.value.error, failed?.value.error);
  assert.equal(visible.value.finishedAt, null);
  assert.equal(failed?.value.state, 'failed'); assert.equal(failed?.value.error?.code, 'chat_collection_native_error'); assert.equal(failed?.value.reads.length, 2);
  const pin = failed!.value.reads[1]!, retained = await f.first.native.evidence.read(f.inputId, { serviceNodeId: 'native-agent', callerPrincipalId: 'chat-owner',
    nativeVersion: '0.154.0', epoch: failed!.value.epoch, method: 'thread/items/list', params: { threadId: f.primary, turnId: f.input.value.turnId!, cursor: null, limit: 8, sortDirection: 'asc' } }, pin);
  assert.deepEqual(retained.reply, { error: { code: -32000, message: 'Original native read refused' } });
  const count = f.frames.length; f.restart(); assert.deepEqual((await new ChatCollector(f.second).step(f.inputId))?.pin, failed?.pin); assert.equal(f.frames.length, count);
  assert.equal((await f.first.store.read('chat-bridge/input', f.inputId)).value.result, null); assert.equal((await f.first.queue.main()).value.queue[0]?.inputId, f.inputId);
  const results = await f.first.store.client.request('objects.query', { contractKey: 'chat-bridge/result', limit: 10 }); assert.equal(results.items.length, 0);
});
