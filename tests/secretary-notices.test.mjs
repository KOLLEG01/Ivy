import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { IvyError, discover, callBound, newOperationId, hashJson } from '../dist/packages/sdk/src/node.js';
import { testChatFixture } from '../tools/testing/test-chat-fixture.mjs';
import { artifactRoot, startNativeService } from '../tools/testing/native-service.mjs';
import { serviceProtocolProxy } from './fixtures/service-protocol-proxy.mjs';
import { principal, rootObject, settingsFor, message, assessment } from './secretary-fixture.mjs';

// Exact clock, Voice, retry/refusal, CAS and archived-delivery boundaries live in
// secretary-rust/{notices,assignment_delivery,browser_notice,notice_evidence,
// notice_scan}_cases.rs. These integration cases retain the real cross-service
// admission, result, restart and acknowledgment assertions.
async function secretary(t, f) {
  const root = await rootObject(f.client), serviceNodeId = 'native-secretary-' + randomUUID();
  const settings = settingsFor(root, serviceNodeId), calls = [], queries = [];
  let lose = false, lost = false, owner;
  const proxy = await serviceProtocolProxy(f.base, {
    onRequest(frame) {
      if (frame.method === 'tools.call' && frame.params.qualifiedName === 'chat.notify') calls.push(structuredClone(frame.params));
      if (frame.method === 'tools.call' && ['chat.operation', 'chat.notice'].includes(frame.params.qualifiedName)) queries.push(structuredClone(frame.params));
    },
    onResponse(frame, request) {
      if (!lose || frame.error || request.method !== 'tools.call' || request.params.qualifiedName !== 'chat.notify') return;
      lose = false; lost = true;
      return { jsonrpc: '2.0', id: frame.id, error: new IvyError('outcome_unknown', 'Injected lost committed notify response.', 'unknown').toWire() };
    },
  });
  const config = { schemaVersion: 1, componentId: 'secretary', instanceId: 'secretary', serviceNodeId, hostId: 'fixture',
    dataRoot: join(f.root, 'secretary-data'), publicBaseUrl: proxy.base, credential: f.credential, settings };
  const until = async (observe, label) => {
    const end = Date.now() + 45000;
    while (Date.now() < end) { const value = await observe(); if (value) return value; await delay(50); }
    throw Error('Secretary did not reach ' + label + ': ' + owner?.logs);
  };
  const start = async () => {
    owner = await startNativeService(config, { artifact: artifactRoot('secretary') });
    await until(async () => { const node = await f.client.request('serviceNodes.get', { serviceNodeId }); return node.ready && node.connected && node.synced; }, 'public readiness');
  };
  const close = async () => {
    const result = await owner.close();
    assert.deepEqual(result, { code: 0, signal: null });
    assert.equal(JSON.parse(await readFile(join(config.dataRoot, 'health.json'), 'utf8')).ready, false);
  };
  t.after(async () => { try { await owner?.close(); } finally { await proxy.close(); } });
  await start();
  const call = async (name, args) => callBound(f.client, await discover(f.client, 'secretary.' + name, { serviceNodeId }), args, args.operationId);
  const read = async id => (await f.client.request('objects.read', { objectId: id })).content.value;
  const save = async occurredAt => {
    const capture = { action: 'capture', operationId: await newOperationId(f.client), expectedScope: settings.identity.scope, sourceId: 'personal-inbox',
      message: message({ occurredAt, observedAt: new Date().toISOString() }) };
    const saved = await call('capture', capture); assert.equal(saved.phase, 'succeeded');
    const assess = { action: 'assess', operationId: await newOperationId(f.client), expectedScope: settings.identity.scope, item: saved.effect, assessment: assessment() };
    const decided = await call('assess', assess); assert.equal(decided.phase, 'succeeded');
    return { id: saved.effect.objectId, capture, saved, assess, decided };
  };
  const browserNotices = async () => {
    const notices = []; let afterSequence = 0;
    for (;;) {
      const page = await f.client.request('events.read', { afterSequence, filter: { topics: ['secretary.browser-notification'] } });
      notices.push(...page.items.filter(event => event.payload.target.fragment === '#/journal?node=' + serviceNodeId));
      if (!page.hasMore) return notices; afterSequence = page.throughSequence;
    }
  };
  const browserEnabled = async enabled => {
    const found = await f.client.request('objects.query', { contractKey: 'secretary/configuration', includeArchived: false,
      where: { op: 'and', args: [{ op: 'eq', field: 'object.parentId', value: root }, { op: 'eq', field: 'object.name', value: 'configuration' }] }, limit: 2 });
    assert.equal(found.items.length, 1);
    const current = await f.client.request('objects.read', { objectId: found.items[0].objectId });
    await call('saveConfiguration', { operationId: await newOperationId(f.client), configuration: { objectId: current.object.id, revision: current.revision.revision },
      value: { ...current.content.value, browserNotifications: enabled } });
  };
  return { calls, queries, read, save, call, settings, browserNotices, browserEnabled, until, start, close,
    get owner() { return owner; }, get lost() { return lost; }, loseNextNotifyReply: () => { lose = true; } };
}
const turns = f => f.agent.owners.flatMap(owner => owner.sent).filter(frame => frame.method === 'turn/start');
const notification = request => ({ action: request.action, operationId: request.operationId, source: request.source, text: request.text });

test('native Secretary defers a young notice while source progression continues', { timeout: 90000 }, async t => {
  const f = await testChatFixture(t), s = await secretary(t, f), item = await s.save(new Date().toISOString());
  await s.until(async () => (await s.read(item.id)).notice.reason === 'minimum_message_age', 'minimum message age guard');
  assert.equal(s.calls.length, 0); assert.equal(turns(f).length, 0); assert.equal((await s.browserNotices()).length, 0);
  const checkpoint = { action: 'checkpoint', operationId: await newOperationId(f.client), expectedScope: s.settings.identity.scope,
    sourceId: 'personal-inbox', previousCursor: null, nextCursor: 'young-notice-source-page', captures: [item.capture.operationId] };
  assert.equal((await s.call('checkpoint', checkpoint)).phase, 'succeeded');
  const status = await s.call('status', { expectedScope: s.settings.identity.scope });
  assert.equal(status.sources[0].data.cursor, checkpoint.nextCursor);
  await s.close(); await f.close();
});

test('native Secretary and Chat retain one Main turn across lost admission, process restarts and late display acknowledgment', { timeout: 180000 }, async t => {
  const f = await testChatFixture(t), s = await secretary(t, f);
  assert.equal(f.principal, principal);
  await s.browserEnabled(false); s.loseNextNotifyReply();
  const item = await s.save(new Date(Date.now() - 6 * 60000).toISOString());
  await s.until(async () => s.lost && turns(f).length === 1 && (await s.read(item.id)).notice.request, 'lost original admission and exactly one Main turn');
  const original = await s.read(item.id), prepared = original.notice.request;
  assert.equal((await s.browserNotices()).length, 0);
  await s.owner.kill(); const queryCount = s.queries.length;
  await f.restart(); await s.start();
  assert.deepEqual(await s.call('capture', item.capture), item.saved);
  assert.deepEqual(await s.call('assess', item.assess), item.decided);
  await s.until(() => s.calls.length >= 2 && s.queries.slice(queryCount).some(call => call.qualifiedName === 'chat.operation' && call.arguments.operationId === prepared.operationId), 'original admission reconciliation and idempotent retry after restart');
  for (const call of s.calls) assert.deepEqual(notification(call.arguments), notification(prepared));
  assert.deepEqual(notification((await s.read(item.id)).notice.request), notification(prepared));
  assert.equal(turns(f).length, 1);
  await s.browserEnabled(true);
  await s.until(async () => (await s.browserNotices()).length === 1, 'one browser notice after re-enabling');
  const threadId = turns(f)[0].params.threadId;
  assert.match(JSON.stringify(turns(f)[0].params.input), /internal service notification/);
  f.agent.complete(threadId, '[[Service] Secretary] Main forwarded the service message.');
  await s.until(async () => (await f.call('workspace', {})).main.data.queue.length === 0, 'original Main result publication');
  const offer = await f.receive(); assert.equal(offer.phase, 'succeeded'); assert.equal(offer.outcome.replies.length, 1);
  await s.until(async () => (await s.read(item.id)).notice.state === 'outcome_unknown', 'unacknowledged display evidence');
  assert.ok((await s.read(item.id)).notice.evidence);
  await s.owner.kill(); await f.restart(); await s.start();
  await f.acknowledge(offer);
  const confirmed = await s.until(async () => { const value = await s.read(item.id); return value.notice.state === 'confirmed' && value; }, 'late display confirmation after both process restarts');
  assert.ok(confirmed.notice.evidence);
  assert.deepEqual(confirmed.decision, original.decision, 'Notice delivery preserves the original assessment.');
  const evidence = await f.client.request('objects.read', confirmed.notice.evidence);
  assert.equal(evidence.object.contractKey, 'chat-bridge/reply'); assert.equal(evidence.content.encoding, 'json');
  const reply = evidence.content.value;
  assert.equal(reply.state, 'confirmed'); assert.equal(reply.origin.kind, 'native');
  assert.equal(evidence.revision.contentHash, hashJson(reply));
  const handoff = (await f.client.request('objects.read', { objectId: reply.origin.inputId })).content.value;
  assert.equal(handoff.identity.senderPrincipalId, principal);
  assert.equal(handoff.operationId, prepared.operationId);
  assert.deepEqual(handoff.result, reply.origin.result); assert.deepEqual(handoff.binding, reply.origin.binding);
  assert.deepEqual(reply.channel, confirmed.notice.target.channel); assert.deepEqual(reply.channel, f.channel);
  assert.deepEqual(notification(confirmed.notice.request), notification(prepared));
  for (const call of s.calls) assert.deepEqual(notification(call.arguments), notification(prepared));
  assert.equal(turns(f).length, 1); assert.equal((await s.browserNotices()).length, 1);
  await s.close(); await f.close();
});
