import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, mkdir } from 'node:fs/promises';
import { join, relative, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { createServer } from 'node:net';
import { PassThrough } from 'node:stream';
import { randomUUID } from 'node:crypto';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import type { Transport } from '@modelcontextprotocol/client';
import { HiveServer } from '../services/hive/src/server.js';
import { HiveClient, discover, callBound, newOperationId } from '../packages/sdk/src/client.js';
import { digest } from '../packages/contracts/src/canonical.js';
import type { Agent, Host, TaskBoard, Wire } from '../packages/contracts/src/generated.js';
import { atomicJson } from '../packages/host-runtime/src/config.js';
import { NativeRpc } from '../services/agent-manager/src/rpc.js';
import { startAgentManager } from '../services/agent-manager/src/main.js';
import type { NativeLauncher } from '../services/agent-manager/src/main.js';
import { startTaskBoard } from '../services/task-board/src/main.js';
import { fields } from './fixtures/task-board.js';
import { until } from './fixtures/host.js';

test('actual services recover a pending native question and read one bounded result through real Hive transports', { timeout: 180000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-task-board-native-service-')), cleanups: (() => Promise<unknown>)[] = [];
  t.after(async () => { for (const cleanup of cleanups.reverse()) await cleanup(); assert.ok(relative(tmpdir(), root).startsWith('ivy-task-board-native-service-')); await rm(root, { recursive: true, force: true }); });
  const listener = createServer(); await new Promise<void>(resolve => listener.listen(0, '127.0.0.1', resolve));
  const port = (listener.address() as { port: number }).port; await new Promise<void>(resolve => listener.close(() => resolve()));
  const base = `http://127.0.0.1:${port}/ivy`, build = JSON.parse(await readFile('dist/build-info.json', 'utf8')) as { version: string; buildId: string };
  const catalog = JSON.parse(await readFile('specs/native/codex-0.154.0/catalog.json', 'utf8')) as Agent.Catalog;
  const hive = new HiveServer({ filename: join(root, 'hive.sqlite'), publicBaseUrl: base, listenHost: '127.0.0.1', listenPort: port, version: 'test', buildId: digest('native-task-board-hive'),
    credentials: ['user', 'task-board-owner', 'agent-owner'].map(principalId => ({ principalId, digest: digest(principalId + '-fixture-token') })) });
  await hive.start(); cleanups.push(() => hive.close());
  const common = { schemaVersion: 1 as const, hostId: 'fixture-host', publicBaseUrl: base, version: build.version, buildId: build.buildId, artifactRoot: resolve('.') };
  const nativeSettings: Agent.Settings = { nativeExecutable: process.execPath, nativeHome: join(root, 'agent', 'native'), nativeVersion: catalog.version as '0.154.0', nativeExecutableHash: catalog.nativeExecutableHash,
    projectRoot: join(root, 'projects'), internalProjectRoot: join(root, 'task-workspaces'), capabilities: [],
    limits: { maxOperations: 100, maxJournalBytes: 64 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 } };
  await mkdir(nativeSettings.internalProjectRoot!, { recursive: true });
  const agentConfig: Host.InstanceConfig = { ...common, componentId: 'agent-manager', instanceId: 'agent', serviceNodeId: 'native-agent', dataRoot: join(root, 'agent'), credential: 'agent-owner-fixture-token', settings: nativeSettings };
  const agentPath = join(root, 'agent.json'); await atomicJson(agentPath, agentConfig);
  const frames: Record<string, Wire.Json>[] = []; let attached = false, completed = false, awaitingInput = false, nativeActive = false;
  let askNativeQuestion: (() => void) | null = null;
  const thread = () => ({ cliVersion: catalog.version, createdAt: 1788690000, cwd: nativeSettings.internalProjectRoot!, ephemeral: false, id: 'native-primary', modelProvider: 'openai', preview: 'Large service fixture',
    projectId: 'fixture-internal-project', sessionId: 'native-primary', source: 'appServer', status: nativeActive ? { type: 'active', activeFlags: awaitingInput ? ['waitingOnUserInput'] : [] } : { type: 'idle' }, turns: [], updatedAt: 1788690000, canAcceptDirectInput: true, name: 'Fixture' });
  const turn = () => ({ id: 'native-turn', status: completed ? 'completed' : 'inProgress', items: [] });
  const launcher: NativeLauncher = async options => {
    const epoch = randomUUID(), input = new PassThrough(), output = new PassThrough(); options.beginEpoch(epoch);
    const rpc = new NativeRpc(catalog, input, output, { onClose: code => options.onClose(epoch, code), onRequest: request => options.onRequest(epoch, request), onNotification: value => options.onNotification(epoch, value) });
    askNativeQuestion = () => {
      awaitingInput = true;
      output.write(JSON.stringify({ id: 'fixture-native-question', method: 'item/tool/requestUserInput', params: {
        threadId: 'native-primary', turnId: 'native-turn', itemId: 'question-item', isBlocking: true,
        questions: [{ id: 'choice', header: 'Fixture', question: 'Continue the isolated fixture?', options: [{ label: 'Proceed', description: 'Finish this simulated native result.' }] }],
      } }) + '\n');
    };
    input.on('data', (bytes: Buffer) => {
      const frame = JSON.parse(bytes.toString('utf8')) as Record<string, Wire.Json>; frames.push(frame);
      if (frame['id'] === 'fixture-native-question' && !frame['method']) {
        assert.deepEqual(frame['result'], { answers: { choice: { answers: ['Proceed'] } } });
        completed = true; awaitingInput = false; nativeActive = false;
        output.write(JSON.stringify({ method: 'serverRequest/resolved', params: { threadId: 'native-primary', requestId: 'fixture-native-question' } }) + '\n'); return;
      }
      const params = frame['params'] as Record<string, Wire.Json>; let result: Wire.Json;
      switch (frame['method']) {
        case 'model/list': result = { data: [{ id: 'fixture-model', model: 'fixture-model', displayName: 'Fixture model', description: 'Isolated service catalog',
          hidden: false, isDefault: true, defaultReasoningEffort: 'high', supportedReasoningEfforts: [{ reasoningEffort: 'high', description: 'High' }] }], nextCursor: null }; break;
        case 'thread/loaded/list': result = { data: attached ? ['native-primary'] : [], nextCursor: null }; break;
        case 'thread/list': result = { data: attached && !params['archived'] ? [thread()] : [], nextCursor: null }; break;
        case 'project/list': result = { data: [{ id: 'fixture-internal-project', name: 'Internal tasks', roots: [{ path: nativeSettings.internalProjectRoot! }], metadata: {}, position: 0, recencyAt: 1788690000000, createdAt: 1788690000, updatedAt: 1788690000 }], nextCursor: null }; break;
        case 'thread/start': case 'thread/resume': attached = true; result = { thread: thread(), cwd: nativeSettings.internalProjectRoot!, model: 'fixture-model', modelProvider: 'openai', approvalPolicy: 'on-request', approvalsReviewer: 'user', sandbox: { type: 'readOnly' }, reasoningEffort: 'high' }; break;
        case 'turn/start': result = { turn: turn() }; nativeActive = true; break;
        case 'thread/read': result = { thread: thread() }; break;
        case 'thread/turns/list': result = { data: [{ ...turn(), itemsView: 'notLoaded' }], nextCursor: null }; break;
        case 'thread/items/list':
          assert.equal(params['limit'], 1); assert.equal(params['sortDirection'], 'desc');
          // Older output would overflow a transport frame if completion requested another page.
          assert.equal(params['cursor'], null);
          result = { data: [{ turnId: 'native-turn', item: { id: 'large-message', type: 'agentMessage', phase: 'final_answer', text: 'Large native output. ' + 'x'.repeat(2 * 1024 * 1024) } }], nextCursor: 'oversized-history' }; break;
        default: output.write(JSON.stringify({ id: frame['id'], error: { code: -32601, message: 'Unsupported fixture request.' } }) + '\n'); return;
      }
      output.write(JSON.stringify({ id: frame['id'], result }) + '\n');
    });
    return { rpc, epoch, catalog, initialized: { codexHome: nativeSettings.nativeHome! }, launcherPid: null, close: async () => { rpc.close('fixture_stop'); input.destroy(); output.destroy(); } };
  };
  const agent = await startAgentManager(agentPath, launcher); cleanups.push(() => agent.close()); await agent.service.waitReady();
  const settings: TaskBoard.Settings = { principalId: 'task-board-owner', rootObjectId: null, scheduler: { enabled: true, intervalMs: 1000, pageSize: 10 },  phoneTarget: null };
  const taskBoardPath = join(root, 'task-board.json'); await atomicJson(taskBoardPath, { ...common, componentId: 'task-board', instanceId: 'task-board', serviceNodeId: 'task-board', dataRoot: join(root, 'task-board'), credential: 'task-board-owner-fixture-token', settings });
  let taskBoard = await startTaskBoard(taskBoardPath); cleanups.push(() => taskBoard.close()); await taskBoard.service.waitReady(); await until(() => taskBoard.runtime?.recovered === true);
  const client = new HiveClient(base, { credential: 'user-fixture-token' });
  const invoke = async (request: TaskBoard.ActionInput) => await callBound(client, await discover(client, 'task-board.' + request.action, { serviceNodeId: 'task-board' }), request as unknown as Wire.Json, request.operationId) as TaskBoard.ActionOutcome;
  const mcp = new Client({ name: 'task-board-validation-test', version: '1' });
  await mcp.connect(new StreamableHTTPClientTransport(new URL(base + '/mcp'), {
    requestInit: { headers: { Authorization: 'Bearer user-fixture-token' } },
  }) as unknown as Transport);
  cleanups.push(() => mcp.close());
  const taskFields = { ...fields, control: 'agent' as const,
    nativeOptions: { model: 'fixture-model', reasoningEffort: 'high', serviceTier: 'standard' as const } };
  for (const nativeOptions of [{ ...taskFields.nativeOptions, model: 'sol-6' }, { ...taskFields.nativeOptions, reasoningEffort: 'max' }]) {
    const rejected = await mcp.callTool({ name: 'task_create', arguments: { serviceNodeId: 'task-board',
      input: { action: 'create', operationId: randomUUID(), fields: { ...taskFields, nativeOptions } } } });
    assert.equal(rejected.isError, true);
    assert.match(JSON.stringify(rejected.content), /task_board_model_unavailable/);
  }
  assert.equal((await callBound(client, await discover(client, 'task-board.list', { serviceNodeId: 'task-board' }), {}) as TaskBoard.TaskListResult).items.length, 0);
  const definitions: TaskBoard.NativePlan['definitions'] = { threadStart: '', threadResume: '', turnStart: '', turnInterrupt: '' };
  for (const [method, key] of [['thread/start', 'threadStart'], ['thread/resume', 'threadResume'], ['turn/start', 'turnStart'], ['turn/interrupt', 'turnInterrupt']] as const)
    definitions[key] = (await discover(client, 'codex.' + method, { serviceNodeId: 'native-agent' })).definitionHash;
  const plan = await invoke({ action: 'savePlan', operationId: randomUUID(), plan: { nativeVersion: '0.154.0', catalogSourceHash: catalog.sourceHash as TaskBoard.NativePlanDraft['catalogSourceHash'], definitions,
    threadStart: {}, threadResume: {}, turnStart: { input: [{ type: 'text', text: 'Produce the isolated native fixture result.', text_elements: [] }] } } as TaskBoard.NativePlanDraft });
  const accepted = await mcp.callTool({ name: 'task_create', arguments: { serviceNodeId: 'task-board',
    input: { action: 'create', operationId: randomUUID(), fields: taskFields } } });
  assert.notEqual(accepted.isError, true);
  const created = (accepted.structuredContent as { result: TaskBoard.ActionOutcome }).result;
  const rejectedEdit = await mcp.callTool({ name: 'task_update', arguments: { serviceNodeId: 'task-board', input: {
    action: 'edit', operationId: randomUUID(), taskId: created.task!.objectId, expectedRevision: created.task!.revision,
    fields: { ...taskFields, nativeOptions: { ...taskFields.nativeOptions, model: 'sol-6' } },
  } } });
  assert.equal(rejectedEdit.isError, true);
  assert.match(JSON.stringify(rejectedEdit.content), /task_board_model_unavailable/);
  assert.equal((await client.request('objects.stat', { objectId: created.task!.objectId })).currentRevision, created.task!.revision);
  await invoke({ action: 'transition', operationId: randomUUID(), taskId: created.task!.objectId, expectedRevision: created.task!.revision, workflowState: 'todo', detail: null });
  const task = async () => { const read = await client.request('objects.read', { objectId: created.task!.objectId }); assert.equal(read.content.encoding, 'json'); return (read.content as { encoding: 'json'; value: Wire.Json }).value as TaskBoard.Task; };
  const signals = async () => {
    const current = await task(); if (!current.lastRun) return null;
    return (await taskBoard.runtime?.engine.store.named('task-board/native-signals', 'Native signals', current.lastRun.objectId))?.value ?? null;
  };
  try { await until(async () => (await task()).claim?.phase === 'running', 60000); }
  catch (error) { t.diagnostic(JSON.stringify({ task: await task(), diagnostics: taskBoard.runtime?.diagnostics, frames: frames.map(value => value['method'] ?? 'reply') })); throw error; }
  const beforeContactEdit = await task();
  assert.equal(beforeContactEdit.workflowState, 'in_progress');
  assert.equal(beforeContactEdit.claim?.phase, 'running');
  assert.ok(beforeContactEdit.lastRun);
  assert.ok(beforeContactEdit.primaryResourceRef);
  const runBeforeContactEdit = await client.request('objects.read', { objectId: beforeContactEdit.lastRun.objectId });
  const contactEdit = await mcp.callTool({ name: 'task_update', arguments: { serviceNodeId: 'task-board', input: {
    action: 'edit', operationId: randomUUID(), taskId: created.task!.objectId,
    expectedRevision: (await client.request('objects.stat', { objectId: created.task!.objectId })).currentRevision,
    fields: { ...beforeContactEdit.fields, userContact: 'chat' },
  } } });
  assert.notEqual(contactEdit.isError, true, JSON.stringify(contactEdit.content));
  const afterContactEdit = await task();
  assert.equal(afterContactEdit.fields.userContact, 'chat');
  assert.equal(afterContactEdit.workflowState, 'in_progress');
  for (const key of ['primaryResourceRef', 'attemptCount', 'waiting'] as const)
    assert.deepEqual(afterContactEdit[key], beforeContactEdit[key], key);
  assert.equal(afterContactEdit.lastRun!.objectId, beforeContactEdit.lastRun.objectId);
  assert.equal(afterContactEdit.claim!.operationId, beforeContactEdit.claim!.operationId);
  assert.equal(afterContactEdit.claim!.phase, 'running');
  const runAfterContactEdit = await client.request('objects.read', { objectId: beforeContactEdit.lastRun.objectId });
  const runBefore = (runBeforeContactEdit.content as { encoding: 'json'; value: Wire.Json }).value as TaskBoard.Run;
  const runAfter = (runAfterContactEdit.content as { encoding: 'json'; value: Wire.Json }).value as TaskBoard.Run;
  for (const key of ['operationId', 'attempt', 'primaryResourceRef', 'turnId', 'calls', 'cancellation'] as const)
    assert.deepEqual(runAfter[key], runBefore[key], key);
  assert.equal(frames.filter(frame => frame['method'] === 'turn/interrupt').length, 0);
  assert.ok(askNativeQuestion);
  (askNativeQuestion as () => void)();
  await until(async () => (await task()).waiting?.reason === 'user' && (await signals())?.inputs.length === 1, 60000);
  const beforeRestart = (await signals())!, priorGeneration = taskBoard.service.connection.generation;
  assert.equal(beforeRestart.inputs[0]!.identity.requestId, 'fixture-native-question');
  await taskBoard.close(); taskBoard = await startTaskBoard(taskBoardPath); await taskBoard.service.waitReady(); await until(() => taskBoard.runtime?.recovered === true, 60000);
  assert.ok(taskBoard.service.connection.generation > priorGeneration);
  const live = await callBound(client, await discover(client, 'agent.inputs', { serviceNodeId: 'native-agent' }), { identity: beforeRestart.inputs[0]!.identity }) as Agent.PendingInputPage;
  assert.equal(live.epoch, beforeRestart.epoch); assert.equal(live.items[0]!.state, 'pending');
  const answerId = await newOperationId(client), binding = await discover(client, 'agent.answer', { serviceNodeId: 'native-agent' });
  const answer = { operationId: answerId, identity: live.items[0]!.identity, reply: { result: { answers: { choice: { answers: ['Proceed'] } } } } };
  const answered = await callBound(client, binding, answer, answerId);
  assert.deepEqual(await callBound(client, binding, answer, answerId), answered);
  try { await until(async () => (await task()).workflowState === 'review', 60000); }
  catch (error) { t.diagnostic(JSON.stringify({ task: await task(), diagnostics: taskBoard.runtime?.diagnostics })); throw error; }
  const final = await task(), result = await client.request('objects.read', { objectId: final.latestResult!.objectId });
  assert.equal(result.content.encoding, 'json'); const material = (result.content as { encoding: 'json'; value: Wire.Json }).value as TaskBoard.Result;
  assert.ok(material.content.summary.startsWith('Large native output.')); assert.ok(Buffer.byteLength(material.content.summary) < 262144);
  assert.equal(material.content.artifacts.length, 0, 'technical completion evidence stays in the local workflow journal');
  const transcripts = await taskBoard.runtime!.engine.store.page('task-board/native-transcript', { where: { op: 'eq', field: 'object.parentId', value: final.lastRun!.objectId }, limit: 2 });
  assert.equal(transcripts.items.length, 0);
  assert.equal(frames.filter(frame => frame['method'] === 'thread/items/list').length, 1);
  for (const method of ['thread/start', 'turn/start']) assert.equal(frames.filter(frame => frame['method'] === method).length, 1);
  assert.equal(frames.filter(frame => frame['id'] === 'fixture-native-question' && !frame['method']).length, 1);
});
