import test from 'node:test';
import assert from 'node:assert/strict';
import { eventFlow, session } from '../services/agent-manager/claude/bridge/session.mjs';
import { fakeSdk } from './fixtures/agent-claude-bridge-fake.mjs';

const tick = () => new Promise(setImmediate);
const message = (index = 0) => ({ version: 2, type: 'message', turnId: 'original', message: { index, text: '\ud800😀' } });

test('SDK event credits keep data ordered while controls and original RPC replies pass', async t => {
  const written = [];
  const flow = eventFlow(frame => written.push(structuredClone(frame)));
  t.after(() => flow.stop());
  const data = Array.from({ length: 16 }, (_, index) => flow.send(message(index)));
  const controls = Array.from({ length: 9 }, (_, callbackId) => flow.send({ version: 2, type: 'permission', callbackId }));
  await flow.send({ version: 2, id: 'original-request', result: '\ud800😀' });
  await tick();
  assert.equal(written.filter(frame => frame.type === 'message').length, 1);
  assert.equal(written.filter(frame => frame.type === 'permission').length, 8);
  assert.deepEqual(written.find(frame => frame.id), { version: 2, id: 'original-request', result: '\ud800😀' });
  flow.acknowledge(written.find(frame => frame.type === 'permission').eventSequence);
  await tick();
  assert.equal(written.filter(frame => frame.type === 'permission').length, 9);
  for (let index = 0; index < 16; index++) {
    const frames = written.filter(frame => frame.type === 'message');
    assert.equal(frames.length, index + 1);
    assert.deepEqual(frames[index].message, { index, text: '\ud800😀' });
    flow.acknowledge(frames[index].eventSequence);
    await tick();
  }
  await Promise.all(data);
  assert.throws(() => flow.acknowledge(0), /Invalid SDK event credit/);
  assert.throws(() => flow.acknowledge(written.find(frame => frame.type === 'message').eventSequence), /Invalid SDK event credit/);
  flow.stop();
  await Promise.all(controls);
});

test('SDK flow shutdown settles emitted and waiting events without emitting queued payloads', async () => {
  const written = [];
  const flow = eventFlow(frame => written.push(frame));
  const pending = Array.from({ length: 16 }, (_, index) => flow.send(message(index)));
  await tick();
  assert.equal(written.length, 1);
  flow.stop();
  await Promise.all(pending);
  await flow.send(message(100));
  assert.equal(written.length, 1);
});

test('SDK query admission holds its original slot through end consumption and keeps discovery independent', async t => {
  const { sdk, calls } = fakeSdk();
  const written = [], failures = [];
  const flow = eventFlow(frame => written.push(structuredClone(frame)));
  const bridge = session(sdk, flow.send, error => failures.push(error));
  t.after(async () => { await bridge.stop(); flow.stop(); });
  let next = 0;
  const receive = async (method, params) => {
    const id = String(++next);
    await bridge.receive({ version: 2, id, method, params });
    return written.find(frame => frame.id === id);
  };
  for (let index = 0; index < 16; index++) {
    assert.equal((await receive('query/start', { turnId: String(index), options: {}, messages: [] })).result, null);
  }
  assert.equal(calls.length, 16);
  assert.equal((await receive('query/start', { turnId: 'extra', options: {}, messages: [] })).error.name, 'native_capacity');
  await receive('query/close', { turnId: '0' });
  await tick();
  assert.equal((await receive('query/start', { turnId: 'extra', options: {}, messages: [] })).error.name, 'native_capacity');
  const end = written.find(frame => frame.type === 'end' && frame.turnId === '0');
  assert.ok(end);
  flow.acknowledge(end.eventSequence);
  await tick();
  assert.equal((await receive('query/start', { turnId: 'extra', options: {}, messages: [] })).result, null);
  assert.equal(calls.length, 17);
  assert.equal((await receive('models', { options: {}, timeoutMs: 30000 })).result[0].value, 'sonnet');
  assert.equal(calls.length, 18);
  assert.equal(calls.at(-1).closed, 1);
  const discoveries = Array.from({ length: 8 }, () => receive('models', { options: { cwd: 'discovery-hold' }, timeoutMs: 30000 }));
  assert.equal(calls.length, 26);
  assert.equal((await receive('models', { options: {}, timeoutMs: 30000 })).error.name, 'native_capacity');
  assert.equal(calls.length, 26);
  await bridge.stop();
  for (const reply of await Promise.all(discoveries)) assert.equal(reply.error.message, 'SDK discovery cancelled');
  assert.ok(calls.every(call => call.closed === 1));
  assert.deepEqual(failures, []);
});

test('SDK callback admission refuses new work before publication and stop settles every original callback', async () => {
  const { sdk, calls } = fakeSdk();
  const written = [], failures = [];
  const flow = eventFlow(frame => written.push(frame));
  const bridge = session(sdk, flow.send, error => failures.push(error));
  await bridge.receive({ version: 2, id: 'start', method: 'query/start', params: { turnId: 'original', options: {}, messages: [], permissionCallback: true } });
  const callbacks = Array.from({ length: 32 }, (_, index) => calls[0].permission('Bash', { command: String(index) }, String(index)));
  await assert.rejects(calls[0].permission('Bash', {}, 'refused'), { name: 'native_capacity' });
  await tick();
  assert.equal(written.filter(frame => frame.type === 'permission').length, 8);
  await bridge.stop();
  flow.stop();
  assert.deepEqual(await Promise.all(callbacks), Array.from({ length: 32 }, () => ({ behavior: 'deny', message: 'turn already finished' })));
  assert.equal(calls[0].closed, 1);
  assert.deepEqual(failures, []);
});
