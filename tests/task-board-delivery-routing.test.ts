import test from 'node:test';
import assert from 'node:assert/strict';
import { hashJson } from '../packages/contracts/src/canonical.js';
import type { TaskBoard } from '../packages/contracts/src/generated.js';
import { phoneRegistry } from '../services/phone-bridge/src/runtime/registry.js';
import { TaskBoardDeliveries } from '../services/task-board/src/runtime/deliveries.js';

const at = '2026-09-15T10:00:00.000Z';
const target: TaskBoard.PhoneTarget = { serviceNodeId: 'phone', recipientId: 'user' };
const chatTarget = { serviceNodeId: 'chat', expectedBridge: { principalId: 'chat', callerPrincipalId: 'task-board', workspaceId: 'main', definitionHash: 'sha256:' + '0'.repeat(64) },
  channel: { adapter: 'whatsapp' as const, accountId: 'user', channelId: 'main' } };
const value = (): TaskBoard.Delivery => ({ schemaVersion: 1, taskId: 'task', commentId: 'comment', publication: { objectId: 'operation', revision: 1 },
  requestedRoute: 'phone', activeRoute: 'phone', state: 'dispatching', chatTarget, phoneTarget: target, operationId: 'phone-operation',
  chatRequest: null, phoneRequest: { operationId: 'phone-operation', recipientId: 'user', route: 'voice', voicePrompt: 'Prompt' }, phoneCallId: 'call',
  deliveryEvidence: null, code: null, createdAt: at, updatedAt: at });
const document = (delivery: TaskBoard.Delivery) => ({ pin: { objectId: 'delivery', revision: 2 }, value: delivery });

function worker(responses: unknown[]) {
  const updates: TaskBoard.Delivery[] = [], instance = new TaskBoardDeliveries({ settings: { phoneTarget: target }, verifyOwner: async () => undefined } as never);
  (instance as unknown as { phoneCall: () => Promise<unknown> }).phoneCall = async () => responses.shift();
  (instance as unknown as { update: (_current: unknown, next: TaskBoard.Delivery) => Promise<void> }).update = async (_current, next) => { updates.push(next); };
  return { instance, updates };
}

test('Phone delivery discovers each current compatible provider definition', async () => {
  const hashes: string[] = [];
  let revision = 0;
  const client = { async request(method: string, params: Record<string, unknown>) {
    if (method === 'serviceNodes.get') return { serviceName: 'phone-bridge', connected: true, synced: true, ready: true };
    if (method === 'tools.list') {
      const definition = structuredClone(phoneRegistry().namespaces[0]!.tools.find(tool => tool.name === 'request')!);
      definition.description += ` Update ${++revision}.`;
      return { provider: { node: { serviceNodeId: 'phone' } }, items: [{ qualifiedName: 'phone.request', definition, definitionHash: hashJson(definition) }], nextCursor: null };
    }
    assert.equal(method, 'tools.call');
    hashes.push(params.expectedDefinitionHash as string);
    return { operationId: params.operationId };
  } };
  const instance = new TaskBoardDeliveries({ store: { client }, verifyOwner: async () => undefined } as never);
  const call = instance as unknown as { phoneCall: (name: 'request', target: TaskBoard.PhoneTarget, args: object, operationId: string) => Promise<unknown> };
  for (const id of ['first', 'second'])
    assert.deepEqual(await call.phoneCall('request', target, { operationId: id }, id), { operationId: id });
  assert.equal(hashes.length, 2);
  assert.notEqual(hashes[0], hashes[1]);
});

test('Phone preparation sends the exact Task and comment context as the initial Voice prompt', async () => {
  const f = worker([]), pending = { ...value(), state: 'pending' as const, operationId: null, phoneRequest: null, phoneCallId: null };
  (f.instance as unknown as { comment: () => Promise<unknown> }).comment = async () => ({ task: { taskKey: 'TASK-0042' }, comment: { authorKind: 'agent', body: 'Which release window works?', requests: [] } });
  await (f.instance as unknown as { phone: (current: unknown) => Promise<void> }).phone(document(pending));
  assert.match(f.updates[0]?.phoneRequest?.voicePrompt ?? '', /TASK-0042/);
  assert.match(f.updates[0]?.phoneRequest?.voicePrompt ?? '', /Which release window works\?/);
  assert.match(f.updates[0]?.phoneRequest?.voicePrompt ?? '', /Do not involve Main chat/);
});

test('a connected Voice call with a confirmed initial prompt completes without Chat fallback', async () => {
  const f = worker([null, { phase: 'result', receipt: { ok: true, result: { state: 'connected' } } }, { phase: 'result', receipt: { ok: true, result: { prompted: true } } }]);
  await (f.instance as unknown as { phone: (current: unknown) => Promise<void> }).phone(document(value()));
  assert.equal(f.updates.at(-1)?.state, 'confirmed'); assert.equal(f.updates.at(-1)?.activeRoute, 'phone'); assert.equal(f.updates.at(-1)?.chatRequest, null);
});

for (const state of ['sent', 'outcome_unknown'] as const) test('Phone delivery follows the forwarded request in an existing incoming call: ' + state, async () => {
  const operation = { intent: { method: 'call.forwardVoice', callId: 'call', operationId: 'phone-operation', prompt: 'Prompt' },
    phase: 'result', receipt: { ok: true, result: { state } } };
  const f = worker([{ operationId: 'original-incoming-call', callId: 'call' }, operation, operation]);
  await (f.instance as unknown as { phone: (current: unknown) => Promise<void> }).phone(document({ ...value(), phoneCallId: null }));
  assert.equal(f.updates.at(-1)?.phoneCallId, 'call');
  await (f.instance as unknown as { phone: (current: unknown) => Promise<void> }).phone(document(f.updates.at(-1)!));
  assert.equal(f.updates.at(-1)?.state, state === 'sent' ? 'confirmed' : 'outcome_unknown');
  assert.equal(f.updates.at(-1)?.activeRoute, 'phone');
  assert.equal(f.updates.at(-1)?.chatRequest, null);
});

test('a conclusively unanswered Voice call falls back to the current Main Chat and retains Phone evidence', async () => {
  const f = worker([null, { phase: 'result', receipt: { ok: true, result: { state: 'local_ended', error: 'no_answer' } } }]);
  await (f.instance as unknown as { phone: (current: unknown) => Promise<void> }).phone(document(value()));
  assert.equal(f.updates.at(-1)?.state, 'pending'); assert.equal(f.updates.at(-1)?.activeRoute, 'chat');
  assert.equal(f.updates.at(-1)?.phoneRequest?.operationId, 'phone-operation'); assert.equal(f.updates.at(-1)?.phoneCallId, 'call');
});

test('an unknown Voice connection outcome is fenced and never falls back to Chat', async () => {
  const f = worker([null, { phase: 'outcome_unknown', receipt: null }]);
  await (f.instance as unknown as { phone: (current: unknown) => Promise<void> }).phone(document(value()));
  assert.equal(f.updates.at(-1)?.state, 'outcome_unknown'); assert.equal(f.updates.at(-1)?.activeRoute, 'phone'); assert.equal(f.updates.at(-1)?.chatRequest, null);
});
