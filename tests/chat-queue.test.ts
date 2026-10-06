import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { canonical, digest, hashJson } from '../packages/contracts/src/canonical.js';
import { operationId as fullOperationId } from '../packages/contracts/src/operation-id.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Agent, Chat, OperationName, Params, Result, Wire } from '../packages/contracts/src/generated.js';
import type { RpcClient } from '../packages/sdk/src/client.js';
import { HiveKernel } from '../services/hive/src/kernel.js';
import type { ConnectionContext } from '../services/hive/src/kernel.js';
import { ChatAdmission } from '../services/chat-bridge/src/admission.js';
import { chatContracts } from '../services/chat-bridge/src/schema.js';
import { ChatStore } from '../services/chat-bridge/src/store.js';
import { ChatOperations } from '../services/chat-bridge/src/operations.js';
import { ChatQueue, mainName } from '../services/chat-bridge/src/queue.js';
import { chunkBytes, maximumEvidenceBytes, saveChatEvidence, readChatEvidence } from '../services/chat-bridge/src/evidence-bytes.js';
import { chatNativeCatalog, ChatNativeEvidence, verifyChatNativeRequest, verifyChatNativeOperation, verifyChatNativeRead } from '../services/chat-bridge/src/native-evidence.js';
import { saveNativePlan } from '../packages/sdk/src/native-plan.js';
import { mainTurnContext } from '../services/chat-bridge/src/main-context.js';

const channel: Chat.WhatsAppChannelConfiguration['channel'] = { adapter: 'whatsapp', accountId: 'synthetic', channelId: 'browser' };
const hasCode = (code: string) => (error: unknown) => error instanceof IvyError && error.code === code;
async function fixture(t: { after(callback: () => void): void }, nativeVersion: Chat.NativePlan['nativeVersion'] = '0.154.0') {
  const dataRoot=mkdtempSync(join(tmpdir(),'ivy-chat-queue-')),stores:ChatStore[]=[];
  const catalog = JSON.parse(readFileSync('specs/native/codex-' + nativeVersion + '/catalog.json', 'utf8')) as Agent.Catalog;
  const nativeHash = (method: string) => hashJson(chatNativeCatalog(nativeVersion).definitions.get(method));
  const draft: Omit<Chat.Definition, 'nativePlan'> & { nativePlan: Agent.PlanDraft } = { workspaceId: 'synthetic-chat', principalId: 'chat-owner', rootObjectId: null,
    project: { serviceNodeId: 'synthetic-agent', namespace: 'codex', kind: 'project', nativeId: '/synthetic/project' },
    nativePlan: { nativeVersion, catalogSourceHash: catalog.sourceHash, definitions: { threadStart: nativeHash('thread/start'), threadResume: nativeHash('thread/resume'), turnStart: nativeHash('turn/start'), turnInterrupt: nativeHash('turn/interrupt') }, threadStart: {}, threadResume: {}, turnStart: {} },
    channels: [{ channel, displayName: 'Synthetic browser' }] };
  const credentialDigest = digest(randomUUID());
  const kernel = new HiveKernel({ filename: ':memory:', publicBaseUrl: 'https://chat.test/ivy', version: 'test', buildId: digest('fixture'), credentials: [{ principalId: 'chat-owner', digest: credentialDigest }] });
  t.after(() => { for(const store of stores)store.close();kernel.close();rmSync(dataRoot,{recursive:true,force:true}); });
  let after: ((params: Params<'objects.write'>, result: Result<'objects.write'>) => void | Promise<void>) | null = null, accesses = 0;
  const mutationIds = new Map<string, string>();
  const retainedOperations = new Map<string, Agent.Operation>();
  const client = (node: string): RpcClient => {
    let context: ConnectionContext = { credentialDigest, principalId: 'chat-owner', transport: 'ws' };
    const execute = <M extends OperationName>(method: M, params: Params<M>): Result<M> => {
      if (params && typeof params === 'object' && 'mutationId' in params && typeof params.mutationId === 'string') {
        let id = mutationIds.get(params.mutationId);
        if (!id) { id = fullOperationId(kernel.store.runtimeEpoch, Date.now(), randomUUID()); mutationIds.set(params.mutationId, id); }
        params = { ...params, mutationId: id } as Params<M>;
      }
      const result = kernel.execute(context, { jsonrpc: '2.0', id: randomUUID(), method, params });
      assert.equal(result.kind, 'result'); if (result.kind !== 'result') throw Error('This persistence fixture performs no native dispatch.'); return result.value as Result<M>;
    };
    const connection = execute('service.connect', { serviceNodeId: node, serviceName: 'chat-bridge', hostId: 'fixture', version: 'test', buildId: digest(node), hiveProtocol: 1 });
    context = { ...context, serviceNodeId: node, generation: connection.generation };
    const contracts = chatContracts(); execute('registry.sync', { namespaces: [], contracts, requiredContracts: contracts.map(value => ({ key: value.key, readVersions: [value.version], writeVersions: [value.version] })) });
    execute('service.heartbeat', { ready: true, diagnostics: [] });
    return { async request<M extends OperationName>(method: M, params: Params<M>) { accesses++;
      if (method === 'tools.list') {
        const definition = { namespace: 'agent', name: 'operation', interfaceVersion: '1.0.0', description: 'Synthetic authoritative operation reader', inputSchema: {}, outputSchema: {} };
        return { items: [{ qualifiedName: 'agent.operation', definition, definitionHash: hashJson(definition) }], nextCursor: null, provider: { node: { serviceNodeId: 'synthetic-agent' } } } as unknown as Result<M>;
      }
      if (method === 'tools.call') {
        const call = params as Params<'tools.call'>; assert.equal(call.qualifiedName, 'agent.operation');
        const operation = retainedOperations.get(String((call.arguments as Record<string, unknown>)['operationId'])); assert.ok(operation);
        assert.equal(call.expectedCallerPrincipalId, operation.callerPrincipalId); return structuredClone(operation) as unknown as Result<M>;
      }
      const value = execute(method, params);
      if (method === 'objects.write') await after?.(params as Params<'objects.write'>, value as Result<'objects.write'>); return value; } };
  };
  const first = client('chat-a'), second = client('chat-b'), makeStore=(connection:RpcClient)=>{const value=new ChatStore(connection,null,dataRoot);stores.push(value);return value;},store = makeStore(first);
  const definition: Chat.Definition = { ...draft, nativePlan: await saveNativePlan(first, chatNativeCatalog(nativeVersion).contract,
    { draft: draft.nativePlan, kind: 'chat', parentId: null, mutationId: 'fixture-plan' }) }, admission = new ChatAdmission(definition);
  // A synthetic already-created native binding isolates persistence; it is not native acceptance.
  const at = new Date().toISOString(), binding: Chat.Binding = { schemaVersion: 1, workspaceId: definition.workspaceId, definitionHash: admission.definitionHash,
    project: definition.project, primary: { ...definition.project, kind: 'thread', nativeId: 'fixture-main-thread' }, nativePlan: definition.nativePlan,
    createdAt: at, operationId: 'fixture-main-creation', predecessor: null };
  const bindingPin = await store.write('chat-bridge/binding', binding, randomUUID(), { create: { parentId: null, name: 'Fixture binding' } });
  const conversation = await store.write('chat-bridge/conversation', { schemaVersion: 1, workspaceId: definition.workspaceId,
    definitionHash: admission.definitionHash, createdAt: at }, randomUUID(), { create: { parentId: null, name: 'Fixture conversation' } });
  await store.write('chat-bridge/main', { schemaVersion: 1, definition, definitionHash: admission.definitionHash, binding: bindingPin,
    conversation, pendingAction: null, publication: null, queue: [], nextSequence: 1, updatedAt: at }, randomUUID(), { create: { parentId: null, name: mainName(definition) } });
  const queue = (connection = first) => new ChatQueue(new ChatOperations(makeStore(connection), admission));
  const actionIds=new Map<string,string>(),operation=(label:string)=>{let value=actionIds.get(label);if(!value){value=fullOperationId(kernel.store.runtimeEpoch,Date.now(),digest(label).slice(7));actionIds.set(label,value);}return value;};
  const request = (label: string, messageId = label, caller = 'alice'): Chat.SendRequest => ({ action: 'send', operationId:operation(label),
    expectedBridge: admission.expected(caller), channel, messageId, expectedBinding: bindingPin, payload: { text: 'Synthetic message ' + messageId, images: [] } });
  return { first, second, store, makeStore, queue, request, operation, admission, bindingPin, retainedOperations, accesses: () => accesses, intercept: (callback: typeof after) => { after = callback; } };
}

test('concurrent ChatBridge instances preserve one input across operation retries and allocate ordered sequences to other callers', async t => {
  const f = await fixture(t), a = f.queue(), b = f.queue(f.second);
  const results = await Promise.all([a.send('alice', f.request('original', 'same')), b.send('alice', f.request('alias', 'same')),
    b.send('bob', f.request('bob', 'same', 'bob')), a.send('alice', f.request('separate'))]);
  assert.ok(results.every(value => value.phase === 'succeeded' && value.outcome?.action === 'send'));
  assert.deepEqual(results[0]!.outcome && 'input' in results[0]!.outcome ? results[0]!.outcome.input : null,
    results[1]!.outcome && 'input' in results[1]!.outcome ? results[1]!.outcome.input : null);
  const main = await a.main(); assert.equal(main.value.pendingAction, null); assert.equal(main.value.queue.length, 3);
  assert.deepEqual(main.value.queue.map(value => value.sequence), [1, 2, 3]); assert.equal(main.value.nextSequence, 4);
  assert.deepEqual(await b.send('alice', f.request('original', 'same')), results[0]);
  await assert.rejects(b.send('alice', { ...f.request('new-alias', 'same'), payload: { text: 'Changed original content', images: [] } }), hasCode('mutation_conflict'));
  await assert.rejects(b.send('alice', { ...f.request('original', 'same'), messageId: 'changed' }), hasCode('mutation_conflict'));
});

test('ChatBridge recovers the retained input commit without replacing message, time, sequence or binding', async t => {
  const f = await fixture(t);
  for (const boundary of ['input'] as const) {
    const request = f.request('lost-' + boundary); let lost = false;
    const before = (await f.queue().main()).value.nextSequence;
    f.intercept((params, result) => {
      if (lost || params.content.encoding !== 'json') return;
      const value = params.content.value as Record<string, unknown>, key = result.object.contractKey;
      const matches = key === 'chat-bridge/input';
      if (matches) { lost = true; throw new IvyError('outcome_unknown', 'Synthetic connection loss after SQLite commit.', 'unknown'); }
    });
    await assert.rejects(f.queue().send('alice', request), hasCode('outcome_unknown')); assert.equal(lost, true); f.intercept(null);
    const replacement = f.queue(f.second), outcome = await replacement.send('alice', request);
    await replacement.recoverPending();
    assert.equal(outcome.phase, 'succeeded'); assert.equal(outcome.outcome?.action, 'send');
    if (outcome.outcome?.action !== 'send') throw Error('Expected original send receipt.');
    assert.equal(outcome.outcome.input.data.admittedAt, outcome.createdAt); assert.equal(outcome.outcome.input.data.sequence, before);
    assert.deepEqual(outcome.outcome.input.data.binding, f.bindingPin);
    const main = await replacement.main(); assert.equal(main.value.pendingAction, null); assert.equal(main.value.nextSequence, before + 1);
    assert.equal(main.value.queue.filter(ticket => ticket.identity.messageId === request.messageId).length, 1);
    assert.deepEqual(await replacement.send('alice', request), outcome);
  }
});

test('caller identity must match its request and retained message identity survives completed input and explicit Main replacement', async t => {
  const f = await fixture(t), queue = f.queue(), request = f.request('original', 'one'), before = f.accesses();
  await assert.rejects(queue.send('Alice', request), hasCode('chat_definition_mismatch')); assert.equal(f.accesses(), before);
  const accepted = await queue.send('alice', request); if (accepted.outcome?.action !== 'send') throw Error('Expected admission.');
  const input = accepted.outcome.input;
  // Simulate later domain work; the current record retains the immutable admission fields and binding.
  await f.store.write('chat-bridge/input', { ...input.data, state: 'cancelled', cancellation: { operationId: 'later-cancel', callerPrincipalId: 'alice', reason: 'Synthetic cancellation', requestedAt: new Date().toISOString() }, finishedAt: new Date().toISOString() },
    randomUUID(), { objectId: input.object.objectId, expectedRevision: input.object.revision });
  const main = await queue.main(), oldBinding = (await f.store.read('chat-bridge/binding', f.bindingPin)).value;
  const newBinding = await f.store.write('chat-bridge/binding', { ...oldBinding, primary: { ...oldBinding.primary, nativeId: 'second-synthetic-thread' }, predecessor: f.bindingPin },
    randomUUID(), { create: { parentId: null, name: 'Replacement fixture binding' } });
  await f.store.write('chat-bridge/main', { ...main.value, binding: newBinding, queue: [] }, randomUUID(), { objectId: main.pin.objectId, expectedRevision: main.pin.revision });
  const alias = await queue.send('alice', { ...request, operationId: f.operation('after-replacement') });
  const current = await f.store.read('chat-bridge/input', input.object.objectId);
  assert.deepEqual(alias.outcome && 'input' in alias.outcome ? alias.outcome.input : null, { object: current.pin, data: current.value });
  assert.equal((await queue.main()).value.queue.length, 0);
  await assert.rejects(queue.send('alice', { ...request, operationId: f.operation('changed-binding'), expectedBinding: newBinding }), hasCode('mutation_conflict'));
  await assert.rejects(queue.send('alice', f.request('new-message-old-binding')), hasCode('chat_binding_mismatch'));
});

test('full Chat queue refuses a new request before accepting a local operation', async t => {
  const f = await fixture(t), queue = f.queue();
  const accepted = await queue.send('alice', f.request('original')); if (accepted.outcome?.action !== 'send') throw Error('Expected admission.');
  const main = await queue.main(), ticket = main.value.queue[0]!;
  await f.store.write('chat-bridge/main', { ...main.value, queue: Array.from({ length: 64 }, (_, index) => ({ ...ticket, inputId: 'synthetic-' + index,
    identity: { ...ticket.identity, messageId: 'synthetic-' + index }, sequence: index + 1 })), nextSequence: 65 }, randomUUID(), { objectId: main.pin.objectId, expectedRevision: main.pin.revision });
  await assert.rejects(queue.send('alice', f.request('overflow')), hasCode('chat_queue_full'));
  assert.equal(await queue.operations.find('alice', 'overflow'), null, 'A known full queue refuses the new action before acceptance.');
  assert.equal((await queue.main()).value.queue.length, 64); assert.equal((await queue.main()).value.pendingAction, null);
});

test('competing messages for the final queue slot retain one input and a definite refused original action', async t => {
  const f = await fixture(t), a = f.queue(), b = f.queue(f.second), main = await a.main(), at = new Date().toISOString();
  await f.store.write('chat-bridge/main', { ...main.value, queue: Array.from({ length: 63 }, (_, index) => ({ inputId: 'previous-' + index,
    identity: { channel, senderPrincipalId: 'alice', messageId: 'previous-' + index }, requestHash: digest('previous-' + index), binding: f.bindingPin, sequence: index + 1, admittedAt: at })), nextSequence: 64 },
    randomUUID(), { objectId: main.pin.objectId, expectedRevision: main.pin.revision });
  let arrivals = 0, release!: () => void;
  const bothAccepted = new Promise<void>(resolve => { release = resolve; });
  f.intercept(async (_params, result) => {
    if (result.object.contractKey === 'chat-bridge/operation' && result.revision.revision === 1) {
      if (++arrivals === 2) release(); await bothAccepted;
    }
  });
  const results = await Promise.all([a.send('alice', f.request('last-a')), b.send('bob', f.request('last-b', 'last-b', 'bob'))]); f.intercept(null);
  assert.deepEqual(results.map(value => value.phase).sort(), ['failed', 'succeeded']);
  const refused = results.find(value => value.phase === 'failed')!;
  assert.equal(refused.error?.code, 'chat_queue_full'); assert.equal(refused.error?.outcome, 'not_executed');
  const final = await a.main(); assert.equal(final.value.queue.length, 64); assert.equal(final.value.nextSequence, 65); assert.equal(final.value.pendingAction, null);
  assert.deepEqual(await b.send(refused.callerPrincipalId, refused.request as Chat.SendRequest), refused);
});

test('concurrent changed content under one message identity cannot strand Main or rewrite the winning input', async t => {
  const f = await fixture(t), a = f.queue(), b = f.queue(f.second);
  let arrivals = 0, release!: () => void;
  const bothAccepted = new Promise<void>(resolve => { release = resolve; });
  f.intercept(async (_params, result) => {
    if (result.object.contractKey === 'chat-bridge/operation' && result.revision.revision === 1) { if (++arrivals === 2) release(); await bothAccepted; }
  });
  const first = f.request('content-a', 'same'), second = { ...f.request('content-b', 'same'), payload: { text: 'Conflicting concurrent content', images: [] } };
  const results = await Promise.all([a.send('alice', first), b.send('alice', second)]); f.intercept(null);
  assert.deepEqual(results.map(value => value.phase).sort(), ['failed', 'succeeded']);
  const refused = results.find(value => value.phase === 'failed')!;
  assert.equal(refused.error?.code, 'mutation_conflict'); assert.equal(refused.error?.outcome, 'not_executed');
  assert.deepEqual(await b.send('alice', refused.request as Chat.SendRequest), refused);
  await a.recoverPending(); const main = await a.main(); assert.equal(main.value.pendingAction, null); assert.equal(main.value.queue.length, 1); assert.equal(main.value.nextSequence, 2);
  const next = await a.send('alice', f.request('later-independent')); assert.equal(next.phase, 'succeeded'); assert.equal((await a.main()).value.queue.length, 2);
});

test('local Chat evidence preserves original large native-request chunks and immutable historical revisions without Hive writes', async t => {
  const f = await fixture(t), operation = await f.queue().send('alice', f.request('evidence-parent'));
  if (operation.outcome?.action !== 'send') throw Error('Expected queued fixture input.');
  const parent = operation.outcome.input.object.objectId;
  const request = { nativeVersion: '0.154.0', catalogSourceHash: f.admission.definition.nativePlan.catalogSourceHash, method: 'turn/start',
    params: { threadId: 'fixture-main-thread', input: [{ type: 'image', url: 'data:image/png;base64,' + Buffer.alloc(chunkBytes, 37).toString('base64') }] } };
  const original = structuredClone(request); let writes = 0;
  f.intercept(() => { writes++; });
  const pin = await saveChatEvidence(f.store, parent, 'native-request/canonical-json', request); f.intercept(null);
  request.params.input[0]!.url = 'Changed caller-owned input after publication';
  assert.equal(writes, 0);
  assert.deepEqual(await readChatEvidence(f.store, parent, pin, 'native-request/canonical-json'), original);
  const manifest = await f.store.read('chat-bridge/evidence', pin, parent); assert.equal(manifest.value.chunks.length, 2);
  assert.ok(manifest.value.chunks.every(part => part.byteLength <= chunkBytes && part.object.revision === 1));
  assert.deepEqual(await readChatEvidence(f.store, parent, pin, 'native-request/canonical-json'), original);
  const changed = await f.store.write('chat-bridge/evidence', { ...manifest.value, contentHash: digest('wrong-complete-body') }, randomUUID(), { objectId: pin.objectId, expectedRevision: pin.revision });
  await assert.rejects(readChatEvidence(f.store, parent, changed, 'native-request/canonical-json'), hasCode('chat_evidence_mismatch'));
});

test('Chat evidence refuses invalid native shape, excessive bytes, foreign scope and duplicated chunk pins', async t => {
  const f = await fixture(t), first = await f.queue().send('alice', f.request('first-evidence')), second = await f.queue().send('alice', f.request('second-evidence'));
  if (first.outcome?.action !== 'send' || second.outcome?.action !== 'send') throw Error('Expected fixture inputs.');
  const parent = first.outcome.input.object.objectId, request = { nativeVersion: '0.154.0', catalogSourceHash: f.admission.definition.nativePlan.catalogSourceHash,
    method: 'turn/start', params: { threadId: 'fixture-main-thread', input: [{ type: 'image', url: 'data:image/png;base64,' + Buffer.alloc(chunkBytes, 38).toString('base64') }] } };
  let writes = 0; f.intercept(() => { writes++; });
  await assert.rejects(saveChatEvidence(f.store, parent, 'native-request/canonical-json', { ...request, method: 'hidden/unregistered' }));
  await assert.rejects(saveChatEvidence(f.store, parent, 'native-request/canonical-json', { ...request, params: { ...request.params, input: [{ type: 'text', text: 'x'.repeat(maximumEvidenceBytes) }] } }));
  assert.equal(writes, 0); f.intercept(null);
  const pin = await saveChatEvidence(f.store, parent, 'native-request/canonical-json', request);
  await assert.rejects(readChatEvidence(f.store, second.outcome.input.object.objectId, pin, 'native-request/canonical-json'), hasCode('chat_scope_mismatch'));
  await assert.rejects(readChatEvidence(f.store, parent, pin, 'agent-operation/canonical-json'), hasCode('chat_evidence_mismatch'));
  const manifest = await f.store.read('chat-bridge/evidence', pin, parent);
  const changed = await f.store.write('chat-bridge/evidence', { ...manifest.value, chunks: [manifest.value.chunks[0]!, manifest.value.chunks[0]!] }, randomUUID(), { objectId: pin.objectId, expectedRevision: pin.revision });
  await assert.rejects(readChatEvidence(f.store, parent, changed, 'native-request/canonical-json'), hasCode('chat_evidence_mismatch'));
});

test('archived Chat input identity is found and refused before a retry can claim Main or publish another input', async t => {
  const f = await fixture(t), queue = f.queue(), original = f.request('before-archive', 'retained-message');
  const accepted = await queue.send('alice', original); if (accepted.outcome?.action !== 'send') throw Error('Expected admission.');
  await f.first.request('objects.archive', { mutationId: randomUUID(), objectId: accepted.outcome.input.object.objectId, archived: true });
  let writes = 0; f.intercept(() => { writes++; });
  await assert.rejects(queue.send('alice', { ...original, operationId: f.operation('after-archive') }), hasCode('chat_scope_mismatch'));
  assert.equal(writes, 0); assert.equal((await queue.main()).value.pendingAction, null);
  assert.equal(await queue.operations.find('alice', 'after-archive'), null);
});

async function nativeEvidenceFixture(t: { after(callback: () => void): void }, nativeVersion: Chat.NativePlan['nativeVersion'], large = false) {
  const f = await fixture(t, nativeVersion), queued = await f.queue().send('alice', f.request('native-evidence-parent'));
  if (queued.outcome?.action !== 'send') throw Error('Expected a retained Chat input.');
  const parent = queued.outcome.input.object.objectId, definition = f.admission.definition, catalog = chatNativeCatalog(nativeVersion).catalog;
  const request = { nativeVersion, catalogSourceHash: catalog.sourceHash, method: 'turn/start', params: { ...mainTurnContext({}, 'conversation input'), threadId: 'fixture-main-thread',
    input: large ? [{ type: 'image', url: 'data:image/png;base64,' + Buffer.alloc(4 * chunkBytes, 31).toString('base64') }]
      : [{ type: 'text', text: 'Synthetic original input.', text_elements: [] }] } } as Chat.NativeRequest;
  const pin = await saveChatEvidence(f.store, parent, 'native-request/canonical-json', request), at = '2026-09-07T09:00:00.000Z';
  const call: Chat.NativeCall = { schemaVersion: 1, operationId: 'original-native-turn', serviceNodeId: definition.project.serviceNodeId,
    callerPrincipalId: definition.principalId, request: pin, nativeVersion, catalogSourceHash: catalog.sourceHash, method: 'turn/start',
    predecessor: null, preparedEpoch: 'original-epoch', requestHash: hashJson({ method: request.method, params: request.params }),
    expectedDefinitionHash: definition.nativePlan.definitions.turnStart, createdAt: at, state: 'prepared', epoch: null, evidence: null, code: null, updatedAt: at };
  const observation: Agent.Operation = { schemaVersion: 1, operationId: call.operationId, callerPrincipalId: call.callerPrincipalId, serviceNodeId: call.serviceNodeId,
    nativeVersion, nativeExecutableHash: catalog.nativeExecutableHash, method: request.method, params: request.params as Wire.Json, requestHash: call.requestHash,
    phase: 'succeeded', createdAt: at, updatedAt: at, epoch: 'original-epoch', requestId: 'native-request-id', code: null,
    reply: { result: { turn: { id: 'original-turn', status: 'completed', items: [{ id: 'native-message', type: 'agentMessage', text: large ? 'r'.repeat(5 * chunkBytes) : 'Saved reply.' }] } } } };
  f.retainedOperations.set(observation.operationId, observation);
  return { ...f, parent, definition, catalog, request, call, observation, evidence: new ChatNativeEvidence(f.store, definition) };
}

for (const version of ['0.154.0'] as const) test('Chat checked native ' + version + ' evidence preserves large request/reply bytes across another instance without Hive writes', async t => {
  const f = await nativeEvidenceFixture(t, version, true);
  assert.ok(Buffer.byteLength(canonical(f.request)) > 4 * chunkBytes);
  assert.ok(Buffer.byteLength(canonical(f.observation)) > 10 * chunkBytes);
  let writes = 0; f.intercept(() => { writes++; });
  const first = await f.evidence.saveOperation(f.parent, f.call, f.observation); f.intercept(null); assert.equal(writes, 0);
  const second = new ChatNativeEvidence(f.makeStore(f.second), f.definition);
  const pin = await second.saveOperation(f.parent, f.call, f.observation);
  assert.deepEqual(pin, first);
  assert.deepEqual(await second.operation(f.parent, f.call, pin), f.observation);
  assert.deepEqual(await f.evidence.saveOperation(f.parent, f.call, f.observation), pin);
  const reference = await f.makeStore(f.second).read('chat-bridge/evidence', pin, f.parent);
  assert.equal(reference.key, 'chat-bridge/evidence');
  assert.equal((await f.queue().main()).value.queue.length, 1, 'Saving native evidence does not complete or advance the input.');
});

test('Chat native authority refuses altered identities, executable, planned settings and schema-valid forged bytes while preserving independent host clocks', async t => {
  const f = await nativeEvidenceFixture(t, '0.154.0');
  await verifyChatNativeOperation(f.store, f.definition, f.call, f.request, f.observation);
  for (const change of [{ callerPrincipalId: 'different-caller' }, { serviceNodeId: 'different-owner' }, { operationId: 'another-operation' },
    { nativeExecutableHash: digest('unverified-binary') }, { requestHash: digest('different-parameters') },
    { params: { threadId: 'another-thread', input: [] } }, { nativeVersion: '0.0.0' }]) {
    await assert.rejects(() => verifyChatNativeOperation(f.store, f.definition, f.call, f.request, { ...f.observation, ...change }), hasCode('chat_native_mismatch'));
  }
  await assert.rejects(() => verifyChatNativeOperation(f.store, f.definition, f.call, f.request, { ...f.observation, reply: { result: { inventedResult: true } } }));
  await assert.rejects(() => verifyChatNativeRequest(f.store, f.definition, { ...f.call, expectedDefinitionHash: digest('other-definition') }, f.request), hasCode('chat_native_mismatch'));
  const substituted = { ...f.request, params: { ...f.request.params, model: 'unplanned-model' } } as Chat.NativeRequest;
  await assert.rejects(() => verifyChatNativeRequest(f.store, f.definition, { ...f.call, requestHash: hashJson({ method: substituted.method, params: substituted.params }) }, substituted), hasCode('chat_native_mismatch'));
  let writes = 0; f.intercept(() => { writes++; });
  const foreign = { ...f.observation, callerPrincipalId: 'different-caller' };
  await assert.rejects(f.evidence.saveOperation(f.parent, f.call, foreign), hasCode('chat_native_mismatch'));
  assert.equal(writes, 0); f.intercept(null);
  // Valid canonical bytes and their hashes do not authorize a fabricated native producer.
  const forged = await saveChatEvidence(f.store, f.parent, 'agent-operation/canonical-json', foreign);
  await assert.rejects(f.evidence.operation(f.parent, f.call, forged), hasCode('chat_native_mismatch'));
  const unavailable = { ...f.observation, phase: 'outcome_unknown', reply: null, code: 'native_epoch_lost' } as Agent.Operation;
  const saved = await f.evidence.saveOperation(f.parent, f.call, unavailable);
  assert.equal((await f.evidence.operation(f.parent, f.call, saved)).phase, 'outcome_unknown');
  const skewed = { ...f.observation, createdAt: '2026-09-07T08:59:59.000Z', updatedAt: '2026-09-07T08:59:59.500Z' };
  const clockPin = await f.evidence.saveOperation(f.parent, f.call, skewed);
  assert.deepEqual(await f.evidence.operation(f.parent, f.call, clockPin), skewed);
  const input = await f.store.read('chat-bridge/input', f.parent);
  await f.store.write('chat-bridge/input', { ...input.value, workspaceId: 'another-workspace' }, randomUUID(), { objectId: f.parent, expectedRevision: input.pin.revision });
  await assert.rejects(f.evidence.operation(f.parent, f.call, clockPin), hasCode('chat_scope_mismatch'));
});

for (const version of ['0.154.0'] as const) test('Chat native ' + version + ' read evidence binds exact epoch, catalog and original error without claiming success', async t => {
  const f = await nativeEvidenceFixture(t, version), expected = { serviceNodeId: f.call.serviceNodeId, callerPrincipalId: f.call.callerPrincipalId,
    nativeVersion: version, epoch: 'original-epoch', method: 'thread/read', params: { threadId: 'fixture-main-thread', includeTurns: true } };
  const value = { schemaVersion: 1, observationId: 'original-read', ...expected, nativeExecutableHash: f.catalog.nativeExecutableHash,
    catalogHash: hashJson(f.catalog), requestId: 'original-read-request', observedAt: '2026-09-07T09:00:01.000Z',
    requestHash: hashJson({ method: expected.method, params: expected.params }), reply: { error: { code: -32601, message: 'Original unsupported read.' } } } as Agent.ReadObservation;
  const pin = await f.evidence.saveRead(f.parent, expected, value);
  assert.deepEqual(await f.evidence.read(f.parent, expected, pin), value);
  for (const change of [{ epoch: 'new-epoch' }, { catalogHash: digest('unverified-catalog') }, { callerPrincipalId: 'foreign' }, { nativeExecutableHash: digest('different-executable') }])
    assert.throws(() => verifyChatNativeRead({ ...value, ...change }, expected), hasCode('chat_native_mismatch'));
  assert.throws(() => verifyChatNativeRead({ ...value, reply: { result: { fabricated: 'thread' } } }, expected));
  await assert.rejects(f.evidence.read(f.parent, { ...expected, serviceNodeId: 'different-owner' }, pin), hasCode('chat_native_mismatch'));
  const reply = (await f.evidence.read(f.parent, expected, pin)).reply;
  assert.ok('error' in reply); assert.equal(reply.error.message, 'Original unsupported read.');
});
