import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { IvyError } from '../dist/packages/sdk/src/node.js';
import { fixture, message, assessment, principal } from './secretary-fixture.mjs';
import { SecretaryNotices } from '../dist/services/secretary/src/notices.js';
import { secretaryRegistry } from '../dist/services/secretary/src/schema.js';

test('Secretary UI requirements match the service interface and published contracts', async () => {
  const ui = JSON.parse(await readFile('ui/secretary-ui/ui.json', 'utf8'));
  const registry = secretaryRegistry();
  const secretary = ui.requirements.services.find(value => value.serviceName === 'secretary');
  assert.ok(registry.namespaces.some(value => value.namespace === secretary.namespace &&
    value.tools.some(tool => tool.interfaceVersion === secretary.interfaceVersion)));
  for (const requirement of ui.requirements.contracts) {
    const supported = registry.requiredContracts.find(value => value.key === requirement.key).readVersions;
    for (const version of requirement.readVersions) assert.ok(supported.includes(version), `${requirement.key}@${version}`);
  }
});

test('Secretary UI only calls tools published by its selected provider', async () => {
  const runtime = await readFile('ui/secretary-ui/src/runtime.ts', 'utf8');
  const calls = [...runtime.matchAll(/\.call\(\s*"(secretary\.[^"]+)"/g)].map((match) => match[1]);
  const registered = new Set(secretaryRegistry().namespaces.flatMap(namespace =>
    namespace.tools.map(tool => `${namespace.namespace}.${tool.name}`)));
  assert.ok(calls.includes('secretary.binding'));
  assert.deepEqual(calls.filter(name => !registered.has(name)), []);
});

test('Secretary saves first message provenance, deduplicates source versions and checkpoints only confirmed captures', { timeout: 45000 }, async t => {
  const f = await fixture(t), request = f.captureRequest(), first = await f.engine.action(principal, request);
  assert.equal(first.phase, 'succeeded'); assert.equal(first.effect.revision, 1);
  assert.deepEqual(await f.engine.action(principal, request), first);
  await assert.rejects(f.engine.action(principal, { ...request, message: { ...request.message, text: 'changed' } }), { code: 'mutation_conflict' });
  const again = await f.engine.action(principal, f.captureRequest({ ...request.message, observedAt: '2026-09-07T09:05:00.000Z' })); assert.deepEqual(again.effect, first.effect);
  assert.equal((await f.engine.item(first.effect)).value.message.observedAt, request.message.observedAt);
  await assert.rejects(f.engine.action(principal, f.captureRequest({ ...request.message, text: 'changed without a native revision' })), { code: 'secretary_identity_conflict' });
  const edited = await f.engine.action(principal, f.captureRequest({ ...request.message, nativeRevision: 'version-2', text: 'Edited message' })); assert.notEqual(edited.effect.objectId, first.effect.objectId);
  const checkpoint = { action: 'checkpoint', operationId: f.operationId(), expectedScope: f.settings.identity.scope, sourceId: 'personal-inbox', previousCursor: null, nextCursor: 'page-1', captures: [request.operationId] };
  await assert.rejects(f.engine.action(principal, { ...checkpoint, captures: ['unconfirmed'] }), { code: 'secretary_capture_pending' }); assert.equal((await f.engine.source('personal-inbox')).value.cursor, null);
  const saved = await f.engine.action(principal, checkpoint); assert.equal(saved.phase, 'succeeded');
  await f.engine.action(principal, { ...checkpoint, operationId: f.operationId(), previousCursor: 'page-1', nextCursor: 'page-2', captures: [] });
  assert.deepEqual(await f.engine.action(principal, checkpoint), saved); assert.equal((await f.engine.source('personal-inbox')).value.cursor, 'page-2');
  const ignored = await f.engine.action(principal, f.captureRequest(message({ senderId: 'me@example.test', text: 'Private content must not be copied.' })));
  assert.equal(ignored.ignoredReason, 'own_sender'); assert.equal(ignored.request, null); assert.equal(ignored.prepared, null); assert.equal(ignored.effect, null);
  assert.ok(!JSON.stringify(ignored).includes('Private content')); assert.equal((await f.items()).length, 2);
});

test('Secretary reopens original capture, assessment and checkpoint operations after lost Hive effect responses', { timeout: 60000 }, async t => {
  const f = await fixture(t);
  for (const action of ['capture', 'assess']) {
    const captureRequest = f.captureRequest();
    const capture = action === 'capture' ? null : await f.engine.action(principal, captureRequest);
    const request = action === 'capture' ? captureRequest : f.assessRequest(capture.effect);
    let lost = false, cut = false;
    const rpc = { async request(method, args, options) {
      if (cut) throw new IvyError('outcome_unknown', 'Injected outage until process reopening.', 'unknown');
      const result = await f.connection.request(method, args, options);
      if (method === 'objects.write' && result.object.contractKey === 'secretary/item' && !lost) { lost = true; cut = true; throw new IvyError('outcome_unknown', 'Injected committed effect.', 'unknown'); } return result;
    } };
    await f.restart(rpc); await assert.rejects(f.engine.action(principal, request)); assert.equal(lost, true, action);
    await f.restart(); for (let i = 0; i < 3; i++) await f.engine.tick();
    const recovered = await f.engine.action(principal, request); assert.equal(recovered.phase, 'succeeded', action);
    assert.deepEqual(recovered.request, request); assert.equal(recovered.createdAt, '2026-09-07T09:00:00.000Z');
    assert.deepEqual(await f.engine.action(principal, request), recovered);
  }
  const captureRequest = f.captureRequest(), capture = await f.engine.action(principal, captureRequest), source = await f.engine.source('personal-inbox');
  const checkpoint = { action: 'checkpoint', operationId: f.operationId(), expectedScope: f.settings.identity.scope, sourceId: 'personal-inbox', previousCursor: source.value.cursor, nextCursor: randomUUID(), captures: [captureRequest.operationId] };
  const saved = await f.engine.action(principal, checkpoint); await f.restart(); assert.deepEqual(await f.engine.action(principal, checkpoint), saved);
});

test('parallel Secretary actions create one message and one assessment while preserving the losing original operation', { timeout: 45000 }, async t => {
  const f = await fixture(t), msg = message(), requests = [f.captureRequest(msg), f.captureRequest(msg)];
  const saved = await Promise.all(requests.map(request => f.engine.action(principal, request))); assert.deepEqual(saved[0].effect, saved[1].effect); assert.equal((await f.items()).length, 1);
  const requests2 = [f.assessRequest(saved[0].effect), f.assessRequest(saved[0].effect, assessment({ summary: 'Competing decision' }))];
  const decisions = await Promise.allSettled(requests2.map(request => f.engine.action(principal, request)));
  assert.equal(decisions.filter(value => value.status === 'fulfilled' && value.value.phase === 'succeeded').length, 1);
  for (const result of decisions) if (result.status === 'rejected') assert.equal(result.reason.code, 'revision_conflict'); else if (result.value.phase === 'failed') assert.equal(result.value.errorCode, 'revision_conflict');
  const item = await f.engine.item(saved[0].effect.objectId); assert.equal(item.pin.revision, 2); assert.ok(item.value.decision); assert.equal(item.value.notice.state, 'queued');
  const winner = requests2.find(request => request.operationId === item.value.decision.operationId), original = await f.engine.action(principal, winner);
  await new SecretaryNotices(f.engine).step(item.pin.objectId); assert.equal((await f.engine.item(item.pin.objectId)).value.notice.reason, 'channel_not_configured');
  assert.deepEqual(await f.engine.action(principal, winner), original, 'Later notice revisions do not change the original action outcome.');
});

test('Secretary detects rewritten original evidence and still recovers later rows beyond a poisoned first page', { timeout: 45000 }, async t => {
  const f = await fixture(t), request = f.captureRequest(), saved = await f.engine.action(principal, request);
  const item = await f.engine.item(saved.effect);
  await f.engine.store.write('secretary/item', { ...item.value, message: { ...item.value.message, text: 'Tampered' } }, { object: item.pin }, f.operationId());
  await assert.rejects(f.engine.item(item.pin.objectId), { code: 'secretary_evidence_conflict' });
  // A malformed retained operation is isolated from the fair recovery cursor.
  const operation = await f.engine.find(principal, request.operationId);
  await f.engine.store.amend(operation, 'secretary/operation', { ...operation.value, phase: 'accepted', createdAt: '2026-09-07T08:00:00.000Z', effect: null });
  await assert.rejects(f.engine.operation(principal, f.settings.identity.scope, request.operationId), { code: 'secretary_evidence_conflict' });
  const later = [];
  for (let i = 0; i < 4; i++) {
    const next = f.captureRequest(); later.push(next); assert.equal((await f.engine.action(principal, next)).phase, 'succeeded');
  }
  for (let i = 0; i < 5; i++) await f.engine.tick();
  for (const next of later) assert.equal((await f.engine.operation(principal, f.settings.identity.scope, next.operationId)).phase, 'succeeded');
  const status = await f.engine.status(principal, f.settings.identity.scope); assert.deepEqual(status.recoveryIssues, [{ objectId: operation.pin.objectId, code: 'secretary_evidence_conflict' }]);
  assert.equal((await f.items()).length, 5);
});
