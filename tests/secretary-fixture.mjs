import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { HiveClient, ServiceClient, digest, newOperationId, operationId } from '../dist/packages/sdk/src/node.js';
import { SecretaryEngine } from '../dist/services/secretary/src/engine.js';
import { secretaryRegistry } from '../dist/services/secretary/src/schema.js';

export const principal = 'isolated-consumer-test';
export const message = (change = {}) => ({ accountId: 'personal-account', conversationId: 'conversation-1', messageId: randomUUID(), nativeRevision: 'version-1', senderId: 'sender@example.test', outgoing: false, attachments: 'none',
  occurredAt: '2026-09-07T08:00:00.000Z', observedAt: '2026-09-07T08:05:00.000Z', title: 'A real observation shape', text: 'Please review the attached agenda.', url: 'https://example.test/item', ...change });
export const assessment = (change = {}) => ({ urgency: 'normal', disposition: 'notify', notification: 'main', summary: 'The agenda needs your review.', question: true, usefulIntel: true, task: null, ...change });
export const settingsFor = (root, serviceNodeId, caller = principal, change = {}) => ({ identity: { scope: { secretaryId: 'test-secretary', rootObjectId: root }, principalId: caller, serviceNodeId, hostId: 'fixture' },
  sources: [{ sourceId: 'personal-inbox', accountId: 'personal-account', kind: 'teams', producerPrincipalIds: [caller], allowedSenderIds: null, ownSenderIds: ['me@example.test'] }],
  policy: { silentTime: null, urgentBypass: false, whatsappQuestionsOnly: true, minimumMainUrgency: 'normal', researchMaxMinutes: 5, voiceEscalation: null },  pollMs: 100, recordsPerTick: 2, ...change });
export async function rootObject(client) {
  await client.request('contracts.register', { mutationId: await newOperationId(client), definition: { key: 'secretary-test/root', version: '1.0.0', owner: { kind: 'agent' }, mediaType: 'application/json',
    retention: { objects: { mode: 'retain' }, revisions: { mode: 'current' } }, jsonSchema: { type: 'object', additionalProperties: false }, specMarkdown: 'Isolated Secretary test root.' } });
  const id = randomUUID(); return (await client.request('objects.write', { mutationId: await newOperationId(client), contractVersion: '1.0.0', references: {}, create: { contractKey: 'secretary-test/root', parentId: null, ownerObjectId: null, name: id }, content: { encoding: 'json', value: {} } })).object.id;
}
export async function fixture(t, changes = {}, registry = secretaryRegistry) {
  const base = process.env.IVY_TEST_HIVE_URL, credential = process.env.IVY_TEST_HIVE_CREDENTIAL;
  assert.ok(base && credential, 'Use the explicit isolated Hive harness.'); assert.equal(new URL(base).hostname, '127.0.0.1');
  const client = new HiveClient(base, { credential }), status = await client.request('system.status', {}); assert.equal(status.version, 'isolated-consumer-test');
  const dataRoot = await mkdtemp(join(tmpdir(), 'ivy-secretary-journal-'));
  const root = await rootObject(client), serviceNodeId = 'secretary-test-' + randomUUID(), settings = settingsFor(root, serviceNodeId, principal, changes);
  const service = new ServiceClient({ publicBaseUrl: base, credential: () => credential, identity: { serviceNodeId, serviceName: 'secretary', hostId: 'fixture', version: 'test', buildId: digest(serviceNodeId), hiveProtocol: 1 },
    registry, handlers: Object.fromEntries(['capture', 'assess', 'attach', 'checkpoint', 'operation', 'operationRead', 'status', 'binding', 'listAssignments', 'getAssignment', 'saveConfiguration', 'saveAssignment', 'createAssignment', 'updateAssignment', 'followUp', 'followUpRead'].map(name => ['secretary.' + name, () => { throw Error('Use actual process fixture for tool handlers.'); }])), reconcile: async () => {}, heartbeatMs: 2000 });
  t.after(async () => { engine?.store.close(); await service.stop(); await rm(dataRoot, { recursive: true, force: true, maxRetries: 3 }); }); service.start(); const connection = await service.waitReady({ timeoutMs: 20000 });
  const owner = { serviceNodeId, generation: connection.generation }; let engine, now = new Date('2026-09-07T09:00:00.000Z');
  const restart = async (rpc = connection) => { engine?.store.close(); engine = new SecretaryEngine(rpc, settings, owner, connection.signal, () => now, dataRoot); await engine.initialize(); return engine; };
  await restart();
  return { client, connection, settings, service, owner, restart, get engine() { return engine; }, setNow(value) { now = new Date(value); },
    operationId() { return operationId(status.runtimeEpoch, Date.now(), randomUUID()); },
    captureRequest(msg = message(), id = operationId(status.runtimeEpoch, Date.now(), randomUUID())) { return { action: 'capture', operationId: id, expectedScope: settings.identity.scope, sourceId: 'personal-inbox', message: msg }; },
    assessRequest(item, value = assessment(), id = operationId(status.runtimeEpoch, Date.now(), randomUUID())) { return { action: 'assess', operationId: id, expectedScope: settings.identity.scope, item, assessment: value }; },
    async items() { return (await client.request('objects.query', { contractKey: 'secretary/item', limit: 100, where: { op: 'eq', field: 'object.parentId', value: root } })).items; } };
}

export const assignmentFor = (at, changes = {}) => ({ schemaVersion: 1, assignmentId: 'test-assignment', name: 'Review incoming event', description: '', enabled: false,
  trigger: { kind: 'event', topic: 'test.events', topicVersion: '1.0.0', sourceServiceNodeId: null, eventKind: 'message.received' },
  prompt: 'Review this event and decide whether the user needs to know.', preflight: null, rules: {}, createdAt: at, updatedAt: at, ...changes });
