import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { join, relative, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { createServer } from 'node:net';
import { PassThrough } from 'node:stream';
import { randomUUID } from 'node:crypto';
import { digest, hashJson } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Agent, Chat, Host, Wire } from '../packages/contracts/src/generated.js';
import { HiveServer } from '../services/hive/src/server.js';
import { HiveClient, discover, callBound, newOperationId } from '../packages/sdk/src/client.js';
import { atomicJson } from '../packages/host-runtime/src/config.js';
import { NativeRpc } from '../services/agent-manager/src/rpc.js';
import { chatNativeCatalog } from '../services/chat-bridge/src/native-evidence.js';
import { ChatBridge } from '../services/chat-bridge/src/bridge.js';
import { startAgentManager } from '../services/agent-manager/src/main.js';
import type { NativeLauncher } from '../services/agent-manager/src/main.js';
import { startChatBridge } from '../services/chat-bridge/src/main.js';
import { chatNativeFixture } from './fixtures/chat-native.js';
import { until } from './fixtures/host.js';
import { saveNativePlan } from '../packages/sdk/src/native-plan.js';

test('Chat API admits every Hive caller while refusing stale generations and changed configurations before content access', async t => {
  const f = await chatNativeFixture(t), bridge = new ChatBridge(f.first), context = { callerPrincipalId: 'alice', generation: f.first.native.owner.generation, signal: new AbortController().signal, operationId: await newOperationId(f.first.store.client) };
  for (const [name, args, changed, code] of [
    ['workspace', {}, { generation: context.generation + 1 }, 'stale_generation'],
    ['operation', { operationId: context.operationId, expectedBridge: { ...f.admission.expected('alice'), workspaceId: 'other' } }, {}, 'chat_definition_mismatch'],
  ] as const) {
    f.accesses.length = 0;
    await assert.rejects(bridge.invoke(name, args, { ...context, ...changed }), (error: unknown) => error instanceof IvyError && error.code === code);
    assert.deepEqual(f.accesses, []);
  }
  assert.equal((await bridge.invoke('workspace', {}, { ...context, callerPrincipalId: 'unknown' }) as unknown as Chat.WorkspaceInfo).expectedBridge.callerPrincipalId, 'unknown');
  f.unavailable(true);
  const workspace = await bridge.workspace('alice'); assert.equal(workspace.nativeOwnerReady, false); assert.equal(workspace.main, null);
  assert.deepEqual(workspace.channels, f.admission.channelsFor('alice'));
});

test('actual ChatBridge and AgentManager services retain late replies, caller outcomes and acknowledgements through restart and native outage', { timeout: 180000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-chat-service-')), cleanups: (() => Promise<unknown>)[] = [];
  t.after(async () => { for (const cleanup of cleanups.reverse()) await cleanup(); assert.ok(relative(tmpdir(), root).startsWith('ivy-chat-service-')); await rm(root, { recursive: true, force: true }); });
  const listener = createServer(); await new Promise<void>(done => listener.listen(0, '127.0.0.1', done));
  const port = (listener.address() as { port: number }).port; await new Promise<void>(done => listener.close(() => done()));
  const base = `http://127.0.0.1:${port}/ivy`, build = JSON.parse(await readFile('dist/build-info.json', 'utf8')) as { version: string; buildId: string };
  const source = chatNativeCatalog('0.154.0'), catalog = source.catalog;
  const hive = new HiveServer({ filename: join(root, 'hive.sqlite'), publicBaseUrl: base, listenHost: '127.0.0.1', listenPort: port,
    version: 'test', buildId: digest('chat-service-hive'), credentials: ['alice', 'bob', 'outsider', 'chat-owner', 'agent-owner'].map(principalId => ({ principalId, digest: digest(principalId + '-fixture-token') })) });
  await hive.start(); cleanups.push(() => hive.close());
  const common = { schemaVersion: 1 as const, hostId: 'fixture-host', publicBaseUrl: base, version: build.version, buildId: build.buildId, artifactRoot: resolve('.') };
  const projectPath = join(root, 'project'); await mkdir(projectPath, { recursive: true });
  const nativeSettings: Agent.Settings = { nativeExecutable: process.execPath, nativeHome: join(root, 'agent', 'native'), nativeVersion: '0.154.0', nativeExecutableHash: catalog.nativeExecutableHash, limits: { maxOperations: 100, maxJournalBytes: 64 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 } };
  const agentPath = join(root, 'agent.json'); await atomicJson(agentPath, { ...common, componentId: 'agent-manager', instanceId: 'agent', serviceNodeId: 'native-agent', dataRoot: join(root, 'agent'), credential: 'agent-owner-fixture-token', settings: nativeSettings });
  const frames: Record<string, Wire.Json>[] = []; let attached = false, completed = false, turnStarted = false, turnPolls=0;
  let notifyCompletion=()=>{};
  const replyText = 'Original delayed answer. ' + '😀'.repeat(16385);
  const thread = () => ({ cliVersion: catalog.version, createdAt: 1788690000, cwd: projectPath, ephemeral: false, id: 'native-primary', modelProvider: 'openai', preview: 'Chat service fixture',
    projectId: null, sessionId: 'native-primary', source: 'appServer', status: turnStarted && !completed ? { type: 'active', activeFlags: [] } : { type: 'idle' }, turns: [], updatedAt: 1788690000, canAcceptDirectInput: true, name: 'Fixture' });
  const turn = () => ({ id: 'native-turn', status: completed ? 'completed' : 'inProgress', items: [] });
  const launcher: NativeLauncher = async options => {
    const epoch = randomUUID(), input = new PassThrough(), output = new PassThrough(); options.beginEpoch(epoch);
    notifyCompletion=()=>options.onNotification(epoch,{method:'turn/completed',params:{threadId:'native-primary',turn:turn()}});
    const rpc = new NativeRpc(catalog, input, output, { onClose: code => options.onClose(epoch, code), onRequest: request => options.onRequest(epoch, request), onNotification: value => options.onNotification(epoch, value) });
    input.on('data', (bytes: Buffer) => {
      const frame = JSON.parse(bytes.toString('utf8')) as Record<string, Wire.Json>; frames.push(frame);
      const params = frame['params'] as Record<string, Wire.Json>; let result: Wire.Json;
      switch (frame['method']) {
        case 'thread/loaded/list': result = { data: attached ? ['native-primary'] : [], nextCursor: null }; break;
        case 'thread/list': result = { data: attached && !params['archived'] ? [thread()] : [], nextCursor: null }; break;
        case 'project/list': result = { data: [{ id: 'fixture-project', name: 'Fixture', roots: [{ path: projectPath }], metadata: {}, position: 0, createdAt: 1788690000, updatedAt: 1788690000 }], nextCursor: null }; break;
        case 'thread/start': case 'thread/resume': attached = true; result = { thread: thread(), cwd: projectPath, model: 'fixture-model', modelProvider: 'openai', approvalPolicy: 'on-request', approvalsReviewer: 'user', sandbox: { type: 'readOnly' }, reasoningEffort: 'high' }; break;
        case 'turn/start': turnStarted = true; result = { turn: turn() }; break;
        case 'thread/read': result = { thread: thread() }; break;
        case 'thread/turns/list': turnPolls++;result = { data: turnStarted ? [{ ...turn(), itemsView: 'notLoaded' }] : [], nextCursor: null }; break;
        case 'thread/items/list': result = { data: [{ turnId: 'native-turn', item: { id: 'answer', type: 'agentMessage', text: replyText } }], nextCursor: null }; break;
        default: output.write(JSON.stringify({ id: frame['id'], error: { code: -32601, message: 'Unsupported fixture request.' } }) + '\n'); return;
      }
      output.write(JSON.stringify({ id: frame['id'], result }) + '\n');
    });
    return { rpc, epoch, catalog, initialized: { codexHome: nativeSettings.nativeHome! }, launcherPid: null, close: async () => { rpc.close('fixture_stop'); input.destroy(); output.destroy(); } };
  };
  const agent = await startAgentManager(agentPath, launcher); cleanups.push(() => agent.close()); await agent.service.waitReady();
  const channel: Chat.WhatsAppChannelConfiguration['channel'] = { adapter: 'whatsapp', accountId: 'fixture', channelId: 'browser' };
  const ownerClient = new HiveClient(base, { credential: 'chat-owner-fixture-token' });
  const settings: Chat.Settings = { pollMs: 60000, definition: { workspaceId: 'fixture-chat', principalId: 'chat-owner', rootObjectId: null,
    project: { serviceNodeId: 'native-agent', namespace: 'codex', kind: 'project', nativeId: 'fixture-project' },
    nativePlan: await saveNativePlan(ownerClient, source.contract, { kind: 'chat', parentId: null, mutationId: await newOperationId(ownerClient), draft: { nativeVersion: '0.154.0', catalogSourceHash: catalog.sourceHash, definitions: {
      threadStart: hashJson(source.definitions.get('thread/start')), threadResume: hashJson(source.definitions.get('thread/resume')),
      turnStart: hashJson(source.definitions.get('turn/start')), turnInterrupt: hashJson(source.definitions.get('turn/interrupt')) },
    threadStart: { cwd: projectPath }, threadResume: {}, turnStart: {} } }), channels: [{ channel, displayName: 'Browser' }] } };
  const path = join(root, 'chat.json'), config: Host.InstanceConfig = { ...common, componentId: 'chat-bridge', instanceId: 'chat', serviceNodeId: 'chat', dataRoot: join(root, 'chat'), credential: 'chat-owner-fixture-token', settings: settings as unknown as Record<string, Wire.Json> };
  await atomicJson(path, config);
  let chat = await startChatBridge(path); cleanups.push(() => chat.close()); await chat.service.waitReady();
  const alice = new HiveClient(base, { credential: 'alice-fixture-token' }), bob = new HiveClient(base, { credential: 'bob-fixture-token' }), outsider = new HiveClient(base, { credential: 'outsider-fixture-token' });
  const invoke = async (name: string, args: unknown, client = alice): Promise<Wire.Json> => {
    const actionId = (args as { operationId?: unknown })?.operationId;
    return callBound(client, await discover(client, 'chat.' + name, { serviceNodeId: 'chat' }), args as Wire.Json,
      typeof actionId === 'string' ? actionId : await newOperationId(client));
  };
  const workspace = () => invoke('workspace', {}) as Promise<Chat.WorkspaceInfo>;
  assert.equal((await alice.request('tools.list', { namespace: 'chat', serviceNodeId: 'chat' })).items.length, 12);
  assert.equal((await invoke('workspace', {}, outsider) as Chat.WorkspaceInfo).expectedBridge.callerPrincipalId, 'outsider');
  const initial = await workspace(); assert.equal(initial.main, null); assert.equal(initial.nativeOwnerReady, true);
  const oldRuntime = chat.runtime!, oldGeneration = chat.service.connection.generation;
  chat.service.connection.close();
  await until(() => chat.service.ready && chat.service.connection.generation > oldGeneration, 15000);
  await assert.rejects(oldRuntime.main.store.named('chat-bridge/main', 'closed-generation'), /not open|closed/i);
  assert.equal((await workspace()).nativeOwnerReady, true);
  const create: Chat.CreateMainRequest = { action: 'createMain', operationId: await newOperationId(alice, 'original-main'), expectedBridge: initial.expectedBridge, expectedMainRevision: null, reason: 'Explicit isolated test Main.' };
  const created = await invoke('createMain', create) as Chat.Operation;
  assert.equal(created.phase, 'succeeded'); if (created.outcome?.action !== 'createMain') throw Error('Expected original Main binding.');
  const send: Chat.SendRequest = { action: 'send', operationId: await newOperationId(alice, 'original-input'), messageId: 'browser-message', channel, expectedBridge: initial.expectedBridge,
    expectedBinding: created.outcome.binding.object, payload: { text: 'Wait for the original delayed reply.', images: [] } };
  const sent = await invoke('send', send) as Chat.Operation; if (sent.outcome?.action !== 'send') throw Error('Expected original input.');
  const inputId = sent.outcome.input.object.objectId;
  await until(() => turnStarted, 45000);
  const generation = chat.service.connection.generation;
  const closing = chat.close(); assert.equal(chat.close(), closing); await closing;
  const pollsBeforeRestart=turnPolls;
  chat = await startChatBridge(path); await chat.service.waitReady(); assert.ok(chat.service.connection.generation > generation);
  await until(()=>turnPolls>pollsBeforeRestart,15000);
  completed=true;notifyCompletion();
  assert.deepEqual(await invoke('send', send), sent);
  assert.deepEqual(await invoke('operation', { expectedBridge: initial.expectedBridge, operationId: send.operationId }), sent);
  const bobWorkspace = await invoke('workspace', {}, bob) as Chat.WorkspaceInfo;
  await assert.rejects(invoke('operation', { expectedBridge: bobWorkspace.expectedBridge, operationId: send.operationId }, bob), (error: unknown) => error instanceof IvyError && error.code === 'not_found');
  const readInput = () => invoke('input', { expectedBridge: initial.expectedBridge, inputId }) as Promise<Chat.InputView>;
  await until(async () => (await workspace()).main?.data.queue.length === 0, 15000);
  assert.equal((await readInput()).data.state, 'completed');
  const receive: Chat.ReceiveRequest = { action: 'receive', operationId: await newOperationId(alice, 'original-offer'), expectedBridge: initial.expectedBridge, channel, afterSequence: 0, limit: 8 };
  const received = await invoke('receive', receive) as Chat.Operation; if (received.outcome?.action !== 'receive') throw Error('Expected saved offer.');
  assert.equal(received.outcome.replies.length, 2); assert.equal(received.outcome.replies.map(reply => reply.data.text).join(''), replyText);
  assert.ok(received.outcome.replies.every(reply => reply.data.state === 'pending' && reply.data.origin.kind === 'native' && reply.data.origin.inputId === inputId));
  assert.deepEqual(received.outcome.replies.map(reply => reply.object), received.outcome.offer.data.replies);
  const offeredHistory = await invoke('history', { expectedBridge: initial.expectedBridge, channel, afterSequence: 0, limit: 8 }) as Chat.HistoryPage;
  assert.ok(offeredHistory.entries.slice(1).every(reply => reply.data.state === 'outcome_unknown'));
  const ack: Chat.AcknowledgeRequest = { action: 'acknowledge', operationId: await newOperationId(alice, 'original-ack'), expectedBridge: initial.expectedBridge,
    offerId: received.outcome.offer.object.objectId, replyIds: received.outcome.replies.map(reply => reply.object.objectId) };
  const acknowledged = await invoke('acknowledge', ack) as Chat.Operation; assert.equal(acknowledged.phase, 'succeeded');
  await agent.close();
  assert.equal((await workspace()).nativeOwnerReady, false); assert.equal(chat.service.ready, true);
  await chat.close(); chat = await startChatBridge(path); await chat.service.waitReady();
  assert.deepEqual(await invoke('receive', receive), received); assert.deepEqual(await invoke('acknowledge', ack), acknowledged);
  const history = await invoke('history', { expectedBridge: initial.expectedBridge, channel, afterSequence: 0, limit: 8 }) as Chat.HistoryPage;
  assert.equal(history.entries.length, 3); assert.ok(history.entries.slice(1).every(reply => reply.data.state === 'confirmed'));
  const queued = await invoke('send', { ...send, operationId: await newOperationId(alice, 'queued-offline'), messageId: 'offline-message' }) as Chat.Operation;
  if (queued.outcome?.action !== 'send') throw Error('Expected offline queued input.');
  const cancelTarget = await invoke('input', { expectedBridge: initial.expectedBridge, inputId: queued.outcome.input.object.objectId }) as Chat.InputView;
  const cancel: Chat.CancelRequest = { action: 'cancel', operationId: await newOperationId(alice, 'cancel-offline'), expectedBridge: initial.expectedBridge,
    inputId: cancelTarget.object.objectId, expectedRevision: cancelTarget.object.revision, reason: 'Cancel before native preparation.' };
  assert.equal((await invoke('cancel', cancel) as Chat.Operation).phase, 'succeeded');
  assert.equal((await invoke('input', { expectedBridge: initial.expectedBridge, inputId: cancel.inputId }) as Chat.InputView).data.state, 'cancelled');
  assert.equal((await workspace()).main?.data.queue.length, 0);
  assert.equal(frames.filter(frame => frame['method'] === 'thread/start').length, 1);
  assert.equal(frames.filter(frame => frame['method'] === 'turn/start').length, 1);
  const health = JSON.parse(await readFile(join(config.dataRoot, 'health.json'), 'utf8')) as Host.Health;
  assert.equal(health.ready, true); assert.equal(JSON.stringify(health).includes(send.payload.text), false);
});
