import test from 'node:test';
import assert from 'node:assert/strict';
import { TeamsSource } from '../dist/services/secretary/src/teams-source.js';
import { teamsFixture, chat, message, page } from './secretary-teams-fixture.mjs';

test('Teams checkpoints the original unread list then each chat with one fixed period and offline recovery', { timeout: 90000 }, async t => {
  const n = await teamsFixture(t); n.state.chats.chats.push(chat('chat-2'));
  n.state.pages.set('chat-2', page([message('second', { chat_id: 'chat-2', path: '/chats/chat-2/messages/second' })]));
  assert.equal(await n.settle(), 'page'); let head = await n.source.head(); assert.equal(head.value.lastCompleteAt, null); assert.equal(head.value.continuation.selection.chat.id, 'chat-1'); assert.equal((await n.f.items()).length, 0);
  n.setNow('2026-09-07T09:00:01.000Z'); assert.equal(await n.settle(), 'page'); head = await n.source.head(); assert.equal(head.value.continuation.selection.chat.id, 'chat-2'); assert.equal(head.value.continuation.selection.until, '2026-09-07T09:00:00.000Z');
  assert.equal(head.value.lastCompleteAt, null); assert.equal((await n.f.items()).length, 1);
  assert.equal(await n.settle(), 'complete'); const complete = await n.source.head(); assert.ok(complete.value.lastCompleteAt); assert.equal(complete.value.pending, null); assert.equal(complete.value.continuation, null);
  assert.equal((await n.f.items()).length, 2); assert.equal(n.state.dispatches.length, 21); assert.equal((await n.f.engine.source('teams')).pin.revision, 4);
  n.state.offline = true; await n.restart(); assert.deepEqual((await n.source.head()).pin, complete.pin); assert.equal(await n.source.step(), 'idle'); assert.equal(n.state.dispatches.length, 21);
});

test('Teams retains known pending list message and cleanup operations and refuses later journal absence', { timeout: 90000 }, async t => {
  for (const method of ['microsoft_teams.list_chats', 'microsoft_teams.list_chat_messages', 'thread/unsubscribe']) {
    const n = await teamsFixture(t);
    if (method === 'microsoft_teams.list_chat_messages') assert.equal(await n.settle(), 'page');
    n.state.pendingMethod = method; assert.equal(await n.source.step(), 'pending'); const count = n.state.dispatches.length;
    n.state.absent = true; await n.restart(); await assert.rejects(n.source.step(), { code: 'microsoft_native_absence_conflict' }); assert.equal(n.state.dispatches.length, count);
    n.state.absent = false; n.completePending(); let result = await n.settle(); if (result === 'page') result = await n.settle(); assert.equal(result, 'complete');
    assert.equal(n.state.dispatches.length, 14); assert.equal((await n.f.items()).length, 1);
  }
});

test('Teams preserves previous completeness through rate limits account changes and later successful rescan', { timeout: 90000 }, async t => {
  const n = await teamsFixture(t); assert.equal(await n.settle(), 'page'); assert.equal(await n.settle(), 'complete');
  const baseline = await n.source.head(), original = await n.f.engine.source('teams');
  n.setNow('2026-09-07T09:01:00.000Z'); n.state.providerError = { error_code: 'RATE_LIMITED', retry_after_seconds: 120 }; assert.equal(await n.settle(), 'gap');
  const gap = await n.source.head(); assert.equal(gap.value.lastCompleteAt, baseline.value.lastCompleteAt); assert.equal(gap.value.nextPollAt, '2026-09-07T09:03:00.000Z'); assert.deepEqual((await n.f.engine.source('teams')).pin, original.pin);
  const count = n.state.dispatches.length; n.setNow('2026-09-07T09:02:00.000Z'); assert.equal(await n.source.step(), 'idle'); assert.equal(n.state.dispatches.length, count);
  n.setNow('2026-09-07T09:03:00.000Z'); n.state.providerError = null; n.state.profile.email = 'another@example.test'; assert.equal(await n.settle(), 'gap'); assert.deepEqual((await n.f.engine.source('teams')).pin, original.pin);
  n.setNow('2026-09-07T09:04:00.000Z'); n.state.profile.email = n.configuration.profile.email; assert.equal(await n.settle(), 'page'); assert.equal(await n.settle(), 'complete'); assert.equal((await n.f.items()).length, 1);
});

test('Competing Teams collectors share original reads and a failed later chat cannot refresh scan completeness', { timeout: 90000 }, async t => {
  const n = await teamsFixture(t), other = new TeamsSource(n.f.engine, n.configuration);
  await Promise.allSettled([n.source.step(), other.step()]); if ((await n.source.head()).value.pending) assert.equal(await n.settle(), 'page');
  assert.equal(n.state.dispatches.length, 7); n.state.pages.set('chat-1', page(Array.from({ length: 9 }, (_, i) => message(String(i)))));
  assert.equal(await n.settle(), 'gap'); assert.equal((await n.f.items()).length, 0); assert.equal((await n.source.head()).value.lastCompleteAt, null);
  assert.equal(n.state.dispatches.length, 12); // Message capacity failure still unsubscribes its own thread.
  const proof = await n.source.evidence((await n.source.head()).value.lastPage); assert.equal(proof.value.gap, 'teams_scan_limit'); assert.equal(proof.value.page, null);
});

test('Teams preserves selection against foreign checkpoints changed definitions and rewritten native evidence', { timeout: 90000 }, async t => {
  const n = await teamsFixture(t); n.state.pendingMethod = 'account/read'; assert.equal(await n.source.step(), 'pending');
  await n.f.engine.action(n.f.settings.identity.principalId, { action: 'checkpoint', operationId: n.f.operationId(), expectedScope: n.f.settings.identity.scope, sourceId: 'teams', previousCursor: null, nextCursor: 'foreign', captures: [] });
  n.state.pendingMethod = null; n.completePending(); await assert.rejects(n.source.step(), { code: 'microsoft_source_cursor_changed' }); assert.equal(n.state.dispatches.length, 1);
  const m = await teamsFixture(t); m.state.afterNative = () => { m.state.changedDefinition = true; };
  await assert.rejects(m.source.step(), { code: 'tool_definition_changed' }); assert.equal(m.state.dispatches.length, 1);
  m.state.afterNative = null; m.state.changedDefinition = false; assert.equal(await m.settle(), 'page'); assert.equal(await m.settle(), 'complete');
  const head = await m.source.head(), proof = await m.source.evidence(head.value.lastPage), call = await m.source.store.read('secretary/teams-call', proof.value.calls[0]), pin = call.value.observation.chunks[0];
  m.f.engine.store.db.prepare('UPDATE technical_blobs SET bytes=? WHERE id=?').run(Buffer.from('{}'), pin.objectId);
  m.state.offline = true; await m.restart(); await assert.rejects(m.source.head(), { code: 'microsoft_evidence_invalid' }); assert.equal(m.state.dispatches.length, 14);
});
