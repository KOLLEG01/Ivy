import test from 'node:test';
import assert from 'node:assert/strict';
import { hashJson, IvyError } from '../packages/sdk/src/node.js';
import type { Wire } from '../packages/sdk/src/node.js';
import { DiagnosticNotifications } from '../services/chat-bridge/src/diagnostic-notifications.js';
import { ChatNotices } from '../services/chat-bridge/src/notices.js';
import { chatNativeFixture } from './fixtures/chat-native.js';

async function fixture(t: Parameters<typeof chatNativeFixture>[0]) {
  const f = await chatNativeFixture(t, '0.154.0');
  await f.first.create('alice', f.request(f.operation('diagnostic-main')));
  let now = Date.now();
  const at = new Date(now).toISOString();
  const diagnostic: Wire.Diagnostic = { code: 'tool_definition_changed', source: 'service:fixture.reader', resource: null,
    severity: 'warning', status: 'current', firstObservedAt: at, lastObservedAt: at, message: 'Private request contents must not appear in notices.' };
  const signal = new AbortController().signal;
  const monitor = () => new DiagnosticNotifications(f.first, 'de', () => now);
  const queue = async () => { await new ChatNotices(f.first).recoverPending(); return (await f.first.queue.main()).value.queue; };
  return { ...f, diagnostic, signal, monitor, queue, advance: (ms = 60_000) => { now += ms; } };
}

test('persistent diagnostics create one durable Main notice, recover once and resume a lost receipt across restart', async t => {
  const f = await fixture(t);
  f.kernel.registry.diagnostic(f.diagnostic);
  await f.monitor().tick(f.signal); assert.equal((await f.queue()).length, 0);
  f.advance(); await f.monitor().tick(f.signal);
  let queue = await f.queue(); assert.equal(queue.length, 1);
  const input = await f.first.store.read('chat-bridge/input', queue[0]!.inputId);
  assert.match(input.value.payload.text, /Anhaltender Fehler.*tool_definition_changed/u);
  assert.ok(!input.value.payload.text.includes(f.diagnostic.message));
  const scope = hashJson([f.admission.definition.principalId, f.admission.definition.workspaceId]);
  const state = f.first.store.diagnosticNoticeState(scope), entry = Object.values(state)[0]!;
  const originalId = entry.alert!.request.operationId;
  // Crash after ChatBridge admission but before persisting its successful acknowledgement.
  entry.alert!.queued = false; f.first.store.saveDiagnosticNoticeState(scope, state);
  await new DiagnosticNotifications(f.second, 'de').tick(f.signal);
  assert.equal((await f.queue()).length, 1);
  assert.equal(Object.values(f.second.store.diagnosticNoticeState(scope))[0]!.alert!.request.operationId, originalId);
  f.kernel.registry.diagnostic({ ...f.diagnostic, status: 'resolved' });
  await f.monitor().tick(f.signal);
  queue = await f.queue(); assert.equal(queue.length, 2);
  assert.match((await f.first.store.read('chat-bridge/input', queue[1]!.inputId)).value.payload.text, /Erholung festgestellt/u);
  await new DiagnosticNotifications(f.second, 'de').tick(f.signal);
  assert.equal((await f.queue()).length, 2);
});

test('brief failures and the notice monitor\'s own delivery failure do not create recursive alerts', async t => {
  const f = await fixture(t);
  f.kernel.registry.diagnostic(f.diagnostic); await f.monitor().tick(f.signal);
  f.kernel.registry.diagnostic({ ...f.diagnostic, status: 'resolved' });
  f.advance(30_000); await f.monitor().tick(f.signal);
  f.kernel.registry.diagnostic({ ...f.diagnostic, code: 'chat_diagnostics_unavailable', severity: 'error' });
  f.advance(); await f.monitor().tick(f.signal);
  assert.equal((await f.queue()).length, 0);
});

test('an incomplete diagnostic snapshot never emits a false recovery', async t => {
  const f = await fixture(t);
  f.kernel.registry.diagnostic(f.diagnostic); await f.monitor().tick(f.signal);
  f.advance(); await f.monitor().tick(f.signal); await f.queue();
  const original = f.first.store.client.request.bind(f.first.store.client);
  f.first.store.client.request = async (method, params, options) => {
    if (method === 'system.diagnostics') {
      if ('cursor' in params) throw new IvyError('service_unavailable', 'Fixture second page failed.');
      return { items: [], nextCursor: 'fixture-next-page' } as never;
    }
    return original(method, params, options);
  };
  await assert.rejects(f.monitor().tick(f.signal), { code: 'service_unavailable' });
  f.first.store.client.request = original;
  assert.equal((await f.queue()).length, 1);
});
