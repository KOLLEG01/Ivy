import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Chat, Params, Wire } from '../packages/contracts/src/generated.js';
import { ChatNotices } from '../services/chat-bridge/src/notices.js';
import { ChatOutbox } from '../services/chat-bridge/src/outbox.js';
import { ChatBridge } from '../services/chat-bridge/src/bridge.js';
import { ChatDelivery } from '../services/chat-bridge/src/delivery.js';
import { ChatCollector } from '../services/chat-bridge/src/collector.js';
import { ChatPublisher } from '../services/chat-bridge/src/publisher.js';
import { ChatWorker } from '../services/chat-bridge/src/worker.js';
import { ChatAdmission } from '../services/chat-bridge/src/admission.js';
import { ChatMain } from '../services/chat-bridge/src/chat-main.js';
import { ChatNativeDriver } from '../services/chat-bridge/src/native-driver.js';
import { ChatOperations } from '../services/chat-bridge/src/operations.js';
import { chatNativeFixture } from './fixtures/chat-native.js';
import { operationId } from '../packages/contracts/src/operation-id.js';
import { mutation } from '../services/chat-bridge/src/store.js';

const code = (value: string) => (error: unknown) => error instanceof IvyError && error.code === value;
const data = (params: Params<'objects.write'>) => params.content.encoding === 'json' ? params.content.value as Record<string, unknown> : {};
async function fixture(t: Parameters<typeof chatNativeFixture>[0], createMain = true) {
  const f = await chatNativeFixture(t, '0.154.0');
  const epoch = (await f.first.store.client.request('system.status', {})).runtimeEpoch, issuedAt = Date.now();
  const actionId = (nonce: string) => operationId(epoch, issuedAt, nonce.replaceAll(':', '-'));
  const created = createMain ? await f.first.create('alice', f.request(actionId('main'))) : null;
  if (createMain) assert.ok(created?.outcome?.action === 'createMain');
  const source = created?.outcome?.action === 'createMain' ? created.outcome.binding.object : await f.first.store.write('chat-bridge/conversation',
    { schemaVersion: 1, workspaceId: f.admission.definition.workspaceId, definitionHash: f.admission.definitionHash, createdAt: new Date().toISOString() },
    randomUUID(), { create: { parentId: f.first.store.rootObjectId, name: 'Notice source without Main' } });
  const first = new ChatNotices(f.first), second = new ChatNotices(f.second), outbox = new ChatOutbox(f.first.operations);
  const request = (identity = 'review-original', caller = 'task-board'): Chat.NotifyRequest => ({ action: 'notify', operationId: actionId(identity),
    source, text: 'Ready for review: original result 😀' });
  const receive = (): Chat.ReceiveRequest => ({ action: 'receive', operationId: operationId(epoch), expectedBridge: f.admission.expected('alice'), channel: f.channel, afterSequence: 0, limit: 8 });
  const history = (): Chat.HistoryQuery => ({ expectedBridge: f.admission.expected('alice'), channel: f.channel, afterSequence: 0, limit: 8 });
  const complete = async (inputId: string, answer = 'Ready for review: original result 😀') => {
    const delivery = new ChatDelivery(f.first);
    for (let i = 0; i < 6; i++) await delivery.step();
    const input = await f.first.store.read('chat-bridge/input', inputId); assert.ok(input.value.turnId);
    f.reply(frame => {
      if (frame['method'] === 'thread/turns/list') return { result: { data: [{ id: input.value.turnId!, status: 'completed', items: [], itemsView: 'notLoaded' }], nextCursor: null } };
      if (frame['method'] === 'thread/items/list') return { result: { data: [{ turnId: input.value.turnId!, item: { id: 'answer', type: 'agentMessage', text: answer } }], nextCursor: null } };
      return undefined;
    });
    const collector = new ChatCollector(f.first), publisher = new ChatPublisher(collector);
    for (let i = 0; i < 8; i++) await collector.step(inputId);
    for (let i = 0; i < 16 && (await f.first.queue.main()).value.queue[0]?.inputId === inputId; i++) await publisher.step();
    assert.notEqual((await f.first.queue.main()).value.queue[0]?.inputId, inputId);
  };
  return { ...f, firstMain: f.first, secondMain: f.second, first, second, source, request, outbox, receive, history, complete };
}

test('a first service handoff automatically creates Main before it enters the shared queue', async t => {
  const f = await fixture(t, false); assert.equal(await f.firstMain.find(), null);
  const accepted = await Promise.all([f.first.notify('task-board', f.request()), f.second.notify('task-board', f.request())]);
  assert.deepEqual(accepted[0], accepted[1]);
  const main = await f.firstMain.queue.main(); assert.ok(main.value.binding); assert.equal(main.value.queue.length, 1);
  assert.equal(main.value.queue[0]!.identity.senderPrincipalId, 'task-board');
  assert.equal(f.frames.filter(frame => frame['method'] === 'thread/start').length, 1);
});

test('a service handoff that fails before its native turn reports the original failure and releases its queue ticket', async t => {
  const f = await fixture(t), request = f.request();
  await f.first.notify('task-board', request);
  f.threadStatus({ type: 'notLoaded' });
  f.reply(frame => frame['method'] === 'thread/resume' ? { error: { code: -32600, message: 'The original Main is unavailable.' } } : undefined);
  const worker = new ChatWorker(f.firstMain);
  for (let i = 0; i < 6; i++) await worker.step();
  assert.deepEqual((await f.firstMain.queue.main()).value.queue, []);
  const failed = await f.first.notify('task-board', request);
  assert.equal(failed.phase, 'failed'); assert.ok(failed.error); assert.equal(failed.outcome, null);
  assert.deepEqual(await f.second.notify('task-board', request), failed);
  await assert.rejects(f.second.notice('task-board', { operationId: request.operationId }), code(failed.error.code));
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 0);
});

test('cancelling an undispatched service handoff also settles its producer operation', async t => {
  const f = await fixture(t), request = f.request(); await f.first.notify('task-board', request);
  const inputId = (await f.firstMain.queue.main()).value.queue[0]!.inputId;
  const input = await f.firstMain.store.read('chat-bridge/input', inputId), bridge = new ChatBridge(f.firstMain);
  const cancelled = await bridge.cancellation.cancel('alice', { action: 'cancel', operationId: f.operation('cancel-notice'),
    expectedBridge: f.admission.expected('alice'), inputId, expectedRevision: input.pin.revision, reason: 'No longer needed' });
  assert.equal(cancelled.phase, 'succeeded');
  const failed = await f.second.notify('task-board', request);
  assert.equal(failed.phase, 'failed'); assert.equal(failed.error?.code, 'chat_notice_cancelled');
  assert.deepEqual((await f.firstMain.queue.main()).value.queue, []);
});

test('a service notice is an idempotent hidden assignment to the current Main and only Main\'s answer is delivered', async t => {
  const f = await fixture(t), request = f.request(), before = f.frames.length;
  const accepted = await f.first.notify('task-board', request);
  assert.equal(accepted.phase, 'accepted'); assert.equal(accepted.outcome, null);
  assert.deepEqual(await f.second.notify('task-board', request), accepted);
  await assert.rejects(f.second.notify('task-board', { ...request, text: 'Changed announcement' }), code('mutation_conflict'));
  await assert.rejects(f.second.notice('task-board', { operationId: request.operationId }), code('chat_publication_pending'));
  const main = await f.firstMain.queue.main(), inputId = main.value.queue[0]!.inputId;
  const handoff = await f.firstMain.store.read('chat-bridge/input', inputId);
  assert.equal(handoff.value.binding.objectId, f.source.objectId);
  assert.equal(handoff.value.identity.senderPrincipalId, 'task-board');
  assert.match(handoff.value.payload.text, /You are already Ivy Main/);
  assert.match(handoff.value.payload.text, /Ready for review: original result/);
  assert.equal((await f.outbox.history('alice', f.history())).entries.length, 0, 'The internal service assignment is not user-visible.');
  await f.complete(inputId);
  const published = await f.second.notify('task-board', request); assert.ok(published.outcome?.action === 'notify');
  assert.equal(published.outcome.reply.data.origin.kind, 'native');
  assert.equal(published.outcome.reply.data.text, '[[Automation] TaskBoard] Ready for review: original result 😀');
  const turn = f.frames.find(frame => frame['method'] === 'turn/start')!;
  const binding = await f.firstMain.store.read('chat-bridge/binding', f.source);
  assert.equal(((turn['params'] as Record<string, Wire.Json>)['threadId']), binding.value.primary.nativeId);
  assert.match(JSON.stringify((turn['params'] as Record<string, Wire.Json>)['input']), /You are already Ivy Main/);
  assert.match(JSON.stringify((turn['params'] as Record<string, Wire.Json>)['additionalContext']), /service notification from task-board/);
  assert.equal(f.firstMain.store.isNoticeInput(mutation(inputId, 'native-user-message')), true);
  assert.ok(f.frames.length > before, 'The notice must invoke the native Main.');
  const visible = await f.outbox.history('alice', f.history()); assert.equal(visible.entries.length, 1); assert.equal(visible.entries[0]!.data.sequence, 2);
  const offered = await f.outbox.receive('alice', f.receive()); assert.ok(offered.outcome?.action === 'receive'); assert.equal(offered.outcome.replies.length, 1);
  await f.outbox.acknowledge('alice', { action: 'acknowledge', operationId: 'displayed', expectedBridge: f.admission.expected('alice'),
    offerId: offered.outcome.offer.object.objectId, replyIds: [published.outcome.reply.object.objectId] });
  assert.equal((await f.second.notice('task-board', { operationId: request.operationId })).data.state, 'confirmed');
});

test('a lost Main reply write recovers the original notice and one visible native answer', async t => {
  const f = await fixture(t), request = f.request(); await f.first.notify('task-board', request);
  const inputId = (await f.firstMain.queue.main()).value.queue[0]!.inputId;
  const delivery = new ChatDelivery(f.firstMain); for (let i = 0; i < 6; i++) await delivery.step();
  const input = await f.firstMain.store.read('chat-bridge/input', inputId); assert.ok(input.value.turnId);
  f.reply(frame => frame['method'] === 'thread/turns/list' ? { result: { data: [{ id: input.value.turnId!, status: 'completed', items: [], itemsView: 'notLoaded' }], nextCursor: null } }
    : frame['method'] === 'thread/items/list' ? { result: { data: [{ turnId: input.value.turnId!, item: { id: 'answer', type: 'agentMessage', text: 'Main answer' } }], nextCursor: null } } : undefined);
  const collector = new ChatCollector(f.firstMain); for (let i = 0; i < 8; i++) await collector.step(inputId);
  let lost = false; f.intercept(async (params, saved) => {
    if (!lost && saved.object.contractKey === 'chat-bridge/reply') { lost = true; assert.equal(data(params)['origin'] && (data(params)['origin'] as Record<string, unknown>)['kind'], 'native'); throw new IvyError('outcome_unknown', 'Lost committed reply', 'unknown'); }
  });
  const publisher = new ChatPublisher(collector);
  for (let i = 0; i < 3; i++) { try { await publisher.step(); } catch (error) { assert.ok(code('outcome_unknown')(error)); } }
  f.intercept(null);
  for (let i = 0; i < 12 && (await f.firstMain.queue.main()).value.queue.length; i++) await publisher.step();
  assert.ok(lost); const published = await f.first.notify('task-board', request); assert.ok(published.outcome?.action === 'notify');
  const history = await f.outbox.history('alice', f.history()); assert.deepEqual(history.entries.map(entry => entry.data.sequence), [2]);
});

test('simultaneous producers and user input retain queue order while service assignments remain hidden', async t => {
  const f = await fixture(t), request = f.request();
  const admitted = await Promise.all([f.first.notify('task-board', request), f.second.notify('task-board', request)]);
  assert.deepEqual(admitted[0], admitted[1]);
  const sent = await f.firstMain.queue.send('alice', { action: 'send', operationId: f.operation('message'), messageId: 'message', expectedBridge: f.admission.expected('alice'),
    channel: f.channel, expectedBinding: f.source, payload: { text: 'User input', images: [] } });
  assert.ok(sent.outcome?.action === 'send'); assert.equal(sent.outcome.input.data.sequence, 2);
  await Promise.all([f.first.notify('task-board', f.request('next')), f.second.notify('digest', f.request('next', 'digest'))]);
  const main = await f.firstMain.queue.main(); assert.deepEqual(main.value.queue.map(ticket => ticket.sequence), [1, 2, 3, 4]);
  const history = await f.outbox.history('alice', f.history()); assert.deepEqual(history.entries.map(entry => entry.data.sequence), [2]);
  assert.equal(f.frames.length, 1, 'Admission alone must not start a separate native task.');
});

test('every authenticated notice producer may use Chat while exact channel and caller bindings remain enforced', async t => {
  const f = await fixture(t), bridge = new ChatBridge(f.firstMain), request = f.request();
  const context = { callerPrincipalId: 'task-board', generation: f.firstMain.native.owner.generation, signal: new AbortController().signal, operationId: request.operationId };
  assert.equal((await bridge.invoke('workspace', {}, context) as unknown as Chat.WorkspaceInfo).expectedBridge.callerPrincipalId, 'task-board');
  f.accesses.length = 0;
  await assert.rejects(f.first.notify('task-board', { ...request, channel: { ...f.channel, channelId: 'missing' } } as unknown as Chat.NotifyRequest), code('invalid_arguments'));
  assert.deepEqual(f.accesses, []);
  const result = await bridge.invoke('notify', request as unknown as Record<string, Wire.Json>, context) as unknown as Chat.Operation;
  assert.equal(result.phase, 'accepted');
  await assert.rejects(f.first.notice('alice', { operationId: request.operationId }), code('not_found'));
});

test('a nonexistent source or rewritten Main answer cannot become notice delivery proof', async t => {
  const f = await fixture(t), request = f.request();
  await assert.rejects(f.first.notify('task-board', { ...request, source: { objectId: randomUUID(), revision: 1 } }), code('not_found'));
  assert.equal(await f.firstMain.operations.find('task-board', request.operationId), null);
  await f.first.notify('task-board', request); const inputId = (await f.firstMain.queue.main()).value.queue[0]!.inputId; await f.complete(inputId, 'Original Main answer');
  const result = await f.first.notify('task-board', request); assert.ok(result.outcome?.action === 'notify'); const reply = result.outcome.reply;
  await f.firstMain.store.write('chat-bridge/reply', { ...reply.data, text: 'Rewritten content' }, randomUUID(), { objectId: reply.object.objectId, expectedRevision: reply.object.revision });
  await assert.rejects(f.second.notice('task-board', { operationId: request.operationId }), code('chat_identity_conflict'));
  await assert.rejects(f.outbox.receive('alice', f.receive()), code('chat_identity_conflict'));
});

test('a native configuration change preserves the original notice, WhatsApp confirmation and caller isolation', async t => {
  const f = await fixture(t), request = f.request();
  await f.first.notify('task-board', request);
  const inputId = (await f.firstMain.queue.main()).value.queue[0]!.inputId;
  await f.complete(inputId, 'Native answer '.repeat(2000));
  const original = await f.first.notice('task-board', { operationId: request.operationId });
  const definition = structuredClone(f.admission.definition);
  definition.channels[0]!.displayName = 'Main WhatsApp';
  definition.nativePlan.turnStart.params = { model: 'updated-model' };
  const admission = new ChatAdmission(definition), store = f.firstMain.store;
  await assert.rejects(store.validateOperationId(request.operationId, Date.now() + 2 * 86_400_000), code('operation_expired'));
  assert.notEqual(admission.definitionHash, f.admission.definitionHash);
  const main = new ChatMain(new ChatOperations(store, admission), new ChatNativeDriver(store, admission, f.firstMain.native.owner));
  const restarted = new ChatNotices(main), outbox = new ChatOutbox(main.operations);
  assert.deepEqual(await restarted.notice('task-board', { operationId: request.operationId }), original);
  const pending = await outbox.pendingNotices();
  assert.deepEqual(pending.replies.map(reply => reply.pin.objectId), [original.object.objectId]);
  const parts = await outbox.noticeParts(pending.replies[0]!);
  assert.deepEqual(parts.map(part => part.value.partIndex), [0, 1]);
  await outbox.confirmNotice(parts);
  assert.equal((await restarted.notice('task-board', { operationId: request.operationId })).data.state, 'confirmed');
  assert.deepEqual((await outbox.pendingNotices()).replies, []);
  assert.equal((await restarted.notify('task-board', request)).phase, 'succeeded');
  await assert.rejects(restarted.notice('another-producer', { operationId: request.operationId }), code('not_found'));
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
});
