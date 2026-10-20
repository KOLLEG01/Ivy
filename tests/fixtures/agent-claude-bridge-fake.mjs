// Hermetic SDK double: no SDK import, executable discovery, account or network.
import assert from 'node:assert/strict';
import { session } from '../../services/agent-manager/claude/bridge/session.mjs';
export function fakeSdk() {
  const calls = [];
  const sdk = { query({ prompt, options }) {
    const queued = [];
    let waiter, closed = false, failure;
    const entry = { prompt, options, closed: 0, interrupts: 0, steers: [],
      push(value) { if (waiter) { const next = waiter; waiter = null; next({ value, done: false }); } else queued.push(value); },
      finish(error) { closed = true; failure = error; if (waiter) { const next = waiter; waiter = null; next({ done: true }); } },
      permission(name, input, id = 'tool-use') { return options.canUseTool(name, input, { toolUseID: id, signal: options.abortController.signal }); },
    };
    calls.push(entry);
    const query = {
      [Symbol.asyncIterator]() { return this; },
      async next() {
        if (queued.length) return { value: queued.shift(), done: false };
        if (failure) throw failure;
        if (closed) return { done: true };
        const value = await new Promise(resolve => { waiter = resolve; });
        if (failure) throw failure;
        return value;
      },
      streamInput(messages) { entry.steers.push(messages); },
      async interrupt() { entry.interrupts++; entry.finish(); },
      close() { entry.closed++; entry.finish(); },
      async supportedModels() {
        if (options.cwd === 'discovery-hold') await new Promise((resolve, reject) => {
          if (options.abortController.signal.aborted) reject(new Error('aborted'));
          else options.abortController.signal.addEventListener('abort', () => reject(new Error('aborted')), { once: true });
        });
        return [{ value: 'sonnet', displayName: 'Sonnet', description: 'fixture' }];
      },
      async usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET(value) {
        assert.deepEqual(value, { skipBehaviors: true });
        return { rate_limits_available: true, rate_limits: { five_hour: { utilization: 0.3 } } };
      },
    };
    return query;
  } };
  return { sdk, calls };
}
export async function check() {
  const { sdk, calls } = fakeSdk();
  const output = [];
  const bridge = session(sdk, value => output.push(structuredClone(value)));
  let id = 0;
  const receive = (method, params = {}, version = 1) => bridge.receive({ version, id: ++id, method, params });
  const messages = [{ type: 'user', message: { role: 'user', content: 'hello' }, parent_tool_use_id: null, origin: { kind: 'human' } }];
  await receive('query/start', { turnId: 'turn-a', options: { cwd: '/fixture', model: 'sonnet' }, messages, permissionCallback: true });
  assert.deepEqual(await Array.fromAsync(calls[0].prompt), messages);
  assert.equal(calls[0].options.model, 'sonnet');
  const first = calls[0].permission('Bash', { command: 'fixture' });
  assert.equal(output.at(-1).type, 'permission');
  const callbackId = output.at(-1).callbackId;
  await receive('permission/reply', { callbackId, result: { behavior: 'allow', updatedInput: { command: 'changed' } } });
  assert.deepEqual(await first, { behavior: 'allow', updatedInput: { command: 'changed' } });
  await receive('permission/reply', { callbackId, result: { behavior: 'deny', message: 'late' } });
  await receive('query/steer', { turnId: 'turn-a', messages });
  assert.deepEqual(await Array.fromAsync(calls[0].steers[0]), messages);
  const interrupted = calls[0].permission('Edit', { file_path: 'a' }, 'edit');
  await receive('query/interrupt', { turnId: 'turn-a' });
  assert.deepEqual(await interrupted, { behavior: 'deny', message: 'turn already finished' });
  await new Promise(setImmediate);
  assert.equal(calls[0].interrupts, 1);
  assert.equal(calls[0].closed, 1);
  assert.equal(output.filter(v => v.type === 'end' && v.turnId === 'turn-a').length, 1);
  await receive('models', { options: { cwd: '/fixture', persistSession: false, tools: [], mcpServers: {} }, timeoutMs: 30000 });
  assert.deepEqual(await Array.fromAsync(calls[1].prompt), []);
  assert.equal(output.at(-1).result[0].value, 'sonnet');
  assert.equal(calls[1].closed, 1);
  await receive('usage', { options: { cwd: '/fixture' }, timeoutMs: 15000 });
  assert.equal(output.at(-1).result.rate_limits.five_hour.utilization, 0.3);
  assert.equal(calls[2].closed, 1);
  await receive('query/start', { turnId: 'turn-b', options: {}, messages: [] });
  calls[3].push({ type: 'assistant', message: { content: [{ type: 'text', text: '\ud800' }] } });
  await new Promise(setImmediate);
  assert.equal(output.at(-1).message.message.content[0].text, '\ud800');
  calls[3].finish(new TypeError('fixture failure'));
  await new Promise(setImmediate);
  assert.deepEqual(output.at(-1).error, { name: 'TypeError', message: 'fixture failure' });
  assert.equal(calls[3].closed, 1);
  await receive('query/start', { turnId: 'turn-c', options: {}, messages: [], permissionCallback: true });
  await receive('query/start', { turnId: 'turn-c', options: {}, messages: [] });
  assert.equal(output.at(-1).error.message, 'SDK query already exists');
  const pending = calls[4].permission('Bash', {});
  const discovery = receive('models', { options: { cwd: 'discovery-hold' }, timeoutMs: 30000 });
  await receive('anything', {}, 2);
  assert.equal(output.at(-1).error.message, 'SDK bridge version mismatch');
  await bridge.stop();
  await discovery;
  assert.deepEqual(await pending, { behavior: 'deny', message: 'turn already finished' });
  assert.equal(calls[4].closed, 1);
  assert.equal(calls[5].closed, 1);
  await bridge.stop();
  return { queries: calls.length, allClosedOnce: calls.every(c => c.closed === 1), messages: output };
}
if (process.argv[2] === '--check') process.stdout.write(JSON.stringify(await check()));
if (process.argv[2] === '--transport') {
  const { sdk, calls } = fakeSdk();
  const bridge = session(sdk, value => process.stdout.write(JSON.stringify(value) + '\n'));
  let text = '';
  for await (const chunk of process.stdin) {
    text += chunk;
    let end;
    while ((end = text.indexOf('\n')) >= 0) {
      const message = JSON.parse(text.slice(0, end)); text = text.slice(end + 1);
      const reply = result => process.stdout.write(JSON.stringify({ version: 1, id: message.id, result }) + '\n');
      const entry = () => calls.find(c => c.options.fixtureTag === message.params.turnId && c.closed === 0);
      if (message.method === 'fixture/emit') { calls.at(-1).push(message.params.message); continue; }
      if (message.method === 'fixture/permission') {
        void entry().permission('Bash', { command: 'fixture' }, 'test-tool').then(reply);
        continue;
      }
      if (message.method === 'fixture/burst') {
        for (let i = 0; i < message.params.count; i++) entry().push({ type: 'assistant', index: i });
        reply(null); continue;
      }
      if (message.method === 'fixture/inspect') {
        reply(calls.map(c => ({ tag: c.options.fixtureTag ?? null, closed: c.closed, interrupts: c.interrupts, steers: c.steers.length })));
        continue;
      }
      if (message.method === 'fixture/exit') process.exit(7);
      void bridge.receive(message);
    }
  }
  await bridge.stop();
}
