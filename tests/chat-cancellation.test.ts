import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Chat, Wire } from '../packages/contracts/src/generated.js';
import { ChatCancellation } from '../services/chat-bridge/src/cancellation.js';
import { ChatWorker } from '../services/chat-bridge/src/worker.js';
import { ChatDelivery } from '../services/chat-bridge/src/delivery.js';
import { ChatQueries } from '../services/chat-bridge/src/queries.js';
import { chatNativeFixture as fixture } from './fixtures/chat-native.js';

const code = (value: string) => (error: unknown) => error instanceof IvyError && error.code === value;
async function queued(t: Parameters<typeof fixture>[0], isolatedChannels = false, channelCount = 1) {
  const f = await fixture(t, '0.154.0', definition => {
    if (isolatedChannels) {
      definition.channels.push({ channel: { ...definition.channels[0]!.channel, channelId: 'bob-only' }, displayName: 'Bob only' });
    } else if (channelCount > 1) {
      const original = definition.channels[0]!;
      definition.channels = Array.from({ length: channelCount }, (_, i) => ({ ...original, displayName: 'Channel ' + i,
        channel: { ...original.channel, channelId: i === channelCount - 1 ? original.channel.channelId : 'channel-' + i } }));
    }
  });
  const created = await f.first.create('alice', f.request('main')); if (created.outcome?.action !== 'createMain') throw Error('Expected Main');
  const binding = created.outcome.binding.object;
  const send = async (messageId: string, payload: Chat.Payload = { text: messageId, images: [] }) => {
    const result = await f.first.queue.send('alice', { action: 'send', operationId: f.operation(messageId), messageId, payload, channel: f.channel,
      expectedBridge: f.admission.expected('alice'), expectedBinding: binding });
    if (result.outcome?.action !== 'send') throw Error('Expected Input'); return result.outcome.input;
  };
  const input = await send('head'), tail = await send('tail'), first = new ChatCancellation(f.first), second = new ChatCancellation(f.second), worker = new ChatWorker(f.first);
  const request = async (inputId = input.object.objectId, identity: string = randomUUID(), caller = 'alice'): Promise<Chat.CancelRequest> => ({ action: 'cancel', operationId: f.operation(identity), inputId,
    expectedRevision: (await f.first.store.read('chat-bridge/input', inputId)).pin.revision, expectedBridge: f.admission.expected(caller), reason: 'Cancel this original synthetic input.' });
  let status = 'inProgress', currentTurn: string | null = null;
  f.reply(frame => {
    if (frame['method'] === 'turn/start') { currentTurn = 'native-turn-' + f.frames.length; return undefined; }
    if (frame['method'] === 'turn/interrupt') { status = 'interrupted'; return { result: {} }; }
    if (frame['method'] === 'thread/turns/list') return { result: { data: [{ id: currentTurn!, status, items: [], itemsView: 'notLoaded' }], nextCursor: null } };
    if (frame['method'] === 'thread/items/list') return { result: { data: [], nextCursor: null } };
    return undefined;
  });
  return { ...f, binding, send, inputId: input.object.objectId, tailId: tail.object.objectId, firstCancel: first, secondCancel: second, worker, mainRequest: f.request, request,
    current: () => f.first.store.read('chat-bridge/input', input.object.objectId), status: (value: string) => { status = value; } };
}
async function untilReleased(f: Awaited<ReturnType<typeof queued>>) {
  for (let i = 0; i < 18; i++) {
    if ((await f.first.queue.main()).value.queue[0]?.inputId !== f.inputId) return;
    await f.worker.step();
  }
  throw Error('Original cancelled input did not settle');
}

test('queued Chat cancellation filters channels before content reads, preserves its original receipt and frees queue tails without native availability', async t => {
  const f = await queued(t, true), foreign = await f.request(f.inputId, 'foreign', 'bob'), before = f.accesses.length;
  await assert.rejects(f.firstCancel.cancel('mallory', foreign), code('chat_definition_mismatch')); assert.ok(!f.accesses.slice(before).includes('objects.read'));
  const request = await f.request(f.tailId, 'tail-cancel'), frames = f.frames.length; f.unavailable(true);
  const completed = await f.firstCancel.cancel('alice', request); assert.equal(completed.phase, 'succeeded');
  if (completed.outcome?.action !== 'cancel') throw Error('Expected original cancellation receipt');
  assert.equal(completed.outcome.input.data.state, 'cancel_requested'); assert.equal(completed.outcome.input.data.cancellation?.reason, request.reason);
  const tail = await f.first.store.read('chat-bridge/input', f.tailId); assert.equal(tail.value.state, 'cancelled'); assert.equal(tail.value.turnId, null);
  assert.deepEqual((await f.first.queue.main()).value.queue.map(ticket => ticket.inputId), [f.inputId]); assert.equal(f.frames.length, frames);
  assert.deepEqual(await f.secondCancel.cancel('alice', request), completed);
  await assert.rejects(f.secondCancel.cancel('alice', { ...request, reason: 'Replace original reason' }), code('mutation_conflict'));
  await assert.rejects(f.firstCancel.cancel('alice', { ...await f.request(f.inputId, 'stale'), expectedRevision: 2 }), code('revision_conflict'));
  await f.firstCancel.cancel('alice', await f.request(f.inputId, 'head-cancel')); assert.equal((await f.first.queue.main()).value.queue.length, 0);
  const view = await new ChatQueries(f.first.operations).input('alice', { inputId: f.inputId, expectedBridge: f.admission.expected('alice') }); assert.equal(view.data.state, 'cancelled');
  f.unavailable(false); const main = await f.first.queue.main(); await f.second.create('alice', f.mainRequest('replacement', main.pin.revision));
  // Historical cancellation recovery needs neither the current native owner nor its old input revision.
  assert.deepEqual(await f.secondCancel.cancel('alice', request), completed); assert.ok(main.value.binding);
});

test('Chat cancellation is locally atomic and retains the exact original identity across instances', async t => {
  const f = await queued(t), request = await f.request(f.tailId, 'local-cancel');
  f.unavailable(true);
  let writes = 0; f.intercept(() => { writes++; });
  const result = await f.secondCancel.cancel('alice', request); f.intercept(null); assert.equal(writes, 2);
  assert.equal(result.phase, 'succeeded'); if (result.outcome?.action !== 'cancel') throw Error('Expected receipt');
  const original = await f.first.store.read('chat-bridge/input', result.outcome.input.object); assert.equal(original.value.state, 'cancel_requested');
  assert.equal((await f.first.store.read('chat-bridge/input', f.tailId)).value.state, 'cancelled'); assert.equal((await f.first.queue.main()).value.pendingAction, null);
  assert.deepEqual(await f.firstCancel.cancel('alice', request), result); assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 0);
});

for (const winner of ['prevention', 'native']) test('Chat original journal ' + winner + ' wins the cancellation/dispatch race without replacement calls', async t => {
  const f = await queued(t), delivery = new ChatDelivery(f.first); await delivery.step();
  let enter!: () => void, release!: () => void; const entered = new Promise<void>(resolve => { enter = resolve; }), gate = new Promise<void>(resolve => { release = resolve; });
  f.beforeNative(async call => { if (call.definition.name === 'turn/start') { enter(); await gate; } });
  const dispatch = delivery.step(); await entered;
  const original = (await f.current()).value.nativeCalls.turn!, request = await f.request(f.inputId, 'race-cancel'); await f.secondCancel.cancel('alice', request);
  if (winner === 'prevention') await f.worker.cancellation.step();
  release(); await dispatch; f.beforeNative(null); await untilReleased(f);
  const input = await f.current(); assert.equal(input.value.state, 'cancelled'); assert.equal(input.value.nativeCalls.turn?.objectId, original.objectId);
  const turn = await f.first.native.get(f.inputId, input.value.nativeCalls.turn!);
  assert.equal(turn.value.state, winner === 'prevention' ? 'failed' : 'succeeded');
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, winner === 'prevention' ? 0 : 1);
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/interrupt').length, winner === 'prevention' ? 0 : 1);
  if (winner === 'native') {
    assert.ok(input.value.result); const interrupt = f.frames.find(frame => frame['method'] === 'turn/interrupt')!.params as Record<string, Wire.Json>;
    assert.equal(interrupt['turnId'], input.value.turnId); assert.equal(interrupt['threadId'], (await f.first.store.read('chat-bridge/binding', f.binding)).value.primary.nativeId);
  } else { assert.equal(input.value.result, null); assert.equal(turn.value.code, 'request_prevented'); }
  assert.deepEqual((await f.first.queue.main()).value.queue.map(ticket => ticket.inputId), [f.tailId]);
});

test('cancellation retains an unknown original turn after restart and never releases or replays it', async t => {
  const f = await queued(t), delivery = new ChatDelivery(f.first); await delivery.step();
  f.hold('turn/start'); await delivery.step(); await f.firstCancel.cancel('alice', await f.request(f.inputId, 'unknown-turn-cancel')); f.restart();
  for (let i = 0; i < 3; i++) await f.worker.step();
  const input = await f.current(); assert.equal(input.value.state, 'outcome_unknown'); assert.ok(input.value.cancellation); assert.equal(input.value.turnId, null);
  assert.equal(input.value.error?.outcome, 'unknown'); assert.equal((await f.first.queue.main()).value.queue[0]?.inputId, f.inputId);
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1); assert.equal(f.frames.filter(frame => frame['method'] === 'turn/interrupt').length, 0);
});

test('an unknown original interrupt is not repeated and terminal native evidence can still complete cancellation', async t => {
  const f = await queued(t), delivery = new ChatDelivery(f.first); for (let i = 0; i < 4; i++) await delivery.step();
  await f.firstCancel.cancel('alice', await f.request(f.inputId, 'unknown-interrupt-cancel'));
  await f.worker.step(); f.hold('turn/interrupt'); await f.worker.step(); f.restart();
  await f.worker.step(); const unknown = await f.current(); assert.equal(unknown.value.state, 'outcome_unknown'); const original = unknown.value.nativeCalls.interrupt!;
  await f.worker.step(); assert.equal((await f.current()).value.nativeCalls.interrupt?.objectId, original.objectId);
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/interrupt').length, 1);
  f.status('interrupted'); await untilReleased(f); const completed = await f.current(); assert.equal(completed.value.state, 'cancelled'); assert.ok(completed.value.result);
  assert.equal(completed.value.nativeCalls.interrupt?.objectId, original.objectId); assert.equal(f.frames.filter(frame => frame['method'] === 'turn/interrupt').length, 1);
});

test('saved terminal completion takes precedence over late cancellation without another native interrupt', async t => {
  const f = await queued(t), delivery = new ChatDelivery(f.first); for (let i = 0; i < 4; i++) await delivery.step();
  f.status('completed'); await f.worker.collector.step(f.inputId);
  await f.firstCancel.cancel('alice', await f.request(f.inputId, 'late-cancel')); await untilReleased(f);
  const input = await f.current(); assert.equal(input.value.state, 'completed'); assert.ok(input.value.cancellation); assert.ok(input.value.result);
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/interrupt').length, 0);
});

test('pre-turn image refusal settles its original failed input and allows the next admitted turn', async t => {
  const f = await queued(t); await f.firstCancel.cancel('alice', await f.request(f.inputId, 'clear-head')); await f.firstCancel.cancel('alice', await f.request(f.tailId, 'clear-tail'));
  const bytes = Buffer.from('Invalid original PNG');
  const main = await f.first.queue.main();
  const image = await f.first.store.client.request('objects.write', { mutationId: randomUUID(), contractVersion: '1.0.0',
    references: {}, create: { parentId: null, ownerObjectId: main.value.conversation.objectId, contractKey: 'chat-bridge/image-png', name: 'Invalid image' }, content: { encoding: 'base64', value: bytes.toString('base64') } });
  const failed = await f.send('invalid-image', { text: '', images: [{ object: { objectId: image.object.id, revision: 1 }, contentHash: digest(bytes), byteLength: bytes.length, mediaType: 'image/png', label: 'Invalid original' }] });
  const next = await f.send('next-valid');
  for (let i = 0; i < 8; i++) await f.worker.step();
  const input = await f.first.store.read('chat-bridge/input', failed.object.objectId); assert.equal(input.value.state, 'failed'); assert.equal(input.value.error?.code, 'chat_image_invalid'); assert.equal(input.value.result, null);
  assert.equal((await f.first.queue.main()).value.queue[0]?.inputId, next.object.objectId);
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
});

test('competing Chat cancellations retain one original caller and reason with a definite refused losing action', async t => {
  const f = await queued(t), alice = { ...await f.request(f.tailId, 'alice-cancel'), reason: 'Original Alice reason.' },
    bob = { ...await f.request(f.tailId, 'bob-cancel', 'bob'), reason: 'Original Bob reason.' };
  const results = await Promise.all([f.firstCancel.cancel('alice', alice), f.secondCancel.cancel('bob', bob)]);
  assert.deepEqual(results.map(value => value.phase).sort(), ['failed', 'succeeded']);
  const winner = results.find(value => value.phase === 'succeeded')!, loser = results.find(value => value.phase === 'failed')!;
  if (winner.outcome?.action !== 'cancel' || winner.request.action !== 'cancel') throw Error('Expected original cancellation');
  const input = await f.first.store.read('chat-bridge/input', f.tailId);
  assert.equal(input.value.state, 'cancelled'); assert.equal(input.value.cancellation?.callerPrincipalId, winner.callerPrincipalId);
  assert.equal(input.value.cancellation?.reason, winner.request.reason); assert.equal(loser.error?.outcome, 'not_executed');
  assert.deepEqual(await f.firstCancel.cancel('alice', alice), results[0]); assert.deepEqual(await f.secondCancel.cancel('bob', bob), results[1]);
  assert.deepEqual((await f.first.queue.main()).value.queue.map(ticket => ticket.inputId), [f.inputId]);
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 0);
});

test('all32configured Chat channels remain queryable and cancellable within the unchanged Hive predicate limit', async t => {
  const f = await queued(t, false, 32), before = f.accesses.length;
  const input = await new ChatQueries(f.first.operations).input('alice', { expectedBridge: f.admission.expected('alice'), inputId: f.tailId });
  const calls = f.accesses.slice(before); assert.equal(calls.filter(method => method === 'objects.query').length, 4);
  assert.equal(calls.filter(method => method === 'objects.read').length, 1); assert.equal(input.object.objectId, f.tailId);
  const result = await f.firstCancel.cancel('alice', await f.request(f.tailId, 'last-channel-cancel')); assert.equal(result.phase, 'succeeded');
  assert.equal((await f.first.store.read('chat-bridge/input', f.tailId)).value.state, 'cancelled');
});
