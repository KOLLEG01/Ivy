import test from 'node:test';
import assert from 'node:assert/strict';
import { hashJson, IvyError } from '../packages/sdk/src/node.js';
import type { Wire } from '../packages/sdk/src/node.js';
import { DiagnosticNotifications, diagnosticNotificationDelayMs } from '../services/chat-bridge/src/diagnostic-notifications.js';
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
  return { ...f, diagnostic, signal, monitor, queue, now: () => now,
    setNow: (at: string) => { now = Date.parse(at); }, advance: (ms = diagnosticNotificationDelayMs) => { now += ms; } };
}

test('operational notices follow live Hive quiet hours without Secretary and discard resolved overnight incidents', async t => {
  const f = await fixture(t);
  const settings = { schemaVersion: 1 as const, quietHours: { timeZone: 'Europe/Berlin', start: '23:00', end: '07:00' } };
  const saved = await f.first.store.client.request('settings.save', { mutationId: f.operation('quiet-hours'), expectedRevision: 0, settings });
  const tick = () => f.monitor().tick(f.signal);
  f.setNow('2026-10-13T21:00:00.000Z'); // 23:00 in Berlin: the configured window just closed.
  f.kernel.registry.diagnostic(f.diagnostic); await tick(); f.advance(); await tick();
  assert.equal((await f.queue()).length, 0);
  f.kernel.registry.diagnostic({ ...f.diagnostic, status: 'resolved' }); await tick();
  f.setNow('2026-10-14T04:50:00.000Z'); // A new failure remains present into the morning.
  f.kernel.registry.diagnostic(f.diagnostic); await tick(); f.advance(); await tick();
  assert.equal((await f.queue()).length, 0);
  settings.quietHours.end = '08:00'; // A live policy change must take effect without restart.
  await f.first.store.client.request('settings.save', { mutationId: f.operation('quiet-hours-changed'), expectedRevision: saved.revision, settings });
  f.setNow('2026-10-14T05:00:00.000Z'); await tick();
  assert.equal((await f.queue()).length, 0);
  f.setNow('2026-10-14T06:00:00.000Z'); await tick();
  assert.equal((await f.queue()).length, 1);
});

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
  assert.equal((await f.queue()).length, 1);
  f.advance(); await f.monitor().tick(f.signal);
  queue = await f.queue(); assert.equal(queue.length, 2);
  assert.match((await f.first.store.read('chat-bridge/input', queue[1]!.inputId)).value.payload.text, /Erholung festgestellt/u);
  await new DiagnosticNotifications(f.second, 'de').tick(f.signal);
  assert.equal((await f.queue()).length, 2);
});

test('Hive storage warnings notify once without claiming that work is blocked', async t => {
  const f = await fixture(t);
  f.kernel.registry.diagnostic({ ...f.diagnostic, code: 'storage_pressure', source: 'hive' });
  await f.monitor().tick(f.signal); f.advance(); await f.monitor().tick(f.signal);
  const queue = await f.queue(); assert.equal(queue.length, 1);
  const input = await f.first.store.read('chat-bridge/input', queue[0]!.inputId);
  assert.match(input.value.payload.text, /Speicherwarnung.*Hive arbeitet weiter/u);
  assert.doesNotMatch(input.value.payload.text, /Anhaltender Fehler|blockiert|veraltet/u);
  f.advance(); await f.monitor().tick(f.signal);
  assert.equal((await f.queue()).length, 1);
});

test('resource failures share one notice and recover only after the whole group is healthy', async t => {
  const f = await fixture(t);
  const members = Array.from({ length: 4 }, (_, index) => ({ ...f.diagnostic, resource: { objectId: 'signals:run-' + index } }));
  for (const member of members) f.kernel.registry.diagnostic(member);
  await f.monitor().tick(f.signal); f.advance(); await f.monitor().tick(f.signal);
  assert.equal((await f.queue()).length, 1);
  for (const member of members.slice(0, 3)) f.kernel.registry.diagnostic({ ...member, status: 'resolved' });
  await f.monitor().tick(f.signal); f.advance(); await f.monitor().tick(f.signal);
  assert.equal((await f.queue()).length, 1, 'Partial recovery must not announce that the service is healthy.');
  const next = { ...f.diagnostic, resource: { objectId: 'signals:replacement-run' } };
  f.kernel.registry.diagnostic(next);
  f.kernel.registry.diagnostic({ ...members[3]!, status: 'resolved' });
  await new DiagnosticNotifications(f.second, 'de', f.now).tick(f.signal);
  f.advance(); await f.monitor().tick(f.signal);
  assert.equal((await f.queue()).length, 1, 'Changing member identities must preserve the ongoing incident.');
  f.kernel.registry.diagnostic({ ...next, status: 'resolved' });
  await f.monitor().tick(f.signal); f.advance(30_000); await f.monitor().tick(f.signal);
  assert.equal((await f.queue()).length, 1);
  f.advance(); await f.monitor().tick(f.signal);
  const queue = await f.queue(); assert.equal(queue.length, 2);
  assert.match((await f.first.store.read('chat-bridge/input', queue[1]!.inputId)).value.payload.text, /Erholung festgestellt/u);
});

test('different callers, dependent services and error codes retain separate notices', async t => {
  const f = await fixture(t), resource = { callerPrincipalId: 'fixture.reader', serviceNodeId: 'owner.one', objectId: 'private-one' };
  const diagnostics = [
    { ...f.diagnostic, resource },
    { ...f.diagnostic, resource: { ...resource, objectId: 'private-two' } },
    { ...f.diagnostic, resource: { ...resource, serviceNodeId: 'owner.two' } },
    { ...f.diagnostic, resource: { ...resource, callerPrincipalId: 'another.reader' } },
    { ...f.diagnostic, code: 'service_unavailable', resource },
  ];
  for (const diagnostic of diagnostics) f.kernel.registry.diagnostic(diagnostic);
  await f.monitor().tick(f.signal); f.advance(); await f.monitor().tick(f.signal);
  const queue = await f.queue(); assert.equal(queue.length, 4);
  const texts = await Promise.all(queue.map(async item => (await f.first.store.read('chat-bridge/input', item.inputId)).value.payload.text));
  assert.equal(new Set(texts).size, 4);
  assert.ok(texts.every(text => !text.includes('private-')));
});

test('grouping saved resource incidents keeps every original pending receipt without repeating their messages', async t => {
  const f = await fixture(t), scope = hashJson([f.admission.definition.principalId, f.admission.definition.workspaceId]);
  f.kernel.registry.diagnostic(f.diagnostic); await f.monitor().tick(f.signal);
  f.advance(); await f.monitor().tick(f.signal);
  const entry = Object.values(f.first.store.diagnosticNoticeState(scope))[0]!;
  const secondRequest = { ...entry.alert!.request, operationId: f.operation('second-original-alert') };
  await new ChatNotices(f.first).notify(f.admission.definition.principalId, secondRequest);
  assert.equal((await f.queue()).length, 2);
  // Both original notices were admitted before their acknowledgements were saved.
  f.first.store.saveDiagnosticNoticeState(scope, {
    'resource-one': { ...entry, alert: { ...entry.alert!, queued: false } },
    'resource-two': { ...entry, alert: { request: secondRequest, queued: false, confirmed: false } },
  });
  await new DiagnosticNotifications(f.second, 'de', f.now).tick(f.signal);
  assert.equal((await f.queue()).length, 2);
  const saved = Object.values(f.second.store.diagnosticNoticeState(scope));
  assert.equal(saved.filter(value => !value.receiptOnly).length, 1);
  assert.deepEqual(new Set(saved.flatMap(value => value.alert ? [value.alert.request.operationId] : [])),
    new Set([entry.alert!.request.operationId, secondRequest.operationId]));
  f.kernel.registry.diagnostic({ ...f.diagnostic, status: 'resolved' });
  await f.monitor().tick(f.signal); f.advance(); await f.monitor().tick(f.signal);
  assert.equal((await f.queue()).length, 3, 'The formerly separate incidents need only one recovery notice.');
});

test('a brief healthy interval across restart keeps the original incident until recovery is stable', async t => {
  const f = await fixture(t);
  f.kernel.registry.diagnostic(f.diagnostic); await f.monitor().tick(f.signal);
  f.advance(); await f.monitor().tick(f.signal);
  assert.equal((await f.queue()).length, 1);
  const scope = hashJson([f.admission.definition.principalId, f.admission.definition.workspaceId]);
  const originalId = Object.values(f.first.store.diagnosticNoticeState(scope))[0]!.alert!.request.operationId;
  f.kernel.registry.diagnostic({ ...f.diagnostic, status: 'resolved' });
  await f.monitor().tick(f.signal); f.advance(30_000);
  await new DiagnosticNotifications(f.second, 'de', f.now).tick(f.signal);
  f.kernel.registry.diagnostic(f.diagnostic); await f.monitor().tick(f.signal);
  f.advance(); await f.monitor().tick(f.signal);
  assert.equal((await f.queue()).length, 1);
  assert.equal(Object.values(f.first.store.diagnosticNoticeState(scope))[0]!.alert!.request.operationId, originalId);
  f.kernel.registry.diagnostic({ ...f.diagnostic, status: 'resolved' });
  await f.monitor().tick(f.signal); f.advance(); await f.monitor().tick(f.signal);
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

test('repeated outages shorter than five minutes remain quiet, but a continuous failure is reported', async t => {
  const f = await fixture(t);
  for (let incident = 0; incident < 3; incident++) {
    f.kernel.registry.diagnostic(f.diagnostic); await f.monitor().tick(f.signal);
    f.advance(150_000); await f.monitor().tick(f.signal);
    assert.equal((await f.queue()).length, 0);
    f.kernel.registry.diagnostic({ ...f.diagnostic, status: 'resolved' });
    await f.monitor().tick(f.signal); f.advance(30_000);
  }
  f.kernel.registry.diagnostic(f.diagnostic); await f.monitor().tick(f.signal);
  f.advance(); await f.monitor().tick(f.signal);
  assert.equal((await f.queue()).length, 1);
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

test('confirmed diagnostic notices form a bounded ring while active and unconfirmed incidents survive', async t => {
  const f = await fixture(t);
  f.kernel.registry.diagnostic(f.diagnostic); await f.monitor().tick(f.signal);
  f.advance(); await f.monitor().tick(f.signal);
  const scope = hashJson([f.admission.definition.principalId, f.admission.definition.workspaceId]);
  const entry = Object.values(f.first.store.diagnosticNoticeState(scope))[0]!;
  const completed = { ...entry, active: false, resolvedAt: f.now(),
    alert: { ...entry.alert!, confirmed: true }, recovery: { ...entry.alert!, confirmed: true } };
  const state = {
    active: { ...completed, active: true, resolvedAt: null },
    unconfirmed: { ...completed, alert: { ...completed.alert, confirmed: false } },
    oldest: { ...completed, resolvedAt: f.now() - 1 },
    newest: completed,
  };
  f.first.store.diagnosticHistoryLimits.count = 3;
  f.first.store.saveDiagnosticNoticeState(scope, state);
  assert.deepEqual(Object.keys(f.second.store.diagnosticNoticeState(scope)), ['active', 'newest', 'unconfirmed']);
  f.first.store.diagnosticHistoryLimits.bytes = 1;
  f.first.store.saveDiagnosticNoticeState(scope, state);
  assert.deepEqual(Object.keys(f.second.store.diagnosticNoticeState(scope)), ['active', 'unconfirmed']);
});
