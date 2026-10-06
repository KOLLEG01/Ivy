import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { admitSchema, hashJson, operationId, validateShared } from '../dist/packages/sdk/src/node.js';
import { secretaryRegistry, validate } from '../dist/services/secretary/src/schema.js';
import { checkPolicy, ignored, quiet, suppression } from '../dist/services/secretary/src/policy.js';
import { SecretaryEngine } from '../dist/services/secretary/src/engine.js';
import { settingsFor, message, assessment } from './secretary-fixture.mjs';

test('Secretary contracts are complete, closed and enforce independent bounds', () => {
  const registry = secretaryRegistry(); assert.equal(registry.contracts.length, 6); assert.equal(registry.namespaces[0].tools.length, 16); validateShared('RegistrySync', registry);
  assert.equal(registry.contracts.find(value => value.key === 'secretary/item')?.specMarkdown,
    'Final selected message for the user-facing inbox. Source identity, assessment and useful notice state are materialized in its current revision. Source cursors and operation receipts live in the local Secretary journal.');
  for (const contract of registry.contracts) admitSchema(contract.jsonSchema);
  for (const tool of registry.namespaces[0].tools) { admitSchema(tool.inputSchema); admitSchema(tool.outputSchema); assert.match(hashJson(tool), /^sha256:/); }
  const settings = settingsFor(randomUUID(), 'secretary'); validate('Settings', settings);
  const req = { action: 'capture', operationId: 'original', expectedScope: settings.identity.scope, sourceId: 'personal-inbox', message: message() }; validate('Request', req);
  validate('Message', message({ attachments: 'expected' }));
  const withoutAttachments = message(); delete withoutAttachments.attachments;
  for (const value of [withoutAttachments, message({ attachments: null }), message({ attachments: 'complete' })]) assert.throws(() => validate('Message', value));
  for (const value of [{ ...req, surprise: true }, { ...req, message: { ...req.message, text: 'x'.repeat(32769) } }, { ...req, action: 'autoReply' }, { ...req, message: { ...req.message, senderId: '' } }]) assert.throws(() => validate('Request', value));
  assert.throws(() => validate('Settings', { ...settings, recordsPerTick: 0 }));
  assert.throws(() => validate('Assessment', { ...assessment(), urgency: 'now' }));
});

test('personal policy handles exact sender identities, source rules, overnight time and DST without pausing other work', () => {
  const source = settingsFor(randomUUID(), 'secretary').sources[0], policy = { silentTime: { timeZone: 'Europe/Berlin', start: '22:00', end: '07:00' }, urgentBypass: false, whatsappQuestionsOnly: true, minimumMainUrgency: 'normal', researchMaxMinutes: 5, voiceEscalation: null };
  checkPolicy(policy); assert.throws(() => checkPolicy({ ...policy, silentTime: { ...policy.silentTime, end: '22:00' } }));
  assert.throws(() => checkPolicy({ ...policy, silentTime: { ...policy.silentTime, timeZone: 'Invalid/Zone' } }));
  for (const at of ['2026-03-29T00:30:00Z', '2026-03-29T01:30:00Z', '2026-10-25T00:30:00Z', '2026-10-25T01:30:00Z']) assert.equal(quiet(policy, 'normal', new Date(at)), true);
  assert.equal(quiet(policy, 'normal', new Date('2026-09-07T05:00:00Z')), false); assert.equal(quiet(policy, 'critical', new Date('2026-09-07T20:00:00Z')), true);
  assert.equal(quiet({ ...policy, urgentBypass: true }, 'critical', new Date('2026-09-07T20:00:00Z')), false);
  assert.equal(ignored(source, message({ senderId: 'me@example.test' })), 'own_sender'); assert.equal(ignored(source, message({ senderId: 'ME@example.test' })), null);
  assert.equal(ignored({ ...source, allowedSenderIds: [] }, message()), 'sender_not_allowed'); assert.equal(ignored(source, message({ outgoing: true })), 'outgoing');
  assert.equal(suppression({ ...source, kind: 'whatsapp' }, assessment({ usefulIntel: false }), policy), null);
  assert.equal(suppression({ ...source, kind: 'whatsapp' }, assessment(), policy), null); assert.equal(suppression(source, assessment({ urgency: 'low' }), policy), 'low_urgency');
});

test('Secretary rejects producer, source, account and scope mistakes before any domain I/O', async () => {
  const settings = settingsFor(randomUUID(), 'secretary'), epoch = randomUUID(); let calls = 0;
  const engine = new SecretaryEngine({ request(method) { if (method === 'system.status') return Promise.resolve({ runtimeEpoch: epoch }); calls++; throw Error('Unauthorized I/O'); } }, settings, { serviceNodeId: 'secretary', generation: 1 }, new AbortController().signal);
  const request = { action: 'capture', operationId: operationId(epoch), expectedScope: settings.identity.scope, sourceId: 'personal-inbox', message: message() };
  await assert.rejects(engine.action('stranger', request), { code: 'forbidden' });
  await assert.rejects(engine.action(settings.identity.principalId, { ...request, expectedScope: { ...request.expectedScope, rootObjectId: randomUUID() } }), { code: 'secretary_scope_conflict' });
  await assert.rejects(engine.action(settings.identity.principalId, { ...request, message: message({ accountId: 'other' }) }), { code: 'secretary_account_mismatch' });
  await assert.rejects(engine.action(settings.identity.principalId, { ...request, sourceId: 'absent' }), { code: 'secretary_source_unknown' });
  assert.equal(calls, 0);
});
