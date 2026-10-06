import test from 'node:test';
import assert from 'node:assert/strict';
import { IvyError } from '../dist/packages/sdk/src/node.js';
import { OutlookSource } from '../dist/services/secretary/src/outlook-source.js';
import { microsoftConfiguration } from '../dist/services/secretary/src/microsoft-config.js';
import { outlookFixture, mail, page } from './secretary-outlook-fixture.mjs';

test('Outlook closes an old epoch after confirmed thread creation and absent next read with original cleanup', { timeout: 90000 }, async t => {
  const n = await outlookFixture(t);
  n.state.afterNative = operation => { if (operation.method === 'thread/start') { n.state.epoch = 'replacement-epoch'; n.state.afterNative = null; } };
  assert.equal(await n.settle(), 'gap'); const head = await n.source.head(), proof = await n.source.evidence(head.value.lastPage);
  assert.equal(head.value.pending, null); assert.equal(head.value.lastCompleteAt, null); assert.equal(proof.value.gap, 'microsoft_epoch_changed');
  assert.deepEqual(n.state.dispatches.map(call => call.method), ['account/read', 'thread/start', 'thread/unsubscribe']);
  assert.equal((await n.f.items()).length, 0); await n.restart(); assert.equal((await n.source.head()).value.pending, null);
});

test('Outlook captures complete original pages with connector offsets before checkpoints and recovers offline', { timeout: 90000 }, async t => {
  const n = await outlookFixture(t); n.state.pages.set(0, { ...page([mail('first')]), has_more: true, next_from_index: 13, next_link: 'https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages?$skip=13' }); n.state.pages.set(13, page([mail('second')]));
  assert.equal(await n.settle(), 'page'); const first = await n.source.head(); assert.equal(first.value.lastCompleteAt, null); assert.equal(first.value.continuation.selection.skip, 13); assert.equal(n.state.dispatches.length, 7);
  assert.equal(await n.settle(), 'complete'); const complete = await n.source.head(); assert.ok(complete.value.lastCompleteAt); assert.equal(complete.value.pending, null); assert.equal((await n.f.items()).length, 2); assert.equal(n.state.dispatches.length, 14);
  const lists = n.state.dispatches.filter(x => x.params.tool?.endsWith('list_messages')); assert.equal(lists.length, 2); assert.equal(lists[0].params.arguments.filter, lists[1].params.arguments.filter);
  const checkpoint = await n.f.engine.source('mail'); assert.match(checkpoint.value.cursor, /^outlook-page:/); assert.equal(checkpoint.pin.revision, 3);
  n.state.offline = true; await n.restart(); assert.deepEqual((await n.source.head()).pin, complete.pin); assert.equal(await n.source.step(), 'idle'); assert.equal(n.state.dispatches.length, 14);
});

test('Outlook recovers original evidence capture checkpoint and head commits after lost replies', { timeout: 240000 }, async t => {
  const boundaries = [ ['secretary/item', 1] ];
  for (const [key, revision] of boundaries) {
    const n = await outlookFixture(t); let hit = false;
    n.state.after = async (method, _args, value) => { if (!hit && method === 'objects.write' && value.object.contractKey === key && value.revision.revision === revision) { hit = true; n.state.cut = true; throw new IvyError('outcome_unknown', 'Lost committed response.', 'unknown'); } };
    await assert.rejects(n.settle(), undefined, key + ':' + revision); assert.ok(hit, key + ':' + revision); n.state.after = null; await n.restart();
    assert.ok(['complete', 'idle'].includes(await n.settle())); assert.equal((await n.f.items()).length, 1); assert.equal(n.state.dispatches.length, 7); assert.equal(new Set(n.state.dispatches.map(x => x.operationId)).size, 7);
    assert.equal((await n.f.engine.source('mail')).pin.revision, 2); assert.ok((await n.source.head()).value.lastCompleteAt);
  }
});

test('Outlook retains known pending list and cleanup operations and refuses later journal absence', { timeout: 90000 }, async t => {
  for (const method of ['microsoft_outlook_email.list_messages', 'thread/unsubscribe']) {
    const n = await outlookFixture(t); n.state.pendingMethod = method; assert.equal(await n.source.step(), 'pending'); const count = n.state.dispatches.length; assert.equal((await n.f.items()).length, 0);
    n.state.absent = true; await n.restart(); await assert.rejects(n.source.step(), { code: 'microsoft_native_absence_conflict' }); assert.equal(n.state.dispatches.length, count);
    n.state.absent = false; n.completePending(); assert.equal(await n.settle(), 'complete'); assert.equal(n.state.dispatches.length, 7); assert.equal((await n.f.items()).length, 1);
  }
});

test('Outlook account and provider failures preserve prior completion and respect rate-limit delays', { timeout: 120000 }, async t => {
  const n = await outlookFixture(t); assert.equal(await n.settle(), 'complete'); const baseline = await n.source.head(), source = await n.f.engine.source('mail');
  n.setNow('2026-09-07T09:01:00.000Z'); n.state.providerError = { error_code: 'RATE_LIMITED', retry_after_seconds: 120 }; assert.equal(await n.settle(), 'gap');
  const gap = await n.source.head(); assert.equal(gap.value.lastCompleteAt, baseline.value.lastCompleteAt); assert.equal(gap.value.nextPollAt, '2026-09-07T09:03:00.000Z'); assert.deepEqual((await n.f.engine.source('mail')).pin, source.pin);
  const count = n.state.dispatches.length; n.setNow('2026-09-07T09:02:00.000Z'); assert.equal(await n.source.step(), 'idle'); assert.equal(n.state.dispatches.length, count);
  n.setNow('2026-09-07T09:03:00.000Z'); n.state.providerError = null; n.state.profile.email = 'another@example.test'; assert.equal(await n.settle(), 'gap'); assert.deepEqual((await n.f.engine.source('mail')).pin, source.pin);
  n.setNow('2026-09-07T09:04:00.000Z'); n.state.profile.email = n.configuration.profile.email; assert.equal(await n.settle(), 'complete'); assert.equal((await n.f.items()).length, 1); assert.equal((await n.f.engine.source('mail')).pin.revision, 3);
});

test('Competing Outlook collectors share native identities and cannot publish duplicate inbox items', { timeout: 90000 }, async t => {
  const n = await outlookFixture(t), other = new OutlookSource(n.f.engine, n.configuration);
  await Promise.allSettled([n.source.step(), other.step()]); assert.ok(['complete', 'idle'].includes(await n.settle()));
  assert.equal(n.state.dispatches.length, 7); assert.equal((await n.f.items()).length, 1); assert.equal((await n.f.engine.source('mail')).pin.revision, 2);
  const calls = n.f.engine.store.technicalList('secretary/outlook-call'); assert.equal(calls.length, 7);
});

test('Outlook protects source selection against changed native definitions and foreign checkpoints', { timeout: 90000 }, async t => {
  const n = await outlookFixture(t); n.state.pendingMethod = 'account/read'; assert.equal(await n.source.step(), 'pending');
  await n.f.engine.action(n.f.settings.identity.principalId, { action: 'checkpoint', operationId: n.f.operationId(), expectedScope: n.f.settings.identity.scope, sourceId: 'mail', previousCursor: null, nextCursor: 'foreign', captures: [] });
  n.state.pendingMethod = null; n.completePending(); await assert.rejects(n.source.step(), { code: 'microsoft_source_cursor_changed' }); assert.equal(n.state.dispatches.length, 1); assert.equal((await n.f.items()).length, 0);
  const config = { schemaVersion: 1, collectors: [n.configuration] }; assert.deepEqual(microsoftConfiguration(config, n.f.settings), config);
  for (const collectors of [[n.configuration, n.configuration], [{ ...n.configuration, sourceId: 'wrong' }], [{ ...n.configuration, profile: { ...n.configuration.profile, id: 'another-account' } }]]) assert.throws(() => microsoftConfiguration({ schemaVersion: 1, collectors }, n.f.settings));
  const m = await outlookFixture(t); m.state.afterNative = () => { m.state.changedDefinition = true; };
  await assert.rejects(m.source.step(), { code: 'tool_definition_changed' }); assert.equal(m.state.dispatches.length, 1);
  m.state.afterNative = null; m.state.changedDefinition = false; assert.equal(await m.settle(), 'complete'); assert.equal(m.state.dispatches.length, 7);
});

test('Outlook refuses rewritten original native chunks after successful publication', { timeout: 60000 }, async t => {
  const n = await outlookFixture(t); assert.equal(await n.settle(), 'complete'); const head = await n.source.head(), proof = await n.source.evidence(head.value.lastPage);
  const call = await n.source.store.read('secretary/outlook-call', proof.value.calls[0]); const pin = call.value.observation.chunks[0];
  n.f.engine.store.db.prepare('UPDATE technical_blobs SET bytes=? WHERE id=?').run(Buffer.from('{}'), pin.objectId);
  n.state.offline = true; await n.restart(); await assert.rejects(n.source.head(), { code: 'microsoft_evidence_invalid' }); assert.equal(n.state.dispatches.length, 7);
});
