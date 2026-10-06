import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Chat, OperationName, Params, Result, Wire } from '../packages/contracts/src/generated.js';
import type { RpcClient } from '../packages/sdk/src/client.js';
import { ChatDelivery } from '../services/chat-bridge/src/delivery.js';
import { ChatCollector } from '../services/chat-bridge/src/collector.js';
import { ChatPublisher } from '../services/chat-bridge/src/publisher.js';
import { ChatWorker } from '../services/chat-bridge/src/worker.js';
import { ChatOutbox } from '../services/chat-bridge/src/outbox.js';
import { ChatOperations } from '../services/chat-bridge/src/operations.js';
import { ChatStore } from '../services/chat-bridge/src/store.js';
import { ChatNotices } from '../services/chat-bridge/src/notices.js';
import { chatNativeFixture as fixture } from './fixtures/chat-native.js';

async function setup(t: Parameters<typeof fixture>[0], options: { version?: Chat.NativePlan['nativeVersion']; text?: string; tail?: boolean; failed?: boolean } = {}) {
  const version = options.version ?? '0.154.0', f = await fixture(t, version), created = await f.first.create('alice', f.request('main'));
  if (created.outcome?.action !== 'createMain') throw Error('Expected Main');
  const binding = created.outcome.binding.object;
  const send = (messageId: string, caller = 'alice') => f.first.queue.send(caller, { action: 'send', operationId: f.operation(messageId), messageId,
    expectedBridge: f.admission.expected(caller), channel: f.channel, expectedBinding: binding, payload: { text: messageId, images: [] } });
  const sent = await send('original'); if (sent.outcome?.action !== 'send') throw Error('Expected Input');
  const inputId = sent.outcome.input.object.objectId, tail = options.tail ? await send('preexisting-tail', 'bob') : null;
  const delivery = new ChatDelivery(f.first); for (let i = 0; i < 4; i++) await delivery.step();
  const input = await f.first.store.read('chat-bridge/input', inputId); assert.ok(input.value.turnId);
  const answer = options.text ?? 'Complete original answer', item = { id: 'original-answer', type: 'agentMessage', text: answer };
  f.reply(frame => {
    if (frame['method'] === 'thread/turns/list') return { result: { data: [{ id: input.value.turnId!, status: options.failed ? 'failed' : 'completed',
      items: [], itemsView: 'notLoaded', ...(options.failed ? { error: { message: 'Original native failure with retained output' } } : {}) }], nextCursor: null } };
    if (frame['method'] === 'thread/items/list') return { result: { data: [{ turnId: input.value.turnId!, item }], nextCursor: null } };
    return undefined;
  });
  const outbox = new ChatOutbox(f.first.operations), history: Chat.HistoryQuery = { expectedBridge: f.admission.expected('alice'), channel: f.channel, afterSequence: 0, limit: 8 };
  const receive = (label = randomUUID(), afterSequence = 0): Chat.ReceiveRequest => ({ action: 'receive', operationId:f.operation(label), ...history, afterSequence });
  const collector = new ChatCollector(f.first), publisher = new ChatPublisher(collector);
  return { ...f, binding, inputId, input, tailId: tail?.outcome?.action === 'send' ? tail.outcome.input.object.objectId : null,
    send, answer, outbox, history, receive, collector, publisher };
}
async function collected(f: Awaited<ReturnType<typeof setup>>) {
  for (let i = 0; i < 6; i++) { const value = await f.collector.step(f.inputId); if (value?.value.state === 'complete') return value; }
  throw Error('Expected complete result');
}
async function released(f: Awaited<ReturnType<typeof setup>>) {
  for (let i = 0; i < 20; i++) {
    if ((await f.first.queue.main()).value.queue[0]?.inputId !== f.inputId) return;
    await f.publisher.step();
  }
  throw Error('Original publication did not release');
}

test('a service notice during native multipart publication cannot advance the browser cursor across the pending result', async t => {
  const f = await setup(t, { text: 'Original native answer. '.repeat(9000) });
  await collected(f); await f.publisher.step();
  const claim = (await f.first.queue.main()).value.publication; assert.ok(claim); assert.ok(claim.partCount > 8);
  const notice = await new ChatNotices(f.second).notify('task-board', { action: 'notify', operationId: f.operation('review-during-publication'),
    source: f.binding, text: 'Another result is ready for review.' });
  assert.equal(notice.phase, 'accepted'); assert.equal(notice.outcome, null);
  const queued = (await f.first.queue.main()).value.queue.at(-1)!; assert.equal(queued.sequence, claim.firstSequence + claim.partCount); assert.equal(queued.identity.senderPrincipalId, 'task-board');
  assert.deepEqual((await f.outbox.history('alice', f.history)).entries.map(entry => entry.data.sequence), [1]);
  const blocked = await f.outbox.receive('alice', f.receive()); assert.ok(blocked.outcome?.action === 'receive'); assert.equal(blocked.outcome.replies.length, 0);
  await released(f);
  const entries: Chat.ReplyView[] = []; let after = 0;
  while (entries.length < claim.partCount) {
    const page = await f.outbox.receive('alice', f.receive(randomUUID(), after)); assert.ok(page.outcome?.action === 'receive');
    assert.ok(page.outcome.replies.length); entries.push(...page.outcome.replies); after = page.outcome.offer.data.throughSequence;
  }
  assert.deepEqual(entries.map(entry => entry.data.sequence), Array.from({ length: claim.partCount }, (_, i) => i + 2));
  assert.equal(entries.map(entry => entry.data.text).join(''), f.answer);
  assert.equal((await f.first.queue.main()).value.queue[0]?.identity.senderPrincipalId, 'task-board');
});

test('Chat publication recovers retained reply response loss, keeps reserved replies invisible and starts the next original input only after release', async t => {
  const f = await setup(t, { text: '😀'.repeat(9 * 16384 + 3), tail: true }); await collected(f);
  const boundaries = ['reply'], lost: string[] = [];
  f.intercept(async (params, saved) => {
    const value = params.content.encoding === 'json' ? params.content.value as Record<string, Wire.Json> : {}, key = saved.object.contractKey, target = boundaries[lost.length];
    if (key === 'chat-bridge/reply') {
      const input = await f.first.store.read('chat-bridge/input', f.inputId); assert.equal(input.value.state, 'completed'); assert.ok(input.value.result);
    }
    const matches = target === 'reply' && key === 'chat-bridge/reply';
    if (matches) { lost.push(target!); throw new IvyError('outcome_unknown', 'Lost committed ' + target, 'unknown'); }
  });
  let laterId: string | null = null;
  for (let i = 0; i < 20 && lost.length < boundaries.length; i++) {
    try { await new ChatPublisher(new ChatCollector(i % 2 ? f.first : f.second)).step(); }
    catch (error) { assert.ok(error instanceof IvyError && error.code === 'outcome_unknown', String(error)); }
    const main = await f.first.queue.main();
    if (main.value.publication) {
      assert.equal(main.value.publication.firstSequence, 3); assert.equal(main.value.publication.partCount, 10);
      if (!laterId) { const later = await f.send('admitted-during-publication', 'bob'); if (later.outcome?.action !== 'send') throw Error('Expected later input'); laterId = later.outcome.input.object.objectId; assert.equal(later.outcome.input.data.sequence, 13); }
      const history = await f.outbox.history('alice', f.history); assert.deepEqual(history.entries.map(entry => entry.data.sequence), [1, 2]);
      const offer = await f.outbox.receive('alice', f.receive()); assert.equal(offer.outcome?.action, 'receive'); if (offer.outcome?.action === 'receive') assert.equal(offer.outcome.replies.length, 0);
      assert.equal(main.value.queue[0]?.inputId, f.inputId); assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
    }
  }
  assert.deepEqual(lost, boundaries); f.intercept(null); await released(f);
  const main = await f.first.queue.main(); assert.equal(main.value.publication, null); assert.deepEqual(main.value.queue.map(ticket => ticket.inputId), [f.tailId, laterId]);
  const first = await f.outbox.receive('alice', f.receive()), texts: string[] = [], replyIds: string[] = [];
  if (first.outcome?.action !== 'receive') throw Error('Expected first offer');
  assert.equal(first.outcome.replies.length, 8); texts.push(...first.outcome.replies.map(reply => reply.data.text)); replyIds.push(...first.outcome.replies.map(reply => reply.object.objectId));
  const second = await f.outbox.receive('alice', f.receive(randomUUID(), first.outcome.offer.data.throughSequence)); if (second.outcome?.action !== 'receive') throw Error('Expected second offer');
  assert.equal(second.outcome.replies.length, 2); texts.push(...second.outcome.replies.map(reply => reply.data.text)); replyIds.push(...second.outcome.replies.map(reply => reply.object.objectId));
  assert.equal(texts.join(''), f.answer); assert.equal(new Set(replyIds).size, 10);
  await f.outbox.acknowledge('alice', { action: 'acknowledge', operationId: f.operation('ack'), expectedBridge: f.admission.expected('alice'), offerId: first.outcome.offer.object.objectId,
    replyIds: first.outcome.replies.map(reply => reply.object.objectId) });
  await f.publisher.step(); assert.equal((await f.first.store.read('chat-bridge/reply', replyIds[0]!)).value.state, 'confirmed');
  const worker = new ChatWorker(f.second);
  for (let i = 0; i < 4 && (await f.first.store.read('chat-bridge/input', f.tailId!)).value.state !== 'running'; i++) await worker.step();
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 2); assert.equal((await f.first.store.read('chat-bridge/input', f.tailId!)).value.state, 'running');
});

test('Chat history and receive retry a publication change during their query instead of offering a partial range', async t => {
  const f = await setup(t, { text: 'x'.repeat(9 * 16384 + 1), tail: true }); await collected(f);
  let change: 'start' | 'finish' | null = 'start', queries = 0;
  const client: RpcClient = { async request<M extends OperationName>(method: M, params: Params<M>): Promise<Result<M>> {
    const isReplies = method === 'objects.query' && (params as Params<'objects.query'>).contractKey === 'chat-bridge/reply';
    if (isReplies && change === 'start') {
      change = null; await f.publisher.step(); await f.publisher.step(); await f.publisher.step(); await f.send('later-during-query');
    }
    const value = await f.clients.first.client.request(method, params);
    if (isReplies) { queries++; if (change === 'finish') { change = null; await released(f); } }
    return value;
  } };
  const queryStore = new ChatStore(client, null, f.dataRoot);
  const outbox = new ChatOutbox(new ChatOperations(queryStore, f.admission));
  const history = await outbox.history('alice', f.history); assert.deepEqual(history.entries.map(entry => entry.data.sequence), [1, 2]); assert.ok(queries >= 2);
  const main = await f.first.queue.main(); assert.equal(main.value.publication?.publishedParts, 8); change = 'finish'; queries = 0;
  const received = await outbox.receive('alice', f.receive()); if (received.outcome?.action !== 'receive') throw Error('Expected recovered offer');
  assert.ok(queries >= 2); assert.deepEqual(received.outcome.replies.map(reply => reply.data.sequence), [3, 4, 5, 6, 7, 8, 9, 10]);
  assert.equal(received.outcome.offer.data.throughSequence, 10); assert.equal((await f.first.queue.main()).value.publication, null);
  queryStore.close();
});

test('concurrent Chat workers publish the complete original failed turn once and delayed delivery keeps its binding after explicit Main replacement', async t => {
  const f = await setup(t, { version: '0.154.0', failed: true }), a = new ChatWorker(f.first), b = new ChatWorker(f.second);
  for (let i = 0; i < 14 && (await f.first.queue.main()).value.queue.length; i++) await Promise.all([a.step(), b.step()]);
  const main = await f.first.queue.main(); assert.equal(main.value.queue.length, 0); assert.equal(main.value.publication, null);
  const terminal = await f.first.store.read('chat-bridge/input', f.inputId); assert.equal(terminal.value.state, 'failed'); assert.equal(terminal.value.error?.code, 'chat_native_turn_failed'); assert.ok(terminal.value.result);
  const result = await f.first.store.read('chat-bridge/result', terminal.value.result!, f.inputId); assert.equal(result.value.nativeState, 'failed');
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
  const replacement = await f.second.create('alice', f.request('replacement', main.pin.revision)); if (replacement.outcome?.action !== 'createMain') throw Error('Expected replacement');
  assert.notEqual(replacement.outcome.binding.object.objectId, f.binding.objectId);
  const received = await f.outbox.receive('alice', f.receive()); if (received.outcome?.action !== 'receive') throw Error('Expected delayed reply');
  assert.equal(received.outcome.replies.length, 1); assert.equal(received.outcome.replies[0]?.data.text, f.answer);
  const origin = received.outcome.replies[0]!.data.origin; assert.equal(origin.kind, 'native'); assert.ok(origin.kind === 'native');
  assert.deepEqual(origin.binding, f.binding); assert.equal(origin.inputId, f.inputId);
  assert.equal((await f.first.find())?.value.binding?.objectId, replacement.outcome.binding.object.objectId);
});

test('Chat history and delivery use retained current records after old input and reply revisions are pruned', async t => {
  const f = await setup(t); await collected(f); await released(f);
  const offered = await f.outbox.receive('alice', f.receive());
  if (offered.outcome?.action !== 'receive') throw Error('Expected offer');
  const reply = offered.outcome.replies[0]!;
  await f.outbox.acknowledge('alice', { action: 'acknowledge', operationId: f.operation('pruned-ack'),
    expectedBridge: f.admission.expected('alice'), offerId: offered.outcome.offer.object.objectId, replyIds: [reply.object.objectId] });
  f.kernel.retention.collect(); f.kernel.retention.collect();
  await assert.rejects(f.first.store.read('chat-bridge/input', { objectId: f.inputId, revision: 1 }), { code: 'revision_pruned' });
  await assert.rejects(f.first.store.read('chat-bridge/reply', { objectId: reply.object.objectId, revision: 1 }), { code: 'revision_pruned' });
  assert.equal((await f.outbox.reply(reply.object.objectId, f.channel)).value.state, 'confirmed');
  const history = await f.outbox.history('alice', f.history);
  assert.deepEqual(history.entries.map(entry => entry.data.sequence), [1, 2]);
});
