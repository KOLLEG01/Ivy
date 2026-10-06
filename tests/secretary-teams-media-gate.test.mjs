import test from 'node:test';
import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { hashJson } from '../dist/packages/sdk/src/node.js';
import { fixture, principal } from './secretary-fixture.mjs';
import { triageFixture } from './secretary-triage-fixture.mjs';
import { message, profile } from './secretary-teams-fixture.mjs';
import { teamsPage } from '../dist/services/secretary/src/teams-projection.js';
import { secretaryRegistry } from '../dist/services/secretary/src/schema.js';
import { triageRegistry } from '../dist/services/secretary/src/triage-schema.js';

test('Teams false attachment flag cannot consume assessment, while original body and checkpoint survive restart', { timeout: 30000 }, async t => {
  const source = { sourceId: 'personal-inbox', kind: 'teams', accountId: profile.id, producerPrincipalIds: [principal], ownSenderIds: ['user:' + profile.id], allowedSenderIds: null };
  const registry = () => { const base = secretaryRegistry(), triage = triageRegistry(); return { ...base, contracts: [...base.contracts, ...triage.contracts], requiredContracts: [...base.requiredContracts, ...triage.requiredContracts] }; };
  const base = await fixture(t, { sources: [source] }, registry), triage = await triageFixture(t, base);
  const configuration = { sourceId: source.sourceId, target: { serviceNodeId: 'native', hostId: 'host', nativeVersion: '0.149.1', nativeExecutableHash: hashJson('exe'), catalogHash: hashJson('catalog') },
    expectedAccountHash: hashJson('account'), profile, since: '2026-09-01T00:00:00.000Z', maximumChats: 8, maximumMessages: 8, excludedChatIds: [], pollMs: 60000, threadCwd: resolve('.local/teams-media-gate') };
  // The real native shape is identical for an ordinary text body and a caption with hosted images.
  const raw = message('inline-image', { has_attachments: false, content: 'Look at the diagram.' });
  const page = teamsPage({ messages: [raw] }, configuration, source, { since: configuration.since, until: '2026-09-07T09:00:00.000Z', chat: { id: 'chat-1', title: '' } }, '2026-09-07T09:00:00.000Z');
  assert.equal(page.messagesWithAttachments, 0); assert.equal(page.messages[0].attachments, 'expected');
  const request = base.captureRequest(page.messages[0]), capture = await base.engine.action(principal, request), item = capture.effect;
  assert.ok(item);
  const checkpoint = { action: 'checkpoint', operationId: base.operationId(), expectedScope: base.settings.identity.scope, sourceId: source.sourceId,
    previousCursor: null, nextCursor: 'original-native-page', captures: [request.operationId] };
  assert.equal((await base.engine.action(principal, checkpoint)).phase, 'succeeded');
  await assert.rejects(triage.worker.step(item), { code: 'secretary_triage_media_pending' });
  assert.deepEqual(triage.state.dispatches, []); assert.equal((await base.engine.item(item)).value.decision, null);
  await triage.restart(); triage.state.offline = true;
  await assert.rejects(triage.worker.step(item), { code: 'secretary_triage_media_pending' });
  assert.deepEqual(triage.state.dispatches, []); assert.equal((await base.engine.item(item)).value.decision, null);
  assert.deepEqual((await base.engine.action(principal, request)).effect, item);
  assert.equal((await base.engine.action(principal, checkpoint)).phase, 'succeeded');
  assert.equal((await base.engine.item(item)).value.message.text, raw.content);
});
