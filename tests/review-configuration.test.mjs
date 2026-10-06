import test from 'node:test';
import assert from 'node:assert/strict';
import { IvyError } from '../dist/packages/sdk/src/node.js';
import { fixture as secretaryFixture, principal, message } from './secretary-fixture.mjs';
import { SecretaryNotices } from '../dist/services/secretary/src/notices.js';
import { outlookFixture } from './secretary-outlook-fixture.mjs';
import { teamsFixture } from './secretary-teams-fixture.mjs';
import { triageFixture } from './secretary-triage-fixture.mjs';

test('Secretary policy changes preserve checkpoint, original operations and deduplication; removed sources retain history', { timeout: 60000 }, async t => {
  const f = await secretaryFixture(t), request = f.captureRequest(), capture = await f.engine.action(principal, request);
  const checkpoint = { action: 'checkpoint', operationId: f.operationId(), expectedScope: f.settings.identity.scope, sourceId: request.sourceId, previousCursor: null, nextCursor: 'page-1', captures: [request.operationId] };
  await f.engine.action(principal, checkpoint);
  const assessed = await f.engine.action(principal, f.assessRequest(capture.effect));
  const initial = await f.engine.source(request.sourceId);
  f.settings.sources[0].allowedSenderIds = [];
  await f.restart(); const changed = await f.engine.source(request.sourceId);
  assert.equal(changed.pin.objectId, initial.pin.objectId); assert.equal(changed.pin.revision, initial.pin.revision + 1); assert.equal(changed.value.cursor, 'page-1');
  assert.deepEqual(await f.engine.operation(principal, request.expectedScope, request.operationId), capture);
  assert.equal(capture.source.definition.allowedSenderIds, null);
  const rejected = await f.engine.action(principal, f.captureRequest(message())); assert.equal(rejected.ignoredReason, 'sender_not_allowed');
  f.settings.sources[0].allowedSenderIds = null; await f.restart();
  const duplicate = (await f.engine.action(principal, f.captureRequest(request.message))).effect;
  assert.equal(duplicate.objectId, capture.effect.objectId); assert.equal(duplicate.revision, assessed.effect.revision);
  const configured = structuredClone(f.settings.sources); f.settings.sources = []; await f.restart();
  assert.equal((await f.engine.item(capture.effect)).value.message.messageId, request.message.messageId);
  assert.deepEqual(await f.engine.operation(principal, request.expectedScope, request.operationId), capture);
  await assert.rejects(f.engine.action(principal, f.captureRequest()), { code: 'secretary_source_unknown' });
  await new SecretaryNotices(f.engine).step(assessed.effect.objectId);
  assert.equal((await f.engine.item(assessed.effect.objectId)).value.notice.reason, 'authorization_changed');
  f.settings.sources = configured; f.settings.sources[0].accountId = 'replacement-account';
  await assert.rejects(f.restart(), { code: 'secretary_identity_conflict' });
});

test('Secretary reconciles committed effects after revocation and retains unexecuted originals without a new effect', { timeout: 60000 }, async t => {
  for (const stage of ['unexecuted', 'committed']) {
    const f = await secretaryFixture(t), request = f.captureRequest(); let cut = false;
    const rpc = { async request(method, args, options) {
      if (cut) throw new IvyError('outcome_unknown', 'Isolated lost response.', 'unknown');
      if (method === 'objects.write' && args.create?.contractKey === 'secretary/item' && stage === 'unexecuted') { cut = true; throw new IvyError('outcome_unknown', 'Isolated effect outage.', 'unknown'); }
      const result = await f.connection.request(method, args, options);
      if (method === 'objects.write' && args.create?.contractKey === 'secretary/item') { cut = true; throw new IvyError('outcome_unknown', 'Isolated committed response loss.', 'unknown'); }
      return result;
    } };
    await f.restart(rpc); await assert.rejects(f.engine.action(principal, request)); assert.equal(cut, true);
    f.settings.sources[0].producerPrincipalIds = ['replacement-producer']; await f.restart(); await f.engine.tick();
    const operation = await f.engine.operation(principal, request.expectedScope, request.operationId);
    assert.equal(operation.phase, stage === 'committed' ? 'succeeded' : 'accepted'); assert.equal((await f.items()).length, stage === 'committed' ? 1 : 0);
    if (stage === 'unexecuted') { assert.equal(f.engine.recoveryIssues.size, 1); f.settings.sources[0].producerPrincipalIds = [principal]; await f.restart(); await f.engine.tick();
      const recovered = await f.engine.operation(principal, request.expectedScope, request.operationId); assert.equal(recovered.phase, 'succeeded'); assert.deepEqual(recovered.source, operation.source); }
  }
});

test('Outlook and Teams retain original page evidence across sender-policy changes without capturing newly excluded messages', { timeout: 120000 }, async t => {
  for (const [provider, fixture] of [['outlook', outlookFixture], ['teams', teamsFixture]]) {
    const n = await fixture(t); if (provider === 'teams') assert.equal(await n.settle(), 'page');
    n.state.pendingMethod = 'thread/unsubscribe'; assert.equal(await n.source.step(), 'pending'); const dispatches = n.state.dispatches.length;
    n.f.settings.sources[0].allowedSenderIds = []; n.state.pendingMethod = null; n.completePending(); await n.restart();
    assert.equal(await n.settle(), 'complete'); assert.equal((await n.f.items()).length, 0); assert.equal(n.state.dispatches.length, dispatches);
    const originalHead = await n.source.head(); await n.restart(); assert.deepEqual((await n.source.head()).pin, originalHead.pin);
  }
});

test('Triage keeps completed original prompts readable and prevents new dispatch for a deactivated source', { timeout: 90000 }, async t => {
  const n = await triageFixture(t), item = await n.capture();
  assert.equal(await n.worker.step(item), 'assessed'); const dispatches = n.state.dispatches.length;
  n.f.settings.sources[0].allowedSenderIds = []; await n.restart();
  assert.equal(await n.worker.step(item), 'assessed'); assert.equal(n.state.dispatches.length, dispatches);
  n.f.settings.sources[0].allowedSenderIds = null; await n.restart(); const pending = await n.capture();
  await n.worker.native.prepare(pending);
  n.f.settings.sources = []; await n.restart();
  await assert.rejects(n.worker.step(pending), { code: 'secretary_triage_authorization_changed' });
  assert.equal(n.state.dispatches.length, dispatches);
});
