import { randomUUID } from 'node:crypto';
import { newOperationId } from '../dist/packages/sdk/src/node.js';
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
export const assignmentFor = (at, changes = {}) => ({ schemaVersion: 1, assignmentId: 'test-assignment', name: 'Review incoming event', description: '', enabled: false,
  trigger: { kind: 'event', topic: 'test.events', topicVersion: '1.0.0', sourceServiceNodeId: null, eventKind: 'message.received' },
  prompt: 'Review this event and decide whether the user needs to know.', preflight: null, rules: {}, createdAt: at, updatedAt: at, ...changes });
