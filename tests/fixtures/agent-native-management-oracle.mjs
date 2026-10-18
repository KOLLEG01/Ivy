import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {PassThrough} from 'node:stream';
import {randomUUID} from 'node:crypto';
import ts from 'typescript';
import {NativeJournal} from '../../dist/services/agent-manager/src/journal.js';
import {NativeOperations} from '../../dist/services/agent-manager/src/operations.js';
import {NativeInteractions} from '../../dist/services/agent-manager/src/interactions.js';
import {NativeRpc} from '../../dist/services/agent-manager/src/rpc.js';
import {answerNativeClock, nativeClockPrincipal} from '../../dist/services/agent-manager/src/clock.js';
import {agentRegistry} from '../../dist/services/agent-manager/src/registry.js';
import {discoverNativeTools} from '../../dist/services/agent-manager/src/native-discovery.js';
import {IvyError, requireThat} from '../../dist/packages/contracts/src/errors.js';
import {hashJson} from '../../dist/packages/contracts/src/canonical.js';
import {toolDefinitionHash} from '../../dist/packages/contracts/src/tool-definition.js';
import {managementFrameBytes} from '../../dist/packages/contracts/src/limits.js';
import {nativeRequestFrameBytes, nativeFrameBytes, nativeAnswerFrameBytes} from '../../dist/packages/host-runtime/src/codex-limits.js';

// Run the actual private handlers without importing the service-starting module.
const source = ts.createSourceFile('main.ts', readFileSync('services/agent-manager/src/main.ts', 'utf8'), ts.ScriptTarget.Latest, true);
const tools = ['catalog', 'discover', 'frameLimits', 'prevent', 'invoke', 'interact', 'interaction', 'read', 'operation', 'inputs', 'inputDefinition', 'answer'];
function expression(kind, name) {
  const found = [];
  function visit(node) {
    if (kind === 'variable' && ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.name.text === name) found.push(node.initializer);
    if (kind === 'property' && ts.isPropertyAssignment(node) && node.name.getText(source).replaceAll('"', '') === name) found.push(node.initializer);
    ts.forEachChild(node, visit);
  }
  visit(source); assert.equal(found.length, 1, name); return found[0].getText(source);
}
const expressions = Object.fromEntries([
  ...['identity', 'available', 'executeNative'].map(name => ['variable', name]),
  ...['onRequest', 'onNotification', 'onClose', ...tools.map(name => 'agent.' + name)].map(name => ['property', name]),
].map(([kind, name]) => [name, expression(kind, name)]));
const globals = {IvyError, requireThat, hashJson, randomUUID, discoverNativeTools, answerNativeClock, nativeClockPrincipal,
  managementFrameBytes, nativeRequestFrameBytes, nativeFrameBytes, nativeAnswerFrameBytes};
const evaluated = Object.fromEntries(Object.entries(expressions).map(([name, expression]) => [name,
  Function('env', ...Object.keys(globals), 'with(env){' + ts.transpileModule('const value=' + expression + ';',
    {compilerOptions: {target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext}}).outputText + ';return value;}')]));
const evaluate = (name, env) => evaluated[name](env, ...Object.values(globals));
const catalog = JSON.parse(readFileSync('specs/native/codex-0.154.0/catalog.json', 'utf8'));
// Fresh owners qualify guards, not unused catalog compilation. Keep each
// exercised native schema byte-for-byte; complete catalogs have a separate oracle.
catalog.clientRequests = catalog.clientRequests.filter(value => ['account/logout', 'thread/read', 'thread/turns/list', 'thread/items/list', 'turn/start'].includes(value.method));
catalog.serverRequests = catalog.serverRequests.filter(value => ['item/tool/requestUserInput', 'currentTime/read'].includes(value.method));
catalog.serverNotifications = catalog.serverNotifications.filter(value => value.method === 'serverRequest/resolved');
catalog.clientNotifications = []; catalog.derivedLegacyTypes = [];
const registry = agentRegistry(catalog), catalogHash = hashJson(catalog);
const nativeDefinitions = new Map(registry.namespaces[0].tools.map(definition => [definition.nativeMethod, toolDefinitionHash(definition)]));
const context = (...values) => ({operationId: values.length ? values[0] : 'one', callerPrincipalId: values[1] ?? 'caller'});
const request = (operationId = 'one', method = 'account/logout', params = null) => ({
  operationId, method, params, nativeVersion: catalog.version, expectedDefinitionHash: nativeDefinitions.get(method) ?? 'sha256:' + 'f'.repeat(64),
});
const c = (action, fields = {}) => ({action, ...fields});
const manage = (tool, input = {}, extra = {}) => c('manage', {tool, input, context: context(), ...extra});
const start = (tool, input, extra = {}) => c('startManage', {tool, input, context: context(input.operationId), handle: input.operationId, ...extra});
const wait = handle => c('wait', {handle});
const sent = c('sent'), snapshot = c('snapshot'), drain = c('drain'), closing = c('closing');
const reply = result => c('reply', {reply: {result}});
const identity = requestId => ({serviceNodeId: 'agent', epoch: 'live', requestId});
const input = (id, method = 'item/tool/requestUserInput') => c('emit', {frame: {id, method, params: {threadId: 'thread'}}});
const definition = id => manage('inputDefinition', {identity: identity(id)}, {summary: true});
const answer = id => ({operationId: 'answer', identity: identity(id), reply: {error: {code: -32000, message: 'declined'}}});
const cases = [], add = (name, commands) => cases.push({name, commands});
add('catalog-discovery-and-frame-bounds', [manage('catalog', {}, {summary: true}), manage('discover', {method: 'thread/read'}, {summary: true}), manage('discover', {query: 'READ TASK'}, {summary: true}), manage('frameLimits'), snapshot]);
for (const tool of ['invoke', 'interact', 'prevent']) {
  for (const [name, patch] of [['version', {nativeVersion: 'other'}], ['definition', {expectedDefinitionHash: 'sha256:' + 'f'.repeat(64)}],
    ['method', {method: 'missing/method'}], ['operation', {operationId: 'other'}]]) {
    add(tool + '-bound-' + name, [manage(tool, {...request(), ...patch}), snapshot]);
  }
  for (const operationId of [undefined, '']) add(tool + '-outer-' + String(operationId), [manage(tool, request(), {context: context(operationId)}), snapshot]);
  add(tool + '-reserved-principal', [manage(tool, request(), {context: context('one', nativeClockPrincipal)}), drain, snapshot]);
  add(tool + '-closing-invalid-binding-order', [closing, manage(tool, request('one', 'account/logout', null), {context: context('other')}), snapshot]);
  add(tool + '-epoch-mismatch', [c('replaceEpoch'), manage(tool, request()), drain, snapshot]);
  for (const method of [null, 7, {}]) add(tool + '-invalid-method-type-' + cases.length,
    [manage(tool, {...request(), method}), snapshot]);
}
add('prevent-returns-original-unexecuted-receipt', [manage('prevent', request()), drain, manage('invoke', request()), manage('operation', {operationId: 'one'}), snapshot]);
add('prevent-without-outer-operation', [manage('prevent', request(), {context: {callerPrincipalId: 'caller'}}), drain, snapshot]);
add('invoke-while-closing-retains-unexecuted-receipt', [closing, manage('invoke', request()), drain, snapshot]);
add('durable-invoke-operation-and-offline-history', [start('invoke', request()), sent, reply({untouched: ['\ud800', null]}), wait('one'), drain,
  manage('operation', {operationId: 'one'}), manage('operation', {operationId: 'one'}, {context: context('outer', 'other')}), closing,
  manage('operation', {operationId: 'one'}), manage('catalog', {}, {summary: true}), manage('frameLimits'), snapshot]);
add('native-error-remains-in-durable-operation', [start('invoke', request()), sent, c('reply', {reply: {error: {code: -32001, message: 'original', data: {a: '\udfff'}}}}), wait('one'), drain, snapshot]);
add('invoke-utf16-caller', [start('invoke', request(), {context: context('one', '\ud800')}), sent, reply(null), wait('one'), drain, manage('operation', {operationId: 'one'}, {context: context('outer', '\ud800')}), snapshot]);
add('interaction-replay-conflict-offline-lookup-and-caller', [start('interact', request()), sent, reply({ok: true}), wait('one'),
  manage('interact', request()), manage('interact', request('one', 'account/logout', {})), closing,
  manage('interaction', {operationId: 'one'}), manage('interaction', {operationId: 'one'}, {context: context('one', 'other')}), snapshot]);
add('interaction-native-error-is-cached', [start('interact', request()), sent, c('reply', {reply: {error: {code: 13, message: 'original'}}}), wait('one'), manage('interaction', {operationId: 'one'}), snapshot]);
for (const method of ['thread/read', 'thread/turns/list', 'thread/items/list']) add('read-observation-' + method,
  [start('read', {nativeVersion: catalog.version, method, params: {threadId: 'thread'}}, {handle: 'read', context: context(undefined)}), sent, reply({exact: '\ud800'}), wait('read'), snapshot]);
add('read-native-error-is-an-observation', [start('read', {nativeVersion: catalog.version, method: 'thread/read', params: {threadId: 'thread'}}, {handle: 'read'}), sent,
  c('reply', {reply: {error: {code: 2, message: 'native error', data: null}}}), wait('read'), snapshot]);
add('management-read-ignores-disconnected-hive-signal', [start('read', {nativeVersion: catalog.version, method: 'thread/read', params: {threadId: 'thread'}}, {handle: 'read', cancelled: true}), sent, reply(null), wait('read'), snapshot]);
add('read-checks-owner-again-after-reply', [start('read', {nativeVersion: catalog.version, method: 'thread/read', params: {threadId: 'thread'}}, {handle: 'read'}), sent, closing, reply(null), wait('read'), snapshot]);
add('read-refuses-mutation-before-version', [manage('read', {nativeVersion: 'wrong', method: 'turn/start', params: {}}), snapshot]);
add('read-version-mismatch', [manage('read', {nativeVersion: 'wrong', method: 'thread/read', params: {threadId: 'thread'}}), snapshot]);
add('read-closed-before-method-check', [closing, manage('read', {nativeVersion: 'wrong', method: 'turn/start', params: {}}), snapshot]);
for (const method of [undefined, null, 7, {}]) add('read-invalid-method-type-' + cases.length,
  [manage('read', {nativeVersion: catalog.version, method, params: {}}), snapshot]);
add('operation-and-interaction-absence', [manage('operation', {operationId: 'absent'}), manage('interaction', {operationId: 'absent'}), snapshot]);
add('operation-reserved-principal', [manage('operation', {operationId: 'absent'}, {context: context('x', nativeClockPrincipal)}), snapshot]);
add('input-definitions-and-typed-answer', [definition(0), input(0), input('0'), definition(0), definition('0'), manage('inputs'),
  start('answer', answer(0), {handle: 'answer'}), sent, wait('answer'), definition(0), c('emit', {frame: {method: 'serverRequest/resolved', params: {requestId: 0, threadId: 'thread'}}}),
  drain, definition(0), definition('0'), manage('inputs', {includeExpired: true}), snapshot]);
add('answer-outer-conflict-before-owner', [closing, manage('answer', answer(7), {context: context('different')}), snapshot]);
add('answer-no-outer-operation', [input(7), start('answer', answer(7), {handle: 'answer', context: {callerPrincipalId: 'caller'}}), sent, wait('answer'), drain, snapshot]);
add('clock-definition-is-visible-but-not-answerable', [input('clock', 'currentTime/read'), definition('clock'), manage('answer', answer('clock'), {context: context('answer')}), drain, snapshot]);
add('closed-catalog-and-discovery-refuse-before-validation', [closing, manage('catalog'), manage('discover', {query: null}), manage('inputDefinition', {identity: identity('absent')}), snapshot]);
add('frame-bounds-and-input-history-survive-lost-owner', [input(7), c('close', {code: 'fixture_lost'}), manage('frameLimits'), manage('inputs', {includeExpired: true}), snapshot]);

const RealDate = Date, at = '2026-10-18T00:00:00.000Z', limits = {maxOperations: 100, maxJournalBytes: 128 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576};
globalThis.Date = class extends RealDate {constructor(...args) {super(...(args.length ? args : [at]));} static now() {return RealDate.parse(at);}};
try {
  for (const row of cases) {
    const journal = new NativeJournal({serviceNodeId: 'agent', hostId: 'host', nativeVersion: catalog.version, nativeExecutableHash: catalog.nativeExecutableHash}, limits); journal.beginEpoch('live');
    const rows = new Map(), frames = [], inputs = [], notifications = [], closes = [], errors = [], handles = new Map(), work = new Set(), arrivals = [];
    const track = promise => {work.add(promise); void promise.then(() => work.delete(promise), () => work.delete(promise)); return promise;};
    const store = {serviceNodeId: 'agent', read: async (_kind, key) => structuredClone(rows.get(key) ?? null), write: async (_kind, key, value, previous) => {
      const prior = rows.get(key); requireThat(prior?.pin.revision === previous?.revision, 'revision_conflict', 'Another owner claimed the operation.');
      const saved = {value: structuredClone(value), pin: {objectId: key, revision: (prior?.pin.revision ?? 0) + 1}}; rows.set(key, saved); return structuredClone(saved);
    }};
    const operations = new NativeOperations(store, journal), inputStream = new PassThrough(), output = new PassThrough();
    journal.onTerminal = key => {if (key.callerPrincipalId !== nativeClockPrincipal) void track(operations.settle(key)).catch(error => errors.push(error.code));};
    const env = {journal, operations, owner: {epoch: 'live', catalog}, closing: false, track, registry, catalogHash, nativeDefinitions,
      config: {serviceNodeId: 'agent'}, startupClocks: [], service: null, native: null, requestThreadRefresh: null, requestProjectRefresh: null,
      noticePolicy: {observe() {}, isInternal() {return false;}}, notifications: {observe(epoch, method, params) {notifications.push({method, params: structuredClone(params)}); return {};}}};
    for (const name of ['identity', 'available', 'executeNative']) env[name] = evaluate(name, env);
    const onRequest = evaluate('onRequest', env), onNotification = evaluate('onNotification', env), onClose = evaluate('onClose', env);
    const rpc = new NativeRpc(catalog, inputStream, output, {onRequest: value => {onRequest('live', value); inputs.push(journal.input(identity(value.id)));},
      onNotification: value => onNotification('live', value), onClose: code => {onClose('live', code); closes.push(code);}});
    env.owner.rpc = rpc; env.interactions = new NativeInteractions(rpc, 'live');
    const handlers = Object.fromEntries(tools.map(name => [name, evaluate('agent.' + name, env)]));
    inputStream.on('data', bytes => {frames.push(JSON.parse(bytes.toString('utf8'))); arrivals.shift()?.();});
    let last, cursor = 0;
    const ids = new Map(); let requests = 0, observations = 0;
    const uuid = /^(?:interaction-)?[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
    function normalize(value) {
      if (typeof value === 'string') return ids.get(value) ?? value;
      if (Array.isArray(value)) return value.map(normalize);
      if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, value]) => {
        if (key === 'observationId') {assert(uuid.test(value)); if (!ids.has(value)) ids.set(value, 'observation-' + ++observations);}
        return [key, normalize(value)];
      }));
      return value;
    }
    const capture = async action => {try {return {result: structuredClone(await action() ?? null)};} catch (error) {return {error: IvyError.from(error).toWire()};}};
    const call = async step => {
      const controller = new AbortController(); if (step.cancelled) controller.abort();
      const result = await handlers[step.tool](step.input, {...step.context, signal: controller.signal});
      return step.summary ? {hash: hashJson(result)} : result;
    };
    row.expected = [];
    for (const step of row.commands) {
      const result = step.action === 'wait' ? await handles.get(step.handle) : await capture(async () => {
        switch (step.action) {
          case 'manage': return call(step);
          case 'startManage': handles.set(step.handle, capture(() => call(step))); break;
          case 'sent': if (cursor === frames.length) await new Promise(resolve => arrivals.push(resolve)); last = frames[cursor++];
            if (typeof last.id === 'string' && uuid.test(last.id)) ids.set(last.id, 'request-' + ++requests); return last;
          case 'reply': output.write(JSON.stringify({id: last.id, ...step.reply}) + '\n'); break;
          case 'emit': output.write(JSON.stringify(step.frame) + '\n'); break;
          case 'closing': env.closing = true; break;
          case 'replaceEpoch': journal.beginEpoch('replacement'); break;
          case 'close': rpc.close(step.code); break;
          case 'drain': while (work.size) await Promise.allSettled([...work]); break;
          case 'snapshot': return {rows: [...rows].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([key, value]) => ({key, ...value})),
            inputs: journal.inputs({includeExpired: true}), clockOperations: inputs.filter(value => value.method === 'currentTime/read')
              .map(value => journal.get({callerPrincipalId: nativeClockPrincipal, operationId: 'clock:' + hashJson(value.identity).slice(7)})),
            frames, notifications, closes, errors, epoch: journal.epoch};
          default: throw Error('Unknown management fixture action ' + step.action);
        }
      });
      row.expected.push(normalize(structuredClone(result)));
    }
    while (work.size) await Promise.allSettled([...work]); rpc.close('fixture_stop'); inputStream.destroy(); output.destroy(); journal.close();
  }
} finally {globalThis.Date = RealDate;}
assert(cases.some(row => row.expected.some(value => value.error?.data.code === 'native_definition_mismatch')));
process.stdout.write(JSON.stringify({catalog, catalogHash, at, limits, extracted: expressions, cases}));
