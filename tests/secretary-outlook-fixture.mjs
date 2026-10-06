import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { hashJson, IvyError, validateAgent } from '../dist/packages/sdk/src/node.js';
import { fixture, principal } from './secretary-fixture.mjs';
import { secretaryRegistry } from '../dist/services/secretary/src/schema.js';
import { microsoftRegistry } from '../dist/services/secretary/src/microsoft-schema.js';
import { OutlookSource } from '../dist/services/secretary/src/outlook-source.js';

export const profile = { id: 'outlook-account', email: 'me@example.test' };
export const mail = (id = 'original', changes = {}) => ({ id, sender: { emailAddress: { address: 'sender@example.test' } }, subject: 'An original message', receivedDateTime: '2026-09-07T08:00:00Z', isRead: false, has_attachments: false,
  body: { contentType: 'text', content: 'The complete body needs a decision.' }, web_link: 'https://outlook.office365.com/mail/id/' + id, ...changes });
export const page = (messages = [mail()]) => ({ value: messages, has_more: false, next_from_index: null, next_link: null });
export const sourceRegistry = () => { const a = secretaryRegistry(), b = microsoftRegistry(); return { ...a, contracts: [...a.contracts, ...b.contracts], requiredContracts: [...a.requiredContracts, ...b.requiredContracts] }; };
export async function outlookFixture(t, registry = sourceRegistry) {
  const f = await fixture(t, { sources: [{ sourceId: 'mail', accountId: profile.id, kind: 'email', producerPrincipalIds: [principal], allowedSenderIds: null, ownSenderIds: [profile.email] }] }, registry);
  let now = '2026-09-07T09:00:00.000Z'; f.setNow(now);
  const target = { serviceNodeId: 'outlook-native', hostId: 'native-host', nativeVersion: '0.149.1', nativeExecutableHash: hashJson('executable'), catalogHash: hashJson('catalogue') };
  const account = { account: { type: 'chatgpt', email: 'native@example.test', planType: 'pro' }, requiresOpenaiAuth: true };
  const configuration = { sourceId: 'mail', target, expectedAccountHash: hashJson({ type: 'chatgpt', email: account.account.email }), profile, since: '2026-09-01T00:00:00.000Z', pageSize: 8, pollMs: 60000, maximumPages: 100, threadCwd: resolve('.local/outlook-fixture-project') };
  const state = { epoch: 'original-epoch', account, profile: structuredClone(profile), pages: new Map([[0, page()]]), operations: new Map(), results: new Map(), dispatches: [], after: null, cut: false, offline: false, absent: false, changedDefinition: false, pendingMethod: null, providerError: null, nativeFailed: false, afterNative: null };
  const binding = name => {
    const [namespace, ...rest] = name.split('.'), definition = { namespace, name: rest.join('.'), interfaceVersion: '1.0.0', description: 'Controlled native read', inputSchema: { type: 'object', additionalProperties: true }, outputSchema: { type: 'object', additionalProperties: true } };
    return { qualifiedName: name, definition, definitionHash: hashJson([definition, namespace === 'codex' && state.changedDefinition]) };
  };
  const rpc = { async request(method, args, options) {
    const receipt = method === 'tools.call' && args.qualifiedName === 'agent.invoke';
    if (receipt) args = { ...args, qualifiedName: 'codex.' + args.arguments.method, expectedDefinitionHash: args.arguments.expectedDefinitionHash, arguments: args.arguments.params };
    if (state.cut) throw new IvyError('outcome_unknown', 'Injected process loss.', 'unknown'); let result;
    if (method === 'tools.list' && ['agent', 'codex'].includes(args.namespace)) {
      if (state.offline) throw new IvyError('service_not_ready', 'Native offline.'); result = { items: [binding(args.namespace + '.' + args.namePrefix)], nextCursor: null, provider: { node: { serviceNodeId: target.serviceNodeId } } };
    } else if (method === 'tools.call' && args.serviceNodeId === target.serviceNodeId) {
      if (state.offline) throw new IvyError('service_not_ready', 'Native offline.'); const name = args.qualifiedName, input = args.arguments;
      if (args.expectedDefinitionHash !== binding(name).definitionHash) throw new IvyError('tool_definition_changed', 'The bound definition changed before dispatch.', 'not_executed');
      if (name === 'agent.status') {
        result = { ...target, state: 'ready', epoch: state.epoch, pid: 42, observedAt: now, code: null, initialized: {}, pendingInputs: 0, operations: { retained: state.operations.size, maximum: 1000, bytes: 1000, maximumBytes: 1048576 }, observedMethods: [] }; validateAgent('Status', result);
      } else if (name === 'agent.operation') {
        result = state.operations.get(input.operationId); if (!result || state.absent) throw new IvyError('not_found', 'Original operation absent.', 'not_executed', { kind: 'agent_operation_absent', serviceNodeId: target.serviceNodeId, operationId: input.operationId, epoch: state.epoch });
      } else if (name.startsWith('codex.')) {
        const nativeMethod = name.slice(6); assert.ok(['account/read', 'thread/start', 'mcpServer/tool/call', 'thread/unsubscribe'].includes(nativeMethod));
        let operation = state.operations.get(args.operationId);
        if (!operation) {
          let reply;
          if (nativeMethod === 'account/read') reply = structuredClone(state.account);
          else if (nativeMethod === 'thread/start') { assert.equal(input.ephemeral, true); assert.equal(input.permissions, ':danger-full-access'); reply = { thread: { id: 'thread-' + args.operationId } }; }
          else if (nativeMethod === 'thread/unsubscribe') reply = { status: 'unsubscribed' };
          else {
            assert.equal(input.server, 'codex_apps'); assert.ok(['microsoft_outlook_email.get_profile', 'microsoft_outlook_email.list_messages', 'microsoft_outlook_email.list_attachments', 'microsoft_outlook_email.fetch_attachment'].includes(input.tool));
            const listing = input.tool.endsWith('list_messages');
            const body = listing ? state.pages.get(input.arguments.skip) : input.tool.endsWith('list_attachments') ? state.media.metadata : input.tool.endsWith('fetch_attachment') ? state.media.payload : state.profile;
            reply = listing && state.providerError ? { content: [], isError: true, structuredContent: structuredClone(state.providerError) } : { content: [], isError: false, structuredContent: structuredClone(body) };
            if (listing) { assert.equal(input.arguments.folder_id, 'inbox'); assert.equal(input.arguments.order_by, 'receivedDateTime desc'); }
          }
          const pending = state.pendingMethod === (nativeMethod === 'mcpServer/tool/call' ? input.tool : nativeMethod), phase = pending ? 'accepted' : state.nativeFailed ? 'failed' : 'succeeded';
          operation = { schemaVersion: 1, operationId: args.operationId, callerPrincipalId: principal, serviceNodeId: target.serviceNodeId, nativeVersion: target.nativeVersion, nativeExecutableHash: target.nativeExecutableHash,
            method: nativeMethod, params: structuredClone(input), requestHash: hashJson({ method: nativeMethod, params: input }), phase, createdAt: now, updatedAt: now, epoch: pending ? null : state.epoch, requestId: pending ? null : state.operations.size,
            reply: phase === 'succeeded' ? { result: reply } : null, code: phase === 'failed' ? 'controlled_native_failure' : null };
          validateAgent('Operation', operation); state.operations.set(args.operationId, operation); state.results.set(args.operationId, reply); state.dispatches.push(structuredClone(operation));
          if (state.afterNative) await state.afterNative(operation);
        } else { assert.equal(operation.method, nativeMethod); assert.deepEqual(operation.params, input); }
        result = operation.reply?.result ?? {};
      } else throw Error('Unexpected native call ' + name);
    } else result = await f.connection.request(method, args, options);
    if (state.after) await state.after(method, args, result); return structuredClone(receipt ? state.operations.get(args.operationId) : result);
  } };
  let source;
  const restart = async () => { state.cut = false; await f.restart(rpc); source = new OutlookSource(f.engine, configuration); };
  await restart();
  return { f, state, configuration, rpc, get source() { return source; }, restart,
    setNow(value) { now = value; f.setNow(value); },
    completePending() { for (const operation of state.operations.values()) if (operation.phase === 'accepted') { operation.phase = 'succeeded'; operation.epoch = state.epoch; operation.requestId = 70; operation.reply = { result: state.results.get(operation.operationId) }; operation.updatedAt = now; validateAgent('Operation', operation); } state.pendingMethod = null; },
    async settle() { for (let i = 0; i < 40; i++) { const result = await source.step(); if (result !== 'pending') return result; } throw Error('Source did not settle within bounded work.'); },
    async objects(key) { return (await f.client.request('objects.query', { contractKey: key, limit: 100, where: { op: 'eq', field: 'object.parentId', value: f.settings.identity.scope.rootObjectId } })).items; } };
}
