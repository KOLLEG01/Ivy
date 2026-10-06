import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { digest } from '../packages/contracts/src/canonical.js';
import { operationId as fullOperationId } from '../packages/contracts/src/operation-id.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Chat, OperationName, Params, Result, TaskBoard } from '../packages/contracts/src/generated.js';
import type { RpcClient } from '../packages/sdk/src/client.js';
import type { ConnectionContext } from '../services/hive/src/kernel.js';
import { chatNativeFixture } from './fixtures/chat-native.js';
import { content, fields } from './fixtures/task-board.js';
import { ChatBridge } from '../services/chat-bridge/src/bridge.js';
import { chatRegistry } from '../services/chat-bridge/src/registry.js';
import { ChatDelivery } from '../services/chat-bridge/src/delivery.js';
import { ChatCollector } from '../services/chat-bridge/src/collector.js';
import { ChatPublisher } from '../services/chat-bridge/src/publisher.js';
import { TaskBoardStore } from '../services/task-board/src/runtime/store.js';
import { taskBoardContracts } from '../services/task-board/src/runtime/schema.js';
import { TaskBoardEngine } from '../services/task-board/src/runtime/engine.js';
import { TaskBoardDeliveries } from '../services/task-board/src/runtime/deliveries.js';
import { TaskBoardReconciler } from '../services/task-board/src/runtime/reconciler.js';

async function fixture(t: Parameters<typeof chatNativeFixture>[0], configured = true) {
  const f = await chatNativeFixture(t, '0.154.0');
  const mainOperationId = fullOperationId(f.kernel.store.runtimeEpoch, Date.now(), randomUUID());
  await f.first.store.validateOperationId(mainOperationId);
  await f.first.create('alice', f.request(mainOperationId));
  await f.clients.first.client.request('registry.sync', chatRegistry());
  await f.clients.first.client.request('service.heartbeat', { ready: true, diagnostics: [] });
  const bridge = new ChatBridge(f.first), calls: { name: string; args: unknown }[] = [];
  let lose = false, beforeNotify: (() => Promise<void>) | null = null;
  const execute = <M extends OperationName>(context: ConnectionContext, method: M, params: Params<M>): Result<M> => {
    const result = f.kernel.execute(context, { jsonrpc: '2.0', id: randomUUID(), method, params });
    assert.equal(result.kind, 'result'); if (result.kind !== 'result') throw Error('Expected direct Hive result.'); return result.value as Result<M>;
  };
  const connect = (id: string) => {
    const initial = { credentialDigest: f.clients.first.context.credentialDigest, principalId: 'chat-owner', transport: 'ws' as const };
    const connected = execute(initial, 'service.connect', { serviceNodeId: id, serviceName: 'task-board', hostId: 'fixture', version: 'test', buildId: digest(id), hiveProtocol: 1 });
    const context = { ...initial, serviceNodeId: id, generation: connected.generation }, callerContext = { ...initial, principalId: 'chat-owner' }, contracts = taskBoardContracts();
    execute(context, 'registry.sync', { namespaces: [], contracts, requiredContracts: contracts.map(contract => ({ key: contract.key, readVersions: [contract.version], writeVersions: [contract.version] })) });
    execute(context, 'service.heartbeat', { ready: true, diagnostics: [] });
    const client: RpcClient = { async request<M extends OperationName>(method: M, params: Params<M>): Promise<Result<M>> {
      if (method !== 'tools.call') return execute(context, method, params);
      const call = f.routing.prepare(callerContext, params as unknown as Parameters<typeof f.routing.prepare>[1]);
      if(call.serviceNodeId==='native-agent')return f.clients.first.client.request(method,params);
      if (call.definition.name !== 'workspace') calls.push({ name: call.definition.name, args: structuredClone(call.arguments) });
      assert.equal(call.callerPrincipalId, 'chat-owner'); assert.equal(call.serviceNodeId, 'chat-a');
      if (call.definition.name === 'notify') await beforeNotify?.();
      const value = await bridge.invoke(call.definition.name as 'notice' | 'notify', call.arguments as never,
        { callerPrincipalId: call.callerPrincipalId, generation: f.first.native.owner.generation, ...(call.operationId ? { operationId: call.operationId } : {}), signal: new AbortController().signal });
      const response = f.routing.complete(call, value);
      if (lose && call.definition.name === 'notify') { lose = false; throw new IvyError('outcome_unknown', 'Lost committed notify response.', 'unknown'); }
      return response as Result<M>;
    } };
    return new TaskBoardEngine(new TaskBoardStore(client, null), settings, { serviceNodeId: id, generation: connected.generation });
  };
  const target: TaskBoard.ChatTarget = { serviceNodeId: 'chat-a', expectedBridge: f.admission.expected('chat-owner', 'notice'), channel: f.channel };
  const settings: TaskBoard.Settings = { principalId: 'chat-owner', rootObjectId: null,
    scheduler: { enabled: false, intervalMs: 1000, pageSize: 2 }, phoneTarget: null };
  const engine = connect('notice-producer'), second = connect('notice-producer-two');
  if (!configured) await f.clients.first.client.request('service.heartbeat', { ready: false, diagnostics: [] });
  const invoke = (request: TaskBoard.ActionInput) => engine.invoke(request, { callerPrincipalId: 'alice', operationId: request.operationId, generation: engine.owner.generation, signal: new AbortController().signal });
  const final = async (taskId?: string) => {
    const task = taskId ? await engine.store.read('task-board/task', taskId) : null;
    const pin = task?.pin ?? (await invoke({ action: 'create', operationId: randomUUID(), fields: { ...fields, title: 'Review original result', userContact: 'chat' } })).task!;
    const saved = await invoke({ action: 'saveResult', operationId: randomUUID(), taskId: pin.objectId, expectedRevision: pin.revision, run: null, kind: 'final', content });
    const current = await engine.store.read('task-board/task', saved.task!), delivery = current.value.comments.at(-1)?.delivery; assert.ok(delivery);
    return { task: current, delivery, result: saved.result! };
  };
  const settleMain = async () => {
    for (let turn = 0; turn < 8; turn++) {
      const main = await f.first.queue.main(), ticket = main.value.queue[0]; if (!ticket) return;
      const delivery = new ChatDelivery(f.first); for (let i = 0; i < 6; i++) await delivery.step();
      const input = await f.first.store.read('chat-bridge/input', ticket.inputId); assert.ok(input.value.turnId);
      f.reply(frame => frame['method'] === 'thread/turns/list' ? { result: { data: [{ id: input.value.turnId!, status: 'completed', items: [], itemsView: 'notLoaded' }], nextCursor: null } }
        : frame['method'] === 'thread/items/list' ? { result: { data: [{ turnId: input.value.turnId!, item: { id: 'main-answer', type: 'agentMessage', text: '[[Automation] TaskBoard] Ready for review: Review original result' } }], nextCursor: null } } : undefined);
      const collector = new ChatCollector(f.first), publisher = new ChatPublisher(collector);
      for (let i = 0; i < 8; i++) await collector.step(ticket.inputId);
      for (let i = 0; i < 16 && (await f.first.queue.main()).value.queue[0]?.inputId === ticket.inputId; i++) await publisher.step();
    }
  };
  const acknowledge = async () => {
    await settleMain();
    const offered = await bridge.outbox.receive('alice', { action: 'receive', operationId: randomUUID(), expectedBridge: f.admission.expected('alice'), channel: f.channel, afterSequence: 0, limit: 8 });
    assert.ok(offered.outcome?.action === 'receive'); assert.ok(offered.outcome.replies.length);
    await bridge.outbox.acknowledge('alice', { action: 'acknowledge', operationId: randomUUID(), expectedBridge: f.admission.expected('alice'),
      offerId: offered.outcome.offer.object.objectId, replyIds: offered.outcome.replies.map(reply => reply.object.objectId) });
    return offered.outcome;
  };
  return { ...f, bridge, calls, settings, target, engine, second, final, acknowledge, invoke, lose: () => { lose = true; }, beforeNotify: (fn: typeof beforeNotify) => { beforeNotify = fn; } };
}

test('TaskBoard saves the original comment delivery before dispatch and recovers a lost real ChatBridge response before browser confirmation', async t => {
  const f = await fixture(t), saved = await f.final(), worker = new TaskBoardDeliveries(f.engine);
  await worker.step(saved.delivery.objectId); assert.equal(f.calls.length, 0);
  const prepared = await f.engine.store.read('task-board/delivery', saved.delivery.objectId);
  assert.equal(prepared.pin.revision, 2); assert.equal(prepared.value.state, 'dispatching'); assert.ok(prepared.value.chatRequest);
  assert.deepEqual(prepared.value.chatRequest.source, saved.task.value.lastHistory);
  f.beforeNotify(async () => { assert.deepEqual((await f.second.store.read('task-board/delivery', saved.delivery.objectId)).value.chatRequest, prepared.value.chatRequest); });
  f.lose(); await worker.step(saved.delivery.objectId);
  assert.equal((await f.engine.store.read('task-board/delivery', saved.delivery.objectId)).value.state, 'outcome_unknown');
  await worker.step(saved.delivery.objectId);
  await worker.step(saved.delivery.objectId);
  assert.ok(f.calls.filter(call => call.name === 'notify').length >= 1, 'Retrying the same retained operation never creates another delivery identity.');
  const waiting = await f.engine.store.read('task-board/delivery', saved.delivery.objectId); assert.equal(waiting.value.state, 'dispatching');
  t.mock.timers.enable({ apis: ['Date'], now: Date.now() }); t.mock.timers.tick(1000);
  await worker.step(saved.delivery.objectId); assert.deepEqual((await f.engine.store.read('task-board/delivery', saved.delivery.objectId)).pin, waiting.pin, 'Idle polls do not create repeated revisions.');
  await f.acknowledge(); await worker.step(saved.delivery.objectId);
  const confirmed = await f.engine.store.read('task-board/delivery', saved.delivery.objectId); assert.equal(confirmed.value.state, 'confirmed'); assert.ok(confirmed.value.deliveryEvidence);
  const reply = await f.first.store.read('chat-bridge/reply', confirmed.value.deliveryEvidence); assert.equal(reply.value.state, 'confirmed');
  assert.equal(reply.value.text, '[[Automation] TaskBoard] Ready for review: Review original result');
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1, 'The comment handoff is written in exactly one turn of the existing Main.');
  await f.engine.store.collectLocal(Date.now() + 2 * 24 * 60 * 60 * 1000);
  await worker.step(saved.delivery.objectId);
});

test('repeated unavailable delivery polls retain the original uncertainty without new writes', async t => {
  const f = await fixture(t), saved = await f.final(), worker = new TaskBoardDeliveries(f.engine);
  await worker.step(saved.delivery.objectId);
  f.kernel.registry.disconnect('chat-a');
  await worker.step(saved.delivery.objectId);
  const uncertain = await f.engine.store.read('task-board/delivery', saved.delivery.objectId);
  assert.equal(uncertain.value.state, 'outcome_unknown');
  assert.equal(uncertain.value.code, 'task_board_chat_unavailable');
  t.mock.timers.enable({ apis: ['Date'], now: Date.now() }); t.mock.timers.tick(1000);
  await worker.step(saved.delivery.objectId);
  assert.deepEqual((await f.engine.store.read('task-board/delivery', saved.delivery.objectId)).pin, uncertain.pin);
  assert.equal(f.calls.length, 0);
});

test('the current result comment is delivered and acceptance completes its Task', async t => {
  const f = await fixture(t), item = await f.final(), worker = new TaskBoardDeliveries(f.engine);
  await worker.step(item.delivery.objectId); await worker.step(item.delivery.objectId); await worker.step(item.delivery.objectId);
  const offered = await f.acknowledge(); assert.equal(offered.replies.length, 1);
  await worker.step(item.delivery.objectId); assert.equal((await f.engine.store.read('task-board/delivery', item.delivery.objectId)).value.state, 'confirmed');
  const current = await f.engine.store.read('task-board/task', item.task.pin.objectId);
  const accepted = await f.invoke({ action: 'review', operationId: randomUUID(), taskId: current.pin.objectId, expectedRevision: current.pin.revision, result: item.result, acceptance: 'Accepted manually.' });
  assert.equal((await f.engine.store.read('task-board/task', accepted.task!)).value.workflowState, 'done');
});

test('pending delivery survives Hive retention and Task archival', async t => {
  const f = await fixture(t), saved = await f.final(), worker = new TaskBoardDeliveries(f.engine);
  await worker.step(saved.delivery.objectId);
  f.kernel.retention.collect(); f.kernel.retention.collect();
  await assert.rejects(f.engine.store.read('task-board/delivery', saved.delivery),
    (error: unknown) => error instanceof IvyError && error.code === 'revision_pruned');
  const operationId = randomUUID();
  await f.engine.archive({ action: 'archive', operationId, taskId: saved.task.pin.objectId,
    expectedRevision: saved.task.pin.revision, archived: true },
  { callerPrincipalId: 'alice', operationId, generation: f.engine.owner.generation, signal: new AbortController().signal });
  await worker.step(saved.delivery.objectId);
  assert.ok(f.calls.some(call => call.name === 'notify'));
});

test('a compatible ChatBridge definition update does not strand a pending delivery', async t => {
  const f = await fixture(t), saved = await f.final(), worker = new TaskBoardDeliveries(f.engine);
  await worker.step(saved.delivery.objectId);
  const registry = structuredClone(chatRegistry());
  const notice = registry.namespaces[0]!.tools.find(tool => tool.name === 'notice');
  assert.ok(notice);
  notice.description += ' Updated provider guidance.';
  await f.clients.first.client.request('registry.sync', registry);
  await f.clients.first.client.request('service.heartbeat', { ready: true, diagnostics: [] });
  await worker.step(saved.delivery.objectId);
  assert.ok(f.calls.some(call => call.name === 'notify'));
});

test('unavailable Main stays pending and every saved comment can continue after configuration becomes available', async t => {
  const f = await fixture(t, false), saved = [await f.final(), await f.final()];
  // Missing configuration is not dispatch uncertainty and does not repeatedly mutate records.
  for (const item of saved) await assert.rejects(new TaskBoardDeliveries(f.engine).step(item.delivery.objectId), (error: unknown) => error instanceof IvyError && ['chat_main_unavailable', 'service_unavailable', 'service_not_ready'].includes(error.code));
  assert.equal(f.calls.length, 0);
  for (const item of saved) { const delivery = await f.engine.store.read('task-board/delivery', item.delivery.objectId); assert.equal(delivery.pin.revision, 1); }
  await f.clients.first.client.request('service.heartbeat', { ready: true, diagnostics: [] });
  const worker = new TaskBoardDeliveries(f.engine);
  for (const item of saved) { await worker.step(item.delivery.objectId); await worker.step(item.delivery.objectId); await worker.step(item.delivery.objectId); }
  assert.ok(f.calls.filter(call => call.name === 'notify').length >= 2);
  const offered = await f.acknowledge(); assert.equal(offered.replies.length, 2);
});

test('changed delivery identity is refused before contacting the channel', async t => {
  const f = await fixture(t), saved = await f.final(), original = await f.engine.store.read('task-board/delivery', saved.delivery);
  await f.engine.store.write('task-board/delivery', { ...original.value, commentId: 'changed-comment' }, randomUUID(), { objectId: original.pin.objectId, expectedRevision: original.pin.revision });
  await assert.rejects(new TaskBoardDeliveries(f.engine).step(original.pin.objectId), (error: unknown) => error instanceof IvyError && error.code === 'task_board_delivery_mismatch');
  assert.equal(f.calls.length, 0);
});
