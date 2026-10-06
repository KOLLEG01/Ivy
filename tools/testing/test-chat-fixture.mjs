// Explicit public test fixture for independently installed SDK consumers. Not a runtime adapter.
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { chatNativeFixture } from '../../dist/tests/fixtures/chat-native.js';
import { ChatBridge } from '../../dist/services/chat-bridge/src/bridge.js';
import { ChatDelivery } from '../../dist/services/chat-bridge/src/delivery.js';
import { ChatCollector } from '../../dist/services/chat-bridge/src/collector.js';
import { ChatPublisher } from '../../dist/services/chat-bridge/src/publisher.js';
import { chatRegistry, chatTools } from '../../dist/services/chat-bridge/src/registry.js';
import { digest } from '../../dist/packages/contracts/src/canonical.js';
import { toolDefinitionHash } from '../../dist/packages/contracts/src/tool-definition.js';
import { IvyError } from '../../dist/packages/contracts/src/errors.js';

export async function testChatFixture(t) {
  const f = await chatNativeFixture(t, '0.154.0');
  await f.first.create('alice', f.request('main'));
  await f.clients.first.client.request('registry.sync', chatRegistry());
  await f.clients.first.client.request('service.heartbeat', { ready: true, diagnostics: [] });
  const bridge = new ChatBridge(f.first), calls = []; let lose = false, beforeNotify = null;
  const execute = (context, method, params) => {
    const result = f.kernel.execute(context, { jsonrpc: '2.0', id: randomUUID(), method, params });
    assert.equal(result.kind, 'result'); return result.value;
  };
  const connect = (serviceNodeId, serviceName, registry) => {
    const initial = { credentialDigest: f.clients.first.context.credentialDigest, principalId: 'chat-owner', transport: 'ws' };
    const connected = execute(initial, 'service.connect', { serviceNodeId, serviceName, hostId: 'fixture', version: 'test', buildId: digest(serviceNodeId), hiveProtocol: 1 });
    const context = { ...initial, serviceNodeId, generation: connected.generation };
    execute(context, 'registry.sync', registry); execute(context, 'service.heartbeat', { ready: true, diagnostics: [] });
    const liveCatalog = f.kernel.registry.liveCatalog(serviceNodeId);
    f.routing.update({ node: f.kernel.registry.node(serviceNodeId), generation: connected.generation, ...(liveCatalog ? { catalog: liveCatalog } : {}) });
    const client = { async request(method, params) {
      if (method !== 'tools.call') return execute(context, method, params);
      const call = f.routing.prepare(context, params); assert.equal(call.serviceNodeId, 'chat-a'); if (call.definition.name !== 'workspace') calls.push({ name: call.definition.name, args: structuredClone(call.arguments) });
      if (call.definition.name === 'notify') await beforeNotify?.();
      const value = await bridge.invoke(call.definition.name, call.arguments, { callerPrincipalId: call.callerPrincipalId, generation: f.first.native.owner.generation,
        operationId: call.operationId, signal: new AbortController().signal });
      const response = f.routing.complete(call, value);
      if (lose && call.definition.name === 'notify') { lose = false; throw new IvyError('outcome_unknown', 'Injected lost committed notify response.', 'unknown'); }
      return response;
    } };
    return { client, owner: { serviceNodeId, generation: connected.generation }, signal: new AbortController().signal };
  };
  const settleMain = async () => {
    for (let turn = 0; turn < 8; turn++) {
      const main = await f.first.queue.main(), ticket = main.value.queue[0]; if (!ticket) return;
      const operation = await f.first.operations.find(ticket.identity.senderPrincipalId, (await f.first.store.read('chat-bridge/input', ticket.inputId)).value.operationId);
      if (operation?.value.request.action !== 'notify') return;
      const delivery = new ChatDelivery(f.first); for (let i = 0; i < 6; i++) await delivery.step();
      const input = await f.first.store.read('chat-bridge/input', ticket.inputId); assert.ok(input.value.turnId);
      f.reply(frame => frame.method === 'thread/turns/list' ? { result: { data: [{ id: input.value.turnId, status: 'completed', items: [], itemsView: 'notLoaded' }], nextCursor: null } }
        : frame.method === 'thread/items/list' ? { result: { data: [{ turnId: input.value.turnId, item: { id: 'main-answer', type: 'agentMessage', text: '[[Service] Secretary] Main forwarded the service message.' } }], nextCursor: null } } : undefined);
      const collector = new ChatCollector(f.first), publisher = new ChatPublisher(collector);
      for (let i = 0; i < 8; i++) await collector.step(ticket.inputId);
      for (let i = 0; i < 16 && (await f.first.queue.main()).value.queue[0]?.inputId === ticket.inputId; i++) await publisher.step();
    }
  };
  const offer = async () => {
    await settleMain();
    return bridge.outbox.receive('alice', { action: 'receive', operationId: randomUUID(), expectedBridge: f.admission.expected('alice'), channel: f.channel, afterSequence: 0, limit: 8 });
  };
  const acknowledge = async () => {
    const offered = await offer(); assert.equal(offered.outcome?.action, 'receive');
    assert.ok(offered.outcome.replies.length > 0, JSON.stringify({ calls, outcome: offered.outcome }));
    await bridge.outbox.acknowledge('alice', { action: 'acknowledge', operationId: randomUUID(), expectedBridge: f.admission.expected('alice'), offerId: offered.outcome.offer.object.objectId, replyIds: offered.outcome.replies.map(reply => reply.object.objectId) });
    return offered.outcome;
  };
  return { ...f, bridge, calls, connect, acknowledge, offer, target: { serviceNodeId: 'chat-a', expectedBridge: f.admission.expected('chat-owner', 'notice'), channel: f.channel,
    notifyDefinitionHash: toolDefinitionHash(chatTools.find(tool => tool.name === 'notify')), noticeDefinitionHash: toolDefinitionHash(chatTools.find(tool => tool.name === 'notice')) },
    lose: () => { lose = true; }, beforeNotify: fn => { beforeNotify = fn; } };
}
