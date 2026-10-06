import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';
import { PassThrough } from 'node:stream';
import type { TestContext } from 'node:test';
import { digest, hashJson } from '../../packages/contracts/src/canonical.js';
import { operationId as fullOperationId } from '../../packages/contracts/src/operation-id.js';
import { IvyError } from '../../packages/contracts/src/errors.js';
import type { Agent, Chat, OperationName, Params, Result, Wire } from '../../packages/contracts/src/generated.js';
import { HiveKernel } from '../../services/hive/src/kernel.js';
import type { ConnectionContext } from '../../services/hive/src/kernel.js';
import { LiveRouting } from '../../services/hive/src/live-routing.js';
import type { PreparedCall } from '../../services/hive/src/live-routing.js';
import type { RpcClient } from '../../packages/sdk/src/client.js';
import { agentRegistry } from '../../services/agent-manager/src/registry.js';
import { NativeJournal } from '../../services/agent-manager/src/journal.js';
import { NativeRpc } from '../../services/agent-manager/src/rpc.js';
import { NativeInteractions } from '../../services/agent-manager/src/interactions.js';
import { ChatAdmission } from '../../services/chat-bridge/src/admission.js';
import { chatContracts } from '../../services/chat-bridge/src/schema.js';
import { ChatStore } from '../../services/chat-bridge/src/store.js';
import { ChatOperations } from '../../services/chat-bridge/src/operations.js';
import { ChatMain } from '../../services/chat-bridge/src/chat-main.js';
import { ChatNativeDriver } from '../../services/chat-bridge/src/native-driver.js';
import { chatNativeCatalog } from '../../services/chat-bridge/src/native-evidence.js';
import { saveNativePlan } from '../../packages/sdk/src/native-plan.js';

/** Real Hive/SQLite, registry/schema boundaries and native outcome journal; simulated native stdio. */
export async function chatNativeFixture(t: TestContext, version: Chat.NativePlan['nativeVersion'] = '0.154.0', configure?: (definition: Omit<Chat.Definition, 'nativePlan'> & { nativePlan: Agent.PlanDraft }) => void) {
  const source = chatNativeCatalog(version), catalog = source.catalog, credentialDigest = digest(randomUUID());
  const kernel = new HiveKernel({ filename: ':memory:', publicBaseUrl: 'https://chat.test/ivy', version: 'test', buildId: digest('fixture'),
    credentials: [{ principalId: 'chat-owner', digest: credentialDigest }] });
  const routing = new LiveRouting(true);
  const root = mkdtempSync(join(tmpdir(), 'ivy-chat-native-'));
  const stores: ChatStore[] = [];
  const journal = new NativeJournal({ serviceNodeId: 'native-agent', hostId: 'fixture', nativeVersion: version, nativeExecutableHash: catalog.nativeExecutableHash },
    { maxOperations: 100, maxJournalBytes: 64 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 });
  let epoch = randomUUID(), held: string | null = null, loseReply: boolean | string = false, unavailable = false, absence: 'generic' | 'owner' | null = null;
  let afterWrite: ((params: Params<'objects.write'>, result: Result<'objects.write'>) => void | Promise<void>) | null = null;
  let beforeNative: ((call: PreparedCall) => Promise<void>) | null = null;
  let reply: Agent.Reply | ((frame: Record<string, Wire.Json>) => Agent.Reply | undefined) | null = null;
  let threadStatus: Wire.Json = { type: 'idle' };
  const frames: Record<string, Wire.Json>[] = [], methods: string[] = [], accesses: string[] = [];
  const mutationIds = new Map<string, string>();
  const normalizeMutation = <M extends OperationName>(method: M, params: Params<M>): Params<M> => {
    if (!params || typeof params !== 'object' || !('mutationId' in params) || typeof params.mutationId !== 'string') return params;
    let id = mutationIds.get(params.mutationId);
    if (!id) { id = fullOperationId(kernel.store.runtimeEpoch, Date.now(), randomUUID()); mutationIds.set(params.mutationId, id); }
    return { ...params, mutationId: id } as Params<M>;
  };
  let input: PassThrough, output: PassThrough, rpc: NativeRpc, interactions:NativeInteractions;
  const launch = () => {
    input = new PassThrough(); output = new PassThrough(); journal.beginEpoch(epoch);
    rpc = new NativeRpc(catalog, input, output, { onClose: code => journal.loseEpoch(epoch, code), onRequest: () => undefined, onNotification: () => undefined });
    interactions=new NativeInteractions(rpc,epoch);
    input.on('data', (bytes: Buffer) => {
      const frame = JSON.parse(bytes.toString('utf8')) as Record<string, Wire.Json>; frames.push(frame); if (held === frame['method']) return;
      if (frame['method'] === 'thread/resume') threadStatus = { type: 'idle' };
      const params = frame['params'] as Record<string, Wire.Json>, id = String(params['threadId'] ?? 'native-main-' + frames.length);
      const thread = { cliVersion: version, createdAt: 1788690000, cwd: '/fixture', ephemeral: false, id, modelProvider: 'openai', preview: 'Chat fixture',
        projectId: null, sessionId: id, source: 'appServer', status: threadStatus, turns: [], updatedAt: 1788690000, canAcceptDirectInput: true, name: 'Fixture' };
      const result = frame['method'] === 'account/read' ? { requiresOpenaiAuth: false, account: null } : frame['method'] === 'turn/interrupt' ? {} : frame['method'] === 'thread/read' ? { thread } : frame['method'] === 'turn/start' ? { turn: { id: 'native-turn-' + frames.length, status: 'inProgress', items: [] } }
        : { thread, cwd: '/fixture', model: 'fixture-model', modelProvider: 'openai', approvalPolicy: 'on-request', approvalsReviewer: 'user', sandbox: { type: 'readOnly' }, reasoningEffort: 'high' };
      output.write(JSON.stringify({ id: frame['id'], ...((typeof reply === 'function' ? reply(frame) : reply) ?? { result }) }) + '\n');
    });
  };
  launch();
  t.after(() => { rpc.close('fixture_stop'); input.destroy(); output.destroy(); for (const store of stores) store.close(); journal.close(); kernel.close();
    assert.ok(relative(tmpdir(), root).startsWith('ivy-chat-native-')); rmSync(root, { recursive: true, force: true }); });
  const projects: Agent.ProjectsResult = { source: 'native', observedAt: new Date().toISOString(),
    projects: [{ nativeId: 'fixture-project', source: 'native', name: 'Fixture', paths: ['/fixture'] }] };
  const channel: Chat.WhatsAppChannelConfiguration['channel'] = { adapter: 'whatsapp', accountId: 'synthetic', channelId: 'browser' };
  const draft: Omit<Chat.Definition, 'nativePlan'> & { nativePlan: Agent.PlanDraft } = { workspaceId: 'synthetic-chat', principalId: 'chat-owner', rootObjectId: null,
    project: { serviceNodeId: 'native-agent', namespace: 'codex', kind: 'project', nativeId: 'fixture-project' },
    nativePlan: { nativeVersion: version, catalogSourceHash: catalog.sourceHash,
      definitions: { threadStart: hashJson(source.definitions.get('thread/start')), threadResume: hashJson(source.definitions.get('thread/resume')),
        turnStart: hashJson(source.definitions.get('turn/start')), turnInterrupt: hashJson(source.definitions.get('turn/interrupt')) },
      threadStart: { cwd: '/fixture' }, threadResume: {}, turnStart: {} },
    channels: [{ channel, displayName: 'Synthetic browser' }] };
  configure?.(draft);
  const execute = <M extends OperationName>(context: ConnectionContext, method: M, params: Params<M>): Result<M> => {
    const result = kernel.execute(context, { jsonrpc: '2.0', id: randomUUID(), method, params });
    assert.equal(result.kind, 'result'); if (result.kind !== 'result') throw Error('Expected fixture Hive result.'); return result.value as Result<M>;
  };
  const client = (node: string, agent = false) => {
    const initial: ConnectionContext = { credentialDigest, principalId: 'chat-owner', transport: 'ws' };
    const connected = execute(initial, 'service.connect', { serviceNodeId: node, serviceName: agent ? 'agent-manager' : 'chat-bridge', hostId: 'fixture',
      version: 'test', buildId: digest(node), hiveProtocol: 1, ...(agent ? { nativeVersion: version } : {}) });
    const context = { ...initial, serviceNodeId: node, generation: connected.generation }, callerContext = { ...initial, principalId: 'chat-owner' }, contracts = chatContracts();
    execute(context, 'registry.sync', agent ? agentRegistry(catalog) : { namespaces: [], contracts, requiredContracts: contracts.map(value => ({ key: value.key, readVersions: [value.version], writeVersions: [value.version] })) });
    execute(context, 'service.heartbeat', { ready: true, diagnostics: [] });
    const liveCatalog = kernel.registry.liveCatalog(node); routing.update({ node: kernel.registry.node(node), generation: connected.generation, ...(liveCatalog ? { catalog: liveCatalog } : {}) });
    const rpcClient: RpcClient = { async request<M extends OperationName>(method: M, params: Params<M>): Promise<Result<M>> {
      accesses.push(method);
      if (method === 'tools.call') {
        const call = routing.prepare(callerContext, params as unknown as Parameters<LiveRouting['prepare']>[1]), args = call.arguments as Record<string, Wire.Json>; methods.push(call.definition.namespace + '.' + call.definition.name);
        assert.equal(call.serviceNodeId, 'native-agent'); if (unavailable) throw new IvyError('service_not_ready', 'Fixture native offline.');
        let value: unknown;
        if (call.definition.namespace === 'agent' && call.definition.name !== 'invoke') {
          if (call.definition.name === 'status') value = { serviceNodeId: 'native-agent', hostId: 'fixture', nativeVersion: version, nativeExecutableHash: catalog.nativeExecutableHash,
            catalogHash: hashJson(catalog), epoch, pid: null, state: 'ready', observedAt: new Date().toISOString(), code: null, initialized: {}, pendingInputs: 0,
            operations: journal.status(), observedMethods: journal.observedMethods() } satisfies Agent.Status;
          else if (call.definition.name === 'interact') {
            const request=call.arguments as Agent.PreventInput;
            value=await interactions.run(call.callerPrincipalId,request.operationId,request.method,request.params);
            if(loseReply === true || loseReply === request.method){loseReply=false;throw new IvyError('outcome_unknown','Lost interactive reply.','unknown');}
          }
          else if (call.definition.name === 'interaction') value=await interactions.lookup(call.callerPrincipalId,String(args['operationId']));
          else if (call.definition.name === 'projects') value = projects;
          else if (call.definition.name === 'frameLimits') value = { serviceNodeId: 'native-agent', nativeVersion: version, nativeExecutableHash: catalog.nativeExecutableHash,
            catalogHash: hashJson(catalog), epoch, requestFrameBytes: 6291456, receivedFrameBytes: 25165824, answerFrameBytes: 4194304, managementFrameBytes: 33554432 } satisfies Agent.FrameLimits;
          else if (call.definition.name === 'read') {
            const request = call.arguments as Agent.ReadInput; let requestId: Agent.RequestId = '';
            const response = await rpc.request(request.method, request.params, { beforeSend: id => { requestId = id; } });
            value = { schemaVersion: 1, observationId: randomUUID(), serviceNodeId: 'native-agent', callerPrincipalId: call.callerPrincipalId,
              nativeExecutableHash: catalog.nativeExecutableHash, catalogHash: hashJson(catalog), epoch, requestId,
              requestHash: hashJson({ method: request.method, params: request.params }), observedAt: new Date().toISOString(), ...request, reply: response } satisfies Agent.ReadObservation;
          }
          else if (call.definition.name === 'prevent') {
            const request = call.arguments as Agent.PreventInput, definition = source.definitions.get(request.method);
            assert.equal(request.nativeVersion, version); assert.ok(definition); assert.equal(request.expectedDefinitionHash, hashJson(definition));
            value = journal.prevent({ callerPrincipalId: call.callerPrincipalId, operationId: request.operationId }, request.method, request.params);
          }
          else if (call.definition.name === 'operation') {
            if (absence === 'generic') throw new IvyError('not_found', 'Generic routing absence.');
            const operationId = String(args['operationId']); value = absence === 'owner' ? null : journal.get({ callerPrincipalId: call.callerPrincipalId, operationId });
            if (!value) throw new IvyError('not_found', 'Exact native owner absence.', 'not_executed', { kind: 'agent_operation_absent', operationId, serviceNodeId: 'native-agent', epoch });
          } else throw Error('Unexpected Chat fixture management call.');
        } else {
const invoke = call.definition.namespace === 'agent' && call.definition.name === 'invoke';
          const method = invoke ? String(args['method']) : call.definition.name, nativeArgs = invoke ? args['params']! : call.arguments;
                    await beforeNative?.(invoke ? { ...call, definition: source.definitions.get(method)!, arguments: nativeArgs } : call); assert.ok(call.operationId);
          const key = { callerPrincipalId: call.callerPrincipalId, operationId: call.operationId! }, accepted = journal.accept(key, method, nativeArgs);
          if (accepted.created) {
            const request = rpc.request(method, nativeArgs, { beforeSend: id => { journal.dispatch(key, epoch, id); }, beforeResolve: (id, response) => { journal.finish(key, epoch, id, response); } });
            if (held === method) { void request.catch(() => undefined); throw new IvyError('outcome_unknown', 'Fixture lost connection during native dispatch.', 'unknown'); }
            await request;
          }
          if (loseReply === true || loseReply === method) { loseReply = false; throw new IvyError('outcome_unknown', 'Lost original successful native tool response.', 'unknown'); }
          const original = journal.get(key)!;
          if (!original.reply || !('result' in original.reply)) throw new IvyError(original.code ?? 'native_operation_pending', 'Original native error.', 'unknown');
          value = invoke ? original : original.reply.result;
        }
        return routing.complete(call, value) as Result<M>;
      }
      const value = execute(context, method, normalizeMutation(method, params));
      if (method === 'registry.sync' || method === 'service.heartbeat') { const liveCatalog = kernel.registry.liveCatalog(node); routing.update({ node: kernel.registry.node(node), ...(liveCatalog ? { catalog: liveCatalog } : {}) }); }
      if (method === 'objects.write') await afterWrite?.(params as Params<'objects.write'>, value as Result<'objects.write'>); return value;
    } };
    return { context, client: rpcClient };
  };
  const agent = client('native-agent', true), first = client('chat-a'), second = client('chat-b');
  const issuedAt = Date.now(), actionIds = new Map<string, string>();
  const operation = (label: string) => { let value = actionIds.get(label); if (!value) { value = fullOperationId(kernel.store.runtimeEpoch, issuedAt, digest(label).slice(7)); actionIds.set(label, value); } return value; };
  const definition: Chat.Definition = { ...draft, nativePlan: await saveNativePlan(first.client, source.contract,
    { draft: draft.nativePlan, kind: 'chat', parentId: null, mutationId: 'fixture-plan' }) }, admission = new ChatAdmission(definition);
  const main = (connection: typeof first) => { const store = new ChatStore(connection.client, null, root); stores.push(store); const operations = new ChatOperations(store, admission);
    return new ChatMain(operations, new ChatNativeDriver(store, admission, { serviceNodeId: connection.context.serviceNodeId!, generation: connection.context.generation! })); };
  const request = (label: string, expectedMainRevision: number | null = null, caller = 'alice'): Chat.CreateMainRequest => ({ action: 'createMain',
    operationId: operation(label), expectedMainRevision, expectedBridge: admission.expected(caller), reason: 'Explicit isolated Main creation.' });
  return { first: main(first), second: main(second), clients: { first, second, agent }, kernel, routing, journal, frames, methods, accesses, projects, admission, request, operation, channel, dataRoot: root,
    intercept: (callback: typeof afterWrite) => { afterWrite = callback; }, beforeNative: (callback: typeof beforeNative) => { beforeNative = callback; },
    reply: (value: typeof reply) => { reply = value; }, hold: (method = 'thread/start') => { held = method; }, loseReply: (method?: string) => { loseReply = method ?? true; },
    threadStatus: (value: Wire.Json) => { threadStatus = value; },
    absence: (value: typeof absence) => { absence = value; }, unavailable: (value: boolean) => { unavailable = value; },
    restart: () => { rpc.close('fixture_restart'); input.destroy(); output.destroy(); epoch = randomUUID(); launch(); } };
}
