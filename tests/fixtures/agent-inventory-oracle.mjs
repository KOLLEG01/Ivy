import assert from 'node:assert/strict';
import {readFileSync, readdirSync} from 'node:fs';
import {NativeInventory, nativeSourceKinds, publishNativeThreads, publishNativeProjects, publishNativeSnapshot} from '../../dist/services/agent-manager/src/inventory.js';
import {NativeJournal} from '../../dist/services/agent-manager/src/journal.js';
import {canonical, hashJson, digest} from '../../dist/packages/contracts/src/canonical.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';

const read = path => JSON.parse(readFileSync(path, 'utf8'));
const base = read('specs/native/codex-0.154.0/catalog.json');
const catalogFor = row => row.catalog ?? (row.projectsSupported === false ? {...base, clientRequests: base.clientRequests.filter(v => v.method !== 'project/list')} : base);
const materialize = value => {
  if (Array.isArray(value)) return value.map(materialize);
  if (value && typeof value === 'object') {
    if ('$fill' in value) return value.$fill.repeat(value.count);
    if ('$repeat' in value) return Array.from({length: value.count}, (_, i) => {
      const item = materialize(value.$repeat); return value.ids ? {...item, id: 'id-' + i} : item;
    });
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, materialize(v)]));
  }
  return value;
};
function capture(value) {return Buffer.byteLength(JSON.stringify(value)) > 4096 ? {encodedBytes: Buffer.byteLength(JSON.stringify(value)), hash: hashJson(value)} : {value};}
const error = (code, outcome = 'not_executed', details) => ({code, message: 'Fixture failure\ud800', outcome, ...(details === undefined ? {} : {details})});
const reason = error('fixture_cancelled', 'unknown', {exact: '\udfff'});
const fromError = value => new IvyError(value.code, value.message, value.outcome, value.details);
const c = (action, fields = {}) => ({action, ...fields}), current = c('currentProjects');
const thread = (id, extra = {}) => ({id, preview: 'Original ' + id, cwd: '/fixture', name: null, status: {type: 'idle'}, source: 'appServer', updatedAt: 1788600000, ephemeral: false, ...extra});
const project = (id = 'project', extra = {}) => ({id, name: 'Project', roots: [{path: '/fixture'}], position: 0, recencyAt: null, ...extra});
const page = (method, data = [], nextCursor = null, extra = {}) => ({method, reply: {result: {data, nextCursor}}, ...extra});
const loaded = (data = [], extra = {}) => page('thread/loaded/list', data, null, extra);
const threads = (active = [], archived = [], ids = [], after = ids) => [loaded(ids), page('thread/list', active), page('thread/list', archived), loaded(after)];
const projects = (data = [], extra = {}) => page('project/list', data, null, extra);
const collections = [];
function collect(name, replies, commands = [c('collectThreads')], extra = {}) {collections.push({name, replies, commands, ...extra});}
collect('empty-thread-snapshot', threads());
collect('all-sources-archives-pagination-and-ephemeral-ownership', [loaded(['loaded', 'ephemeral']), page('thread/list', [thread('loaded')], 'next'), page('thread/list', [thread('historical')]), page('thread/list', [thread('archived')]), {method: 'thread/read', reply: {result: {thread: thread('ephemeral', {ephemeral: true, canAcceptDirectInput: false})}}}, loaded(['ephemeral', 'loaded'])]);
collect('thread-summary-utf16-slices-and-nullable-metadata', threads([thread('id-\ud800', {preview: 'x' + '😀'.repeat(1200), name: 'x' + '😀'.repeat(300), threadSource: 'realtime_voice', projectId: 'p\udfff', recencyAt: 0, canAcceptDirectInput: true})], [], ['id-\ud800']));
collect('combined-snapshot-and-project-cache', [...threads([thread('one')]), projects([project('p\ud800', {name: 'Name\udfff', roots: [{path: 'C:\\one'}, {path: '/two'}], position: 7, recencyAt: 1788600000000})], {advanceMs: 1000})], [current, c('collect'), current, c('currentProjects', {observedAt: 'explicit\ud800'})]);
collect('projects-unsupported-precedes-cancellation', [], [c('abort'), c('collectProjects'), current], {projectsSupported: false});
collect('thread-enumeration-mixed-page-retries-once', [loaded(), page('thread/list', [thread('same')]), page('thread/list', [thread('same')]), ...threads([thread('new')])]);
collect('ownership-change-retries-within-original-reader', [...threads([thread('new')], [], [], ['new']), ...threads([thread('new')], [], ['new'])]);
collect('second-mixed-snapshot-still-fails', [loaded(), page('thread/list', [thread('same')]), page('thread/list', [thread('same')]), loaded(), page('thread/list', [thread('same')]), page('thread/list', [thread('same')])]);
collect('coded-inventory-changed-from-transport-retries-once', [{method: 'thread/loaded/list', throw: error('native_inventory_changed')}, ...threads()]);
collect('uncoded-transport-error-never-retries', [{method: 'thread/loaded/list', uncoded: true}]);
collect('native-error-wraps-original-code', [{method: 'thread/loaded/list', reply: {error: {code: -32001, message: 'native'}}}]);
collect('read-budget-does-not-shorten-transport-watchdog', [loaded([], {advanceMs: 30000}), page('thread/list'), page('thread/list'), loaded()]);
collect('deadline-after-read-refuses-snapshot', [loaded([], {advanceMs: 120000})]);
collect('project-read-has-independent-ten-minute-budget', [projects([project()], {advanceMs: 120001})], [c('collectProjects'), current]);
collect('project-deadline-does-not-replace-cache', [projects([project('retained')]), projects([project('discarded')], {advanceMs: 600000})], [c('collectProjects'), c('collectProjects'), current]);
collect('cancel-before-read-does-not-call-native', [], [c('abort'), c('collectThreads')]);
collect('cancel-after-reply-precedes-native-error', [loaded([], {abort: true, reply: {error: {code: 17, message: 'native'}}})]);
collect('cancel-after-successful-read-refuses-publication', [loaded([], {abort: true})]);
for (const result of [null, [], 7, {}, {data: []}, {data: [], nextCursor: 7}, {data: {}, nextCursor: null}]) collect('malformed-native-page-' + collections.length, [{method: 'thread/loaded/list', reply: {result}}]);
collect('repeated-empty-page-cursor-is-bounded', [page('thread/loaded/list', [], 'same'), page('thread/loaded/list', [], 'same')]);
collect('empty-cursor-is-an-explicit-continuation', [page('thread/loaded/list', [], ''), loaded(), page('thread/list'), page('thread/list'), loaded()]);
collect('loaded-ids-must-be-unique', [loaded(['same', 'same'])]);
collect('loaded-ids-must-be-strings', [loaded([7])]);
collect('loaded-count-capacity-refuses-before-enumeration', [loaded({$repeat: 'same', count: 5001})]);
collect('loaded-accumulated-bytes-are-bounded', [page('thread/loaded/list', [{$fill: 'a', count: 5 * 1024 * 1024}], 'more'), loaded([{$fill: 'b', count: 5 * 1024 * 1024}])]);
collect('single-loaded-value-byte-boundary', [loaded([{$fill: 'x', count: 9 * 1024 * 1024}])]);
for (const extra of [{threadSource: 7}, {projectId: false}, {recencyAt: -1}, {recencyAt: 1.5}, {recencyAt: 9007199254740992}, {preview: null}, {name: 7}, {cwd: 7}, {ephemeral: null}]) collect('thread-contract-guard-' + collections.length, [loaded(), page('thread/list', [thread('one', extra)])]);
collect('thread-preview-array-reaches-schema-validation', [loaded(), page('thread/list', [thread('one', {preview: ['x']})])]);
collect('thread-summaries-share-byte-capacity', [loaded(), page('thread/list', [thread('a', {source: {$fill: 'x', count: 3 * 1024 * 1024}}), thread('b', {source: {$fill: 'x', count: 3 * 1024 * 1024}}), thread('c', {source: {$fill: 'x', count: 3 * 1024 * 1024}})])]);
collect('project-count-capacity-refuses-before-summary', [projects({$repeat: project(), count: 257, ids: true})], [c('collectProjects'), current]);
for (const bad of [null, [], project(7), project('p', {name: null}), project('p', {roots: {}}), project('p', {position: -1}), project('p', {recencyAt: '0'}), project('p', {roots: [null]}), project('p', {roots: [{}]})]) collect('project-contract-guard-' + collections.length, [projects([bad])], [c('collectProjects'), current]);
collect('duplicate-projects-remain-in-cache-after-final-check-failure', [projects([project('same'), project('same')]), projects([project('fixed')])], [c('collectProjects'), current, c('collectProjects'), current]);

const paths = readdirSync('specs/native').filter(n => /^codex-/.test(n)).flatMap(d => readdirSync('specs/native/' + d).filter(n => /^catalog(?:\.[a-z0-9]+-[a-z0-9]+)?\.json$/.test(n)).map(n => 'specs/native/' + d + '/' + n));
assert.equal(paths.length, 7);
const sourceKinds = paths.map(path => ({path}));
const schema = (sourceKinds, extra = {}) => ({type: 'object', properties: {sourceKinds}, ...extra});
const catalog = inputSchema => ({clientRequests: [{method: 'thread/list', inputSchema}]});
for (const input of [null, true, [], {}, schema({items: {enum: []}}), schema({items: {enum: ['one', 7]}}), schema({$ref: 'https://invalid/source'}), schema({$ref: '#/$defs/missing'}), schema({$ref: 1}), schema({items: {enum: ['two', 'one', 'two']}}), schema({$ref: '#/$defs/a~1b'}, {$defs: {'a~1b': {items: {enum: ['encoded-key']}}}}), {$ref: '#/$defs/root', $defs: {root: {properties: {sourceKinds: {$ref: '#/$defs/kinds'}}}, kinds: {items: {$ref: '#/$defs/item'}}, item: {enum: ['custom\ud800']}}}]) sourceKinds.push({catalog: catalog(input)});
sourceKinds.push({catalog: {clientRequests: []}});

const snapshot = {observedAt: '2026-10-18T00:00:00.000Z', threads: [{nativeId: 't\ud800', summary: {raw: '\udfff'}, observedAt: 'thread-time'}], projects: {source: 'native', observedAt: 'project-time', projects: [{nativeId: 'p\udfff', source: 'native', name: 'P', paths: ['/fixture'], position: 0, recencyAt: null}]}};
const conflict = (kind = 'thread', changes = {}) => error('revision_conflict', 'not_executed', {serviceNodeId: 'owner', namespace: 'codex', kind, currentRevision: 100, ...changes});
const publications = [];
function publish(name, replies = [], commands = [c('snapshot')], extra = {}) {publications.push({name, replies, commands, ...extra});}
publish('combined-projects-use-snapshot-observation-time', [{}, {}]);
publish('thread-conflict-retries-only-once-above-authority', [{throw: conflict()}, {}, {}]);
publish('project-conflict-retries-independently', [{}, {throw: conflict('project', {currentRevision: 700})}, {}]);
publish('second-conflict-is-original-error', [{throw: conflict()}, {throw: conflict()}], [c('threads')]);
for (const outcome of ['unknown', 'completed']) publish('uncertain-conflict-is-not-retried-' + outcome, [{throw: {...conflict(), outcome}}], [c('threads')]);
publish('different-coded-error-is-not-retried', [{throw: error('service_unavailable', 'unknown')}], [c('threads')]);
publish('uncoded-error-is-not-retried', [{uncoded: true}], [c('threads')]);
for (const changes of [{serviceNodeId: 'other'}, {namespace: 'other'}, {kind: 'project'}]) publish('conflict-owner-guard-' + publications.length, [{throw: conflict('thread', changes)}], [c('threads')]);
for (const details of [undefined, null, [], {}, {...conflict().details, extra: true}, {...conflict().details, currentRevision: -1}, {...conflict().details, currentRevision: 1.5}, {...conflict().details, currentRevision: 9007199254740992}]) publish('conflict-metadata-validation-' + publications.length, [{throw: error('revision_conflict', 'not_executed', details)}], [c('threads')]);
publish('cancellation-before-publication-does-not-increment', [], [c('abort'), c('threads'), c('revision', {kind: 'thread', minimum: 0})], {signal: true});
publish('cancellation-before-retry-keeps-first-revision', [{throw: conflict(), abort: true}], [c('threads'), c('revision', {kind: 'thread', minimum: 0})], {signal: true});
publish('successful-thread-abort-does-not-publish-projects', [{abort: true}], [c('snapshot'), c('revision', {kind: 'project', minimum: 0})], {signal: true});
publish('standalone-thread-success-has-no-post-reply-abort-check', [{abort: true}], [c('threads')], {signal: true});
publish('absent-signal-does-not-observe-controller', [{abort: true}, {}]);
publish('revision-max-keeps-original-number-arithmetic', [{}, {}], [c('revision', {kind: 'thread', minimum: 9007199254740990}), c('threads'), c('revision', {kind: 'thread', minimum: 0})]);
publish('close-resets-counter-but-authority-is-still-required', [{}, {}, {throw: conflict()}, {}, {}], [c('snapshot'), c('close'), c('snapshot')]);
publish('utf16-owner-metadata-matches-exactly', [{throw: conflict('thread', {serviceNodeId: 'owner\ud800'})}, {}], [c('threads')], {ownerId: 'owner\ud800'});

const RealDate = Date; let now = 1788600000000;
globalThis.Date = class extends RealDate {constructor(...args) {super(...(args.length ? args : [now]));} static now() {return now;}};
try {
  for (const row of sourceKinds) {
    try {row.expected = {result: nativeSourceKinds(row.path ? read(row.path) : row.catalog)};} catch (e) {row.expected = {error: IvyError.from(e).toWire()};}
  }
  for (const row of collections) {
    now = 1788600000000; const controller = new AbortController(); let cursor = 0; const calls = [];
    const rpc = {catalog: catalogFor(row), request: async (method, params, hooks, budget) => {
      const step = row.replies[cursor++]; assert(step, row.name + ' unexpected request ' + method); assert.equal(method, step.method);
      calls.push({method, params: structuredClone(params), detachOnTimeout: hooks.detachOnTimeout, budget});
      now += step.advanceMs ?? 0; if (step.abort) controller.abort(fromError(reason));
      if (step.throw) throw fromError(step.throw); if (step.uncoded) throw new TypeError('Fixture native boundary.');
      return materialize(step.reply);
    }};
    const manager = new NativeInventory(rpc, 'epoch\ud800'); row.expected = [];
    for (const step of row.commands) {
      try {
        let value = null;
        if (step.action === 'abort') controller.abort(fromError(reason));
        else if (step.action === 'currentProjects') value = manager.currentProjects(step.observedAt);
        else value = await manager[step.action](controller.signal);
        row.expected.push({result: capture(value)});
      } catch (e) {row.expected.push({error: IvyError.from(e).toWire()});}
    }
    row.calls = calls; row.consumed = cursor;
  }
  for (const row of publications) {
    const ownerId = row.ownerId ?? 'owner';
    const journal = new NativeJournal({serviceNodeId: ownerId, hostId: 'host', nativeVersion: 'fixture', nativeExecutableHash: digest('fixture')}, {maxOperations: 100, maxJournalBytes: 64 * 1024 * 1024, maxPendingInputs: 10, maxNotificationBytes: 1024});
    const controller = new AbortController(); let cursor = 0; const calls = [];
    const connection = {request: async (method, params, options) => {
      const step = row.replies[cursor++]; assert(step, row.name + ' unexpected publication'); assert.equal(method, 'inventory.sync');
      assert.equal(options.signal, row.signal ? controller.signal : undefined);
      if (step.abort) controller.abort(fromError(reason));
      calls.push({method, params: structuredClone(params), signalAbortedAfterHook: options.signal?.aborted ?? false});
      if (step.throw) throw fromError(step.throw); if (step.uncoded) throw new TypeError('Fixture Hive boundary.');
      return {};
    }};
    row.expected = [];
    for (const step of row.commands) {
      try {
        let result = null; const signal = row.signal ? controller.signal : undefined;
        if (step.action === 'abort') controller.abort(fromError(reason));
        else if (step.action === 'close') journal.close();
        else if (step.action === 'revision') result = journal.inventoryRevision(step.kind, step.minimum);
        else if (step.action === 'threads') await publishNativeThreads(connection, journal, snapshot, signal);
        else if (step.action === 'projects') await publishNativeProjects(connection, journal, snapshot, signal);
        else await publishNativeSnapshot(connection, journal, snapshot, signal);
        row.expected.push({result});
      } catch (e) {row.expected.push({error: IvyError.from(e).toWire()});}
    }
    row.calls = calls; row.consumed = cursor; journal.close();
  }
  console.log(JSON.stringify({sourceKinds, collections, publications, snapshot, abortReason: reason, epoch: 'epoch\ud800'}));
} finally {globalThis.Date = RealDate;}
