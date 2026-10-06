import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { digest, hashJson } from '../packages/contracts/src/canonical.js';
import { deriveOperationId } from '../packages/sdk/src/client.js';
import { resolveChatPlan } from '../services/chat-bridge/src/native-plan.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Chat, Wire } from '../packages/contracts/src/generated.js';
import { ChatDelivery } from '../services/chat-bridge/src/delivery.js';
import { chatNativeFixture as fixture } from './fixtures/chat-native.js';

const hasCode = (code: string) => (error: unknown) => error instanceof IvyError && error.code === code;
async function queued(t: Parameters<typeof fixture>[0], version: Chat.NativePlan['nativeVersion'] = '0.154.0', payload: Chat.Payload = { text: 'Original user input', images: [] }) {
  const f = await fixture(t, version), main = await f.first.create('alice', f.request('main')); if (main.outcome?.action !== 'createMain') throw Error('Expected Main.');
  const request: Chat.SendRequest = { action: 'send', operationId: f.operation('message'), expectedBridge: f.admission.expected('alice'), channel: f.channel,
    messageId: 'message', expectedBinding: main.outcome.binding.object, payload };
  const input = await f.first.queue.send('alice', request); if (input.outcome?.action !== 'send') throw Error('Expected queued input.');
  const secondInput = await f.second.queue.send('alice', { ...request, operationId: f.operation('later'), messageId: 'later', payload: { text: 'Later queued input', images: [] } });
  if (secondInput.outcome?.action !== 'send') throw Error('Expected queue tail.');
  return { ...f, worker: new ChatDelivery(f.first), secondWorker: new ChatDelivery(f.second), inputId: input.outcome.input.object.objectId,
    tailId: secondInput.outcome.input.object.objectId, primary: main.outcome.binding.data.primary.nativeId, request,
    current: () => f.first.store.read('chat-bridge/input', input.outcome!.action === 'send' ? input.outcome!.input.object.objectId : '') };
}
const calls = (f: { frames: Record<string, Wire.Json>[] }, method: string) => f.frames.filter(frame => frame['method'] === method);

for (const version of ['0.154.0'] as const) test('Chat ' + version + ' queue head delivers original native input once across concurrent workers and retains the later message', async t => {
  const f = await queued(t, version);
  for (let i = 0; i < 4; i++) await Promise.all([f.worker.step(), f.secondWorker.step()]);
  const input = await f.current(); assert.equal(input.value.state, 'running'); assert.ok(input.value.turnId); assert.ok(input.value.epoch);
  assert.equal(calls(f, 'thread/start').length, 1); assert.equal(calls(f, 'thread/resume').length, 0); assert.equal(calls(f, 'turn/start').length, 1);
  const turn = calls(f, 'turn/start')[0]!.params as Record<string, Wire.Json>;
  assert.equal(turn['threadId'], f.primary); assert.deepEqual(turn['input'], [{ type: 'text', text: 'Original user input' }]); assert.ok(turn['clientUserMessageId']);
  await f.secondWorker.step(); assert.equal(calls(f, 'turn/start').length, 1);
  assert.equal((await f.first.store.read('chat-bridge/input', f.tailId)).value.state, 'queued');
  assert.deepEqual((await f.first.find())!.value.queue.map(ticket => ticket.inputId), [f.inputId, f.tailId]);
  assert.equal((await f.second.queue.send('alice', f.request)).phase, 'succeeded');
});

test('fresh loaded Chat Main delivers without resuming a nonexistent rollout', async t => {
  const f = await queued(t);
  for(const main of [f.first,f.second])main.native.evidence.saveRead=async()=>{throw Error('Transient availability must not be persisted');};
  f.reply(frame => frame['method'] === 'thread/resume' ? { error: { code: -32600, message: 'no rollout found for thread id ' + f.primary } } : undefined);
  await f.worker.step(); const prepared = await f.current();
  assert.ok(prepared.value.nativeCalls.turn); assert.equal(prepared.value.nativeCalls.resume, null);
  await f.secondWorker.step(); const running = await f.current();
  assert.equal(running.value.state, 'running'); assert.equal(running.value.nativeCalls.turn?.objectId, prepared.value.nativeCalls.turn.objectId);
  assert.equal(calls(f, 'thread/resume').length, 0); assert.equal(calls(f, 'turn/start').length, 1);
});

test('fresh Chat Main unloaded before dispatch resumes the same primary without replacing its prepared turn', async t => {
  const f = await queued(t); await f.worker.step(); const prepared = await f.current();
  assert.ok(prepared.value.nativeCalls.turn); assert.equal(prepared.value.nativeCalls.resume, null);
  f.restart(); let unloaded = true;
  f.reply(frame => {
    if (frame['method'] === 'thread/resume') unloaded = false;
    return unloaded && frame['method'] === 'thread/read' ? {error:{code:-32600,message:'thread not loaded: '+f.primary}} : undefined;
  });
  await f.secondWorker.step();
  const attaching = await f.current(); assert.ok(attaching.value.nativeCalls.resume);
  assert.deepEqual(attaching.value.nativeCalls.turn, prepared.value.nativeCalls.turn); assert.equal(calls(f, 'turn/start').length, 0);
  for (let i = 0; i < 3; i++) await f.worker.step();
  const running = await f.current(); assert.equal(running.value.state, 'running');
  assert.equal(running.value.nativeCalls.turn?.objectId, prepared.value.nativeCalls.turn.objectId);
  assert.equal(calls(f, 'thread/start').length, 1); assert.equal(calls(f, 'thread/resume').length, 1); assert.equal(calls(f, 'turn/start').length, 1);
});

test('manual native activity blocks Chat before preparation and immediately before turn dispatch', async t => {
  const f = await queued(t); f.threadStatus({ type: 'active', activeFlags: [] });
  await f.worker.step(); assert.equal(calls(f, 'thread/resume').length, 0); assert.equal((await f.current()).value.state, 'queued');
  f.threadStatus({ type: 'idle' }); await f.worker.step();
  const prepared = await f.current(); assert.ok(prepared.value.nativeCalls.turn); assert.equal(calls(f, 'turn/start').length, 0);
  f.threadStatus({ type: 'active', activeFlags: [] }); await f.secondWorker.step(); assert.equal(calls(f, 'turn/start').length, 0);
  f.threadStatus({ type: 'idle' }); await f.secondWorker.step(); assert.equal((await f.current()).value.state, 'running'); assert.equal(calls(f, 'turn/start').length, 1);
});

test('metadata resume preparation recovers an already saved full-history request without changing its identity', async t => {
  const f = await queued(t), input = await f.current(), origin = { predecessor: null, preparedEpoch: null };
  const plan = await resolveChatPlan(f.first.store, f.admission.definition.nativePlan);
  const operationId = deriveOperationId(input.value.operationId, 'chat:native:resume:' + hashJson(origin));
  const request = { nativeVersion: plan.nativeVersion, catalogSourceHash: plan.catalogSourceHash, method: 'thread/resume',
    params: { ...plan.threadResume, threadId: f.primary } } as Chat.NativeRequest;
  const retained = await f.first.native.prepare(f.inputId, operationId, request, origin);
  f.threadStatus({ type: 'notLoaded' }); await f.worker.step();
  const attached = await f.current(); assert.deepEqual(attached.value.nativeCalls.resume, retained.pin);
  for (let index = 0; index < 3; index++) await f.worker.step();
  const saved = await f.first.native.get(f.inputId, retained.pin);
  assert.equal(saved.value.requestHash, retained.value.requestHash);
  assert.deepEqual((calls(f, 'thread/resume')[0]!['params']), request.params);
});

test('queued Chat input resumes an unloaded Main only on its exact authenticated native error', async t => {
  const f = await queued(t);
  f.reply(frame => frame['method'] === 'thread/read' ? {error:{code:-32600,message:'thread not loaded: unrelated'}} : undefined);
  await assert.rejects(f.worker.step(), hasCode('chat_native_unavailable'));
  assert.equal(calls(f, 'thread/resume').length, 0);
  let unloaded = true;
  f.reply(frame => {
    if (frame['method'] === 'thread/resume') unloaded = false;
    return unloaded && frame['method'] === 'thread/read' ? {error:{code:-32600,message:'thread not loaded: '+f.primary}} : undefined;
  });
  for (let i = 0; i < 4; i++) await f.worker.step();
  assert.equal((await f.current()).value.state, 'running');
  assert.equal(calls(f, 'thread/resume').length, 1);
  assert.equal((calls(f, 'thread/resume')[0]!['params'] as Record<string, unknown>)['excludeTurns'], true);
  assert.equal(calls(f, 'turn/start').length, 1);
});

test('native epoch changes reattach the same Chat Main and retain the prepared original turn identity', async t => {
  const f = await queued(t); f.threadStatus({ type: 'notLoaded' });
  await f.worker.step(); await f.worker.step(); await f.worker.step();
  const prepared = await f.current(), originalTurn = prepared.value.nativeCalls.turn!, originalResume = prepared.value.nativeCalls.resume!;
  f.restart(); f.threadStatus({ type: 'notLoaded' }); await f.secondWorker.step();
  const reattaching = await f.current(); assert.notEqual(reattaching.value.nativeCalls.resume!.objectId, originalResume.objectId); assert.deepEqual(reattaching.value.nativeCalls.turn, originalTurn);
  // Another restart after preparing reattachment must reconcile its unchanged original resume ID.
  f.restart(); f.threadStatus({ type: 'notLoaded' }); await f.worker.step(); await f.secondWorker.step();
  const running = await f.current(); assert.equal(running.value.state, 'running'); assert.equal(running.value.nativeCalls.turn!.objectId, originalTurn.objectId);
  assert.equal(calls(f, 'thread/start').length, 1); assert.equal(calls(f, 'thread/resume').length, 2); assert.equal(calls(f, 'turn/start').length, 1);
  const resumed = await f.first.native.get(f.inputId, running.value.nativeCalls.resume!);
  assert.deepEqual(resumed.value.predecessor, originalResume); assert.ok(resumed.value.preparedEpoch); assert.notEqual(resumed.value.preparedEpoch, resumed.value.epoch);
});

test('lost Chat input publication after successful native turn delivery recovers the original turn without replay', async t => {
  const f = await queued(t); await f.worker.step(); let lost = false;
  f.intercept((params, result) => { if (!lost && result.object.id === f.inputId && params.content.encoding === 'json' && (params.content.value as Record<string, unknown>)['state'] === 'running') {
    lost = true; throw new IvyError('outcome_unknown', 'Lost committed running input response.', 'unknown');
  } });
  await assert.rejects(f.worker.step(), hasCode('outcome_unknown')); assert.equal(lost, true); f.intercept(null);
  const original = await f.current(); await f.secondWorker.step(); assert.deepEqual((await f.current()).pin, original.pin); assert.ok(original.value.turnId);
  assert.equal(calls(f, 'turn/start').length, 1);
});

test('a Main unloaded again in the same epoch gets a new resume without replacing its prepared turn', async t => {
  const f = await queued(t); f.threadStatus({ type: 'notLoaded' });
  await f.worker.step(); await f.worker.step(); await f.worker.step();
  const prepared = await f.current(), originalResume = prepared.value.nativeCalls.resume!, originalTurn = prepared.value.nativeCalls.turn!;
  assert.ok(originalResume); assert.ok(originalTurn);
  f.threadStatus({ type: 'notLoaded' });
  for (let i = 0; i < 4; i++) await f.secondWorker.step();
  const running = await f.current();
  assert.equal(running.value.state, 'running');
  assert.equal(running.value.nativeCalls.turn!.objectId, originalTurn.objectId);
  assert.notEqual(running.value.nativeCalls.resume!.objectId, originalResume.objectId);
  const resumed = await f.first.native.get(f.inputId, running.value.nativeCalls.resume!);
  assert.deepEqual(resumed.value.predecessor, originalResume);
  assert.equal(resumed.value.preparedEpoch, resumed.value.epoch);
  assert.equal(calls(f, 'thread/resume').length, 2);
  assert.equal(calls(f, 'turn/start').length, 1);
});

test('dispatched and unknown original Chat turns block the queue through restart and contradictory absence', async t => {
  const f = await queued(t); await f.worker.step(); f.hold('turn/start');
  await f.worker.step(); assert.equal((await f.current()).value.state, 'delivering'); assert.equal(calls(f, 'turn/start').length, 1);
  f.absence('owner'); await assert.rejects(f.secondWorker.step(), hasCode('chat_native_absence_conflict')); f.absence(null);
  f.restart(); await f.secondWorker.step(); const unknown = await f.current();
  assert.equal(unknown.value.state, 'outcome_unknown'); assert.equal(unknown.value.error?.outcome, 'unknown');
  await f.worker.step(); assert.deepEqual((await f.current()).pin, unknown.pin); assert.equal(calls(f, 'turn/start').length, 1); assert.equal(calls(f, 'thread/resume').length, 0);
  assert.equal((await f.first.store.read('chat-bridge/input', f.tailId)).value.state, 'queued');
});

test('invalid original Chat image becomes an explicit input failure before any native turn', async t => {
  const image = Buffer.from('This is not a PNG image.');
  const g = await fixture(t), created = await g.first.create('alice', g.request('image-main')); if (created.outcome?.action !== 'createMain') throw Error('Expected Main.');
  const main = await g.first.queue.main();
  const saved = await g.first.store.client.request('objects.write', { mutationId: randomUUID(), contractVersion: '1.0.0',
    references: {}, create: { parentId: null, ownerObjectId: main.value.conversation.objectId, contractKey: 'chat-bridge/image-png', name: 'Original invalid image' }, content: { encoding: 'base64', value: image.toString('base64') } });
  const result = await g.first.queue.send('alice', { action: 'send', operationId: g.operation('image'), messageId: 'image', expectedBridge: g.admission.expected('alice'),
    channel: g.channel, expectedBinding: created.outcome.binding.object, payload: { text: '', images: [{ object: { objectId: saved.object.id, revision: saved.revision.revision },
      contentHash: digest(image), byteLength: image.length, mediaType: 'image/png', label: 'Invalid original PNG' }] } });
  if (result.outcome?.action !== 'send') throw Error('Expected image admission.');
  const worker = new ChatDelivery(g.first); await worker.step(); await worker.step(); await worker.step();
  const failed = await g.first.store.read('chat-bridge/input', result.outcome.input.object.objectId);
  assert.equal(failed.value.state, 'failed'); assert.equal(failed.value.error?.code, 'chat_image_invalid'); assert.equal(failed.value.error?.outcome, 'not_executed');
  assert.equal(calls(g, 'turn/start').length, 0); assert.ok(failed.value.finishedAt);
});
