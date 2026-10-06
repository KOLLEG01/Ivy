import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { hashJson, IvyError, validateAgent, ServiceClient, NativeContract } from '../dist/packages/sdk/src/node.js';
import { fixture, principal, assessment, message } from './secretary-fixture.mjs';
import { secretaryRegistry } from '../dist/services/secretary/src/schema.js';
import { triageRegistry } from '../dist/services/secretary/src/triage-schema.js';
import { SecretaryTriage } from '../dist/services/secretary/src/triage.js';
import { nativeTriageCatalog } from './native-owner-fixture.mjs';

export async function triageFixture(t, existingFixture, fallbackRpc) {
  const registry = () => { const base = secretaryRegistry(), extra = triageRegistry(); return { ...base, contracts: [...base.contracts, ...extra.contracts], requiredContracts: [...base.requiredContracts, ...extra.requiredContracts] }; };
  const f = existingFixture ?? await fixture(t, {}, registry);
  const target = { serviceNodeId: 'triage-native-fixture', hostId: 'native-fixture-host', nativeVersion: '0.154.0', nativeExecutableHash: hashJson('triage-executable'), catalogHash: hashJson('triage-catalog') };
  const catalog = nativeTriageCatalog(target.nativeVersion, target.nativeExecutableHash); target.catalogHash = hashJson(catalog); const contract = new NativeContract(catalog);
  const settings = { schemaVersion: 1, target, model: 'gpt-5.6-luna', effort: 'high', instructions: 'Wichtige persönliche Fragen kurz melden. Keine Werbung melden.', threadCwd: process.platform === 'win32' ? 'C:\\isolated\\triage' : '/isolated/triage', maximumConcurrent: 2 };
  const state = { epoch: 'triage-epoch-1', operations: new Map(), turns: new Map(), threads: new Map(), inputs: new Map(), dispatches: [], reads: [], result: assessment(), terminal: 'completed',
    phase: 'succeeded', cut: false, offline: false, absent: false, changedDefinition: false, staleOwner: false, lostReply: false, rewriteRead: null, after: null, mediaResolver: null, itemsSupported: false, extraItems: [] };
  let sequence = 0, threadSequence = 0, turnSequence = 0;
  const stamp = () => new Date(Date.parse('2026-09-07T09:00:00.000Z') + sequence++).toISOString();
  const binding = name => { const [namespace, ...parts] = name.split('.'), method = parts.join('.');
    const definition = namespace === 'codex' ? structuredClone(contract.definition(method)) : { namespace, name: method, interfaceVersion: '1.0.0', description: 'Controlled native triage fixture', inputSchema: { type: 'object', additionalProperties: true }, outputSchema: { type: 'object', additionalProperties: true } };
    if (state.changedDefinition && namespace === 'codex') definition.description += ' changed';
    return { qualifiedName: name, definition, definitionHash: hashJson(definition) }; };
  const rpc = { async request(method, args, options) {
    const receipt = method === 'tools.call' && args.qualifiedName === 'agent.invoke';
    if (receipt) args = { ...args, qualifiedName: 'codex.' + args.arguments.method, expectedDefinitionHash: args.arguments.expectedDefinitionHash, arguments: args.arguments.params };
    if (state.cut) throw new IvyError('outcome_unknown', 'Injected stopped triage caller.', 'unknown'); let value;
    if (method === 'tools.list' && ['agent', 'codex'].includes(args.namespace) && (!args.serviceNodeId || args.serviceNodeId === target.serviceNodeId)) {
      if (state.offline) throw new IvyError('service_not_ready', 'Native fixture is offline.');
      value = { items: [binding(args.namespace + '.' + args.namePrefix)], nextCursor: null, provider: { node: { serviceNodeId: target.serviceNodeId } } };
    } else if (method === 'tools.call' && args.serviceNodeId === target.serviceNodeId) {
      if (state.offline) throw new IvyError('service_not_ready', 'Native fixture is offline.'); const input = args.arguments, name = args.qualifiedName;
      if (args.expectedDefinitionHash !== binding(name).definitionHash) throw new IvyError('tool_definition_changed', 'The bound definition changed before dispatch.', 'not_executed');
      if (name === 'agent.status') {
        value = { ...target, state: 'ready', epoch: state.epoch, pid: 42, observedAt: stamp(), code: null, initialized: {}, pendingInputs: 0,
          operations: { retained: state.operations.size, maximum: 1000, bytes: 1000, maximumBytes: 1048576 }, observedMethods: [] }; validateAgent('Status', value);
      } else if (name === 'agent.resolveProject') {
        value = { project: { nativeId: 'secretary-assignment-fixture', source: 'native', name: input.selection.name, paths: [settings.threadCwd], kind: 'internal', position: 0, recencyAt: null }, cwd: settings.threadCwd, kind: 'internal' };
      } else if (name === 'agent.projects') {
        value = { source: 'native', observedAt: stamp(), projects: [{ nativeId: state.projectId ?? 'internal-project', source: 'native', name: 'Internal', paths: [settings.threadCwd] }] };
      } else if (name === 'agent.catalog') { value = catalog;
      } else if (name === 'agent.frameLimits') { value = { serviceNodeId: target.serviceNodeId, nativeVersion: target.nativeVersion,
        nativeExecutableHash: target.nativeExecutableHash, catalogHash: target.catalogHash, epoch: state.epoch,
        requestFrameBytes: 6291456, receivedFrameBytes: 25165824, answerFrameBytes: 4194304, managementFrameBytes: 33554432 };
      } else if (name === 'agent.operation') {
        value = state.operations.get(input.operationId);
        if (!value || state.absent) throw new IvyError('not_found', 'Original native operation is absent.', 'not_executed', { kind: 'agent_operation_absent', serviceNodeId: target.serviceNodeId, operationId: input.operationId, epoch: state.epoch });
      } else if (name === 'agent.read') {
        const full = input.params.itemsView === 'full' || input.method === 'thread/items/list', turnId = state.turns.get(input.params.threadId), thread = state.threads.get(input.params.threadId); assert.ok(thread);
        const turn = { id: turnId, status: state.terminal, itemsView: input.params.itemsView, error: null,
          items: full ? [{ id: 'commentary', type: 'agentMessage', phase: 'commentary', text: 'I am assessing the original observation.' },
            { id: 'final', type: 'agentMessage', phase: 'final_answer', text: JSON.stringify(state.result) },
            { id: 'user', type: 'userMessage', content: state.inputs.get(input.params.threadId) }, ...state.extraItems] : [] };
        const offset = Number(input.params.cursor ?? 0), itemReply = !state.itemsSupported ? { error: { code: -32601, message: 'Synthetic item pagination unavailable.' } }
          : { result: { data: turn.items.slice(offset, offset + input.params.limit).map(item => ({ turnId, item })), nextCursor: offset + input.params.limit < turn.items.length ? String(offset + input.params.limit) : null } };
        value = { schemaVersion: 1, observationId: randomUUID(), callerPrincipalId: principal, serviceNodeId: target.serviceNodeId, nativeExecutableHash: target.nativeExecutableHash, catalogHash: target.catalogHash,
          epoch: state.epoch, requestId: sequence++, observedAt: stamp(), requestHash: hashJson({ method: input.method, params: input.params }), nativeVersion: target.nativeVersion, method: input.method, params: input.params,
          reply: input.method === 'thread/items/list' ? itemReply : { result: input.method === 'thread/read' ? { thread } : { data: [turn], nextCursor: null } } };
        if (state.rewriteRead) value = state.rewriteRead(value); validateAgent('ReadObservation', value); state.reads.push(structuredClone(value));
      } else if (name.startsWith('codex.')) {
        const nativeMethod = name.slice(6), old = state.operations.get(args.operationId);
        if (old) { assert.equal(old.method, nativeMethod); assert.deepEqual(old.params, input); value = old.reply?.result ?? {}; }
        else {
          let result;
          if (nativeMethod === 'thread/start') {
            const thread = { id: 'triage-thread-' + ++threadSequence, ephemeral: false, canAcceptDirectInput: true, status: { type: 'idle' } }; state.threads.set(thread.id, thread); result = { thread, model: input.model };
          } else if (nativeMethod === 'thread/resume') { assert.ok(state.threads.has(input.threadId)); result = { thread: state.threads.get(input.threadId), model: input.model }; }
          else if (nativeMethod === 'turn/start') { assert.ok(state.threads.has(input.threadId)); const id = 'triage-turn-' + ++turnSequence; state.turns.set(input.threadId, id); state.inputs.set(input.threadId, structuredClone(input.input)); result = { turn: { id, status: 'inProgress', items: [] } }; }
          else { assert.equal(nativeMethod, 'thread/unsubscribe'); assert.ok(state.threads.has(input.threadId)); result = { status: 'unsubscribed' }; }
          const at = stamp(), phase = state.phase;
          const operation = { schemaVersion: 1, operationId: args.operationId, callerPrincipalId: principal, serviceNodeId: target.serviceNodeId, nativeVersion: target.nativeVersion, nativeExecutableHash: target.nativeExecutableHash,
            method: nativeMethod, params: structuredClone(input), requestHash: hashJson({ method: nativeMethod, params: input }), phase, createdAt: at, updatedAt: at,
            epoch: phase === 'accepted' ? null : state.epoch, requestId: phase === 'accepted' ? null : sequence++, reply: phase === 'succeeded' ? { result } : null,
            code: phase === 'outcome_unknown' ? 'native_connection_lost' : phase === 'failed' ? 'native_failed' : null };
          validateAgent('Operation', operation); state.operations.set(args.operationId, operation); state.dispatches.push(structuredClone(operation)); value = operation.reply?.result ?? {};
        }
        if (state.lostReply) throw new IvyError('outcome_unknown', 'Lost native triage reply.', 'unknown');
      } else throw Error('Unexpected native fixture method ' + name);
    } else {
      value = await (fallbackRpc ?? f.connection).request(method, args, options); if (method === 'serviceNodes.get' && state.staleOwner) value = { ...value, connected: false };
    }
    if (state.after) await state.after(method, args, value); return structuredClone(receipt ? state.operations.get(args.operationId) : value);
  } };
  await f.restart(rpc);
  return { f, state, settings, rpc, get worker() { return new SecretaryTriage(f.engine, settings, state.mediaResolver ?? undefined); },
    async startNativeTransport() {
      const methods = { agent: ['status', 'projects', 'resolveProject', 'catalog', 'frameLimits', 'operation', 'read', 'invoke'], codex: ['thread/start', 'thread/resume', 'turn/start', 'thread/unsubscribe'] };
      const service = new ServiceClient({ publicBaseUrl: process.env.IVY_TEST_HIVE_URL, credential: () => process.env.IVY_TEST_HIVE_CREDENTIAL,
        identity: { serviceNodeId: target.serviceNodeId, serviceName: 'agent-manager', hostId: target.hostId, version: 'fixture', buildId: hashJson(target), hiveProtocol: 1 },
        registry: () => ({ contracts: [], requiredContracts: [], namespaces: Object.entries(methods).map(([namespace, names]) => ({ namespace,
          description: 'Controlled native triage protocol fixture', guideMarkdown: 'Synthetic native replies; actual Hive routing.', topics: [], inventoryKinds: [],
          tools: names.map(name => binding(namespace + '.' + name).definition) })) }),
        handlers: Object.fromEntries(Object.entries(methods).flatMap(([namespace, names]) => names.map(name => {
          const qualifiedName = namespace + '.' + name, expectedDefinitionHash = binding(qualifiedName).definitionHash;
          return [qualifiedName, (args, context) => rpc.request('tools.call', { serviceNodeId: target.serviceNodeId, qualifiedName, expectedDefinitionHash, arguments: args, operationId: context.operationId })];
        }))), reconcile: async () => {}, heartbeatMs: 2000 });
      t.after(() => service.stop()); service.start(); await service.waitReady({ timeoutMs: 20000 }); return service;
    },
    async capture(patch = {}) { return (await f.engine.action(principal, f.captureRequest(message(patch)))).effect; },
    async restart() { state.cut = false; await f.restart(rpc); },
  };
}
