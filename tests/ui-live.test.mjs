import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import ts from "typescript";
import * as vue from "vue";
import { parse, compileScript } from "@vue/compiler-sfc";
import * as nativeBackend from '../dist/packages/contracts/src/native-backend.js';
import * as nativeBackends from '../dist/packages/ui-client/src/native-backends.js';
import * as nativeRoute from '../dist/packages/ui-client/src/native-route.js';
import { nativeActionMethod, nativeActionParams } from '../dist/packages/ui-client/src/native-action.js';
import { canonical } from '../dist/packages/sdk/src/client.js';
import * as executionModels from '../dist/packages/ui-client/src/execution-models.js';

// Exercise the actual composable and SFC setup with controlled transports and time.
const require = createRequire(import.meta.url);
function load(file, bindings = {}, inlineTemplate = false) {
  let source = readFileSync(file, "utf8");
  if (file.endsWith(".vue")) source = compileScript(parse(source).descriptor, { id: file, inlineTemplate }).content;
  const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", code)(name => name in bindings ? bindings[name] : name === '../../contracts/src/native-backend.js' ? nativeBackend :
    name === './native-backends.js' ? nativeBackends : name === '../../../packages/ui-client/src/native-route' ? nativeRoute :
    name === '../../../packages/ui-client/src/execution-models' ? executionModels : require(name), module, module.exports);
  return module.exports;
}
const remote = load("packages/ui/src/composables/remote.ts");
const ui = { ...remote, ...load("packages/ui/src/composables/page-position.ts") };
const { liveUpdates, nativeInputUpdates, objectParentScope } = load("packages/ui-client/src/live.ts", { "@ivy/ui": remote });
const runtime = load("packages/ui-client/src/runtime.ts", {
  "@ivy/ui": ui, "../../sdk/src/client.js": { browserClient: () => ({}), browserNotifications: () => ({}) }, "./live": { liveUpdates },
  "../../contracts/src/ui-route.js": load("packages/contracts/src/ui-route.ts"),
});
const native = load("packages/ui-client/src/native.ts", { "../../sdk/src/client.js": {} });
const { IvyError } = load("packages/contracts/src/errors.ts");

test("native panels share owner readiness, suppress outage calls and recover without rerouting", async t => {
  t.mock.timers.enable({ apis: ["Date", "setTimeout"], now: 100000 });
  const owners = new Map([
    ["offline", { connected: true, synced: true, ready: false, desiredEnabled: true }],
    ["other", { connected: true, synced: true, ready: true, desiredEnabled: true }],
  ]);
  const checks = [], calls = [], discoveries = [];
  const client = { request: async (method, params) => {
    if (method === 'discovery.describe') return { provider: { serviceNodeId: params.serviceNodeId },
      items: params.tools.map(qualifiedName => ({ qualifiedName, method: qualifiedName, definition: {}, definitionHash: 'fixture', serviceNodeId: params.serviceNodeId })) };
    assert.equal(method, "serviceNodes.get"); checks.push(params.serviceNodeId);
    return { serviceName: 'agent-manager', ...owners.get(params.serviceNodeId) };
  } };
  const { nativeRead } = load("packages/ui-client/src/native.ts", { "../../sdk/src/client.js": {
    IvyError,
    canonical: JSON.stringify,
    discover: async (_client, method, target) => { discoveries.push(target.serviceNodeId); return { method, definition: {}, ...target }; },
    callBound: async (_client, binding) => { calls.push(binding.serviceNodeId); return {}; },
    newOperationId: async () => "read-operation",
  } });
  const burst = () => Promise.allSettled(["agent.status", "codex.model/list", "agent.inputs", "agent.notifications"].map(method => nativeRead(client, "offline", method, {})));
  const first = burst(); await settle();
  assert.deepEqual(checks, ["offline"], "panels share one owner check during an outage");
  t.mock.timers.tick(5000); const unavailable = await first;
  assert.ok(unavailable.every(result => result.status === "rejected" && result.reason.code === "service_not_ready"));
  assert.deepEqual(checks, ["offline", "offline"], "one bounded retry shares a second owner check");
  assert.equal(discoveries.length, 0);
  assert.equal(calls.length, 0, "known unavailable owners receive no failing provider calls");
  await nativeRead(client, "other", "agent.status", {});
  assert.deepEqual(calls, ["other"], "another owner remains independently usable");
  owners.get("offline").ready = true;
  t.mock.timers.tick(5000);
  assert.ok((await burst()).every(result => result.status === "fulfilled"));
  assert.equal(checks.filter(node => node === "offline").length, 3);
  const sent = calls.length;
  const cancelled = new AbortController(); cancelled.abort();
  const before = checks.length;
  await assert.rejects(nativeRead(client, "offline", "agent.status", {}, cancelled.signal));
  assert.equal(checks.length, before);
  assert.equal(calls.length, sent, "cancelled observations create no native work");
});

test('read-only provider recovery coalesces reconnect retries, cancels unused work and preserves permanent errors', async t => {
  t.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 100000 });
  let ready = false, failure = null;
  const checks = [], calls = [];
  const client = { request: async (_method, params) => {
    checks.push(params.serviceNodeId);
    return { connected: ready, synced: true, ready, desiredEnabled: true };
  } };
  const { nativeRead } = load('packages/ui-client/src/native.ts', { '../../sdk/src/client.js': {
    IvyError, canonical: JSON.stringify, newOperationId: async () => 'read-operation',
    discover: async (_client, method, target) => ({ method, definition: {}, ...target }),
    callBound: async (_client, binding) => { calls.push(binding.serviceNodeId); if (failure) throw failure; return { fresh: true }; },
  } });
  const cancelled = new AbortController();
  const abandoned = assert.rejects(nativeRead(client, 'owner', 'task-board.read', {}, cancelled.signal));
  const retained = nativeRead(client, 'owner', 'task-board.read', {});
  await settle(); assert.deepEqual(checks, ['owner']); assert.deepEqual(calls, []);
  cancelled.abort(); await abandoned; ready = true;
  await advance(t, 5000); assert.deepEqual(await retained, { fresh: true });
  assert.deepEqual(checks, ['owner', 'owner']); assert.deepEqual(calls, ['owner']);
  for (const code of ['service_unavailable', 'outcome_unknown', 'deadline_exceeded']) {
    failure = new IvyError(code, 'Read transport was interrupted.', code === 'service_unavailable' ? 'not_executed' : 'unknown');
    const reconnected = nativeRead(client, 'owner', 'task-board.read', {});
    await settle(); failure = null; await advance(t, 5000);
    assert.deepEqual(await reconnected, { fresh: true }, 'A fresh observation recovers a lost read after dispatch: ' + code);
  }
  const before = calls.length;
  failure = new IvyError('forbidden', 'Access denied.');
  await assert.rejects(nativeRead(client, 'owner', 'task-board.read', {}), { code: 'forbidden' });
  await advance(t, 5000); assert.equal(calls.length, before + 1, 'Authorization failures do not retry.');
  failure = new IvyError('service_unavailable', 'Still disconnected.');
  const persistent = assert.rejects(nativeRead(client, 'owner', 'task-board.read', {}), { message: 'Still disconnected.' });
  await settle(); await advance(t, 5000); await persistent;
  const exhausted = calls.length; await advance(t, 30000);
  assert.equal(calls.length, exhausted, 'A persistent outage exhausts its single reconnect retry.');
  ready = false; failure = null;
  const last = new AbortController(), stopped = assert.rejects(nativeRead(client, 'unused', 'task-board.read', {}, last.signal));
  await settle(); const count = checks.length; last.abort(); await stopped; await advance(t, 5000);
  assert.equal(checks.length, count, 'The last consumer cancels the retry timer and its owner check.');
});
test("native failures retain known contracts and do not block other tasks, while changed definitions refresh once", async () => {
  let version = 1, discoveries = 0;
  const client = { request: async () => ({ connected: true, synced: true, ready: true, desiredEnabled: true }) };
  const { nativeRead } = load("packages/ui-client/src/native.ts", { "../../sdk/src/client.js": {
    IvyError, canonical: JSON.stringify, newOperationId: async () => "read-operation",
    discover: async (_client, method, target) => { discoveries++; return { method, definition: {}, version, ...target }; },
    callBound: async (_client, binding, args) => {
      if (binding.version !== version) throw new IvyError("tool_definition_changed", "Changed definition");
      if (args.threadId === "broken") throw new IvyError("native_error", "Native task failed");
      return { version };
    },
  } });
  for (let attempt = 0; attempt < 3; attempt++)
    await assert.rejects(nativeRead(client, "one", "codex.thread/read", { threadId: "broken" }), { code: "native_error" });
  assert.equal(discoveries, 1, "a native failure does not invalidate its unchanged contract");
  assert.deepEqual(await nativeRead(client, "one", "codex.thread/read", { threadId: "other" }), { version: 1 });
  version = 2;
  assert.deepEqual(await nativeRead(client, "one", "codex.thread/read", { threadId: "other" }), { version: 2 });
  assert.equal(discoveries, 2, "a rejected definition is rediscovered once");
});

test("native capability discovery and identical reads are shared with independent cancellation", async t => {
  t.mock.timers.enable({ apis: ["Date"], now: 100000 });
  const discoveries = [], calls = [], signals = [];
  let release;
  const client = { request: async () => ({ connected: true, synced: true, ready: true, desiredEnabled: true }) };
  const { nativeRead, optionalTool } = load("packages/ui-client/src/native.ts", { "../../sdk/src/client.js": {
    IvyError, canonical: JSON.stringify, newOperationId: async () => "read-operation",
    discover: async (_client, method, target) => {
      discoveries.push([target.serviceNodeId, method]);
      if (method === "codex.absent") throw new IvyError("not_found", "Absent");
      return { method, definition: {}, ...target };
    },
    callBound: async (_client, binding, args, _operation, options) => {
      calls.push([binding.serviceNodeId, args]); signals.push(options.signal);
      return new Promise(resolve => { release = resolve; });
    },
  } });
  await Promise.all(Array.from({ length: 10 }, () => optionalTool(client, "one", "codex.read")));
  await Promise.all(Array.from({ length: 10 }, () => optionalTool(client, "one", "codex.absent")));
  assert.equal(discoveries.length, 2, "positive and negative discoveries coalesce and stay cached");
  const controller = new AbortController();
  const cancelled = nativeRead(client, "one", "codex.read", { threadId: "one" }, controller.signal);
  const retained = nativeRead(client, "one", "codex.read", { threadId: "one" });
  await settle();
  controller.abort();
  await assert.rejects(cancelled);
  assert.equal(signals[0].aborted, false, "one panel cannot cancel another panel's observation");
  release({ value: "observed" });
  assert.deepEqual(await retained, { value: "observed" });
  assert.equal(calls.length, 1);
  assert.equal(discoveries.length, 2, "native reads reuse optional discovery");
  await optionalTool(client, "two", "codex.read");
  assert.equal(discoveries.length, 3, "binding caches retain the exact owner");
  t.mock.timers.tick(60000);
  await optionalTool(client, "one", "codex.absent");
  assert.equal(discoveries.length, 4, "absent capabilities can recover after cache expiry");
});

test('native capability bursts gather bounded exact definitions and isolate missing or large methods and other owners', async () => {
  const requests = [], singles = [];
  const definition = method => ({ qualifiedName: method, method, definition: {}, definitionHash: 'fixture' });
  const client = { request: async (method, params) => {
    if (method === 'serviceNodes.get') return { serviceName: 'agent-manager' };
    assert.equal(method, 'discovery.describe');
    requests.push(params);
    assert.ok(params.tools.length <= 5);
    if (params.tools.includes('codex.absent')) throw new IvyError('not_found', 'Optional method absent');
    if (params.tools.includes('codex.large')) throw new IvyError('result_too_large', 'Detail budget exceeded');
    return { provider: { serviceNodeId: params.serviceNodeId }, items: params.tools.map(definition) };
  } };
  const { optionalTool } = load('packages/ui-client/src/native.ts', { '../../sdk/src/client.js': {
    IvyError, canonical: JSON.stringify,
    discover: async (_client, method, target) => {
      singles.push([target.serviceNodeId, method]);
      if (method === 'codex.absent') throw new IvyError('not_found', 'Optional method absent');
      return { ...definition(method), ...target };
    },
  } });
  const methods = Array.from({ length: 12 }, (_, index) => 'codex.read' + index);
  const found = await Promise.all([...methods, methods[0]].map(method => optionalTool(client, 'one', method)));
  assert.equal(requests.length, 3, 'twelve distinct definitions use three bounded operations');
  assert.equal(singles.length, 0);
  assert.equal(found[0], found.at(-1), 'duplicate readers retain one exact binding');
  assert.ok(found.every(binding => binding.serviceNodeId === 'one'));
  const mixed = await Promise.all(['codex.read', 'codex.absent', 'codex.large'].map(method => optionalTool(client, 'two', method)));
  assert.equal(mixed[1], undefined);
  assert.equal(mixed[2].qualifiedName, 'codex.large', 'a large individual schema uses normal exact discovery');
  assert.ok(singles.every(([node]) => node === 'two'));
  const sent = requests.length + singles.length;
  await optionalTool(client, 'two', 'codex.absent');
  assert.equal(requests.length + singles.length, sent, 'known absence is shared');
});

test('batched reads discover the selected service owner across non-agent providers', async () => {
  const owners = new Map([
    ['tasks', { serviceName: 'task-board', methods: ['task-board.workspace', 'task-board.configuration'] }],
    ['secretary', { serviceName: 'secretary', methods: ['secretary.binding', 'secretary.assignments'] }],
    ['agents', { serviceName: 'agent-manager', methods: ['agent.status', 'codex.model/list'] }],
  ]);
  const checks = [], descriptions = [], calls = [];
  const client = { request: async (method, params) => {
    const owner = owners.get(params.serviceNodeId);
    if (method === 'serviceNodes.get') {
      checks.push(params.serviceNodeId);
      return { serviceName: owner.serviceName, connected: true, synced: true, ready: true, desiredEnabled: true };
    }
    assert.equal(method, 'discovery.describe');
    assert.equal(params.serviceName, owner.serviceName);
    descriptions.push(params.serviceNodeId);
    return { provider: { serviceNodeId: params.serviceNodeId },
      items: params.tools.map(qualifiedName => ({ qualifiedName, definition: {}, definitionHash: 'fixture' })) };
  } };
  const { nativeRead } = load('packages/ui-client/src/native.ts', { '../../sdk/src/client.js': {
    IvyError, canonical: JSON.stringify, newOperationId: async () => 'read-operation',
    discover: async () => assert.fail('Concurrent methods should share a batch'),
    callBound: async (_client, binding) => { calls.push(binding.serviceNodeId); return {}; },
  } });
  await Promise.all([...owners].flatMap(([node, owner]) => owner.methods.map(method => nativeRead(client, node, method, {}))));
  assert.deepEqual(checks.sort(), ['agents', 'secretary', 'tasks'], 'discovery reuses the shared readiness observation');
  assert.deepEqual(descriptions.sort(), ['agents', 'secretary', 'tasks']);
  assert.deepEqual(calls.sort(), ['agents', 'agents', 'secretary', 'secretary', 'tasks', 'tasks']);
});

test('native catalog observations expire only that owner and metadata failures never substitute a binding', async () => {
  let version = 'one', fail = false, discoveries = 0;
  const definition = method => ({ qualifiedName: method, method, definition: {}, definitionHash: version });
  const client = { request: async (method, params) => {
    if (method === 'serviceNodes.get') return { serviceName: 'agent-manager', connected: true, ready: true, synced: true, desiredEnabled: true };
    assert.equal(method, 'discovery.describe');
    if (fail) throw new IvyError('service_unavailable', 'Closed');
    return { provider: { serviceNodeId: params.serviceNodeId }, items: params.tools.map(definition) };
  } };
  const { nativeRead, optionalTool } = load('packages/ui-client/src/native.ts', { '../../sdk/src/client.js': {
    IvyError, canonical: JSON.stringify,
    discover: async (_client, method, target) => { discoveries++; return { ...definition(method), ...target }; },
    callBound: async (_client, binding) => ({ serviceNodeId: binding.serviceNodeId, epoch: 'epoch', nativeVersion: 'native', nativeExecutableHash: 'executable', catalogHash: version }),
  } });
  await nativeRead(client, 'one', 'agent.status', {});
  const first = await optionalTool(client, 'one', 'codex.read'), other = await optionalTool(client, 'other', 'codex.read');
  version = 'two';
  await nativeRead(client, 'one', 'agent.status', {});
  assert.notEqual(await optionalTool(client, 'one', 'codex.read'), first);
  assert.equal(await optionalTool(client, 'other', 'codex.read'), other);
  fail = true;
  const before = discoveries;
  const failed = await Promise.allSettled(['codex.a', 'codex.b'].map(method => optionalTool(client, 'failed', method)));
  assert.ok(failed.every(result => result.status === 'rejected' && result.reason.code === 'service_unavailable'));
  assert.equal(discoveries, before, 'unavailability does not trigger extra single lookups');
  fail = false;
  assert.ok((await Promise.all(['codex.a', 'codex.b'].map(method => optionalTool(client, 'failed', method)))).every(Boolean));
});
const renderer = vue.createRenderer({
  createElement: () => ({}), createText: () => ({}), createComment: () => ({}),
  setText() {}, setElementText() {}, patchProp() {}, insert() {}, remove() {},
  parentNode: () => null, nextSibling: () => null,
});
class Notifications {
  connected = true;
  changes = new Map();
  statuses = new Set();
  providers = new Map();
  subscribeChanges(scopes, callback) { this.changes.set(callback, scopes); return () => this.changes.delete(callback); }
  subscribe(filter, callback) { this.providers.set(callback, filter); return () => this.providers.delete(callback); }
  onStatus(callback) { this.statuses.add(callback); callback(this.connected); return () => this.statuses.delete(callback); }
  reconnectNow() {}
  ready(value) { this.connected = value; for (const callback of this.statuses) callback(value); }
  change(scope) {
    for (const [callback, scopes] of this.changes)
      if (scopes.some(watched => scope === watched || scope.startsWith(watched + "/") || watched.startsWith(scope + "/"))) callback([scope]);
  }
  emit(serviceNodeId, name, payload) {
    for (const [callback, filter] of this.providers)
      if (filter.name === name && (!filter.serviceNodeId || filter.serviceNodeId === serviceNodeId)) callback({ params: { payload } });
  }
}
test('UI runtime derives the Hive base and canonical link from short and legacy paths', t => {
  setup(t);
  for (const prefix of ['', '/ivy']) for (const path of ['/wiki/', '/wiki/index.html', '/ui/wiki-ui/', '/ui/wiki-ui/index.html']) {
    globalThis.location = new URL('https://fixture.invalid' + prefix + path);
    const current = runtime.uiRuntime({ uiId: 'wiki-ui', slug: 'wiki' });
    assert.equal(current.base.href, 'https://fixture.invalid' + prefix + '/');
    assert.equal(current.uiUrl, 'https://fixture.invalid' + prefix + '/wiki/');
  }
  globalThis.location = new URL('https://fixture.invalid/ui/custom-ui/');
  assert.equal(runtime.uiRuntime({ uiId: 'custom-ui' }).uiUrl, 'https://fixture.invalid/ui/custom-ui/');
  globalThis.location = new URL('https://fixture.invalid/unrelated/');
  assert.throws(() => runtime.uiRuntime({ uiId: 'wiki-ui', slug: 'wiki' }), /Hive URL/);
});
function setup(t) {
  const globals = ["window", "document", "sessionStorage", "location"];
  const previous = globals.map(key => Object.getOwnPropertyDescriptor(globalThis, key));
  globalThis.window = new EventTarget();
  globalThis.document = Object.assign(new EventTarget(), { hidden: false });
  const storage = new Map();
  Object.defineProperty(globalThis, "sessionStorage", { configurable: true, value: {
    get length() { return storage.size; }, key: index => [...storage.keys()][index] ?? null,
    getItem: key => storage.get(key) ?? null,
    setItem: (key, value) => storage.set(key, value), removeItem: key => storage.delete(key),
  } });
  globalThis.location = new URL("https://fixture.invalid/");
  t.mock.timers.enable({ apis: ["setTimeout", "setInterval"] });
  const apps = [];
  t.after(() => {
    for (const app of apps) app.unmount();
    for (const [i, key] of globals.entries())
      if (previous[i]) Object.defineProperty(globalThis, key, previous[i]); else delete globalThis[key];
  });
  return (component, props = {}, notifications = new Notifications()) => {
    const app = renderer.createApp({ ...component, render: () => null }, props);
    app.use(liveUpdates(notifications));
    app.mount({}); apps.push(app);
    return app._instance.setupState;
  };
}
async function settle() { for (let i = 0; i < 20; i++) await Promise.resolve(); await vue.nextTick(); }
async function advance(t, ms) { t.mock.timers.tick(ms); await settle(); }

function taskDetailFixture(t, options = {}) {
  const mount = setup(t), previous = Object.getOwnPropertyDescriptor(globalThis, 'matchMedia');
  Object.defineProperty(globalThis, 'matchMedia', { configurable: true, value: () => ({ matches: true, addEventListener() {}, removeEventListener() {} }) });
  t.after(() => { if (previous) Object.defineProperty(globalThis, 'matchMedia', previous); else delete globalThis.matchMedia; });
  const workspace = vue.reactive({ serviceNodeId: 'owner', callerPrincipalId: 'caller', principalId: 'workflow', rootObjectId: 'root', role: 'user', scheduler: { enabled: false } });
  const view = vue.ref({ object: { objectId: 'ticket', revision: 7 }, archived: false,
    observation: { runtimeEpoch: 'hive', sequence: 10 }, openQuestions: [], task: {
    taskKey: 'TASK-1', workflowState: 'done', claim: null, publication: { objectId: 'publication', revision: 1 },
    fields: { title: 'Saved ticket', description: '', acceptanceCriteria: [], control: 'user', nativeOptions: null, executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' } },
    comments: [], commentDeliveries: [], attachments: [], dependencies: [],
  } });
  const refreshError = vue.ref('Refresh interrupted'), calls = [], reads = [], comments = [], archives = [], stats = [], forgotten = [];
  const behavior = { outcome: { task: { objectId: 'ticket', revision: 12 } }, commentAccepted: false, archiveResult: null, stat: null };
  const action = { saved: vue.ref(null), busy: vue.ref(false), locked: vue.ref(false), error: vue.ref(null), identityMatches: vue.ref(true),
    start: async (label, draft, archiveBase) => { const request = typeof draft === 'function' ? await draft() : draft;
      calls.push({ label, request, ...(archiveBase ? { archiveBase: archiveBase() } : {}) }); return behavior.outcome; } };
  const editAction = { ...action, saved: vue.ref(null), busy: vue.ref(false), locked: vue.ref(false), error: vue.ref(null) };
  const archive = { pending: vue.ref(null), busy: vue.ref(false), error: vue.ref(null), clearError() { this.error.value = null; },
    run: async (...args) => { archives.push(args); return behavior.archiveResult; } };
  const optimistic = { pending: vue.ref(null), queued: vue.ref([]), preparing: vue.ref(false), messages: vue.ref([]), isComment: vue.ref(false),
    canSubmit: vue.computed(() => !action.locked.value), submissionIssue: vue.ref(''), sending: () => false,
    issue: () => '', submit: request => { comments.push(request); return behavior.commentAccepted; } };
  const emptyRemote = () => ({ value: vue.ref(null), loading: vue.ref(false), error: vue.ref(null), refresh: async () => {} });
  const bindings = {
    '@ivy/ui': { useRemote: (_read, interval) => interval === 15000 ? { ...emptyRemote(), value: view, error: refreshError } : emptyRemote(), markdownImageSources: () => [] },
    '@lucide/vue': {},
    '../../../packages/ui-client/src/task-board': { crossAgentComment: () => false, openQuestions: () => [] },
    './runtime': { base: new URL('https://fixture.invalid/'), client: { request: async (method, params) => {
      stats.push([method, params]); await behavior.stat;
      assert.equal(method, 'objects.stat');
      return { id: 'ticket', parentId: 'root', contractKey: 'task-board/task', currentRevision: 11,
        effectivelyArchived: view.value.archived, observation: { ...view.value.observation } };
    } }, tr: (_de, en) => en, taskRoute: () => '', taskUrl: () => '',
      readDocument: async (...args) => { reads.push(args); return { value: { ...view.value.task, publication: null }, pin: { objectId: 'ticket', revision: 11 } }; } },
    './board': { taskContext: value => value, transitions: () => [], stateIcons: {}, stateColors: {} },
    './action': { useAction: (_workspace, scope) => scope.startsWith('edit:') ? editAction : action },
    './status-projection': load('ui/task-board-ui/src/status-projection.ts'),
    './execution-defaults': { useExecutionDefaults: () => ({ owner: vue.ref(null), defaults: vue.ref(null) }) },
    './comment-preferences': { useCommentPreferences: () => ({ preferences: vue.ref({ order: 'oldest-first' }) }) },
    './comment-visibility': { useCommentVisibility: () => vue.ref(true) },
    './optimistic-comment': { useOptimisticComment: (...args) => {
      behavior.commentEnabled = args[5]; behavior.commentReady = args[6]; return optimistic;
    } },
    './attachment-images': { useAttachmentImages: () => ({ urls: vue.ref({}), files: vue.ref({}), pin() {} }),
      attachmentSource: id => 'attachment:' + id, sourceAttachmentId: source => source?.slice('attachment:'.length) },
    './attachment-view': { attachmentView: () => 'file' }, '../../../packages/ui-client/src/object-archive': { useObjectArchive: () => archive },
    '../../../packages/ui-client/src/native': {}, '../../../packages/ui-client/src/native-backends': { sameBackend: () => false, nativeBackendKey: () => '' },
    '../../../packages/ui-client/src/native-route': {}, '../../../packages/sdk/src/client.js': {}, '../../../packages/sdk/src/project-inventory.js': {},
    './task-read': { forgetTask: (...args) => forgotten.push(args) },
    './comment-pages': { useCommentPages: () => ({ latestComments: vue.computed(() => view.value?.task.comments ?? []),
      comments: vue.computed(() => view.value?.task.comments ?? []), historical: vue.ref(false), latest() {} }) },
  };
  for (const file of ['ArchiveOutputsHint', 'ActionState', 'TaskEditor', 'TaskHistory', 'TaskQuestions', 'TaskAttachment', 'WikiLinkPicker']) bindings['./' + file + '.vue'] = {};
  bindings['../../../packages/ui-client/src/ObjectDeleteDialog.vue'] = {};
  const component = load('ui/task-board-ui/src/TaskDetail.vue', bindings).default;
  const mountDetail = overrides => mount(component, { taskId: 'ticket', workspace, available: true, query: new URLSearchParams(), ...options, ...overrides });
  const state = mountDetail();
  return { state, mount: mountDetail, workspace, view, refreshError, behavior, action, editAction, archive, optimistic, calls, reads, comments, archives, stats, forgotten };
}

test('Task detail reuses supplied task and edit owners without constructing another action', async t => {
  const f = taskDetailFixture(t);
  const task = { ...f.action, saved: vue.ref(null) }, edit = { ...f.editAction, saved: vue.ref(null) };
  const state = f.mount({ taskActions: { task, edit } });
  await settle(); assert.equal(state.action, task); assert.equal(state.editAction, edit);
  await state.perform('Change title', { action: 'edit', fields: { title: 'Requested title' } });
  assert.equal(f.calls.length, 1);
});

test('Task detail permits current writes after a stale publication snapshot or refresh error without publishing an optimistic task', async t => {
  const f = taskDetailFixture(t); await settle();
  assert.equal(f.state.writable, true); assert.equal(f.state.canArchive, true); assert.equal(f.state.canDelete, false);
  const original = structuredClone(vue.toRaw(f.view.value));
  assert.ok(await f.state.perform('Change title', { action: 'edit', fields: { title: 'Requested title' } }));
  assert.equal(f.calls[0].request.expectedRevision, 11, 'A new action uses a fresh compact header, not the displayed stale revision.');
  assert.deepEqual(f.reads[0], ['task', 'ticket', 'root', undefined, { limit: 0 }]);
  assert.deepEqual(vue.toRaw(f.view.value), original, 'An operation result does not replace the authoritative task snapshot.');
  f.state.commentBody = 'Retained draft'; f.state.pendingAttachments = [{ attachmentId: 'file' }];
  await f.state.submitComment();
  assert.equal(f.comments.length, 1); assert.equal(f.state.commentBody, 'Retained draft');
  assert.deepEqual(f.state.pendingAttachments, [{ attachmentId: 'file' }], 'A refused local submission retains the text and files.');
  f.behavior.outcome = null; f.action.error.value = 'Revision conflict';
  assert.equal(await f.state.perform('Rejected edit', { action: 'edit', fields: {} }), null);
  assert.deepEqual(vue.toRaw(f.view.value), original); assert.equal(f.state.commentBody, 'Retained draft');
  await f.state.setArchived(); assert.equal(f.archives.length, 0);
  assert.deepEqual(f.calls.at(-1).request, { action: 'archive', taskId: 'ticket', expectedRevision: 11, archived: true });
  assert.deepEqual(f.stats, [['objects.stat', { objectId: 'ticket', observe: true }]]);
  f.view.value.archived = true; f.view.value.observation.sequence = 20; await settle();
  assert.equal(f.state.writable, false); assert.equal(f.state.canArchive, true);
  await f.state.setArchived(); assert.equal(f.calls.at(-1).request.archived, false, 'Restore remains available for an archived ticket.');
});

test('Task detail retains permission, availability, archive and original-operation gates without dropping a draft', async t => {
  const f = taskDetailFixture(t); await settle(); f.state.commentBody = 'Keep this';
  const blocked = async (message, archiveAllowed = false) => {
    assert.equal(f.state.writable, false, message); assert.equal(f.state.canArchive, archiveAllowed, message);
    await f.state.perform('Blocked edit', { action: 'edit', fields: {} }); await f.state.submitComment();
    if (!archiveAllowed) await f.state.setArchived();
    assert.equal(f.calls.length, 0); assert.equal(f.comments.length, 0); assert.equal(f.archives.length, 0);
    assert.equal(f.state.commentBody, 'Keep this');
  };
  const saved = f.view.value; f.view.value = null; await settle(); await blocked('No loaded task'); f.view.value = saved;
  f.workspace.role = 'reader'; await settle(); await blocked('Read-only caller'); f.workspace.role = 'user';
  f.action.locked.value = true; await settle(); await blocked('Unresolved original operation'); f.action.locked.value = false;
  f.archive.busy.value = true; await settle(); await blocked('Archive dispatch is active'); f.archive.busy.value = false;
  f.archive.pending.value = { mutationId: 'original-archive' }; await settle(); await blocked('Original archive remains unresolved'); f.archive.pending.value = null;
  f.view.value.archived = true; await settle(); await blocked('Archived comments stay read-only', true); f.view.value.archived = false;
  f.view.value.task.claim = { target: { hostId: 'host' } }; await settle();
  assert.equal(f.state.writable, true); assert.equal(f.state.canArchive, false); await f.state.setArchived(); assert.equal(f.archives.length, 0);
  f.view.value.task.claim = null; f.view.value.task.publication = null; await settle(); assert.equal(f.state.canDelete, true);
  const offline = f.mount({ available: false }); await settle();
  assert.equal(offline.writable, false); assert.equal(offline.canArchive, false); assert.equal(offline.canDelete, false);
  offline.commentBody = 'Offline draft'; await offline.submitComment(); await offline.setArchived();
  assert.equal(f.comments.length, 0); assert.equal(f.archives.length, 0); assert.equal(offline.commentBody, 'Offline draft');
});

test('Task detail coordinates the selected board action while retaining comment drafts and independent tickets', async t => {
  const f = taskDetailFixture(t, { boardBlocked: true }); await settle();
  f.view.value.task.publication = null;
  assert.equal(f.state.writable, false); assert.equal(f.state.editAvailable, false);
  assert.equal(f.state.canArchive, false); assert.equal(f.state.canDelete, false);
  assert.equal(f.behavior.commentEnabled(), true); assert.equal(f.behavior.commentReady(), false);
  f.state.commentBody = 'Keep this draft';
  await f.state.perform('Blocked transition', { action: 'transition', workflowState: 'todo' });
  await f.state.setArchived(); assert.equal(f.calls.length, 0); assert.equal(f.stats.length, 0);
  f.behavior.commentAccepted = true; await f.state.submitComment();
  assert.equal(f.comments[0].body, 'Keep this draft'); assert.equal(f.state.commentBody, '');
  const independent = f.mount({ boardBlocked: false }); await settle();
  assert.equal(independent.writable, true); assert.equal(independent.canArchive, true);
  assert.equal(f.behavior.commentReady(), true);
});

test('Task detail uses confirmed board archive metadata for write gates and preserves a newer actual restore', async t => {
  const receipt = { id: 'ticket', contractKey: 'task-board/task', archivedAt: '2026-01-01T00:00:00Z',
    effectivelyArchived: true, observation: { runtimeEpoch: 'hive', sequence: 20 } };
  const f = taskDetailFixture(t, { boardArchive: receipt }); await settle();
  assert.equal(f.state.displayArchived, true); assert.equal(f.state.task.value.value.archived, true);
  assert.equal(f.state.writable, false); assert.equal(f.behavior.commentEnabled(), false);
  assert.equal(f.state.canArchive, true, 'A confirmed archive still permits an explicit restore.');
  f.view.value = { ...f.view.value, archived: false, observation: { runtimeEpoch: 'hive', sequence: 30 } }; await settle();
  assert.equal(f.state.displayArchived, false); assert.equal(f.state.writable, true);
  f.view.value = { ...f.view.value, archived: true, observation: { runtimeEpoch: 'hive', sequence: 15 } }; await settle();
  assert.equal(f.state.displayArchived, false); assert.equal(f.state.writable, true);
  f.view.value = { ...f.view.value, archived: false, observation: { runtimeEpoch: 'new-hive', sequence: 1 } }; await settle();
  assert.equal(f.state.displayArchived, false); assert.equal(f.state.writable, true);
});

test('Task detail displays prepared board status while newer actual states and write permissions remain authoritative', async t => {
  const peer = vue.reactive({ card: boardCard('ticket', 7), state: 'todo', statusPending: true });
  const f = taskDetailFixture(t, { boardBlocked: true, boardProjection: peer }); await settle();
  assert.equal(f.state.displayStatus, 'todo'); assert.equal(f.state.task.value.value.value.workflowState, 'done');
  assert.equal(f.state.writable, false); assert.equal(f.state.canArchive, false);
  f.view.value = { ...f.view.value, object: { objectId: 'ticket', revision: 8 }, task: { ...f.view.value.task, workflowState: 'cancelled' } }; await settle();
  assert.equal(f.state.displayStatus, 'cancelled');
  f.view.value = { ...f.view.value, object: { objectId: 'ticket', revision: 7 }, task: { ...f.view.value.task, workflowState: 'done' } }; await settle();
  assert.equal(f.state.displayStatus, 'cancelled', 'A late old detail read cannot revive the peer intent.');
  peer.confirmedRevision = 9; peer.state = 'waiting';
  f.view.value = { ...f.view.value, object: { objectId: 'ticket', revision: 9 }, task: { ...f.view.value.task, workflowState: 'todo' } }; await settle();
  assert.equal(f.state.displayStatus, 'todo', 'A read including the peer completion decides its actual state.');
});

test('Task detail displays prepared board archive intent without promoting it to business state or crossing runtime epochs', async t => {
  const peer = vue.reactive({ card: archiveCard('ticket'), state: 'done', archived: true, archivePending: true, archiveEpoch: 'hive' });
  const f = taskDetailFixture(t, { boardBlocked: true, boardProjection: peer }); await settle();
  assert.equal(f.state.displayArchived, true); assert.equal(f.state.task.value.value.archived, false);
  assert.equal(f.state.writable, false); assert.equal(f.behavior.commentEnabled(), true); assert.equal(f.behavior.commentReady(), false);
  f.view.value = { ...f.view.value, observation: { runtimeEpoch: 'replacement', sequence: 1 } }; await settle();
  assert.equal(f.state.displayArchived, false); assert.equal(f.state.task.value.value.archived, false);
});

test('Task detail reuses newer actual board card observations across navigation without treating a detail mirror as a second intent', async t => {
  const peer = vue.reactive({ card: { ...archiveCard('ticket', true, 30), object: { objectId: 'ticket', revision: 9 },
    task: { ...boardCard('ticket').task, workflowState: 'cancelled' } }, state: 'todo', archived: false,
    statusPending: true, archivePending: true, archiveEpoch: 'hive', fromDetail: true });
  const f = taskDetailFixture(t, { boardProjection: peer }); await settle();
  assert.equal(f.state.displayStatus, 'cancelled'); assert.equal(f.state.displayArchived, true);
  assert.equal(f.state.task.value.value.archived, true); assert.equal(f.state.writable, false);
  f.view.value = { ...f.view.value, archived: false, observation: { runtimeEpoch: 'hive', sequence: 15 } }; await settle();
  assert.equal(f.state.displayStatus, 'cancelled'); assert.equal(f.state.displayArchived, true);
});

test('Task detail keeps a rejected change visible while later comments reuse the action journal', async t => {
  const f = taskDetailFixture(t); await settle();
  f.optimistic.pending.value = { request: { commentId: 'waiting' } };
  f.action.saved.value = { label: 'Move task', phase: 'failed', detail: 'Change rejected', call: {
    operationId: 'failed-change', arguments: { action: 'transition', taskId: 'ticket' },
  } };
  assert.deepEqual(f.state.failedChange, { operationId: 'failed-change', message: 'Move task: Change rejected' });
  f.action.saved.value = { label: 'Add comment', phase: 'succeeded', outcome: { task: { objectId: 'ticket', revision: 12 } }, call: {
    operationId: 'later-comment', arguments: { action: 'comment', taskId: 'ticket' },
  } };
  await settle(); assert.equal(f.state.failedChange.operationId, 'failed-change');
  f.state.failedChange = null;
  assert.equal(f.action.saved.value.call.operationId, 'later-comment', 'Dismissing the notice leaves the current operation intact.');
  assert.equal(f.calls.length, 0);
});

test('Task detail projects a requested status during preparation without changing business state or enabling follow-up actions', async t => {
  const f = taskDetailFixture(t); await settle();
  let release;
  f.action.start = async () => new Promise(resolve => { release = resolve; });
  const original = structuredClone(vue.toRaw(f.view.value));
  const change = f.state.transition('todo');
  assert.equal(f.state.displayStatus, 'todo', 'The header and status property update before the fresh-header read finishes.');
  assert.equal(f.state.terminal, true, 'Business rules still read the authoritative Done task.');
  assert.equal(f.state.canDelete, false, 'A projected status cannot remove the publication guard.');
  assert.deepEqual(vue.toRaw(f.view.value), original);
  f.action.error.value = 'Preparation was not sent'; release(null); await change;
  assert.equal(f.state.displayStatus, 'done', 'Unsent preparation failure removes only its own projection.');
  assert.deepEqual(vue.toRaw(f.view.value), original);
});

function statusProjectionFixture(t, retained = null, mount = setup(t)) {
  const workspace = vue.reactive({ serviceNodeId: 'owner', callerPrincipalId: 'caller', principalId: 'workflow', rootObjectId: 'root' });
  const view = vue.ref({ object: { objectId: 'ticket', revision: 10 }, task: { workflowState: 'done' } });
  const action = { saved: vue.ref(retained), identityMatches: vue.ref(true) };
  const record = (operationId = 'move', state = 'todo') => ({ phase: 'unknown', outcome: null, call: { qualifiedName: 'task-board.transition', operationId,
    arguments: { action: 'transition', taskId: 'ticket', workflowState: state, expectedRevision: 10 } } });
  const { useStatusProjection } = load('ui/task-board-ui/src/status-projection.ts');
  let projection;
  mount({ setup() { projection = useStatusProjection(() => workspace, 'ticket', action, () => view.value); return {}; } });
  const read = (revision, state) => { view.value = { object: { objectId: 'ticket', revision }, task: { workflowState: state } }; };
  return { projection, workspace, view, action, record, read, mount };
}

test('Status projection retains original unknown and confirmed changes across comment journal reuse and stale reads', async t => {
  const f = statusProjectionFixture(t), p = f.projection;
  const cancel = p.begin('todo'); assert.equal(p.state.value, 'todo');
  f.action.saved.value = f.record(); cancel();
  assert.equal(p.state.value, 'todo'); assert.equal(f.action.saved.value.phase, 'unknown', 'Display never promotes acceptance to success.');
  f.action.saved.value.outcome = { task: { objectId: 'ticket', revision: 12 } }; f.action.saved.value.phase = 'succeeded';
  f.action.saved.value = { phase: 'succeeded', call: { operationId: 'later-comment', qualifiedName: 'task-board.comment', arguments: { action: 'comment', taskId: 'ticket' } } };
  f.read(11, 'done'); assert.equal(p.state.value, 'todo', 'A stale refresh cannot undo the confirmed display while comments reuse the journal.');
  f.read(12, 'todo'); f.read(10, 'done'); assert.equal(p.state.value, 'todo');
  f.read(15, 'waiting'); f.read(12, 'todo'); assert.equal(p.state.value, 'waiting', 'A later authoritative change wins and stays visible.');
  p.begin('cancelled'); assert.equal(p.state.value, 'cancelled', 'The next display intent uses the latest observed watermark, even after an older raw read.');
  f.action.saved.value = f.record('second', 'cancelled'); f.action.saved.value.phase = 'failed';
  assert.equal(p.state.value, 'waiting', 'A rejection returns to the latest authoritative status without rolling back somebody else.');
  assert.equal(f.view.value.task.workflowState, 'todo', 'No projected or watermark state is written into the task.');
});

test('Status projection orders concurrent server changes against the original committed revision', async t => {
  const f = statusProjectionFixture(t); f.projection.begin('todo');
  f.read(11, 'waiting'); assert.equal(f.projection.state.value, 'waiting', 'A newer server status wins during preparation.');
  f.action.saved.value = f.record(); f.action.saved.value.call.arguments.expectedRevision = 11;
  f.action.saved.value.outcome = { task: { objectId: 'ticket', revision: 12 } }; f.action.saved.value.phase = 'succeeded';
  assert.equal(f.projection.state.value, 'todo', 'The original later commit supersedes an earlier read even when that read changed the status.');
  f.read(13, 'cancelled'); assert.equal(f.projection.state.value, 'cancelled');
  f.read(11, 'waiting'); assert.equal(f.projection.state.value, 'cancelled', 'A still later remote change wins over both the receipt and old reads.');
  assert.equal(f.view.value.task.workflowState, 'waiting');
});

test('Status projection recovers from the existing original journal and excludes foreign tasks and workspace identities', async t => {
  const retained = { phase: 'unknown', outcome: null, call: { operationId: 'original', qualifiedName: 'task-board.transition',
    arguments: { action: 'transition', taskId: 'ticket', expectedRevision: 10, workflowState: 'todo' } } };
  const f = statusProjectionFixture(t, retained), original = structuredClone(retained);
  assert.equal(f.projection.state.value, 'todo'); assert.deepEqual(vue.toRaw(f.action.saved.value), original);
  f.read(11, 'waiting'); assert.equal(f.projection.state.value, 'waiting', 'A concurrent newer status wins while the original remains unresolved.');
  f.action.identityMatches.value = false; assert.equal(f.projection.state.value, 'waiting');
  f.action.identityMatches.value = true; f.workspace.callerPrincipalId = 'another'; f.read(12, 'done');
  assert.equal(f.projection.state.value, 'done', 'The previous caller projection never follows a new workspace.');
  assert.deepEqual(vue.toRaw(f.action.saved.value), original, 'Recovery display does not replay or replace the original operation.');
  const foreign = statusProjectionFixture(t, { ...retained, call: { ...retained.call, arguments: { ...retained.call.arguments, taskId: 'another' } } }, f.mount);
  assert.equal(foreign.projection.state.value, 'done');
});

function contentProjectionFixture(t, retained = null) {
  const mount = setup(t), workspace = vue.reactive({ serviceNodeId: 'owner', callerPrincipalId: 'caller', principalId: 'workflow', rootObjectId: 'root' });
  const fields = { title: 'Saved title', description: 'Saved text', acceptanceCriteria: ['Saved criterion'], category: 'Saved category', priority: 2 };
  const view = vue.ref({ object: { objectId: 'ticket', revision: 10 }, task: { fields } });
  const action = { saved: vue.ref(retained), identityMatches: vue.ref(true), error: vue.ref(null) }; let projection;
  const { useContentProjection } = load('ui/task-board-ui/src/status-projection.ts');
  mount({ setup() { projection = useContentProjection(() => workspace, 'ticket', action, () => view.value); return {}; } });
  const intent = { title: 'Requested title', description: 'Requested text', acceptanceCriteria: ['Requested criterion'], category: 'Requested category', priority: 4 };
  const record = (operationId = 'original', values = intent) => ({ phase: 'unknown', outcome: null,
    call: { operationId, qualifiedName: 'task-board.edit', arguments: { action: 'edit', taskId: 'ticket', expectedRevision: 10, fields: structuredClone(values) } } });
  const read = (revision, values) => { view.value = { object: { objectId: 'ticket', revision }, task: { fields: { ...fields, ...values } } }; };
  return { projection, workspace, view, action, fields, intent, record, read };
}

test('Content projection displays an edit during preparation and rejects it without changing the business task', async t => {
  const f = contentProjectionFixture(t), original = structuredClone(vue.toRaw(f.view.value));
  const cancel = f.projection.begin(f.intent); assert.deepEqual(f.projection.state.value, f.intent);
  assert.deepEqual(vue.toRaw(f.view.value), original); cancel(); assert.deepEqual(f.projection.state.value, f.fields);
  const keepOriginal = f.projection.begin(f.intent); f.action.saved.value = f.record(); keepOriginal();
  assert.deepEqual(f.projection.state.value, f.intent, 'A bound unknown original is not an unsent preparation.');
  f.action.saved.value.phase = 'prepared'; f.action.error.value = 'The original request could not be retained';
  assert.deepEqual(f.projection.state.value, f.fields, 'A known unsent storage failure keeps the draft without pretending the edit is still underway.');
  f.action.error.value = null; f.action.saved.value.phase = 'unknown';
  f.read(11, { title: 'Remote title' }); assert.equal(f.projection.state.value.title, 'Remote title');
  f.action.saved.value.phase = 'failed'; assert.equal(f.projection.state.value.title, 'Remote title');
  assert.equal(f.view.value.task.fields.title, 'Remote title');
});

test('Content projection orders the original commit against newer fields and ignores stale reads', async t => {
  const f = contentProjectionFixture(t); f.projection.begin(f.intent);
  f.read(11, { title: 'Concurrent edit', acceptanceCriteria: ['Remote criterion'] });
  assert.equal(f.projection.state.value.title, 'Concurrent edit');
  f.action.saved.value = f.record(); f.action.saved.value.outcome = { task: { objectId: 'ticket', revision: 12 } }; f.action.saved.value.phase = 'succeeded';
  assert.deepEqual(f.projection.state.value, f.intent, 'A later original commit may supersede an earlier read.');
  f.read(13, { description: 'Later remote text', priority: 1 });
  assert.equal(f.projection.state.value.description, 'Later remote text'); assert.equal(f.projection.state.value.priority, 1);
  f.read(10, f.fields); assert.equal(f.projection.state.value.description, 'Later remote text');
  assert.equal(f.view.value.task.fields.description, 'Saved text', 'The raw read remains distinct from its display watermark.');
});

test('Content projection restores only the matching retained edit and excludes other owners', async t => {
  const retained = { phase: 'unknown', outcome: null, call: { operationId: 'retained', qualifiedName: 'task-board.edit',
    arguments: { action: 'edit', taskId: 'ticket', expectedRevision: 10, fields: { title: 'Retained title', description: '', acceptanceCriteria: [], category: null, priority: 0 } } } };
  const f = contentProjectionFixture(t, retained), original = structuredClone(retained);
  assert.equal(f.projection.state.value.title, 'Retained title'); assert.deepEqual(vue.toRaw(f.action.saved.value), original);
  f.action.identityMatches.value = false; assert.equal(f.projection.state.value.title, 'Saved title');
  f.action.identityMatches.value = true; f.workspace.callerPrincipalId = 'another'; assert.equal(f.projection.state.value.title, 'Saved title');
  assert.deepEqual(vue.toRaw(f.action.saved.value), original, 'Display recovery does not dispatch or replace an original operation.');
});

test('Task detail projects edited content while permissions and conflicting actions keep authoritative data', async t => {
  const f = taskDetailFixture(t); await settle(); f.state.panel = 'edit';
  f.action.locked.value = true; assert.equal(f.state.editAvailable, false, 'Another original ticket write blocks only edit dispatch.');
  f.action.locked.value = false; assert.equal(f.state.editAvailable, true);
  const fields = { ...vue.toRaw(f.view.value.task.fields), title: 'Projected title', description: 'Projected text', acceptanceCriteria: [], category: 'Projected category', priority: 4,
    control: 'agent', executionRequirement: { kind: 'host', hostId: 'another-host' } };
  const finish = f.state.beginEdit(fields); f.editAction.busy.value = true;
  assert.equal(f.state.panel, ''); assert.equal(f.state.displayContent.title, 'Projected title');
  assert.equal(f.view.value.task.fields.control, 'user'); assert.equal(f.view.value.task.fields.title, 'Saved ticket');
  assert.equal(f.state.writable, false); assert.equal(f.state.canArchive, false); assert.equal(f.state.canDelete, false);
  assert.equal(f.state.commentWritable, true, 'Local comment input stays available while its dispatch waits.');
  f.editAction.busy.value = false; finish({ current: { pin: { objectId: 'ticket', revision: 11 }, fields: { ...fields, title: 'Concurrent saved title' },
    wakeAt: null, dependencies: ['remote-dependency'] }, conflict: true });
  assert.equal(f.state.displayContent.title, 'Concurrent saved title'); assert.match(f.state.editIssue, /draft is retained/);
  assert.deepEqual([...f.state.editConflict.dependencies], ['remote-dependency']); assert.equal(f.state.editConflict.pin.revision, 11);
  assert.equal(f.view.value.task.fields.title, 'Saved ticket');
});

function archiveProjectionFixture(t, retained = null) {
  const mount = setup(t), workspace = vue.reactive({ serviceNodeId: 'owner', callerPrincipalId: 'caller', principalId: 'workflow', rootObjectId: 'root' });
  const observation = (sequence, runtimeEpoch = 'hive') => ({ runtimeEpoch, sequence });
  const view = vue.ref({ object: { objectId: 'ticket', revision: 7 }, archived: false, observation: observation(10) });
  const action = { saved: vue.ref(retained), identityMatches: vue.ref(true), error: vue.ref(null) };
  const record = (operationId = 'archive', archived = true) => ({ phase: 'unknown', outcome: null, archiveBase: observation(10),
    call: { qualifiedName: 'task-board.archive', operationId, arguments: { action: 'archive', operationId, taskId: 'ticket', expectedRevision: 7, archived } } });
  const metadata = (sequence, archived, runtimeEpoch = 'hive') => ({ id: 'ticket', currentRevision: 7,
    archivedAt: archived ? '2026-11-10T12:00:00Z' : null, effectivelyArchived: archived, observation: observation(sequence, runtimeEpoch) });
  const { useArchiveProjection } = load('ui/task-board-ui/src/status-projection.ts'); let projection;
  mount({ setup() { projection = useArchiveProjection(() => workspace, 'ticket', action, () => view.value); return {}; } });
  const read = (sequence, archived, runtimeEpoch = 'hive') => { view.value = {
    object: { objectId: 'ticket', revision: 7 }, archived, observation: observation(sequence, runtimeEpoch) }; };
  const succeed = (sequence, archived, runtimeEpoch = 'hive') => {
    action.saved.value.outcome = { archive: metadata(sequence, archived, runtimeEpoch) }; action.saved.value.phase = 'succeeded';
  };
  return { projection, workspace, view, action, record, read, succeed, metadata };
}

test('Archive projection separates immediate intent from confirmed metadata and restores the actual state after rejection', async t => {
  const f = archiveProjectionFixture(t), p = f.projection, original = structuredClone(vue.toRaw(f.view.value));
  const first = p.begin(true); assert.equal(p.state.value, true); assert.equal(p.actual.value, false);
  first.cancel(); assert.equal(p.state.value, false);
  const pending = p.begin(true); f.action.saved.value = f.record(); pending.cancel();
  assert.equal(p.state.value, true); assert.equal(p.actual.value, false); assert.equal(f.action.saved.value.phase, 'unknown');
  f.action.saved.value.phase = 'failed'; assert.equal(p.state.value, false);
  assert.deepEqual(vue.toRaw(f.view.value), original, 'Display does not change the task, revision or archive metadata.');
});

test('Archive projection orders same-revision receipts and reads while later journal actions retain the actual watermark', async t => {
  const f = archiveProjectionFixture(t), p = f.projection;
  p.begin(true); f.action.saved.value = f.record(); f.succeed(20, true);
  assert.equal(p.actual.value, true); assert.equal(p.state.value, true); assert.equal(f.view.value.archived, false);
  f.read(15, false); assert.equal(p.actual.value, true, 'A delayed read cannot restore the pre-archive state.');
  f.action.saved.value = { phase: 'succeeded', call: { qualifiedName: 'task-board.comment', operationId: 'later', arguments: { action: 'comment' } } };
  f.read(30, false); f.read(22, true); assert.equal(p.state.value, false, 'A newer foreign restore stays visible despite a later stale read.');
  const next = p.begin(true); f.action.saved.value = f.record('second'); next.cancel();
  f.read(50, false); f.succeed(40, true); assert.equal(p.actual.value, false, 'A foreign change read before receipt reconciliation still wins over the older original commit.');
  assert.equal(f.view.value.object.revision, 7);
});

test('Archive projection restores the exact unknown journal without replay and rejects incomparable epochs and foreign owners', async t => {
  const retained = { phase: 'unknown', outcome: null, archiveBase: { runtimeEpoch: 'hive', sequence: 10 },
    call: { qualifiedName: 'task-board.archive', operationId: 'retained', arguments: { action: 'archive', operationId: 'retained', taskId: 'ticket', expectedRevision: 7, archived: true } } };
  const f = archiveProjectionFixture(t, retained), original = structuredClone(retained);
  assert.equal(f.projection.state.value, true); assert.equal(f.projection.actual.value, false);
  assert.deepEqual(vue.toRaw(f.action.saved.value), original);
  f.read(1, false, 'another-epoch'); assert.equal(f.projection.state.value, false);
  f.succeed(200, true); assert.equal(f.projection.actual.value, false, 'An old-epoch receipt cannot override a current-epoch read.');
  f.action.identityMatches.value = false; f.view.value.archived = true; assert.equal(f.projection.state.value, true);
  f.workspace.callerPrincipalId = 'someone-else'; f.read(4, false, 'another-epoch'); assert.equal(f.projection.state.value, false);
});

test('Archive projection preserves legacy originals and rolls back known unsent storage errors without declaring success', async t => {
  const f = archiveProjectionFixture(t), p = f.projection;
  f.action.saved.value = { ...f.record(), archiveBase: undefined }; assert.equal(p.state.value, false, 'A legacy unknown request has no comparable display base.');
  f.action.saved.value = null; p.begin(true); f.action.saved.value = { ...f.record('unsent'), phase: 'prepared' };
  f.action.error.value = 'Browser storage is unavailable'; assert.equal(p.state.value, false); assert.equal(p.actual.value, false);
  assert.equal(f.action.saved.value.phase, 'prepared'); assert.equal(f.action.saved.value.call.operationId, 'unsent');
  f.action.error.value = null; f.action.saved.value.phase = 'unknown'; assert.equal(p.state.value, true);
  f.action.saved.value.phase = 'failed'; assert.equal(p.state.value, false);
});

test('Task detail displays archive intent before its compact stat completes and retains real write gates and drafts', async t => {
  const f = taskDetailFixture(t); await settle();
  let release; f.behavior.stat = new Promise(resolve => { release = resolve; }); t.after(() => release());
  f.state.commentBody = 'Preserved draft'; const original = structuredClone(vue.toRaw(f.view.value));
  const pending = f.state.setArchived(); assert.equal(f.state.displayArchived, true); assert.equal(f.state.task.value.value.archived, false);
  assert.equal(f.calls.length, 0); assert.deepEqual(f.stats, [['objects.stat', { objectId: 'ticket', observe: true }]]);
  assert.equal(f.reads.length, 0, 'Archive preparation never loads the task body or conversation.');
  assert.deepEqual(vue.toRaw(f.view.value), original); assert.equal(f.state.commentBody, 'Preserved draft');
  release(); await pending; assert.equal(f.calls.at(-1).request.action, 'archive');
  assert.equal(f.calls.at(-1).request.expectedRevision, 11); assert.equal(f.calls.at(-1).archiveBase.sequence, 10);
  assert(f.forgotten.length >= 1, 'A retained hover snapshot cannot survive the lifecycle change.');
});

function archiveFixture(t) {
  setup(t);
  const workspace = { serviceNodeId: 'owner', callerPrincipalId: 'caller', principalId: 'workflow', rootObjectId: 'root' };
  const calls = [], requests = [], bindings = [], workspaceReads = [], receipts = new Map();
  const behavior = { prepare: null, lost: false, unavailable: false, receiptAvailable: true };
  const object = { id: 'ticket', contractKey: 'task-board/task', parentId: 'root', currentRevision: 7, archivedAt: null, effectivelyArchived: false };
  const hashes = load('packages/ui-client/src/hash.ts', { '../../sdk/src/client.js': { canonical } });
  const client = { request: async (method, args) => {
    requests.push([method, JSON.parse(JSON.stringify(args))]);
    if (method === 'objects.stat') return structuredClone(object);
    if (method === 'serviceNodes.list') return args.cursor ? { items: [{ serviceNodeId: 'owner', connected: true, synced: true, ready: true }], nextCursor: null }
      : { items: [{ serviceNodeId: 'other', connected: true, synced: true, ready: true }], nextCursor: 'next-owner-page' };
    if (method === 'objects.archive') { object.archivedAt = args.archived ? '2026-01-01T00:00:00Z' : null; object.effectivelyArchived = args.archived; return structuredClone(object); }
    assert.equal(method, 'tools.call'); calls.push(JSON.parse(JSON.stringify(args)));
    object.archivedAt = args.arguments.archived ? '2026-01-01T00:00:00Z' : null; object.effectivelyArchived = args.arguments.archived;
    const result = structuredClone(object);
    receipts.set(args.operationId, { receipt: true, operationId: args.operationId, callerPrincipalId: args.expectedCallerPrincipalId ?? args.arguments.expectedWorkspace.callerPrincipalId,
      action: 'archive', taskId: args.arguments.taskId, submittedHash: await hashes.hashJson(args.arguments), phase: 'succeeded', outcome: { archive: result } });
    if (behavior.lost) throw new IvyError('outcome_unknown', 'Lost archive response', 'unknown');
    return result;
  } };
  let ids = 0;
  const helpers = load('packages/ui-client/src/object-archive.ts', {
    '@ivy/ui': { useRemote: () => ({}) }, './hash': hashes,
    '../../sdk/src/client.js': { IvyError, newOperationId: async () => { await behavior.prepare; return 'archive-operation-' + ++ids; },
      discover: async (_client, qualifiedName, target) => {
        bindings.push([qualifiedName, target.serviceNodeId]);
        if (behavior.unavailable && qualifiedName === 'task-board.archive') throw new IvyError('service_unavailable', 'Selected owner unavailable', 'not_executed');
        return { qualifiedName, ...target, definitionHash: 'archive-definition' };
      }, callBound: async (_client, binding, args) => {
        if (binding.qualifiedName === 'task-board.workspace') {
          workspaceReads.push(binding.serviceNodeId);
          return { ...workspace, serviceNodeId: binding.serviceNodeId, rootObjectId: binding.serviceNodeId === 'other' ? 'another-root' : 'root' };
        }
        assert.equal(binding.qualifiedName, 'task-board.operation');
        const receipt = receipts.get(args.operationId);
        if (!receipt || !behavior.receiptAvailable) throw new IvyError('service_unavailable', 'Original receipt unavailable', 'unknown');
        return structuredClone(receipt);
      } },
  });
  return { workspace, object, behavior, calls, requests, bindings, workspaceReads, receipts,
    client, helpers, hashes, mount: (key = 'archive-control') => helpers.useObjectArchive(client, key), ids: () => ids };
}

test('Action display keeps ordinary progress and success quiet while preserving relevant recovery and confirmed change events', async t => {
  const mount = setup(t), events = [], calls = [];
  const action = { saved: vue.ref(null), busy: vue.ref(false), error: vue.ref(null), identityMatches: vue.ref(true),
    needsOriginalFile: vue.ref(false), canRetryPreparation: vue.ref(false), canDismiss: vue.ref(false),
    reconcile: async () => { calls.push('check'); }, replay: async () => { calls.push('resume'); } };
  const component = load('ui/task-board-ui/src/ActionState.vue', {
    '@ivy/ui': {}, './runtime': { tr: (_de, en) => en, statusTone: () => 'neutral' },
  }).default;
  const state = mount(component, { action, onChanged: () => events.push('confirmed') });
  assert.equal(state.problem, false);
  action.saved.value = { phase: 'unknown', label: 'Save task', detail: 'Internal phase', call: { operationId: 'original' } };
  action.busy.value = true; assert.equal(state.problem, false, 'An ordinary in-flight request creates no progress panel.');
  action.busy.value = false; assert.equal(state.problem, true); assert.match(state.message, /not confirmed/);
  await state.check(); assert.deepEqual(events, [], 'An inspection is not a successful save.');
  action.saved.value.phase = 'failed'; action.saved.value.detail = 'Revision conflict (revision_conflict; not_executed)';
  assert.equal(state.message, 'Revision conflict'); await state.check(); assert.deepEqual(events, []);
  action.saved.value.phase = 'prepared'; assert.match(state.message, /not been sent/);
  action.replay = async () => { calls.push('resume'); action.saved.value.phase = 'succeeded'; };
  await state.check(true); assert.deepEqual(events, ['confirmed']); assert.equal(state.problem, false, 'Success needs no dismissal.');
  action.error.value = 'Browser storage is unavailable'; assert.equal(state.problem, true); assert.match(state.message, /storage/);
  action.error.value = null; action.identityMatches.value = false; assert.equal(state.problem, true); assert.match(state.message, /original account/);
  await state.check(); assert.deepEqual(events, ['confirmed'], 'A foreign identity never publishes success.');
  assert.deepEqual(calls, ['check', 'check', 'resume', 'check']);
});

test('Action preparation can decline an unchanged or conflicting edit without erasing its original journal or dispatching a write', async t => {
  const f = boardActionsFixture(t), scope = vue.effectScope(); t.after(() => scope.stop());
  const action = scope.run(() => f.actions.useAction(() => f.owner, 'edit:ticket'));
  await action.start('Original edit', { action: 'edit', taskId: 'ticket', expectedRevision: 1, fields: { title: 'Original' } });
  const original = structuredClone(vue.toRaw(action.saved.value));
  let release, duplicate = false; const gate = new Promise(resolve => { release = resolve; });
  const cancelled = action.start('Prepare another edit', async () => { await gate; return null; });
  assert.equal(action.busy.value, true); assert.equal(action.locked.value, true);
  await action.start('Duplicate save', async () => { duplicate = true; return null; });
  assert.equal(duplicate, false); release(); assert.equal(await cancelled, null);
  assert.equal(action.busy.value, false); assert.equal(action.locked.value, false);
  assert.deepEqual(vue.toRaw(action.saved.value), original); assert.equal(f.calls.length, 1);
});

function taskEditorFixture(t, options = {}) {
  const f = boardActionsFixture(t); f.owner.role = 'user';
  const document = vue.shallowRef({ pin: { objectId: 'ticket', revision: 5 }, value: { taskKey: 'TASK-1', workflowState: 'todo',
    fields: { title: 'Saved title', description: 'Saved description', acceptanceCriteria: [], category: null, control: 'user', priority: 2,
      executionRequirement: null, nativeOptions: { model: null, reasoningEffort: null, serviceTier: null },
      workspaceRequirement: { kind: 'task_workspace' }, userContact: 'ticket', allowParallel: false, dueAt: null },
    wakeAt: null, dependencies: [], attachments: [], claim: null, primaryResourceRef: null } });
  const behavior = { header: null, current: null, historical: null }, reads = [], events = [];
  const remote = () => ({ value: vue.ref(null), loading: vue.ref(false), error: vue.ref(null), refresh: async () => {} });
  const bindings = {
    '@ivy/ui': { useRemote: remote }, '@lucide/vue': {},
    './board': { taskStarted: () => false, stateLabels: { todo: 'Todo' }, priorityLabels: {}, stateLabel: value => value },
    './runtime': { base: new URL('https://fixture.invalid/'), client: {}, tr: (_de, en) => en, taskRoute: () => '#/ticket',
      readDocument: async (...args) => { reads.push(args); await behavior.header;
        return structuredClone(vue.toRaw((typeof args[1] === 'object' ? behavior.historical : behavior.current) ?? document.value)); } },
    '../../../packages/sdk/src/client.js': { canonical }, '../../../packages/ui-client/src/runtime': { route: () => '#/tasks' },
    './action': f.actions, './ActionState.vue': {}, './TaskLocationPicker.vue': {}, './WikiLinkPicker.vue': {},
    '../../../packages/ui-client/src/native': {},
    '../../../packages/ui-client/src/native-backends': { nativeBackendKey: () => '', sameBackend: () => false },
    './execution-options': { executionModels: () => [], executionBackendChoices: () => [], executionSpeedAvailable: () => false },
    './execution-defaults': { useExecutionDefaults: () => ({ defaults: vue.ref(null) }) }, './blockers': {},
    './attachment-images': { useAttachmentImages: () => ({ urls: vue.ref({}), files: vue.ref({}) }) }, './attachment-view': {},
  };
  const component = load('ui/task-board-ui/src/TaskEditor.vue', bindings).default;
  const parentScope = vue.effectScope(); t.after(() => parentScope.stop());
  const parentAction = options.parentOwnsAction ? parentScope.run(() => f.actions.useAction(() => f.owner, 'edit:ticket')) : undefined;
  let app, stopped = true;
  const stopEditor = () => { if (!stopped) { stopped = true; app.unmount(); } };
  const reopen = () => {
    stopEditor();
    app = renderer.createApp({ ...component, render: () => null }, { workspace: f.owner, available: true,
      task: document.value, action: parentAction, conflict: options.conflict, onEdit: options.onEdit, onSaved: () => events.push('saved') });
    stopped = false; app.mount({}); return app._instance.setupState;
  };
  reopen(); t.after(stopEditor);
  return { ...f, get state() { return app._instance.setupState; }, document, editorBehavior: behavior, headerReads: reads, events,
    draftKey: 'ivy:task-board:draft:https://fixture.invalid/owner:caller:ticket', parentAction, stopEditor, reopen };
}

test('Task editor locks compact header preparation once and confirms only the saved draft', async t => {
  const f = taskEditorFixture(t); f.state.form.title = 'Requested title'; await settle();
  let release; f.editorBehavior.header = new Promise(resolve => { release = resolve; });
  const saving = f.state.save(); await settle(); assert.equal(f.state.locked, true);
  await f.state.save(); assert.equal(f.headerReads.length, 1, 'Duplicate saves share the existing busy gate before reading.');
  release(); await saving; await settle();
  assert.equal(f.calls.length, 1); assert.equal(f.calls[0].arguments.expectedRevision, 5);
  assert.equal(f.calls[0].arguments.fields.title, 'Requested title');
  assert.deepEqual(f.headerReads[0], ['task', 'ticket', 'root', undefined, { limit: 0 }]);
  assert.deepEqual(f.events, ['saved']); assert.equal(sessionStorage.getItem(f.draftKey), null);
  assert.equal(f.document.value.value.fields.title, 'Saved title', 'Saving never overwrites authoritative display data.');
});

test('Task editor retains its draft after pending or failed inspection, uploads and foreign saves and closes only a matching recovered edit', async t => {
  const f = taskEditorFixture(t); f.state.form.title = 'Retained edit'; await settle(); f.behavior.lost = true;
  await f.state.save(); await settle(); assert.equal(f.state.action.saved.value.phase, 'unknown');
  const original = structuredClone(vue.toRaw(f.state.action.saved.value));
  f.state.recoveredAction(); assert.deepEqual(f.events, []); assert(sessionStorage.getItem(f.draftKey));
  f.state.action.saved.value.phase = 'failed'; f.state.recoveredAction(); await settle();
  assert.deepEqual(f.events, []); assert(sessionStorage.getItem(f.draftKey));
  f.state.action.saved.value = { ...structuredClone(original), phase: 'succeeded', outcome: { task: { objectId: 'ticket', revision: 6 } },
    call: { ...original.call, arguments: { ...original.call.arguments, action: 'uploadAttachment' } } };
  f.state.recoveredAction(); await settle(); assert.deepEqual(f.events, []); assert(sessionStorage.getItem(f.draftKey));
  f.state.action.saved.value = { ...structuredClone(original), phase: 'succeeded', outcome: { task: { objectId: 'another-ticket', revision: 6 } } };
  f.state.recoveredAction(); assert.deepEqual(f.events, []);
  f.state.action.saved.value = structuredClone(original); await settle();
  await f.state.action.reconcile(); await settle();
  assert.equal(f.calls.length, 1); assert.equal(f.state.action.saved.value.call.operationId, original.call.operationId);
  assert.deepEqual(f.events, ['saved'], 'Existing receipt reconciliation closes a matching edit without a success click.');
  assert.equal(sessionStorage.getItem(f.draftKey), null); f.state.recoveredAction(); assert.deepEqual(f.events, ['saved']);
});

test('Task editor keeps a newer draft and a conflicting server change when an older edit is confirmed', async t => {
  const f = taskEditorFixture(t); f.state.form.title = 'Original edit'; await settle();
  f.document.value.pin.revision = 6; f.document.value.value.fields.title = 'Remote edit';
  await f.state.save(); await settle(); assert(f.state.conflict); assert.equal(f.calls.length, 0); assert.equal(f.state.locked, false);
  assert.equal(f.state.form.title, 'Original edit'); assert(sessionStorage.getItem(f.draftKey));
  f.state.revision = 6; f.state.baseFields = structuredClone(vue.toRaw(f.document.value.value.fields)); f.state.conflict = null;
  f.behavior.lost = true; await f.state.save(); await settle();
  f.state.form.title = 'Later local draft'; await settle(); await f.state.action.reconcile(); await settle();
  assert.deepEqual(f.events, []); assert.equal(f.state.form.title, 'Later local draft');
  assert.equal(JSON.parse(sessionStorage.getItem(f.draftKey)).fields.title, 'Later local draft');
  assert.equal(f.document.value.value.fields.title, 'Remote edit'); assert.equal(f.calls.length, 1);
});

test('Task editor leaves its parent owned original alive when optimistic navigation closes the form', async t => {
  const submitted = [], settled = [];
  const f = taskEditorFixture(t, { parentOwnsAction: true, onEdit: fields => { submitted.push(fields.title); return result => settled.push(result); } });
  f.state.form.title = 'Requested outside form'; await settle(); let release;
  f.editorBehavior.header = new Promise(resolve => { release = resolve; });
  const save = f.state.save(); await settle(); assert.deepEqual(submitted, ['Requested outside form']);
  const originalAction = f.parentAction; assert.equal(originalAction.busy.value, true); f.stopEditor();
  release(); await save; await settle();
  assert.equal(f.calls.length, 1); assert.equal(originalAction.saved.value.phase, 'succeeded');
  assert.equal(originalAction.saved.value.call.arguments.fields.title, 'Requested outside form');
  assert.equal(settled.length, 1); assert.equal(settled[0].conflict, false); assert.equal(settled[0].current.pin.revision, 5);
  assert.equal(sessionStorage.getItem(f.draftKey), null); assert.equal(f.document.value.value.fields.title, 'Saved title');
});

test('Task editor prefills a rapid second draft from its confirmed original while a fresh header remains mandatory', async t => {
  const f = taskEditorFixture(t, { parentOwnsAction: true }); f.behavior.outcomeRevision = 6;
  f.state.form.title = 'First saved title'; await settle(); await f.state.save(); await settle();
  assert.equal(f.calls.length, 1); assert.equal(sessionStorage.getItem(f.draftKey), null);
  f.reopen(); assert.equal(f.state.form.title, 'First saved title'); assert.equal(f.state.revision, 6);
  assert.equal(f.document.value.value.fields.title, 'Saved title', 'A confirmed draft does not replace the authoritative task.');
  f.state.form.description = 'Second saved description'; await settle();
  f.editorBehavior.current = { pin: { objectId: 'ticket', revision: 6 }, value: { ...structuredClone(vue.toRaw(f.document.value.value)),
    fields: structuredClone(f.calls[0].arguments.fields) } };
  f.behavior.outcomeRevision = 7; await f.state.save(); await settle();
  assert.equal(f.headerReads.length, 2); assert.equal(f.calls.length, 2); assert.equal(f.calls[1].arguments.expectedRevision, 6);
  assert.equal(f.calls[1].arguments.fields.title, 'First saved title'); assert.equal(f.calls[1].arguments.fields.description, 'Second saved description');
  assert.equal(f.document.value.pin.revision, 5); assert.equal(sessionStorage.getItem(f.draftKey), null);
});

test('Task editor rebases only a matching retained draft after its unmounted original is confirmed', async t => {
  const f = taskEditorFixture(t, { parentOwnsAction: true }); f.behavior.outcomeRevision = 6; f.behavior.lost = true;
  f.state.form.title = 'Recovered first title'; await settle(); await f.state.save(); await settle();
  assert.equal(f.parentAction.saved.value.phase, 'unknown'); assert(sessionStorage.getItem(f.draftKey));
  f.stopEditor(); await f.parentAction.reconcile(); await settle(); f.reopen();
  assert.equal(f.state.form.title, 'Recovered first title'); assert.equal(f.state.revision, 6);
  assert.equal(f.state.baseFields.title, 'Recovered first title'); f.state.form.description = 'Next local description'; await settle();
  f.editorBehavior.current = { pin: { objectId: 'ticket', revision: 6 }, value: { ...structuredClone(vue.toRaw(f.document.value.value)),
    fields: structuredClone(f.calls[0].arguments.fields) } };
  f.behavior.lost = false; f.behavior.outcomeRevision = 7; await f.state.save(); await settle();
  assert.equal(f.calls.length, 2); assert.equal(f.calls[1].arguments.expectedRevision, 6); assert.equal(f.calls[1].arguments.fields.title, 'Recovered first title');
  assert.equal(f.calls[1].arguments.fields.description, 'Next local description'); assert.equal(f.document.value.value.fields.title, 'Saved title');
});

test('Task editor excludes pending, failed, foreign and older originals from a new draft', async t => {
  const f = taskEditorFixture(t, { parentOwnsAction: true }); f.behavior.outcomeRevision = 6;
  f.state.form.title = 'Confirmed prefill'; await settle(); await f.state.save(); await settle();
  const original = structuredClone(vue.toRaw(f.parentAction.saved.value));
  for (const change of [saved => { saved.phase = 'unknown'; }, saved => { saved.phase = 'failed'; },
    saved => { saved.call.qualifiedName = 'task-board.uploadAttachment'; }, saved => { saved.call.arguments.taskId = 'another-ticket'; },
    saved => { saved.outcome.task.objectId = 'another-ticket'; }, saved => { saved.outcome.task.revision = 5; },
    saved => { saved.callerPrincipalId = 'another-caller'; }]) {
    f.stopEditor(); const saved = structuredClone(original); change(saved); f.parentAction.saved.value = saved;
    f.reopen(); assert.equal(f.state.form.title, 'Saved title'); assert.equal(f.document.value.pin.revision, 5);
  }
  f.stopEditor(); f.parentAction.saved.value = structuredClone(original);
  sessionStorage.setItem(f.draftKey, JSON.stringify({ rootObjectId: 'root', principalId: 'workflow', revision: 5,
    fields: { ...structuredClone(vue.toRaw(f.document.value.value.fields)), title: 'Later retained draft' },
    baseFields: structuredClone(vue.toRaw(f.document.value.value.fields)), wakeAt: null, dependencies: [] }));
  f.reopen(); assert.equal(f.state.form.title, 'Later retained draft', 'A later retained local draft wins over the confirmed original prefill.');
  assert.equal(f.calls.length, 1);
});

test('Task editor checks concurrent scheduling and dependencies even when task fields are unchanged', async t => {
  const f = taskEditorFixture(t); f.state.wakeAt = '2026-12-01T10:00:00Z'; await settle();
  f.editorBehavior.current = { pin: { objectId: 'ticket', revision: 6 }, value: { ...structuredClone(vue.toRaw(f.document.value.value)),
    wakeAt: '2026-12-02T10:00:00Z' } };
  await f.state.save(); await settle(); assert(f.state.conflict); assert.equal(f.calls.length, 0);
  assert.equal(f.state.wakeAt, '2026-12-01T10:00:00Z'); assert(sessionStorage.getItem(f.draftKey));
  f.state.conflict = null; f.state.wakeAt = null; f.state.dependencies = ['local-dependency']; await settle();
  f.editorBehavior.current.value.wakeAt = null; f.editorBehavior.current.value.dependencies = ['remote-dependency'];
  await f.state.save(); await settle(); assert(f.state.conflict); assert.equal(f.calls.length, 0);
  assert.deepEqual([...f.state.dependencies], ['local-dependency']);
  f.state.useCurrentRevision(); assert.equal(f.state.conflict, null);
  await f.state.save(); await settle(); assert.equal(f.calls.length, 1); assert.equal(f.calls[0].arguments.expectedRevision, 6);
  assert.deepEqual(f.calls[0].arguments.dependencies, ['local-dependency'], 'Only explicit comparison accepts the new scheduling base.');
});

test('Task editor verifies confirmed draft fields against a fresh header at the same confirmed revision', async t => {
  const f = taskEditorFixture(t, { parentOwnsAction: true }); f.behavior.outcomeRevision = 6;
  f.state.form.title = 'Confirmed requested title'; await settle(); await f.state.save(); await settle();
  f.reopen(); f.state.form.description = 'Later local description'; await settle();
  f.editorBehavior.current = { pin: { objectId: 'ticket', revision: 6 }, value: { ...structuredClone(vue.toRaw(f.document.value.value)),
    fields: { ...structuredClone(f.calls[0].arguments.fields), title: 'Actual server title' } } };
  await f.state.save(); await settle(); assert(f.state.conflict); assert.equal(f.calls.length, 1);
  assert.equal(f.state.form.title, 'Confirmed requested title'); assert.equal(f.state.form.description, 'Later local description');
  assert(sessionStorage.getItem(f.draftKey));
});

test('Task editor recovers an older retained scheduling base with one bounded historical header', async t => {
  const f = taskEditorFixture(t); f.stopEditor();
  const original = structuredClone(vue.toRaw(f.document.value));
  sessionStorage.setItem(f.draftKey, JSON.stringify({ rootObjectId: 'root', principalId: 'workflow', revision: 5,
    fields: { ...original.value.fields, title: 'Retained title' }, baseFields: original.value.fields,
    wakeAt: '2026-12-01T10:00:00Z', dependencies: [] }));
  f.document.value = { ...original, pin: { objectId: 'ticket', revision: 7 }, value: { ...original.value, wakeAt: '2026-12-02T10:00:00Z' } };
  f.editorBehavior.historical = original; f.reopen(); await f.state.save(); await settle();
  assert(f.state.conflict); assert.equal(f.calls.length, 0); assert.equal(f.headerReads.length, 2);
  assert.deepEqual(f.headerReads[1], ['task', { objectId: 'ticket', revision: 5 }, 'root', undefined, { limit: 0 }]);
  const retained = JSON.parse(sessionStorage.getItem(f.draftKey)); assert.equal(retained.wakeAt, '2026-12-01T10:00:00Z');
  assert.deepEqual(retained.baseSchedule, { wakeAt: null, dependencies: [] });
});

test('Task editor preserves an explicit cleared schedule across confirmed prefill and retained draft recovery', async t => {
  const f = taskEditorFixture(t, { parentOwnsAction: true }); f.behavior.outcomeRevision = 6;
  f.document.value.value.wakeAt = '2026-12-01T10:00:00Z';
  f.state.form.title = 'Saved with cleared schedule'; f.state.wakeAt = null; await settle(); await f.state.save(); await settle();
  f.reopen(); assert.equal(f.state.wakeAt, null); assert.equal(f.state.form.title, 'Saved with cleared schedule');
  f.stopEditor(); const fields = structuredClone(vue.toRaw(f.document.value.value.fields));
  sessionStorage.setItem(f.draftKey, JSON.stringify({ rootObjectId: 'root', principalId: 'workflow', revision: 5,
    fields: { ...fields, title: 'Retained without schedule' }, baseFields: fields,
    wakeAt: null, dependencies: [], baseSchedule: { wakeAt: '2026-12-01T10:00:00Z', dependencies: [] } }));
  f.reopen(); assert.equal(f.state.wakeAt, null); assert.equal(f.state.form.title, 'Retained without schedule');
  assert.equal(f.document.value.value.wakeAt, '2026-12-01T10:00:00Z'); assert.equal(f.calls.length, 1);
});

test('Archive uses the selected owner and current header without repeating object or owner discovery', async t => {
  const f = archiveFixture(t), archive = f.mount();
  let release; f.behavior.prepare = new Promise(resolve => { release = resolve; });
  const context = { workspace: { ...f.workspace }, task: { objectId: 'ticket', revision: 7 } };
  const result = archive.run('ticket', true, context); await settle();
  context.workspace.serviceNodeId = 'new-owner'; context.workspace.callerPrincipalId = 'new-caller'; context.task.revision = 99;
  release(); assert.ok(await result);
  assert.deepEqual(f.requests.map(([method]) => method), ['tools.call']);
  assert.deepEqual(f.workspaceReads, []); assert.deepEqual(f.bindings, [['task-board.archive', 'owner']]);
  assert.equal(f.calls[0].serviceNodeId, 'owner'); assert.equal(f.calls[0].arguments.expectedRevision, 7);
  assert.deepEqual(f.calls[0].arguments.expectedWorkspace, { callerPrincipalId: 'caller', principalId: 'workflow', rootObjectId: 'root' });
  assert.equal(f.calls[0].operationId, f.calls[0].arguments.operationId);
  assert.equal(archive.pending.value, null);
  f.behavior.prepare = null;
  assert.ok(await archive.run('ticket', false, { workspace: f.workspace }));
  assert.deepEqual(f.requests.slice(1).map(([method]) => method), ['objects.stat', 'tools.call'], 'A detail control still takes a fresh compact revision.');
  assert.deepEqual(f.workspaceReads, []);
});

test('Archive action uses its exact successful metadata reply and reconciles a lost response from the same original without replay', async t => {
  const f = archiveFixture(t), scopes = []; let ids = 0;
  const actions = load('ui/task-board-ui/src/action.ts', {
    '../../../packages/sdk/src/client.js': { IvyError,
      discover: async (_client, qualifiedName, target) => ({ qualifiedName, ...target, definitionHash: 'archive-definition' }),
      newOperationId: async () => 'original-archive-' + ++ids },
    './runtime': { base: new URL('https://fixture.invalid/'), client: f.client },
    '../../../packages/ui-client/src/native': { nativeRead: async (_client, _node, method) => {
      assert.equal(method, 'task-board.workspace'); return f.workspace;
    } },
    '../../../packages/ui-client/src/hash': f.hashes,
    '../../../packages/ui-client/src/content': {}, '../../../packages/ui-client/src/message-attachments': {},
    '../../../packages/ui-client/src/object-archive': f.helpers, './upload-store': {},
  });
  const mount = () => { const scope = vue.effectScope(); scopes.push(scope);
    return { action: scope.run(() => actions.useAction(() => f.workspace, 'task:ticket')), stop: () => scope.stop() }; };
  t.after(() => { for (const scope of scopes) scope.stop(); });
  let mounted = mount(); f.object.observation = { runtimeEpoch: 'hive', sequence: 20 };
  const outcome = await mounted.action.start('Archive task', { action: 'archive', taskId: 'ticket', expectedRevision: 7, archived: true },
    () => ({ runtimeEpoch: 'hive', sequence: 10 }));
  assert.equal(outcome.archive.observation.sequence, 20); assert.equal(mounted.action.saved.value.phase, 'succeeded');
  assert.deepEqual(f.requests.map(([method]) => method), ['tools.call'], 'The normal successful path needs no receipt read.');
  const saved = JSON.parse(sessionStorage.getItem([...Array(sessionStorage.length)].map((_, n) => sessionStorage.key(n)).find(key => key.includes('action:'))));
  assert.deepEqual(saved.archiveBase, { runtimeEpoch: 'hive', sequence: 10 }); assert(!('archiveBase' in saved.call.arguments));
  f.behavior.lost = true; f.object.observation.sequence = 30;
  await mounted.action.start('Restore task', { action: 'archive', taskId: 'ticket', expectedRevision: 7, archived: false },
    () => ({ runtimeEpoch: 'hive', sequence: 20 }));
  assert.equal(mounted.action.saved.value.phase, 'unknown'); const original = structuredClone(vue.toRaw(mounted.action.saved.value.call));
  mounted.stop(); mounted = mount(); await mounted.action.reconcile();
  assert.equal(mounted.action.saved.value.phase, 'succeeded'); assert.deepEqual(vue.toRaw(mounted.action.saved.value.call), original);
  assert.equal(mounted.action.saved.value.outcome.archive.observation.sequence, 30); assert.equal(f.calls.length, 2); assert.equal(ids, 2);
  assert.throws(() => f.helpers.archiveActionOutcome({ ...f.object, id: 'foreign' }, {
    objectId: 'ticket', archived: false, mutationId: original.operationId, call: original }), /does not confirm/);
});

test('Archive reload and explicit retry retain the original owner, revision and receipt identity', async t => {
  const f = archiveFixture(t), archive = f.mount(); f.behavior.lost = true; f.behavior.receiptAvailable = false;
  await archive.run('ticket', true, { workspace: f.workspace, task: { objectId: 'ticket', revision: 7 } });
  // Keep the first lost response unresolved even though its object is archived.
  const originalReceipt = f.receipts.get('archive-operation-1'); f.receipts.clear(); f.behavior.receiptAvailable = true;
  const original = JSON.parse(JSON.stringify(archive.pending.value)); assert.ok(original?.call);
  const reopened = f.mount();
  f.receipts.set(original.mutationId, { ...originalReceipt, callerPrincipalId: 'another-caller' });
  assert.equal(await reopened.reconcile(), null); assert.equal(reopened.pending.value.mutationId, original.mutationId);
  f.receipts.set(original.mutationId, { ...originalReceipt, submittedHash: 'another-request' });
  assert.equal(await reopened.reconcile(), null); assert.equal(reopened.pending.value.mutationId, original.mutationId);
  f.receipts.set(original.mutationId, { ...originalReceipt, phase: 'needs_attention', outcome: null });
  assert.equal(await reopened.reconcile(), null); assert.equal(f.calls.length, 1, 'Recovery never resends a write.');
  reopened.clearError(); f.behavior.lost = false;
  assert.ok(await reopened.run('ticket', true, { workspace: { ...f.workspace, serviceNodeId: 'replacement-owner' }, task: { objectId: 'another-ticket', revision: 99 } }));
  assert.deepEqual(f.calls[1], original.call, 'An explicit retry sends exactly the retained original, ignoring new context.');
  assert.equal(f.ids(), 1); assert.equal(reopened.pending.value, null);
});

test('Archive preserves generic Wiki and paged TaskBoard owner discovery without falling back from a selected owner', async t => {
  const f = archiveFixture(t), archive = f.mount();
  assert.ok(await archive.run('ticket', true));
  assert.deepEqual(f.workspaceReads, ['other', 'owner']);
  assert.equal(f.requests.filter(([method]) => method === 'serviceNodes.list').length, 2);
  assert.equal(f.calls[0].serviceNodeId, 'owner');
  f.object.contractKey = 'wiki/page';
  assert.ok(await archive.run('ticket', false));
  assert.equal(f.requests.at(-1)[0], 'objects.archive');
  f.object.contractKey = 'task-board/task'; f.behavior.unavailable = true;
  const before = f.requests.length;
  assert.equal(await archive.run('ticket', true, { workspace: f.workspace, task: { objectId: 'ticket', revision: 7 } }), null);
  assert.ok(f.requests.slice(before).every(([method]) => method === 'objects.stat'), 'No owner search or generic archive may replace an unavailable selected owner.');
  assert.equal(f.calls.length, 1); assert.equal(archive.pending.value, null);
});

test('Archive rejects a foreign header or workspace and stops before dispatch when storage fails', async t => {
  const f = archiveFixture(t), archive = f.mount();
  assert.equal(await archive.run('ticket', true, { workspace: f.workspace, task: { objectId: 'another-ticket', revision: 7 } }), null);
  assert.equal(f.ids(), 0); assert.equal(f.requests.length, 0);
  archive.clearError();
  assert.equal(await archive.run('ticket', true, { workspace: { ...f.workspace, rootObjectId: 'another-root' } }), null);
  assert.equal(f.calls.length, 0); assert.equal(archive.pending.value, null);
  archive.clearError();
  const original = sessionStorage.setItem; sessionStorage.setItem = () => { throw new Error('Storage unavailable'); };
  try { assert.equal(await archive.run('ticket', true, { workspace: f.workspace, task: { objectId: 'ticket', revision: 7 } }), null); }
  finally { sessionStorage.setItem = original; }
  assert.equal(f.calls.length, 0); assert.ok(archive.pending.value, 'The unsent original remains available in memory.');
});

function boardActionsFixture(t) {
  setup(t);
  const owner = vue.reactive({ serviceNodeId: 'owner', callerPrincipalId: 'caller', principalId: 'workflow', rootObjectId: 'root' });
  const calls = [], reads = [], receipts = new Map(), scopes = [];
  const behavior = { lost: false, discovery: null, write: null, wrongCaller: false, reject: null, outcomeRevision: null };
  const hashes = load('packages/ui-client/src/hash.ts', { '../../sdk/src/client.js': { canonical } });
  let ids = 0;
  const archiveHelpers = load('packages/ui-client/src/object-archive.ts', {
    '@ivy/ui': { useRemote: () => ({}) }, './hash': hashes,
    '../../sdk/src/client.js': { IvyError, discover: async (_client, qualifiedName, target) => ({ qualifiedName, ...target }),
      callBound: async (_client, binding, args) => {
        if (binding.qualifiedName === 'task-board.workspace') return owner;
        reads.push(args.operationId); await behavior.read;
        const receipt = structuredClone(receipts.get(args.operationId));
        if (!receipt) throw new IvyError('not_found', 'Not found');
        if (behavior.wrongCaller) receipt.callerPrincipalId = 'another-caller';
        return receipt;
      } },
  });
  const actions = load('ui/task-board-ui/src/action.ts', {
    '../../../packages/sdk/src/client.js': { IvyError, discover: async (_client, name) => {
      await behavior.discovery; return { qualifiedName: name, serviceNodeId: owner.serviceNodeId, definitionHash: 'definition' };
    }, newOperationId: async () => 'board-operation-' + ++ids },
    './runtime': { base: new URL('https://fixture.invalid/'), client: { request: async (_method, call) => {
      calls.push(JSON.parse(JSON.stringify(call)));
      await behavior.write;
      if (behavior.reject === call.arguments.commentId || behavior.rejectTask === call.arguments.taskId) throw new IvyError('revision_conflict', 'Fixture rejected comment', 'not_executed');
      behavior.onWrite?.(call);
      const archive = call.arguments.action === 'archive' ? { id: call.arguments.taskId, contractKey: 'task-board/task', parentId: 'root',
        currentRevision: call.arguments.expectedRevision, archivedAt: call.arguments.archived ? '2026-01-01T00:00:00Z' : null,
        effectivelyArchived: call.arguments.archived, observation: { runtimeEpoch: 'hive', sequence: 20 } } : null;
      const outcome = { operationId: call.operationId, task: archive ? null : { objectId: call.arguments.taskId, revision: behavior.outcomeRevision ?? 2 }, ...(archive ? { archive } : {}) };
      receipts.set(call.operationId, { receipt: true, callerPrincipalId: owner.callerPrincipalId, operationId: call.operationId,
        action: call.arguments.action, taskId: call.arguments.taskId, submittedHash: await hashes.hashJson(call.arguments), phase: 'succeeded', outcome });
      if (behavior.lost) throw new IvyError('outcome_unknown', 'Lost response', 'unknown');
      return archive ?? outcome;
    } } },
    '../../../packages/ui-client/src/native': { nativeRead: async (_client, _node, method, args) => {
      if (method === 'task-board.workspace') return owner;
      reads.push(args.operationId); await behavior.read;
      const receipt = structuredClone(receipts.get(args.operationId));
      if (!receipt) throw new IvyError('not_found', 'Not found');
      if (behavior.wrongCaller) receipt.callerPrincipalId = 'another-caller';
      return receipt;
    } },
    '../../../packages/ui-client/src/hash': hashes,
    '../../../packages/ui-client/src/content': {},
    '../../../packages/ui-client/src/message-attachments': {},
    '../../../packages/ui-client/src/object-archive': archiveHelpers,
    './upload-store': {},
  });
  const { useBoardActions } = load('ui/task-board-ui/src/board-actions.ts', { './action': actions,
    './status-projection': load('ui/task-board-ui/src/status-projection.ts') });
  const mount = (synchronizeArchives, visibleCard, selectedTask) => {
    const scope = vue.effectScope(); scopes.push(scope);
    return { board: scope.run(() => useBoardActions(() => owner, synchronizeArchives, visibleCard, selectedTask)), stop: () => scope.stop() };
  };
  const send = (board, taskId) => board.run(taskId, async action => {
    await action.start('Move task', { action: 'transition', taskId, expectedRevision: 1, workflowState: 'todo', detail: null });
  });
  const entry = (board, taskId) => board.entries.value.find(entry => entry.taskId === taskId)?.action;
  t.after(() => { for (const scope of scopes) scope.stop(); });
  return { owner, calls, reads, receipts, behavior, mount, send, entry, actions };
}

function optimisticCommentsFixture(t) {
  const f = boardActionsFixture(t), owner = vue.reactive(f.owner), task = vue.ref({ workflowState: 'done', comments: [] });
  const enabled = vue.ref(true), ready = vue.ref(true), headers = [], scopes = [], behavior = { header: null }, changed = [];
  const { useOptimisticComment } = load('ui/task-board-ui/src/optimistic-comment.ts', { './runtime': {
    base: new URL('https://fixture.invalid/'), tr: (_de, en) => en,
    readDocument: async (...args) => { headers.push(args); await behavior.header; return { pin: { objectId: 'ticket', revision: f.calls.length + 1 } }; },
  } });
  const mount = () => {
    const scope = vue.effectScope(); scopes.push(scope);
    return scope.run(() => {
      const action = f.actions.useAction(() => owner, 'task:ticket');
      return { action, comments: useOptimisticComment(() => owner, 'ticket', action, () => task.value, () => changed.push(true), () => enabled.value, () => ready.value), stop: () => scope.stop() };
    });
  };
  t.after(() => { for (const scope of scopes) scope.stop(); });
  const draft = (id, extra = {}) => ({ action: 'comment', commentId: id, body: 'Message ' + id, moveToTodo: false, requests: [], responses: [], attachments: [], replyTo: null, ...extra });
  return { ...f, owner, task, enabled, ready, headers, readBehavior: behavior, changed, mount, draft };
}
async function drainComments(until) {
  const deadline = performance.now() + 5000;
  for (let n = 0; n < 20 || until && !until(); n++) {
    assert(performance.now() < deadline, 'The expected comment outcomes did not settle.');
    await new Promise(setImmediate); await settle();
  }
}

test('Comments retain rapid drafts behind a separate edit owner and send them only after its outcome is settled', async t => {
  const f = optimisticCommentsFixture(t); f.ready.value = false;
  const mounted = f.mount();
  assert.equal(mounted.comments.canSubmit.value, true);
  assert(mounted.comments.submit(f.draft('after-content-edit', { attachments: [{ attachmentId: 'kept-file' }] })));
  assert(mounted.comments.submit(f.draft('after-content-edit-too'))); await drainComments();
  assert.equal(f.calls.length, 0); assert.equal(f.headers.length, 0); assert.equal(mounted.comments.messages.value.length, 2);
  assert.equal(mounted.action.saved.value, null, 'The comment buffer cannot replace the separate original edit journal.');
  let release; f.behavior.write = new Promise(resolve => { release = resolve; }); t.after(() => release());
  f.ready.value = true; await settle(); assert.equal(f.calls.length, 1);
  f.ready.value = false; f.behavior.write = null; release(); await drainComments();
  assert.equal(mounted.comments.pending.value, null); assert.equal(mounted.comments.queued.value.length, 1);
  assert.equal(f.calls.length, 1, 'A new conflicting owner also pauses promotion of later drafts.');
  f.ready.value = true; await drainComments();
  assert.deepEqual(f.calls.map(call => call.arguments.commentId), ['after-content-edit', 'after-content-edit-too']);
  assert.deepEqual(f.calls.map(call => call.arguments.expectedRevision), [1, 2]);
  assert.equal(f.calls[0].arguments.attachments[0].attachmentId, 'kept-file');
  assert.equal(new Set(f.calls.map(call => call.operationId)).size, 2);
});

test('Comments accept rapid drafts, serialize fresh revisions and retain receipt-confirmed projections through stale reads', async t => {
  const f = optimisticCommentsFixture(t), { comments, action } = f.mount();
  let release; f.behavior.write = new Promise(resolve => { release = resolve; }); t.after(() => release());
  assert(comments.submit(f.draft('one', { moveToTodo: true, attachments: [{ attachmentId: 'file', filename: 'saved.txt' }] })));
  await settle(); assert.equal(f.calls.length, 1); assert(action.locked.value);
  assert(comments.submit(f.draft('two', { replyTo: 'question' })));
  assert(comments.submit(f.draft('three')));
  assert.deepEqual(comments.messages.value.map(message => message.commentId), ['one', 'two', 'three']);
  assert.equal(f.calls.length, 1); assert.equal(comments.queued.value.length, 2); assert.equal(f.task.value.workflowState, 'done');
  f.behavior.write = null; release(); await drainComments(() => comments.pending.value === null && comments.queued.value.length === 0);
  assert.deepEqual(f.calls.map(call => call.arguments.commentId), ['one', 'two', 'three']);
  assert.deepEqual(f.calls.map(call => call.arguments.expectedRevision), [1, 2, 3]);
  assert.equal(new Set(f.calls.map(call => call.operationId)).size, 3); assert(f.headers.every(args => args[4].limit === 0));
  assert.equal(f.calls[0].arguments.moveToTodo, true); assert.equal(f.calls[1].arguments.replyTo, 'question');
  assert.equal(f.calls[0].arguments.attachments[0].attachmentId, 'file');
  assert.equal(comments.pending.value, null); assert.equal(comments.queued.value.length, 0);
  f.task.value = { workflowState: 'done', comments: [] }; await settle();
  assert.equal(comments.messages.value.length, 3, 'A stale read cannot remove receipt-confirmed messages.');
  assert.equal(f.task.value.workflowState, 'done', 'A display projection never applies requested business transitions.');
  f.task.value = { workflowState: 'todo', comments: comments.messages.value.map(message => ({ ...message })) }; await settle();
  assert.equal(comments.messages.value.length, 0, 'Authoritative comments replace their projections without duplicates.');
});

test('Comments reload preserves an unresolved original and later drafts without replay or replacement owner', async t => {
  const f = optimisticCommentsFixture(t); let mounted = f.mount(); f.behavior.lost = true;
  assert(mounted.comments.submit(f.draft('original'))); await drainComments();
  const call = structuredClone(f.calls[0]), receipt = structuredClone(f.receipts.get(call.operationId));
  f.receipts.set(call.operationId, { ...receipt, phase: 'accepted', outcome: null });
  assert(mounted.comments.submit(f.draft('later', { attachments: [{ attachmentId: 'retained' }] })));
  mounted.stop(); mounted = f.mount(); await settle();
  await mounted.action.reconcile(); await settle(); assert.equal(f.calls.length, 1);
  assert.equal(mounted.action.saved.value.call.operationId, call.operationId); assert.equal(mounted.comments.messages.value.length, 2);
  f.owner.callerPrincipalId = 'another-caller'; await settle();
  assert.equal(mounted.comments.submit(f.draft('foreign')), false); assert.equal(f.calls.length, 1);
  f.owner.callerPrincipalId = 'caller'; f.behavior.lost = false; f.receipts.set(call.operationId, receipt);
  await mounted.action.reconcile(); await drainComments();
  assert.deepEqual(f.calls.map(call => call.arguments.commentId), ['original', 'later']);
  assert.deepEqual(f.calls[0], call); assert.equal(f.calls[1].arguments.attachments[0].attachmentId, 'retained');
  assert.notEqual(f.calls[1].operationId, call.operationId);
});

test('Comments wait behind another task action without blocking drafts or replacing its original operation', async t => {
  const f = optimisticCommentsFixture(t); let mounted = f.mount(), release, prepare;
  f.behavior.write = new Promise(resolve => { release = resolve; }); t.after(() => release());
  const preparation = new Promise(resolve => { prepare = resolve; }); t.after(() => prepare());
  f.behavior.lost = true;
  const edit = mounted.action.start('Edit task', async () => { await preparation;
    return { action: 'edit', taskId: 'ticket', expectedRevision: 1, fields: { title: 'Requested title' } }; });
  await settle(); assert.equal(f.calls.length, 0); assert(mounted.action.busy.value); assert.equal(mounted.comments.canSubmit.value, true);
  assert(mounted.comments.submit(f.draft('after-edit'))); assert(mounted.comments.submit(f.draft('after-edit-too')));
  await settle(); assert.equal(f.headers.length, 0, 'A conflicting original owns the write slot, without blocking local drafts.');
  assert.equal(f.calls.length, 0); prepare(); await settle();
  assert.equal(f.calls.length, 1); assert.equal(mounted.comments.messages.value.length, 2);
  const original = structuredClone(f.calls[0]); f.behavior.write = null; release(); await edit; await drainComments();
  const receipt = structuredClone(f.receipts.get(original.operationId));
  f.receipts.set(original.operationId, { ...receipt, phase: 'accepted', outcome: null });
  mounted.stop(); mounted = f.mount(); await mounted.action.reconcile(); await settle();
  assert.equal(mounted.action.saved.value.call.operationId, original.operationId);
  assert.equal(mounted.comments.canRetry('after-edit'), false); mounted.comments.retry('after-edit'); await settle();
  assert.equal(f.headers.length, 0); assert.equal(f.calls.length, 1, 'Retrying a waiting message cannot replace the unknown edit.');
  const otherScope = vue.effectScope(); t.after(() => otherScope.stop());
  const { useOptimisticComment } = load('ui/task-board-ui/src/optimistic-comment.ts', { './runtime': { base: new URL('https://fixture.invalid/'), tr: (_de, en) => en } });
  const other = otherScope.run(() => useOptimisticComment(() => f.owner, 'another-ticket', mounted.action, () => f.task.value, () => {}));
  assert.equal(other.submit(f.draft('wrong-target')), false, 'A retained action from another target does not open this buffer.');
  const saved = mounted.action.saved.value;
  const archiveAction = { ...mounted.action, saved: vue.ref({ ...saved, call: { ...saved.call, arguments: {
    action: 'archive', operationId: saved.call.operationId, taskId: 'ticket', expectedRevision: 1, archived: true,
  } } }) };
  const archived = otherScope.run(() => useOptimisticComment(() => f.owner, 'ticket', archiveAction, () => f.task.value, () => {}));
  assert.equal(archived.submit(f.draft('during-archive')), false, 'An unresolved archive keeps its lifecycle protection.');
  otherScope.stop();
  f.behavior.lost = false; f.receipts.set(original.operationId, receipt);
  await mounted.action.reconcile(); await drainComments();
  assert.deepEqual(f.calls.map(call => call.arguments.action), ['edit', 'comment', 'comment']);
  assert.deepEqual(f.calls.map(call => call.arguments.expectedRevision), [1, 2, 3]);
  assert.deepEqual(f.calls[0], original); assert.equal(new Set(f.calls.map(call => call.operationId)).size, 3);
  assert.equal(mounted.comments.messages.value.length, 2); assert.equal(mounted.comments.pending.value, null);
  let resume; const changedOwner = new Promise(resolve => { resume = resolve; }); t.after(() => resume());
  const withheld = mounted.action.start('Edit under original owner', async () => { await changedOwner;
    return { action: 'edit', taskId: 'ticket', expectedRevision: 3, fields: { title: 'Never sent' } }; });
  f.owner.rootObjectId = 'another-root'; resume(); await withheld;
  assert.equal(f.calls.length, 3); assert.match(mounted.action.error.value, /caller or workspace changed/);
  f.owner.rootObjectId = 'root';
});

test('Comments retain rejected text and files, continue later drafts and refuse storage failures before accepting input', async t => {
  const f = optimisticCommentsFixture(t), { comments } = f.mount();
  const storage = sessionStorage.setItem;
  sessionStorage.setItem = () => { throw Error('Storage unavailable'); };
  try { assert.equal(comments.submit(f.draft('unsaved')), false); }
  finally { sessionStorage.setItem = storage; }
  assert.equal(f.calls.length, 0); assert.equal(comments.messages.value.length, 0); assert.match(comments.submissionIssue.value, /draft is retained/);
  let release; f.behavior.write = new Promise(resolve => { release = resolve; }); t.after(() => release());
  f.behavior.reject = 'rejected';
  assert(comments.submit(f.draft('rejected', { attachments: [{ attachmentId: 'saved' }] }))); await settle();
  assert(comments.submit(f.draft('next'))); f.behavior.write = null; release(); await drainComments();
  assert.deepEqual(f.calls.map(call => call.arguments.commentId), ['rejected', 'next']);
  assert.match(comments.issue('rejected'), /Fixture rejected comment/); assert.equal(comments.messages.value[0].attachments[0].attachmentId, 'saved');
  assert.equal(comments.canRetry('rejected'), true); f.behavior.reject = null;
  comments.retry('rejected'); await drainComments();
  assert.equal(f.calls.length, 3); assert.notEqual(f.calls[0].operationId, f.calls[2].operationId);
  assert.equal(f.calls[2].arguments.commentId, 'rejected'); assert.equal(comments.issue('rejected'), '');
});

test('Comments bound retained messages and cancel undispatched preparation on disposal without losing drafts', async t => {
  const f = optimisticCommentsFixture(t), mounted = f.mount(); let release;
  f.readBehavior.header = new Promise(resolve => { release = resolve; }); t.after(() => release());
  for (let n = 0; n < 32; n++) assert(mounted.comments.submit(f.draft('bounded-' + n)));
  assert.equal(mounted.comments.canSubmit.value, false); assert.equal(mounted.comments.submit(f.draft('overflow')), false);
  assert.equal(mounted.comments.messages.value.length, 32); assert.equal(f.calls.length, 0);
  mounted.stop(); assert.equal(f.headers[0][3].aborted, true); release(); await drainComments();
  assert.equal(f.calls.length, 0); assert.equal(mounted.comments.submit(f.draft('disposed')), false);
  const retained = [...Array(sessionStorage.length)].map((_, n) => sessionStorage.key(n)).find(key => key.startsWith('ivy.task-board.pending-comment:'));
  const saved = JSON.parse(sessionStorage.getItem(retained)); assert.equal(saved.queued.length, 31); assert.equal(saved.pending.request.commentId, 'bounded-0');
});

test('Comments cap retained UTF-8 bytes and preserve a draft when the buffer is full', async t => {
  const f = optimisticCommentsFixture(t), { comments } = f.mount(); let release;
  f.readBehavior.header = new Promise(resolve => { release = resolve; }); t.after(() => release());
  for (let n = 0; n < 31; n++) assert(comments.submit(f.draft('bytes-' + n, { body: 'é'.repeat(65536) })));
  assert.equal(comments.submit(f.draft('too-large', { body: 'é'.repeat(65536) })), false);
  assert.equal(comments.messages.value.length, 31); assert.match(comments.submissionIssue.value, /too large/); assert.equal(f.calls.length, 0);
});

test('Action admission preserves pending board and edit originals across views and allows independent tickets', async t => {
  const f = boardActionsFixture(t), { board } = f.mount(), scope = vue.effectScope(); t.after(() => scope.stop());
  const detail = scope.run(() => f.actions.useAction(() => f.owner, 'task:shared'));
  let release; f.behavior.write = new Promise(resolve => { release = resolve; }); t.after(() => release());
  const pending = f.send(board, 'shared'); await settle();
  const key = f.actions.actionStorageKey(f.owner, 'board:task:shared'), original = sessionStorage.getItem(key);
  let prepared = false;
  assert.equal(await detail.start('Move task', async () => { prepared = true; return { action: 'transition', taskId: 'shared', expectedRevision: 1, workflowState: 'todo' }; }), null);
  assert.equal(prepared, false); assert.match(detail.error.value, /still being confirmed/);
  assert.equal(detail.saved.value, null); assert.equal(sessionStorage.getItem(key), original);
  f.behavior.write = null; await f.send(board, 'independent'); assert.equal(f.calls.length, 2);
  release(); await pending; detail.retryPreparation();
  await detail.start('Move task', { action: 'transition', taskId: 'shared', expectedRevision: 2, workflowState: 'waiting' });
  assert.equal(f.calls.length, 3); assert.equal(detail.saved.value.phase, 'succeeded');
  f.behavior.lost = true;
  const edit = scope.run(() => f.actions.useAction(() => f.owner, 'edit:edited'));
  await edit.start('Edit task', { action: 'edit', taskId: 'edited', expectedRevision: 1, fields: { title: 'Keep original' } });
  const editKey = f.actions.actionStorageKey(f.owner, 'edit:edited'), editOriginal = sessionStorage.getItem(editKey);
  await f.send(board, 'edited'); assert.equal(f.calls.length, 4);
  assert.match(board.error.value.message, /still being confirmed/); assert.equal(sessionStorage.getItem(editKey), editOriginal);
});

test('Action admission rechecks other view originals after asynchronous preparation without dispatching a replacement', async t => {
  const f = boardActionsFixture(t), { board } = f.mount(), scope = vue.effectScope(); t.after(() => scope.stop());
  const detail = scope.run(() => f.actions.useAction(() => f.owner, 'task:shared'));
  let release; const gate = new Promise(resolve => { release = resolve; }); t.after(() => release());
  const pending = detail.start('Move task', async () => { await gate;
    return { action: 'transition', taskId: 'shared', expectedRevision: 1, workflowState: 'waiting' }; });
  await settle(); f.behavior.lost = true; await f.send(board, 'shared');
  const key = f.actions.actionStorageKey(f.owner, 'board:task:shared'), original = sessionStorage.getItem(key);
  release(); await pending;
  assert.equal(f.calls.length, 1); assert.equal(detail.saved.value, null);
  assert.match(detail.error.value, /still being confirmed/); assert.equal(sessionStorage.getItem(key), original);
  f.behavior.lost = false; await f.entry(board, 'shared').reconcile();
  detail.retryPreparation(); await detail.start('Move task', { action: 'transition', taskId: 'shared', expectedRevision: 2, workflowState: 'waiting' });
  assert.equal(f.calls.length, 2); assert.equal(f.calls[0].operationId, JSON.parse(original).call.operationId);
});

test('Action admission preserves malformed and misdirected peer originals without preventing unrelated work', async t => {
  const f = boardActionsFixture(t), scope = vue.effectScope(); t.after(() => scope.stop());
  const detail = scope.run(() => f.actions.useAction(() => f.owner, 'task:ticket'));
  const key = f.actions.actionStorageKey(f.owner, 'board:task:ticket');
  for (const raw of ['{broken', JSON.stringify({ phase: 'succeeded', call: { operationId: 'original', arguments: { taskId: 'another' } } })]) {
    sessionStorage.setItem(key, raw); detail.retryPreparation();
    assert.equal(await detail.start('Move task', { action: 'transition', taskId: 'ticket', expectedRevision: 1, workflowState: 'todo' }), null);
    assert(detail.error.value); assert.equal(detail.saved.value, null); assert.equal(sessionStorage.getItem(key), raw);
  }
  const independent = scope.run(() => f.actions.useAction(() => f.owner, 'task:independent'));
  await independent.start('Move task', { action: 'transition', taskId: 'independent', expectedRevision: 1, workflowState: 'todo' });
  assert.equal(f.calls.length, 1); assert.equal(f.calls[0].arguments.taskId, 'independent');
});

test('Action retry keeps an undispatched prepared original behind a pending peer without making its outcome unknown', async t => {
  const f = boardActionsFixture(t), { board } = f.mount(), scope = vue.effectScope(); t.after(() => scope.stop());
  const detail = scope.run(() => f.actions.useAction(() => f.owner, 'task:shared'));
  const denied = t.mock.method(sessionStorage, 'setItem', () => { throw new Error('Storage unavailable'); });
  await detail.start('Move task', { action: 'transition', taskId: 'shared', expectedRevision: 1, workflowState: 'waiting' });
  denied.mock.restore(); assert.equal(detail.saved.value.phase, 'prepared'); assert.equal(f.calls.length, 0);
  const original = structuredClone(vue.toRaw(detail.saved.value).call);
  f.behavior.lost = true; await f.send(board, 'shared'); assert.equal(f.calls.length, 1);
  await detail.replay(); assert.equal(f.calls.length, 1); assert.equal(detail.saved.value.phase, 'prepared');
  assert.deepEqual(vue.toRaw(detail.saved.value).call, original); assert.match(detail.error.value, /still being confirmed/);
  f.behavior.lost = false; await f.entry(board, 'shared').reconcile();
  await detail.replay(); assert.equal(f.calls.length, 2); assert.deepEqual(f.calls[1], original);
});

test('Board task changes overlap independently while one original ticket remains locked', async t => {
  const f = boardActionsFixture(t), { board } = f.mount();
  let release; f.behavior.write = new Promise(resolve => { release = resolve; });
  const first = f.send(board, 'first'); await settle();
  assert.deepEqual(board.blocked.value, ['first']);
  let duplicate = false; await board.run('first', async () => { duplicate = true; });
  assert.equal(duplicate, false);
  f.behavior.write = null;
  await f.send(board, 'second');
  assert.equal(f.entry(board, 'second').saved.value.phase, 'succeeded');
  assert.deepEqual(board.blocked.value, ['first']);
  assert.deepEqual(f.calls.map(call => call.arguments.taskId), ['first', 'second']);
  assert.notEqual(f.calls[0].operationId, f.calls[1].operationId);
  await f.send(board, 'third');
  assert(Boolean(f.entry(board, 'second')), 'Recent confirmed changes keep their bounded display watermark until a slot is needed.');
  assert(f.entry(board, 'first').busy.value);
  release(); await first;
  assert.deepEqual(board.blocked.value, []);
});

test('Board reload settles the original lost reply without resending and rejects foreign receipts', async t => {
  const f = boardActionsFixture(t), first = f.mount();
  f.behavior.lost = true; await f.send(first.board, 'original');
  const original = structuredClone(vue.toRaw(f.entry(first.board, 'original').saved.value));
  first.stop(); await advance(t, 5000); assert.equal(f.reads.length, 0, 'Disposed scopes release their recovery timers.');
  const restored = f.mount();
  assert.deepEqual(restored.board.blocked.value, ['original']);
  f.behavior.wrongCaller = true;
  await f.entry(restored.board, 'original').reconcile();
  assert.match(f.entry(restored.board, 'original').error.value, /differs/);
  assert.equal(f.entry(restored.board, 'original').saved.value.phase, 'unknown');
  f.behavior.wrongCaller = false; f.behavior.lost = false;
  await f.send(restored.board, 'independent');
  await f.entry(restored.board, 'original').reconcile();
  assert.equal(f.entry(restored.board, 'original').saved.value.phase, 'succeeded');
  assert.equal(f.entry(restored.board, 'original').saved.value.call.operationId, original.call.operationId);
  assert.equal(f.calls.filter(call => call.arguments.taskId === 'original').length, 1);
});

test('Board preserves mismatched retained targets and storage failures without sending replacements', async t => {
  const f = boardActionsFixture(t), mounted = f.mount();
  f.behavior.lost = true; await f.send(mounted.board, 'original'); mounted.stop();
  const key = f.actions.actionStorageKey(f.owner, 'board:task:original');
  const raw = JSON.parse(sessionStorage.getItem(key)); raw.call.arguments.taskId = 'different';
  const mismatched = JSON.stringify(raw); sessionStorage.setItem(key, mismatched);
  const restored = f.mount(); assert(restored.board.recoveryError.value);
  await f.send(restored.board, 'original');
  assert.match(restored.board.error.value.message, /another task/);
  assert.equal(sessionStorage.getItem(key), mismatched); assert.equal(f.calls.length, 1);
  f.behavior.lost = false; await f.send(restored.board, 'independent'); assert.equal(f.calls.length, 2);
  const unavailable = t.mock.method(sessionStorage, 'setItem', () => { throw new Error('Storage unavailable'); });
  await f.send(restored.board, 'storage-failure');
  assert.equal(f.calls.length, 2); assert.match(f.entry(restored.board, 'storage-failure').error.value, /retained/);
  unavailable.mock.restore();
});

test('Board stops undispatched preparation on disposal and bounds retained task slots', async t => {
  const f = boardActionsFixture(t), first = f.mount();
  let release; f.behavior.discovery = new Promise(resolve => { release = resolve; });
  const pending = f.send(first.board, 'abandoned'); await settle(); first.stop(); release(); await pending;
  assert.equal(f.calls.length, 0); assert.equal(sessionStorage.length, 0);
  f.behavior.discovery = null; f.behavior.lost = true;
  const restored = f.mount();
  for (let index = 0; index < 32; index++) await f.send(restored.board, 'task-' + index);
  await f.send(restored.board, 'overflow');
  assert.equal(f.calls.length, 32); assert.equal(restored.board.entries.value.length, 32);
  assert.equal(restored.board.error.value.taskId, 'overflow'); assert.equal(sessionStorage.length, 32);
  assert(restored.board.entries.value.every(entry => entry.action.saved.value.phase === 'unknown'));
  const confirmed = f.entry(restored.board, 'task-0');
  await confirmed.reconcile(); await confirmed.dismiss();
  assert.equal(confirmed.saved.value, null);
  await f.send(restored.board, 'overflow');
  assert.equal(f.calls.length, 33); assert.equal(restored.board.entries.value.length, 32);
  assert.equal(restored.board.error.value, null); assert.equal(sessionStorage.length, 32);
});

function boardCard(id, revision = 1, workflowState = 'done') {
  return { object: { objectId: id, revision }, task: { taskKey: id, workflowState,
    fields: { title: id, category: 'Alpha', control: 'user', priority: 2, executionRequirement: null, dueAt: null },
    claim: null, publication: null, latestResult: null, openQuestionCount: 0, wakeAt: null, runtimeWait: null },
    executionCondition: { state: 'idle', detail: '' }, commentCount: 0, directCommentCount: 0 };
}

const archiveCard = (id, archived = false, sequence = 10) => ({ ...boardCard(id), archived, observation: { runtimeEpoch: 'hive', sequence } });

test('Board keeps the same detail archive owner across closing and reopening its view', async t => {
  const f = boardActionsFixture(t), { board } = f.mount(undefined, id => archiveCard(id));
  const { task: detail } = await board.detail('ticket');
  f.behavior.lost = true;
  await detail.start('Archive task', { action: 'archive', taskId: 'ticket', expectedRevision: 1, archived: true }, () => ({ runtimeEpoch: 'hive', sequence: 10 }));
  const key = f.actions.actionStorageKey(f.owner, 'task:ticket');
  assert.equal(board.projections.value[0].archived, true); assert.equal(board.projections.value[0].fromDetail, true);
  assert.equal(board.projections.value[0].card.archived, false); assert.equal(f.entry(board, 'ticket').saved.value, null);
  assert.deepEqual(board.blocked.value, ['ticket']); assert.equal(sessionStorage.length, 1);
  assert.equal((await board.detail('ticket')).task, detail);
  await advance(t, 5000); await drainComments(() => !detail.busy.value);
  assert.equal(f.reads.length, 1); assert.equal(f.calls.length, 1);
  assert.equal(JSON.parse(sessionStorage.getItem(key)).phase, 'succeeded');
  assert.equal((await board.detail('ticket')).task, detail); assert.equal(board.projections.value[0].archived, true);
  board.observe([archiveCard('ticket', false, 30)]); assert.equal(board.projections.value[0].archived, false);
  board.observe([archiveCard('ticket', true, 15)]); assert.equal(board.projections.value[0].archived, false);
});

test('Board follows successive confirmed detail status originals through stale cards without changing their business data', async t => {
  const f = boardActionsFixture(t), { board } = f.mount(undefined, id => boardCard(id));
  const { task: detail } = await board.detail('ticket');
  await detail.start('Move task', { action: 'transition', taskId: 'ticket', expectedRevision: 1, workflowState: 'todo' });
  assert.equal(board.projections.value[0].state, 'todo'); assert.equal(board.projections.value[0].card.task.workflowState, 'done');
  f.behavior.outcomeRevision = 3;
  await detail.start('Move task', { action: 'transition', taskId: 'ticket', expectedRevision: 2, workflowState: 'waiting' });
  assert.equal(board.projections.value[0].state, 'waiting'); assert.equal(f.calls.length, 2); assert.equal(f.reads.length, 0);
  board.observe([boardCard('ticket', 3, 'cancelled')]); assert.equal(board.projections.value[0].state, 'cancelled');
  board.observe([boardCard('ticket', 2, 'todo')]); assert.equal(board.projections.value[0].state, 'cancelled');
});

test('Board bounds shared detail owners and retires archived originals only after their actual fence', async t => {
  const f = boardActionsFixture(t), { board } = f.mount(undefined, id => archiveCard(id));
  f.behavior.lost = true;
  for (let n = 0; n < 32; n++) {
    const { task: action } = await board.detail('peer-' + n);
    await action.start('Archive task', { action: 'archive', taskId: 'peer-' + n, expectedRevision: 1, archived: true }, () => ({ runtimeEpoch: 'hive', sequence: 10 }));
  }
  f.behavior.wrongCaller = true; await advance(t, 5000);
  await drainComments(() => board.detailEntries.value.every(entry => !entry.action.busy.value)); assert.equal(f.reads.length, 32);
  await f.send(board, 'overflow'); assert.equal(f.calls.length, 32); assert.equal(board.entries.value.length, 32);
  await assert.rejects(board.detail('overflow'), /Resolve a previous/);
  const { task: original } = await board.detail('peer-0'); f.behavior.wrongCaller = false;
  await original.reconcile(); board.observe([archiveCard('peer-0', false, 19)]);
  await f.send(board, 'overflow'); assert.equal(f.calls.length, 32);
  board.observe([archiveCard('peer-0', true, 20)]); await f.send(board, 'overflow');
  assert.equal(f.calls.length, 33); assert.equal(board.entries.value.length, 32);
  assert.equal(original.saved.value, null); assert.equal(sessionStorage.getItem(f.actions.actionStorageKey(f.owner, 'task:peer-0')), null);
});

test('Board ignores foreign detail originals and stops owned recovery on disposal', async t => {
  const f = boardActionsFixture(t), scope = vue.effectScope(); t.after(() => scope.stop());
  const detail = scope.run(() => f.actions.useAction(() => f.owner, 'task:ticket'));
  await detail.start('Move task', { action: 'transition', taskId: 'ticket', expectedRevision: 1, workflowState: 'todo' });
  const key = f.actions.actionStorageKey(f.owner, 'task:ticket'), value = JSON.parse(sessionStorage.getItem(key));
  value.callerPrincipalId = 'another-caller'; const raw = JSON.stringify(value); sessionStorage.setItem(key, raw);
  const mounted = f.mount(); mounted.board.observe([boardCard('ticket')]); assert.deepEqual(mounted.board.projections.value, []);
  mounted.stop(); sessionStorage.setItem(key, JSON.stringify({ ...value, phase: 'unknown', callerPrincipalId: f.owner.callerPrincipalId }));
  const restored = f.mount(); assert.equal(restored.board.detailEntries.value.length, 2); restored.stop();
  await advance(t, 5000); assert.equal(mounted.board.entries.value.length, 0); assert.equal(f.calls.length, 1); assert.equal(f.reads.length, 0);
});

test('Board retains one detail owner while preparation and an independent ticket overlap', async t => {
  const f = boardActionsFixture(t), { board } = f.mount();
  let release; f.behavior.discovery = new Promise(resolve => { release = resolve; });
  const owners = await board.detail('ticket');
  const pending = owners.task.start('Move task', { action: 'transition', taskId: 'ticket', expectedRevision: 1, workflowState: 'todo' });
  await settle(); assert.equal(owners.task.busy.value, true); assert.equal(f.calls.length, 0);
  assert.equal(await board.detail('ticket'), owners);
  const independent = f.send(board, 'other'); await settle(); release(); await Promise.all([pending, independent]);
  assert.equal(f.calls.length, 2); assert.equal(new Set(f.calls.map(call => call.operationId)).size, 2);
  assert.equal(board.detailEntries.value.filter(entry => entry.taskId === 'ticket').length, 2);
});

test('Board recovers existing detail edit and comment journals while their views are closed', async t => {
  const f = boardActionsFixture(t), mounted = f.mount(), owners = await mounted.board.detail('ticket');
  f.behavior.lost = true;
  await owners.edit.start('Change title', { action: 'edit', taskId: 'ticket', expectedRevision: 1, fields: { title: 'Requested title' } });
  const editId = owners.edit.saved.value.call.operationId;
  mounted.stop(); const restored = f.mount(); await advance(t, 5000);
  const same = await restored.board.detail('ticket');
  await drainComments(() => !same.edit.busy.value);
  assert.equal(same.edit.saved.value.phase, 'succeeded'); assert.equal(same.edit.saved.value.call.operationId, editId);
  assert.equal(f.calls.length, 1); assert.equal(f.reads.length, 1);
  await same.task.start('Comment', { action: 'comment', taskId: 'ticket', expectedRevision: 2, body: 'Retained comment', commentId: 'comment', requests: [], responses: [], attachments: [], replyTo: null });
  await advance(t, 5000);
  await drainComments(() => !same.task.busy.value);
  assert.equal(same.task.saved.value.phase, 'succeeded'); assert.equal(f.calls.length, 2); assert.equal(f.reads.length, 2);
  assert.equal(same.task.saved.value.call.arguments.body, 'Retained comment');
});

test('Board disposal prevents a late detail write reply from replacing the retained unknown original', async t => {
  const f = boardActionsFixture(t), mounted = f.mount(), { task } = await mounted.board.detail('ticket');
  let release; f.behavior.write = new Promise(resolve => { release = resolve; });
  const pending = task.start('Move task', { action: 'transition', taskId: 'ticket', expectedRevision: 1, workflowState: 'todo' });
  await settle(); assert.equal(f.calls.length, 1);
  const key = f.actions.actionStorageKey(f.owner, 'task:ticket'), original = sessionStorage.getItem(key);
  mounted.stop(); release(); await pending;
  assert.equal(sessionStorage.getItem(key), original); assert.equal(JSON.parse(original).phase, 'unknown');
  const restored = f.mount(); await advance(t, 5000);
  const recovered = (await restored.board.detail('ticket')).task; await drainComments(() => !recovered.busy.value);
  assert.equal(recovered.saved.value.phase, 'succeeded'); assert.equal(f.calls.length, 1);
});

test('Board disposal prevents a late detail reconciliation from overwriting a newer original', async t => {
  const f = boardActionsFixture(t), mounted = f.mount(), { task } = await mounted.board.detail('ticket');
  f.behavior.lost = true;
  await task.start('Move task', { action: 'transition', taskId: 'ticket', expectedRevision: 1, workflowState: 'todo' });
  let release; f.behavior.read = new Promise(resolve => { release = resolve; });
  const pending = task.reconcile(); await settle(); assert.equal(f.reads.length, 1); mounted.stop();
  f.behavior.read = null; f.behavior.lost = false;
  const restored = f.mount(), current = (await restored.board.detail('ticket')).task;
  await current.reconcile();
  await current.start('Move again', { action: 'transition', taskId: 'ticket', expectedRevision: 2, workflowState: 'waiting' });
  const key = f.actions.actionStorageKey(f.owner, 'task:ticket'), newer = sessionStorage.getItem(key);
  release(); await pending;
  assert.equal(sessionStorage.getItem(key), newer); assert.equal(f.calls.length, 2);
  assert.equal(JSON.parse(newer).call.arguments.workflowState, 'waiting');
});

test('Board releases closed confirmed owners without waiting for capacity and preserves the selected view', async t => {
  const f = boardActionsFixture(t), selected = vue.ref('open'), fences = [];
  const { board } = f.mount(async ids => {
    fences.push([...ids]); board.observe(ids.map(id => boardCard(id, 2, 'todo'))); return true;
  }, id => boardCard(id), () => selected.value);
  const current = await board.detail('open'), originals = [];
  for (let n = 0; n < 8; n++) {
    const id = 'closed-' + n, owner = await board.detail(id); originals.push(owner);
    await owner.edit.start('Change task', { action: 'edit', taskId: id, expectedRevision: 1,
      fields: { title: id, description: 'Retained content '.repeat(8192) } });
  }
  const bytes = () => [...Array(sessionStorage.length)].reduce((sum, _, n) => sum + sessionStorage.getItem(sessionStorage.key(n)).length, 0);
  assert(bytes() > 1024 * 1024); assert.equal(board.entries.value.length, 9); assert.equal(board.detailEntries.value.length, 18);
  await board.release();
  assert.deepEqual(fences, [Array.from({ length: 8 }, (_, n) => 'closed-' + n)]);
  assert.equal(bytes(), 0); assert.equal(board.entries.value.length, 1); assert.equal(board.detailEntries.value.length, 2);
  assert.equal(await board.detail('open'), current); assert.equal(f.calls.length, 8); assert.equal(f.reads.length, 0);
  await originals[0].edit.start('Never send from a released owner', { action: 'edit', taskId: 'closed-0', expectedRevision: 2, fields: { title: 'Never sent' } });
  assert.equal(f.calls.length, 8); assert(originals.every(owner => owner.edit.saved.value === null));
  selected.value = null; await board.release(); assert.equal(board.entries.value.length, 0); assert.equal(sessionStorage.length, 0);
});

test('Board release waits for actual cards and settled pages, coalesces reads and protects a reopened view', async t => {
  const f = boardActionsFixture(t), selected = vue.ref(null); let gate, finish, ready = false, reads = 0, revision = 1;
  const { board } = f.mount(async ids => {
    reads++; await gate; board.observe(ids.map(id => boardCard(id, revision, 'todo'))); return ready;
  }, id => boardCard(id), () => selected.value);
  const owner = await board.detail('ticket');
  await owner.edit.start('Change title', { action: 'edit', taskId: 'ticket', expectedRevision: 1, fields: { title: 'Requested' } });
  const key = f.actions.actionStorageKey(f.owner, 'edit:ticket'), original = sessionStorage.getItem(key);
  await board.release(); assert.equal(sessionStorage.getItem(key), original, 'A successful receipt cannot replace the old page.');
  ready = true; board.clearCards(); await board.release();
  assert.equal(sessionStorage.getItem(key), original, 'A card below the committed revision is insufficient.');
  gate = new Promise(resolve => { finish = resolve; }); revision = 2;
  const first = board.release(), second = board.release(); assert.equal(reads, 3);
  selected.value = 'ticket'; finish(); await Promise.all([first, second]);
  assert.equal(sessionStorage.getItem(key), original); assert.equal((await board.detail('ticket')).edit, owner.edit);
  selected.value = null; await board.release(); assert.equal(sessionStorage.getItem(key), null); assert.equal(board.entries.value.length, 0);
  assert.equal(f.calls.length, 1);
});

test('Board release retains failed and unknown originals and requires an archive read in its receipt epoch', async t => {
  const f = boardActionsFixture(t); let epoch = 'before-reconnect';
  const { board } = f.mount(async ids => {
    board.observe(ids.map(id => ({ ...archiveCard(id, true, 100), observation: { runtimeEpoch: epoch, sequence: 100 } }))); return true;
  }, id => archiveCard(id));
  const archive = (await board.detail('archive')).task;
  await archive.start('Archive task', { action: 'archive', taskId: 'archive', expectedRevision: 1, archived: true }, () => ({ runtimeEpoch: 'hive', sequence: 10 }));
  f.behavior.rejectTask = 'failed'; await f.send(board, 'failed');
  f.behavior.lost = true; await f.send(board, 'unknown');
  const retained = Array.from({ length: sessionStorage.length }, (_, n) => sessionStorage.getItem(sessionStorage.key(n)));
  await board.release(); assert.equal(board.entries.value.length, 3); assert.equal(sessionStorage.length, 3);
  assert(retained.every(value => Array.from({ length: sessionStorage.length }, (_, n) => sessionStorage.getItem(sessionStorage.key(n))).includes(value)));
  epoch = 'hive'; await board.release();
  assert.equal(board.entries.value.length, 2); assert.equal(archive.saved.value, null);
  assert.equal(f.entry(board, 'failed').saved.value.phase, 'failed'); assert.equal(f.entry(board, 'unknown').saved.value.phase, 'unknown');
  assert.equal(f.calls.length, 3); assert.equal(f.reads.length, 0);
});

test('Board release preserves a new original admitted while its earlier terminal record is dismissed', async t => {
  const f = boardActionsFixture(t), { board } = f.mount(undefined, id => boardCard(id, 2));
  const { task: original } = await board.detail('ticket');
  await original.start('Move', { action: 'transition', taskId: 'ticket', expectedRevision: 1, workflowState: 'todo' });
  let finish, followup; const held = new Promise(resolve => { finish = resolve; }); t.after(() => finish());
  const stop = vue.watch(original.saved, value => {
    if (!value && !followup) followup = original.start('Move again', async () => {
      await held; return { action: 'transition', taskId: 'ticket', expectedRevision: 2, workflowState: 'waiting' };
    });
  }, { flush: 'sync' }); t.after(stop);
  await board.release(); assert(followup); assert.equal((await board.detail('ticket')).task, original);
  finish(); await followup;
  assert.equal(original.saved.value.phase, 'succeeded'); assert.equal(original.saved.value.call.arguments.workflowState, 'waiting');
  assert.equal(f.calls.length, 2); assert.notEqual(f.calls[0].operationId, f.calls[1].operationId);
});

test('Board release preserves originals after storage failure, disposal and caller changes during a read', async t => {
  const f = boardActionsFixture(t); let finish, gate;
  const mounted = f.mount(async ids => { await gate; mounted.board.observe(ids.map(id => boardCard(id, 2))); return true; }, id => boardCard(id));
  const action = (await mounted.board.detail('ticket')).edit;
  await action.start('Change title', { action: 'edit', taskId: 'ticket', expectedRevision: 1, fields: { title: 'Requested' } });
  const key = f.actions.actionStorageKey(f.owner, 'edit:ticket'), original = sessionStorage.getItem(key);
  const storage = t.mock.method(sessionStorage, 'removeItem', () => { throw Error('Storage unavailable'); });
  await mounted.board.release(); storage.mock.restore();
  assert.equal(sessionStorage.getItem(key), original); assert.equal((await mounted.board.detail('ticket')).edit, action); assert.match(action.error.value, /retained/);
  await action.reconcile(); const recovered = sessionStorage.getItem(key);
  assert.deepEqual(JSON.parse(recovered).call, JSON.parse(original).call);
  assert.deepEqual(JSON.parse(recovered).outcome, JSON.parse(original).outcome);
  gate = new Promise(resolve => { finish = resolve; });
  const pending = mounted.board.release(); f.owner.callerPrincipalId = 'another-caller'; finish(); await pending;
  assert.equal(sessionStorage.getItem(key), recovered); f.owner.callerPrincipalId = 'caller';
  gate = new Promise(resolve => { finish = resolve; });
  const disposed = mounted.board.release(); mounted.stop(); finish(); await disposed;
  assert.equal(sessionStorage.getItem(key), recovered); assert.equal(f.calls.length, 1);
});

test('Board reclaims idle detail slots without disposing the currently selected view', async t => {
  const f = boardActionsFixture(t), { board } = f.mount(undefined, undefined, () => 'ticket-0');
  const current = await board.detail('ticket-0');
  for (let n = 1; n < 32; n++) await board.detail('ticket-' + n);
  assert.equal(board.entries.value.length, 32);
  await board.detail('overflow'); assert.equal(board.entries.value.length, 2);
  assert.equal(await board.detail('ticket-0'), current);
  await current.task.start('Move task', { action: 'transition', taskId: 'ticket-0', expectedRevision: 1, workflowState: 'todo' });
  assert.equal(f.calls.length, 1); assert.equal(current.task.saved.value.phase, 'succeeded');
});

const archiveBoardCard = (board, card, prepare = Promise.resolve()) => board.run(card.object.objectId, async action => {
  await action.start('Archive task', async () => { await prepare; return { action: 'archive', taskId: card.object.objectId,
    expectedRevision: card.object.revision, archived: true }; }, () => card.observation);
}, { card, archived: true });

function batchArchiveFixture(t, count = 40) {
  const f = boardActionsFixture(t), requests = [], cardReads = [], retirements = [], forgotten = [];
  const cardLimit = JSON.parse(readFileSync('specs/schemas/task-board.schema.json', 'utf8')).$defs.CardListQuery.properties.limit.maximum;
  const cards = new Map(Array.from({ length: count }, (_, index) => { const id = 'batch-' + String(index).padStart(4, '0'); return [id, archiveCard(id)]; }));
  const ids = [...cards.keys()], behavior = { selectionGate: null, cardGate: null, viewGate: null, settled: true, badCards: null, beforeCards: null, repeatCursor: false };
  let selectionDone = false, maximumSlots = 0, changed = 0, board;
  const bindings = { and: (...args) => ({ op: 'and', args }), rootWhere: root => ({ op: 'eq', field: 'object.parentId', value: root }), taskContractVersions: ['3.5.0'],
    client: { request: async (method, params, options) => {
      requests.push([method, structuredClone(params)]);
      if (method === 'objects.query') {
        await behavior.selectionGate; options.signal.throwIfAborted();
        const start = params.cursor ? Number(params.cursor) : 0, nextCursor = start + 200 < ids.length ? String(start + 200) : null;
        selectionDone = !nextCursor;
        return { items: ids.slice(start, start + 200).map(objectId => ({ objectId })), nextCursor: behavior.repeatCursor ? '200' : nextCursor };
      }
      assert.equal(method, 'objects.stat'); assert.equal(params.observe, true);
      const card = cards.get(params.objectId);
      return { id: params.objectId, parentId: f.owner.rootObjectId, contractKey: 'task-board/task', currentRevision: card.object.revision,
        effectivelyArchived: card.archived, archivedAt: card.archived ? '2026-01-01T00:00:00Z' : null, observation: card.observation };
    } } };
  const module = load('ui/task-board-ui/src/board-archive.ts', { './runtime': bindings,
    './task-filter': load('ui/task-board-ui/src/task-filter.ts', { './runtime': bindings }), './task-read': { forgetTask: (...args) => forgotten.push(args) },
    '../../../packages/ui-client/src/native': { nativeRead: async (_client, node, method, params, signal) => {
      assert.equal(node, f.owner.serviceNodeId); assert.equal(method, 'task-board.list'); assert.equal(params.includeArchived, true);
      const ids = params.where.args.find(predicate => predicate.field === 'object.id').value;
      assert(params.limit <= cardLimit); assert.equal(ids.length, params.limit); cardReads.push(ids);
      behavior.beforeCards?.(); await behavior.cardGate; signal?.throwIfAborted();
      let items = ids.flatMap(id => cards.has(id) ? [structuredClone(cards.get(id))] : []);
      if (behavior.badCards === 'foreign') items = [archiveCard('foreign')];
      if (behavior.badCards === 'duplicate') items.push(items[0]);
      return { view: 'cards', items, nextCursor: behavior.badCards === 'cursor' ? 'unexpected' : null };
    } },
  });
  board = f.mount(async ids => {
    retirements.push([...ids]); const current = await module.readArchiveCards(f.owner, ids);
    if (current.length !== ids.length) return false;
    board.observe(current); await behavior.viewGate; return behavior.settled;
  }).board;
  f.behavior.onWrite = call => {
    assert(selectionDone, 'Every selection page is collected before a batch mutation changes it.');
    const card = cards.get(call.arguments.taskId); card.archived = true; card.observation.sequence = 30;
    maximumSlots = Math.max(maximumSlots, board.entries.value.length);
  };
  return { ...f, actionBehavior: f.behavior, module, board, cards, ids, behavior, requests, cardReads, retirements, forgotten,
    maximumSlots: () => maximumSlots, changed: () => changed,
    batch: (blocked = () => false, signal = new AbortController().signal) => module.archiveBoardSelection(() => f.owner,
      new URLSearchParams('category=Alpha'), board, () => { changed++; }, blocked, signal) };
}

test('Board batch archive collects all selection pages and archives beyond the slot limit with bounded current cards', async t => {
  const f = batchArchiveFixture(t, 201), result = await f.batch();
  assert.deepEqual(result, { skipped: 0, stopped: false }); assert.equal(f.calls.length, 201);
  assert.equal(new Set(f.calls.map(call => call.operationId)).size, 201); assert.equal(f.changed(), 201);
  assert.equal(f.requests.filter(([method]) => method === 'objects.query').length, 2);
  assert.equal(f.requests.filter(([method]) => method === 'objects.stat' || method === 'objects.read').length, 0);
  assert(f.maximumSlots() <= 32); assert(f.board.entries.value.length <= 32); assert(sessionStorage.length <= 32);
  assert.equal(f.retirements.length, 6); assert(f.retirements.every(ids => ids.length <= 32));
  assert(f.cardReads.every(ids => ids.length <= 20)); assert([...f.cards.values()].every(card => card.archived));
});

test('Board batch archive skips changed or blocked tickets and retains a lost original without replay', async t => {
  const f = batchArchiveFixture(t, 4);
  f.behavior.beforeCards = () => { f.cards.get(f.ids[0]).task.workflowState = 'todo'; f.behavior.beforeCards = null; };
  const result = await f.batch(id => id === f.ids[1]);
  assert.deepEqual(result, { skipped: 2, stopped: false }); assert.deepEqual(f.calls.map(call => call.arguments.taskId), f.ids.slice(2));
  const earlier = f.calls.length; f.actionBehavior.lost = true;
  const stopped = await f.batch(); assert.equal(stopped.stopped, true); assert.equal(f.calls.length, earlier + 1);
  const original = f.entry(f.board, f.ids[1]); assert.equal(original.saved.value.phase, 'unknown');
  await original.reconcile(); assert.equal(original.saved.value.phase, 'succeeded'); assert.equal(f.calls.length, earlier + 1);
});

test('Board archive capacity keeps confirmed originals until current metadata and held views are both usable', async t => {
  const f = batchArchiveFixture(t, 33); f.behavior.settled = false;
  let result = await f.batch(); assert(result.stopped); assert.equal(f.calls.length, 32); assert.equal(sessionStorage.length, 32);
  assert.match(f.board.error.value.message, /previous task change/);
  f.behavior.settled = true;
  result = await f.batch(); assert.equal(result.stopped, false); assert.equal(f.calls.length, 33);
  assert.equal(f.board.error.value, null); assert(f.board.entries.value.length <= 32);
});

test('Board archive collection aborts or rejects repeated cursors before sending and rejects foreign card pages', async t => {
  const f = batchArchiveFixture(t, 201), controller = new AbortController();
  let release; f.behavior.selectionGate = new Promise(resolve => { release = resolve; });
  const pending = f.batch(undefined, controller.signal); await settle(); controller.abort(); release();
  await assert.rejects(pending); assert.equal(f.calls.length, 0);
  f.behavior.selectionGate = null; f.behavior.repeatCursor = true;
  await assert.rejects(f.batch(), /selection could not finish/); assert.equal(f.calls.length, 0);
  f.behavior.repeatCursor = false;
  for (const value of ['foreign', 'duplicate', 'cursor']) {
    f.behavior.badCards = value; await assert.rejects(f.batch(), /differ from the requested/); assert.equal(f.calls.length, 0);
  }
});

test('Board archive preparation preserves the displayed revision and avoids body reads for a single card', async t => {
  const f = batchArchiveFixture(t, 1), card = structuredClone(f.cards.get(f.ids[0]));
  f.cards.get(f.ids[0]).object.revision++;
  assert.equal(await f.module.archiveBoardCard(() => f.owner, f.board, card, () => {}), false);
  assert.equal(f.calls.length, 0); assert.match(f.entry(f.board, f.ids[0]).error.value, /This task changed/);
  assert.deepEqual(f.requests.map(([method]) => method), ['objects.stat']);
});

test('Board card archives project during preparation and overlap without changing actual card context', async t => {
  const f = boardActionsFixture(t), { board } = f.mount(), card = archiveCard('archive-first');
  let prepare, finish; const header = new Promise(resolve => { prepare = resolve; });
  f.behavior.write = new Promise(resolve => { finish = resolve; }); t.after(() => { prepare(); finish(); });
  const pending = archiveBoardCard(board, card, header);
  assert.equal(board.projections.value[0].archived, true); assert.equal(card.archived, false); assert.equal(f.calls.length, 0);
  prepare(); await settle(); assert.equal(f.calls.length, 1);
  f.behavior.write = null; await archiveBoardCard(board, archiveCard('archive-independent'));
  assert.deepEqual(board.blocked.value, ['archive-first']); assert.equal(f.calls.length, 2);
  board.observe([archiveCard('archive-first', false, 30)]);
  finish(); await pending;
  assert.equal(board.projections.value.find(p => p.card.object.objectId === 'archive-first').archived, false,
    'A newer foreign restore wins over the late successful original archive receipt at the same content revision.');
  assert.equal(f.entry(board, 'archive-first').saved.value.outcome.archive.effectivelyArchived, true);
  board.observe([archiveCard('archive-independent', false, 11)]);
  assert.equal(board.projections.value.find(p => p.card.object.objectId === 'archive-independent').archived, true);
});

test('Board card archive reload reconciles its retained original without replay or accepting a foreign receipt', async t => {
  const f = boardActionsFixture(t), first = f.mount(); f.behavior.lost = true;
  await archiveBoardCard(first.board, archiveCard('archive-lost'));
  const original = structuredClone(vue.toRaw(f.entry(first.board, 'archive-lost').saved.value)); first.stop();
  const { board } = f.mount(); board.observe([archiveCard('archive-lost')]);
  assert.equal(board.projections.value[0].archived, true); assert.equal(f.entry(board, 'archive-lost').saved.value.phase, 'unknown');
  f.behavior.wrongCaller = true; await f.entry(board, 'archive-lost').reconcile();
  assert.equal(f.entry(board, 'archive-lost').saved.value.phase, 'unknown');
  f.behavior.wrongCaller = false; await f.entry(board, 'archive-lost').reconcile();
  assert.equal(f.entry(board, 'archive-lost').saved.value.phase, 'succeeded'); assert.equal(board.projections.value[0].archived, true);
  assert.equal(f.entry(board, 'archive-lost').saved.value.call.operationId, original.call.operationId); assert.equal(f.calls.length, 1);
});

test('Board card archive rejection and unsent storage failure restore actual display while retaining the original', async t => {
  const f = boardActionsFixture(t), { board } = f.mount(); f.behavior.rejectTask = 'archive-rejected';
  await archiveBoardCard(board, archiveCard('archive-rejected'));
  assert.equal(f.entry(board, 'archive-rejected').saved.value.phase, 'failed'); assert.equal(board.projections.value[0].archived, false);
  const storage = t.mock.method(sessionStorage, 'setItem', () => { throw new Error('Storage unavailable'); });
  await archiveBoardCard(board, archiveCard('archive-unsent')); storage.mock.restore();
  assert.equal(f.calls.length, 1); assert.equal(f.entry(board, 'archive-unsent').saved.value.phase, 'prepared');
  assert.equal(board.projections.value.find(p => p.card.object.objectId === 'archive-unsent').archived, false);
  assert.match(f.entry(board, 'archive-unsent').error.value, /retained/);
});

test('Board card archive watermark survives cleared cards and cannot retire from a content revision alone', async t => {
  const f = boardActionsFixture(t), { board } = f.mount();
  for (let index = 0; index < 32; index++) await archiveBoardCard(board, archiveCard('archive-' + index));
  board.clearCards(); await f.send(board, 'overflow'); assert.equal(f.calls.length, 32);
  board.observe([archiveCard('archive-0', false, 19)]); await f.send(board, 'overflow'); assert.equal(f.calls.length, 32);
  board.observe([archiveCard('archive-0', true, 20)]); await f.send(board, 'overflow');
  assert.equal(f.calls.length, 33); assert.equal(board.entries.value.length, 32);
});

test('Board preserves unresolved detail originals and allows an unrelated card archive', async t => {
  const f = boardActionsFixture(t), { board } = f.mount();
  const key = f.actions.actionStorageKey(f.owner, 'task:detail-pending'), original = JSON.stringify({ phase: 'unknown', call: { operationId: 'detail-original', arguments: { taskId: 'detail-pending' } } });
  sessionStorage.setItem(key, original);
  await archiveBoardCard(board, archiveCard('detail-pending')); assert.equal(f.calls.length, 0); assert.match(board.error.value.message, /still being confirmed/);
  await archiveBoardCard(board, archiveCard('unrelated')); assert.equal(f.calls.length, 1); assert.equal(sessionStorage.getItem(key), original);
});

test('Board archive retirement preserves an action started while its earlier terminal record is dismissed', async t => {
  const f = boardActionsFixture(t), { board } = f.mount();
  for (let index = 0; index < 32; index++) await archiveBoardCard(board, archiveCard('retire-' + index));
  board.observe(Array.from({ length: 32 }, (_, index) => archiveCard('retire-' + index, true, 20)));
  const original = f.entry(board, 'retire-0'); let release, followup;
  const prepared = new Promise(resolve => { release = resolve; }); t.after(() => release());
  const stop = vue.watch(() => original.saved.value, value => {
    if (!value && !followup) followup = board.run('retire-0', async action => {
      await prepared; await action.start('Move task', { action: 'transition', taskId: 'retire-0', expectedRevision: 1, workflowState: 'todo', detail: null });
    });
  }, { flush: 'sync' }); t.after(stop);
  await f.send(board, 'overflow'); assert(followup); assert.equal(f.entry(board, 'retire-0'), original);
  release(); await followup; assert.equal(f.calls.filter(call => call.arguments.taskId === 'retire-0').length, 2);
  assert.equal(original.saved.value.phase, 'succeeded');
});

test('Board projects cards during preparation using the existing original slot without changing authoritative action context', async t => {
  const f = boardActionsFixture(t), { board } = f.mount(), card = boardCard('projected');
  let prepare, finish; const header = new Promise(resolve => { prepare = resolve; });
  f.behavior.write = new Promise(resolve => { finish = resolve; }); t.after(() => { prepare(); finish(); });
  const first = board.run('projected', async action => {
    await action.start('Move task', async () => { await header; return { action: 'transition', taskId: 'projected', expectedRevision: 1, workflowState: 'todo', detail: null }; });
  }, { card, state: 'todo' });
  assert.equal(board.projections.value[0].state, 'todo'); assert.equal(card.task.workflowState, 'done');
  assert.equal(board.projections.value[0].card, card); assert.equal(f.calls.length, 0);
  prepare(); await settle(); assert.equal(f.calls.length, 1);
  const independent = f.send(board, 'independent'); await settle(); assert.equal(f.calls.length, 2);
  assert.deepEqual([...board.blocked.value].sort(), ['independent', 'projected']);
  finish(); await Promise.all([first, independent]);
  assert.equal(board.projections.value[0].card.object.revision, 1, 'A receipt does not turn the display card into an authoritative new revision.');
  board.observe([boardCard('projected', 2, 'todo')]); board.observe([card]);
  assert.equal(board.projections.value[0].state, 'todo'); assert.equal(board.projections.value[0].card.object.revision, 2);
  board.observe([boardCard('projected', 3, 'waiting')]); assert.equal(board.projections.value[0].state, 'waiting');
  f.behavior.reject = undefined;
  await board.run('rejected', async action => { await action.start('Move', { action: 'transition', taskId: 'rejected', expectedRevision: 1, workflowState: 'todo', detail: null }); }, { card: boardCard('rejected'), state: 'todo' });
  assert.equal(f.entry(board, 'rejected').saved.value.phase, 'failed'); assert.equal(board.projections.value.find(x => x.card.object.objectId === 'rejected').state, 'done');
  const count = f.calls.length;
  await board.run('unsent', async () => { throw Error('Header unavailable'); }, { card: boardCard('unsent'), state: 'todo' });
  assert.equal(f.calls.length, count); assert.equal(f.entry(board, 'unsent').saved.value, null); assert.equal(board.projections.value.find(x => x.card.object.objectId === 'unsent').state, 'done');
  board.clearCards(); assert.deepEqual(board.projections.value, [], 'A filter change drops retained display cards, preserving original actions.');
});

test('Board restores a card projection from the original journal and observed page without replay or foreign ownership', async t => {
  const f = boardActionsFixture(t), first = f.mount(); f.behavior.lost = true;
  await first.board.run('original', async action => { await action.start('Move', { action: 'transition', taskId: 'original', expectedRevision: 1, workflowState: 'todo', detail: null }); }, { card: boardCard('original'), state: 'todo' });
  const original = structuredClone(vue.toRaw(f.entry(first.board, 'original').saved.value)); first.stop();
  const restored = f.mount(); assert.deepEqual(restored.board.projections.value, []);
  restored.board.observe([boardCard('another'), boardCard('original')]);
  assert.equal(restored.board.projections.value.length, 1); assert.equal(restored.board.projections.value[0].state, 'todo');
  assert.deepEqual(vue.toRaw(f.entry(restored.board, 'original').saved.value), original); assert.equal(f.calls.length, 1); assert.deepEqual(f.reads, []);
  await f.entry(restored.board, 'original').reconcile(); assert.equal(f.calls.length, 1);
  f.owner.callerPrincipalId = 'another-caller'; assert.deepEqual(restored.board.projections.value, []);
  await f.send(restored.board, 'foreign'); assert.equal(f.calls.length, 1);
});

test('Board bounds display cards with the existing slots and retires successes only after their card revision is observed', async t => {
  const f = boardActionsFixture(t), { board } = f.mount(); f.behavior.lost = true;
  for (let index = 0; index < 32; index++) {
    const id = 'card-' + index;
    await board.run(id, async action => { await action.start('Move', { action: 'transition', taskId: id, expectedRevision: 1, workflowState: 'todo', detail: null }); }, { card: boardCard(id), state: 'todo' });
  }
  assert.equal(board.projections.value.length, 32); await f.send(board, 'overflow'); assert.equal(f.calls.length, 32);
  for (const entry of board.entries.value) await entry.action.reconcile();
  await f.send(board, 'overflow'); assert.equal(f.calls.length, 32, 'Receipt success alone does not evict a still-unobserved display change.');
  board.observe(Array.from({ length: 32 }, (_, index) => boardCard('card-' + index, 2, 'todo')));
  await f.send(board, 'overflow'); assert.equal(f.calls.length, 33); assert(board.entries.value.length <= 32);
  assert.equal(board.error.value, null);
});

test('shared page controls distinguish omitted previous availability from explicit false in their rendered buttons', async t => {
  const mount = setup(t), icon = { render: () => null };
  for (const [props, previousDisabled] of [
    [{ page: 1 }, true], [{ page: 3 }, false],
    [{ page: 3, hasPrevious: false }, true], [{ page: 1, hasPrevious: true }, false],
    [{ page: 3, loading: true }, true],
  ]) {
    const buttons = [];
    const component = load('packages/ui/src/patterns/PageControls.vue', {
      '@lucide/vue': { ChevronLeft: icon, ChevronRight: icon },
      '../components/button': { Button: { props: { disabled: Boolean },
        setup(props) { buttons.push(props.disabled); return () => null; } } },
    }, true).default;
    mount(component, { count: 20, hasNext: true, ...props }); await settle();
    assert.deepEqual(buttons, [previousDisabled, props.loading ?? false], JSON.stringify(props));
  }
});

test('virtual rows bound the rendered range, preserve focused controls and discard previous page measurements', async t => {
  setup(t);
  const library = require('@tanstack/vue-virtual');
  let observeOffset;
  const component = load('packages/ui/src/patterns/VirtualList.vue', {
    '@tanstack/vue-virtual': { ...library, useVirtualizer: options => library.useVirtualizer(vue.computed(() => ({
      ...options.value,
      observeElementRect: (_instance, callback) => { callback({ width: 390, height: 600 }); return () => {}; },
      observeElementOffset: (_instance, callback) => { observeOffset = callback; callback(0, false); return () => {}; },
      scrollToFn: offset => observeOffset?.(offset, false),
    }))) },
  }).default;
  const items = Array.from({ length: 100 }, (_, i) => ({ id: 'row-' + i }));
  const app = renderer.createApp({ ...component, render: () => null }, { items, itemKey: item => item.id });
  app.mount({});
  const api = app._instance.setupState;
  try {
    api.viewport = { ownerDocument: { defaultView: { performance, setTimeout, clearTimeout,
      requestAnimationFrame: callback => setTimeout(() => callback(performance.now()), 16), cancelAnimationFrame: clearTimeout } },
      scrollHeight: 20000, scrollWidth: 390, clientHeight: 600, clientWidth: 390, contains: () => false };
    await settle(); assert.ok(api.rows.length > 0 && api.rows.length < 12);
    api.scrollToKey('row-80'); await settle(); assert.ok(api.rows.some(row => row.key === 'row-80'));
    api.focus({ target: { closest: () => ({ dataset: { index: '80' } }) } });
    api.scrollToKey('row-2'); await settle();
    assert.ok(api.rows.some(row => row.key === 'row-80'), 'A focused control is retained outside the visible range.');
    assert.ok(api.rows.length < 12);
    api.blur({ relatedTarget: null }); await settle(); assert.ok(!api.rows.some(row => row.key === 'row-80'));
    api.virtualizer.resizeItem(2, 1200); await settle();
    assert.equal(api.virtualizer.itemSizeCache.get('row-2'), 1200);
    app._instance.props.items = Array.from({ length: 50 }, (_, i) => ({ id: 'next-' + i })); await settle();
    assert.equal(api.virtualizer.itemSizeCache.size, 0, 'Old page measurements cannot accumulate across navigation.');
    assert.ok(api.rows.every(row => String(row.key).startsWith('next-')));
    api.scrollToKey('next-49', 'end'); await settle(); assert.ok(api.rows.some(row => row.key === 'next-49'));
    let prevented = 0;
    api.navigate({ target: api.viewport, key: 'Home', preventDefault() { prevented++; } }); await settle();
    assert.ok(api.rows.some(row => row.key === 'next-0'));
    api.navigate({ target: api.viewport, key: 'End', preventDefault() { prevented++; } }); await settle();
    assert.ok(api.rows.some(row => row.key === 'next-49'));
    api.navigate({ target: {}, key: 'Home', preventDefault() { prevented++; } }); await settle();
    assert.equal(prevented, 2, 'Keyboard navigation belongs to the viewport and leaves focused controls alone.');
  } finally { app.unmount(); }
});

function commentPages(t, count = 1003, follow = () => true) {
  const mount = setup(t), calls = [];
  const fixture = Array.from({ length: count }, (_, i) => ({ commentId: 'message-' + (i + 1), sequence: i + 1 }));
  const snapshot = (before = fixture.length + 1) => ({ task: { comments: fixture.filter(comment => comment.sequence < before).slice(-50).toReversed() },
    commentPage: { total: fixture.length } });
  const current = vue.shallowRef(snapshot());
  let failure = false, deferred;
  const { useCommentPages } = load('ui/task-board-ui/src/comment-pages.ts');
  const api = mount({ setup() { return useCommentPages(current, async (before, signal) => {
    calls.push({ before, signal });
    if (deferred) return deferred.promise;
    if (failure) throw new Error('History temporarily unavailable');
    return snapshot(before);
  }, follow); } });
  return { api, calls, fixture, current, snapshot, fail: value => { failure = value; }, delay: value => { deferred = value; } };
}

test('ticket history traverses both ways with one selected page and a separate current snapshot', async t => {
  const { api, calls, fixture, current, snapshot } = commentPages(t); await settle();
  const seen = [...api.comments];
  while (api.hasOlder) {
    assert.equal(await api.older(), true); await settle();
    assert.ok(api.comments.length <= 50); assert.ok(api.latestComments.length <= 50);
    seen.unshift(...api.comments);
  }
  assert.deepEqual(seen.map(comment => comment.sequence), fixture.map(comment => comment.sequence));
  assert.equal(calls.length, 20, 'No hidden full-history fetch or page cache is needed.');
  const selected = api.comments;
  fixture.push({ commentId: 'message-1004', sequence: 1004 }); current.value = snapshot(); await settle();
  assert.equal(api.comments, selected, 'A live append does not replace the page being read.');
  assert.equal(api.total, 1004); assert.equal(api.latestComments.at(-1).sequence, 1004);
  await api.newer(); assert.deepEqual(api.comments.map(comment => comment.sequence), Array.from({ length: 50 }, (_, i) => i + 4));
  while (api.hasNewer) { await api.newer(); assert.ok(api.comments.length <= 50); }
  assert.equal(api.historical, false); assert.equal(api.comments.at(-1).sequence, 1004);
});

test('history failures, cancelled late results and old question jumps retain bounded pages', async t => {
  const { api, calls, fixture, current, snapshot, fail, delay } = commentPages(t, 151); await settle();
  const original = api.comments; fail(true);
  assert.equal(await api.older(), false); assert.equal(api.error, 'History temporarily unavailable'); assert.equal(api.comments, original);
  fail(false); await api.older(); assert.equal(calls.at(-1).before, 102); assert.equal(api.error, null);
  const selected = api.comments;
  let resolve;
  delay({ promise: new Promise(done => { resolve = done; }) });
  const loading = api.older(); await settle(); assert.equal(api.loading, true);
  api.latest(); assert.equal(calls.at(-1).signal.aborted, true);
  resolve(snapshot(52)); await loading; assert.equal(api.historical, false); assert.equal(api.comments, original);
  delay(null);
  const before = calls.length;
  assert.equal(await api.find('message-2', 2), true); assert.equal(calls.length, before + 1);
  assert.ok(api.comments.length <= 50); assert.ok(api.comments.some(comment => comment.commentId === 'message-2'));
  fixture.push({ commentId: 'message-152', sequence: 152 }); current.value = snapshot(); await settle();
  assert.equal(api.comments.at(-1).sequence, 26, 'Fresh status/comments do not accumulate in a historical page.');
  assert.equal(await api.find('message-150'), true); assert.equal(api.historical, false);
  assert.ok(selected.length <= 50);
});

test('a reader away from the newest message keeps their visible page when live comments arrive', async t => {
  let following = false;
  const { api, fixture, current, snapshot } = commentPages(t, 80, () => following); await settle();
  const ids = api.comments.map(comment => comment.commentId);
  fixture.push({ commentId: 'message-81', sequence: 81 }); current.value = snapshot(); await settle();
  assert.deepEqual(api.comments.map(comment => comment.commentId), ids); assert.equal(api.historical, true);
  assert.equal(api.total, 81); assert.equal(api.hasNewer, true);
  api.latest(); following = true;
  fixture.push({ commentId: 'message-82', sequence: 82 }); current.value = snapshot(); await settle();
  assert.equal(api.historical, false); assert.equal(api.comments.at(-1).sequence, 82);
});

test('leaving a ticket aborts its original history read and ignores an uncancellable late reply', async t => {
  setup(t);
  const { useCommentPages } = load('ui/task-board-ui/src/comment-pages.ts');
  let api, signal, resolve;
  const app = renderer.createApp({ setup() {
    api = useCommentPages(vue.shallowRef({ task: { comments: [{ sequence: 80 }] }, commentPage: { total: 80 } }),
      (_before, input) => { signal = input; return new Promise(done => { resolve = done; }); });
    return () => null;
  } });
  app.mount({}); const read = api.older(); app.unmount(); assert.equal(signal.aborted, true);
  resolve({ task: { comments: [{ sequence: 30 }] } }); await read;
  assert.equal(api.historical.value, false); assert.equal(api.loading.value, false);
});

function taskPrefetch(read) {
  const calls = [], client = { request: async () => ({ connected: true, synced: true, ready: true, desiredEnabled: true }) };
  const { nativeRead } = load('packages/ui-client/src/native.ts', { '../../sdk/src/client.js': {
    IvyError, canonical: JSON.stringify, discover: async (_client, method, owner) => ({ method, definition: {}, ...owner }),
    callBound: async (_client, _binding, args, _operation, options) => {
      calls.push({ task: args.task, signal: options.signal }); return read(args.task, options.signal);
    },
  } });
  return { calls, ...load('ui/task-board-ui/src/task-read.ts', { './runtime': { client }, '../../../packages/ui-client/src/native': { nativeRead } }) };
}

test('hover snapshots expire without further hover activity and a selected panel owns cancellation', async t => {
  t.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 100000 });
  const active = new Map();
  const { calls, prefetchTask, readTask } = taskPrefetch((task, signal) => new Promise((resolve, reject) => {
    active.set(task, { resolve, signal }); signal.addEventListener('abort', () => reject(signal.reason), { once: true });
  }));
  prefetchTask('owner', 'one'); prefetchTask('owner', 'two'); prefetchTask('owner', 'three'); await settle();
  assert.equal(calls.length, 2, 'Pointer movement has at most two speculative reads.');
  const consumer = new AbortController(), selected = readTask('owner', 'one', consumer.signal); await settle();
  assert.equal(calls.length, 2, 'Selection joins the SDK read before releasing the hover.');
  t.mock.timers.tick(5000); await settle();
  assert.equal(active.get('two').signal.aborted, true, 'Unused speculative work expires actively.');
  assert.equal(active.get('one').signal.aborted, false, 'Hover expiry cannot cancel the selected reader.');
  consumer.abort(); await assert.rejects(selected); assert.equal(active.get('one').signal.aborted, true);
  const cancelled = new AbortController(); cancelled.abort();
  await assert.rejects(readTask('owner', 'cancelled', cancelled.signal)); assert.equal(calls.length, 2);
});

test('hover payload admission uses string bytes, bounds the aggregate and consumes snapshots once', async t => {
  t.mock.timers.enable({ apis: ['Date', 'setTimeout'], now: 100000 });
  const { calls, prefetchTask, readTask } = taskPrefetch(async task => ({ task, body: 'x'.repeat(task === 'oversized' ? 300000 : 180000) }));
  const signal = new AbortController().signal;
  prefetchTask('owner', 'oversized'); await settle(); await readTask('owner', 'oversized', signal);
  assert.equal(calls.length, 2, 'A view over 512 KiB is not retained by hover.');
  for (let i = 0; i < 20; i++) { prefetchTask('owner', 'small-' + i); await settle(); }
  const before = calls.length; await readTask('owner', 'small-0', signal);
  assert.equal(calls.length, before + 1, 'The 4 MiB aggregate evicts before reaching the count cap.');
  await readTask('owner', 'small-19', signal); assert.equal(calls.length, before + 1);
  await readTask('owner', 'small-19', signal); assert.equal(calls.length, before + 2, 'A cached snapshot is consumed once.');
  prefetchTask('owner', 'expiry'); await settle(); t.mock.timers.tick(5000); await settle();
  const expired = calls.length; await readTask('owner', 'expiry', signal); assert.equal(calls.length, expired + 1);
});

test('waiting cards distinguish queued continuation from real execution failure', () => {
  const { waitingGroup, conditionTone } = load('ui/task-board-ui/src/board.ts', {
    '@lucide/vue': {}, './runtime': { tr: (_de, en) => en },
  });
  const task = { workflowState: 'waiting', fields: { control: 'agent' }, claim: null };
  assert.equal(waitingGroup(task, { state: 'queued' }), 'preparing');
  assert.equal(conditionTone('queued'), 'neutral');
  assert.equal(waitingGroup(task, { state: 'failed' }), 'error');
  assert.equal(conditionTone('failed'), 'bad');
  assert.equal(waitingGroup(task, { state: 'recovering' }), 'runtime');
});

function boardColumn(t, columnId, swimlane, options = {}) {
  const mount = options.mount ?? setup(t), calls = [], fixture = options.rows ?? Array.from({ length: 53 }, (_, i) => ({
    object: { objectId: 'task-' + i, revision: i + 1 },
    task: { taskKey: 'TASK-' + i, workflowState: columnId === 'waiting' ? 'waiting' : 'review',
      fields: { title: 'Task ' + i, category: ['Alpha', 'Beta', 'Gamma'][Math.floor(i / 20)],
        control: 'user', priority: i < 20 ? 4 : i < 40 ? 2 : 0 }, claim: null, publication: null,
      runtimeWait: null, latestResult: null, openQuestionCount: 0, wakeAt: null },
    executionCondition: { state: i < 20 ? 'failed' : i < 40 ? 'deferred' : 'needs_user', detail: '' },
    commentCount: i, directCommentCount: i,
  }));
  const and = (...args) => ({ op: 'and', args }), runtimeBindings = { and,
    rootWhere: root => ({ op: 'eq', field: 'object.parentId', value: root }),
    tr: (_de, en) => en, base: new URL('https://fixture.invalid/'), client: {} };
  const board = load('ui/task-board-ui/src/board.ts', { '@lucide/vue': {}, './runtime': runtimeBindings });
  const filter = load('ui/task-board-ui/src/task-filter.ts', { './runtime': runtimeBindings });
  let failure = false;
  const component = load('ui/task-board-ui/src/BoardColumn.vue', {
    '@lucide/vue': {}, '@ivy/ui': ui, './runtime': runtimeBindings, './board': board,
    './task-filter': filter, './ArchiveOutputsHint.vue': {}, './task-read': { prefetchTask() {} },
    './comment-visibility': { useCommentVisibility: () => vue.ref(false) },
    '../../../packages/ui-client/src/runtime': runtime,
    '../../../packages/ui-client/src/live': { objectParentScope },
    '../../../packages/ui-client/src/native': { nativeRead: async (_client, node, name, args, signal) => {
      assert.equal(node, 'owner'); assert.equal(name, 'task-board.list'); assert.equal(args.view, 'cards');
      assert.equal(args.limit, 20); assert.ok(signal instanceof AbortSignal); calls.push(structuredClone(args));
      if (failure && args.cursor) throw new Error('Page temporarily unavailable');
      const start = args.cursor ? Number(args.cursor.slice('opaque-'.length)) : 0;
      return { view: 'cards', items: structuredClone(fixture.slice(start, start + 20)),
        nextCursor: start + 20 < fixture.length ? 'opaque-' + (start + 20) : null };
    } },
  }).default;
  const notifications = new Notifications();
  const api = mount(component, { workspace: { serviceNodeId: 'owner', callerPrincipalId: 'reader',
    rootObjectId: 'root', role: 'user' }, query: new URLSearchParams('control=user&host=host'),
    column: { id: columnId, title: columnId, states: columnId === 'waiting' ? ['todo', 'waiting'] : ['review'] },
    count: fixture.length, href: id => '#/' + id, disabled: false, active: true, swimlane,
    lanes: swimlane === 'category' ? ['Alpha', 'Beta', 'Gamma'] : [''], ...options.props }, notifications);
  return { api, calls, fixture, notifications, mount, fail: value => { failure = value; } };
}

test('Board column projections move one displayed card without changing its action context, neighbours or server page', async t => {
  const projections = vue.reactive([]);
  const source = boardColumn(t, 'waiting', 'category', { props: { projections } }); await settle();
  const card = source.fixture[0], other = boardCard('other', 1, 'done');
  const target = boardColumn(t, 'done', 'category', { mount: source.mount, rows: [other], props: {
    projections, column: { id: 'done', title: 'Done', states: ['done', 'cancelled'] } } }); await settle();
  projections.push({ card, state: 'done' }); await settle();
  assert(!source.api.items.some(x => x.id === card.object.objectId));
  const shown = target.api.items.find(x => x.id === card.object.objectId); assert(shown);
  assert.equal(shown.context.workflowState, card.task.workflowState); assert.notEqual(shown.context.workflowState, 'done');
  assert.equal(target.api.taskDisabled(shown.id), true); assert.equal(target.api.movable(shown), false);
  assert.equal(target.api.sameLane(target.api.items[0], shown), false, 'A projected card is not a business ranking neighbour.');
  assert.equal(source.api.pages.value.value.items.length, 20); assert.equal(source.api.pages.value.value.items[0].context.workflowState, card.task.workflowState);
  assert.equal(source.calls.length, 1); assert.equal(target.calls.length, 1, 'Display projection adds no full task reads or page requests.');
  const confirmed = structuredClone(card); confirmed.object.revision++; confirmed.task.workflowState = 'done';
  target.fixture.push(confirmed); projections[0] = { card: confirmed, state: 'done' };
  await target.api.pages.refresh(); await settle();
  assert.equal(target.api.items.filter(x => x.id === shown.id).length, 1); assert.equal(target.api.taskDisabled(shown.id), false);
  await source.api.pages.refresh(); await settle(); assert(!source.api.items.some(x => x.id === shown.id), 'An older source page cannot make a confirmed card reappear.');
  source.api.pages.next(); await settle(); assert.equal(source.api.pages.page.value, 2);
  source.api.reveal('done'); assert.equal(source.api.pages.page.value, 2, 'Other destination columns keep their current page.');
  source.api.reveal('todo'); await settle(); assert.equal(source.api.pages.page.value, 1);
});

test('Board archive projection hides a stale normal card and displays it in the archive without granting workflow actions', async t => {
  const card = archiveCard('archive-card'), projections = vue.reactive([{ card, state: 'done', archived: true }]);
  const source = boardColumn(t, 'done', 'none', { rows: [card], props: { projections, column: { id: 'done', title: 'Done', states: ['done'] } } });
  await settle(); assert.equal(source.api.items.length, 0); assert.equal(source.api.pages.value.value.items[0].card.archived, false);
  const target = boardColumn(t, 'archive', 'none', { mount: source.mount, rows: [], props: { archived: true, projections } });
  await settle(); assert.equal(target.api.items.length, 1); assert.deepEqual(target.api.items[0].card, card);
  assert.equal(target.api.movable(target.api.items[0]), false); assert.deepEqual(target.api.actions(target.api.items[0]), []);
  projections[0] = { card, state: 'done', archived: false };
  assert.equal(source.api.items.length, 1); assert.equal(target.api.items.length, 0);
});

test('Board archive view barrier keeps failed or hidden pages and clears inactive held cards', async t => {
  const f = boardColumn(t, 'review', 'none'); await settle();
  const first = f.api.pages.value.value; assert.equal(await f.api.settle(), true); assert.notEqual(f.api.pages.value.value, first);
  f.api.pages.next(); await settle(); f.fail(true); const held = f.api.pages.value.value;
  assert.equal(await f.api.settle(), false); assert.equal(f.api.pages.value.value, held);
  document.hidden = true; const before = f.calls.length; assert.equal(await f.api.settle(), false); assert.equal(f.calls.length, before);
  document.hidden = false;
  const inactive = boardColumn(t, 'archive', 'none', { mount: f.mount, props: { active: false, archived: true } }); await settle();
  inactive.api.pages.value.value = held; assert.equal(await inactive.api.settle(), true); assert.equal(inactive.api.pages.value.value, null);
  const fenced = boardColumn(t, 'done', 'none', { mount: f.mount, rows: [archiveCard('fenced')] }); await settle();
  const observation = { runtimeEpoch: 'hive', sequence: 20 };
  assert.equal(await fenced.api.settle(observation), false, 'A new response with an old page observation cannot retire archive records.');
  fenced.fixture[0].observation.sequence = 20; assert.equal(await fenced.api.settle(observation), true);
});

test('workspace write hints isolate board pages and preserve broad, owner, hidden and reconnect updates', async t => {
  const { calls, notifications } = boardColumn(t, 'waiting', 'category'); await settle();
  assert.deepEqual([...notifications.changes.values()], [['objects/task-board/task/parent/root', 'services/task-board/owner']]);
  notifications.change(objectParentScope('task-board/task', 'other')); await advance(t, 250);
  notifications.change(objectParentScope('task-board/task', null)); await advance(t, 250);
  assert.equal(calls.length, 1, 'Another workspace and top-level writes do not reload this page.');
  for (const scope of [objectParentScope('task-board/task', 'root'), 'objects/task-board/task', 'objects', 'services/task-board/owner']) {
    for (let i = 0; i < 5; i++) notifications.change(scope);
    await advance(t, 250);
  }
  assert.equal(calls.length, 5, 'Each relevant burst uses one existing page refresh.');
  document.hidden = true; document.dispatchEvent(new Event('visibilitychange'));
  notifications.change(objectParentScope('task-board/task', 'root')); await advance(t, 30000);
  assert.equal(calls.length, 5);
  document.hidden = false; document.dispatchEvent(new Event('visibilitychange')); await advance(t, 250);
  assert.equal(calls.length, 6, 'Visibility resumes from the retained dirty hint.');
  notifications.ready(false); notifications.ready(true); await advance(t, 250);
  assert.equal(calls.length, 7, 'Reconnect observes authoritative current state.');
});

test('parent hint scopes distinguish top-level objects and preserve bounded family fallback', () => {
  assert.equal(objectParentScope('fixture/value', null), 'objects/fixture/value/root');
  assert.equal(objectParentScope('fixture/value', 'root'), 'objects/fixture/value/parent/root');
  assert.equal(objectParentScope('fixture/value', 'alpha._-9'), 'objects/fixture/value/parent/alpha._-9');
  for (const id of ['', 'a/b', '🙂', 'x'.repeat(500)])
    assert.equal(objectParentScope('fixture/value', id), 'objects/fixture/value');
  const length = 512 - 'objects/fixture/value/parent/'.length;
  assert.equal(objectParentScope('fixture/value', 'x'.repeat(length)).length, 512);
  assert.equal(objectParentScope('fixture/value', 'x'.repeat(length + 1)), 'objects/fixture/value');
});

for (const [columnId, swimlane] of [['waiting', 'none'], ['waiting', 'category'], ['review', 'category']])
test(`board ${columnId}/${swimlane} retains one bounded page and reaches every group through original cursors`, async t => {
  const { api, calls, fixture } = boardColumn(t, columnId, swimlane); await settle();
  assert.equal(calls.length, 1, 'A continuation never triggers an automatic whole-column collection.');
  assert.equal(api.items.length, 20); assert.equal(api.paginated, true);
  assert.equal(api.pages.value.value.nextCursor, 'opaque-20');
  assert.match(JSON.stringify(calls[0].where), /object.parentId.*root/);
  assert.match(JSON.stringify(calls[0].where), /data:\/fields\/control.*user/);
  assert.match(JSON.stringify(calls[0].where), /data:\/fields\/executionRequirement\/hostId.*host/);
  const ids = api.items.map(item => item.id);
  api.pages.next(); await settle(); assert.equal(api.items.length, 20); ids.push(...api.items.map(item => item.id));
  api.pages.next(); await settle(); assert.equal(api.items.length, 13); ids.push(...api.items.map(item => item.id));
  assert.equal(api.pages.value.value.nextCursor, null); assert.equal(api.paginated, true);
  assert.deepEqual(calls.map(call => call.cursor), [undefined, 'opaque-20', 'opaque-40']);
  assert.deepEqual([...ids].sort(), fixture.map(row => row.object.objectId).sort());
  if (columnId === 'waiting') assert.equal(api.sections.flatMap(section => section.groups).find(group => group.id === 'user').items.length, 13,
    'Lower-priority User waits remain reachable on the final page.');
  api.pages.previous(); await settle(); assert.equal(calls.at(-1).cursor, 'opaque-20'); assert.equal(api.items.length, 20);
  api.toggleOrder(); await settle(); assert.equal(api.pages.page.value, 1); assert.equal(calls.at(-1).cursor, undefined);
  assert.deepEqual(calls.at(-1).orderBy.at(-1), { field: 'object.position', direction: 'asc' });
});

test('board grouped page failures retain cards and retry the original cursor without fetching later pages', async t => {
  const { api, calls, fail } = boardColumn(t, 'waiting', 'category'); await settle();
  const first = api.items.map(item => item.id); fail(true); api.pages.next(); await settle();
  assert.equal(api.pages.error.value, 'Page temporarily unavailable'); assert.deepEqual(api.items.map(item => item.id), first);
  assert.equal(calls.length, 2); assert.equal(calls.at(-1).cursor, 'opaque-20');
  fail(false); await api.pages.refresh(); await settle();
  assert.equal(api.pages.error.value, null); assert.equal(calls.length, 3); assert.equal(calls.at(-1).cursor, 'opaque-20');
  assert.equal(api.items.length, 20); assert.equal(api.items[0].id, 'task-20');
});

function attachmentsFixture(t, call, backend) {
  const mount = setup(t);
  const { useMessageAttachments } = load('packages/ui-client/src/message-attachments.ts', {
    '../../sdk/src/client.js': { serviceTools: () => ({ call }) },
  });
  let api;
  mount({ setup() { api = useMessageAttachments({}, () => 'owner', backend); return {}; } });
  return api;
}
async function settleFiles() { for (let i = 0; i < 8; i++) { await new Promise(setImmediate); await settle(); } }

test('Blob URL retention evicts by bytes and entry count, preserves the open image and revokes replaced URLs', t => {
  const created = [], revoked = [], removed = [];
  t.mock.method(URL, 'createObjectURL', file => { const url = 'blob:fixture-' + created.length; created.push({ url, bytes: file.size }); return url; });
  t.mock.method(URL, 'revokeObjectURL', url => revoked.push(url));
  const { BlobUrls } = load('packages/ui-client/src/blob-urls.ts');
  const cache = new BlobUrls(key => removed.push(key), 12, 3);
  const first = cache.put('first', new Blob(['12345678'])); cache.pin('first');
  const second = cache.put('second', new Blob(['1234']));
  const third = cache.put('third', new Blob(['12']));
  assert.equal(cache.get('first'), first); assert.equal(cache.get('second'), null);
  assert.deepEqual(revoked, [second]);
  assert.throws(() => cache.put('large', new Blob(['1234567890123'])), /budget/);
  assert.throws(() => cache.put('blocked', new Blob(['12345'])), /open image/);
  assert.equal(cache.get('third'), third, 'Rejected admission keeps existing images intact.');
  cache.pin(null);
  const replacement = cache.put('first', new Blob(['1234']));
  assert.notEqual(first, replacement); assert(revoked.includes(first));
  cache.put('empty-1', new Blob()); cache.put('empty-2', new Blob());
  assert.equal(cache.get('third'), null, 'Even zero-byte files respect the entry bound.');
  cache.clear();
  assert.equal(new Set(revoked).size, created.length); assert.equal(revoked.length, created.length);
  assert.equal(removed.length, created.length);
});

test('Attachment wire decoding uses bounded binary strings and refuses incomplete or padded intermediate data', t => {
  const original = globalThis.atob, chunks = [];
  t.mock.method(globalThis, 'atob', value => { chunks.push(value.length); return original(value); });
  const { base64Bytes } = load('packages/ui-client/src/content.ts', { '../../sdk/src/client.js': { canonical } });
  const bytes = Buffer.from(Uint8Array.from({ length: 100001 }, (_, n) => n % 256));
  assert.deepEqual(Buffer.from(base64Bytes(bytes.toString('base64'))), bytes);
  assert(Math.max(...chunks) <= 32768); assert(chunks.length > 1);
  assert.equal(base64Bytes('').length, 0);
  assert.throws(() => base64Bytes('abc'), /length/);
  assert.throws(() => base64Bytes('AA=='.repeat(8193)));
});

test('Task uploads retain compact metadata and replay only the exact original file and operation after a lost reply', async t => {
  const mount = setup(t), files = new Map(), calls = [];
  const owner = { serviceNodeId: 'owner', callerPrincipalId: 'caller', principalId: 'workflow', rootObjectId: 'root' };
  const content = load('packages/ui-client/src/content.ts', { '../../sdk/src/client.js': { canonical } });
  const hashes = load('packages/ui-client/src/hash.ts', { '../../sdk/src/client.js': { canonical } });
  const encoder = load('packages/ui-client/src/message-attachments.ts', { '../../sdk/src/client.js': {} });
  let ids = 0, receipt = null, failWrite = true;
  const { useAction } = load('ui/task-board-ui/src/action.ts', {
    '../../../packages/sdk/src/client.js': { IvyError, discover: async (_client, name) => ({ qualifiedName: name, serviceNodeId: owner.serviceNodeId, definitionHash: 'definition' }), newOperationId: async () => 'upload-' + ++ids },
    './runtime': { base: new URL('https://fixture.invalid/'), client: { request: async (_method, call) => {
      calls.push(JSON.parse(JSON.stringify(call)));
      if (failWrite) throw new IvyError('outcome_unknown', 'Lost response', 'unknown');
      return { operationId: call.operationId };
    } } },
    '../../../packages/ui-client/src/native': { nativeRead: async (_client, _node, method) => {
      if (method === 'task-board.workspace') return owner;
      if (!receipt) throw new IvyError('not_found', 'Not found');
      return receipt;
    } },
    '../../../packages/ui-client/src/hash': hashes,
    '../../../packages/ui-client/src/content': content,
    '../../../packages/ui-client/src/message-attachments': encoder,
    '../../../packages/ui-client/src/object-archive': {},
    './upload-store': { maximumUploadBytes: 8388608, recoverUpload: async () => null,
      retainUpload: async (_scope, action, file) => files.set(action.call.operationId, file),
      saveUpload: async () => {}, uploadFile: async action => files.get(action.call.operationId) ?? null,
      releaseUpload: async action => files.delete(action.call.operationId) },
  });
  let action;
  mount({ setup() { action = useAction(() => owner, 'task:fixture'); return {}; } });
  const file = new File([new Uint8Array(8388608)], 'limit.bin');
  const request = { action: 'uploadAttachment', taskId: 'task', expectedRevision: 1, attachmentId: 'attachment', filename: file.name, mediaType: 'application/octet-stream', file };
  await action.start('Upload', request);
  assert.equal(calls.length, 1); assert.equal(action.saved.value.phase, 'unknown'); assert.equal(ids, 1);
  const key = 'ivy:task-board:action:https://fixture.invalid/owner:task:fixture';
  const stored = sessionStorage.getItem(key);
  assert(stored.length < 4096); assert(!stored.includes('bytesBase64')); assert.equal(action.saved.value.call.arguments.bytesBase64, undefined);
  assert.equal(Buffer.from(calls[0].arguments.bytesBase64, 'base64').length, 8388608);
  failWrite = false;
  await action.reconcile();
  assert.equal(calls.length, 2); assert.deepEqual(calls[1], calls[0]); assert.equal(ids, 1);
  assert.equal(action.saved.value.phase, 'succeeded'); assert.equal(files.size, 0);

  failWrite = true;
  await action.start('Upload again', { ...request, file: new File(['original'], 'small.txt') });
  const original = JSON.parse(JSON.stringify(action.saved.value)); files.clear();
  await action.reconcile();
  assert.equal(action.needsOriginalFile.value, true); assert.equal(calls.length, 3);
  await action.restoreFile(new File(['different'], 'wrong.txt'));
  assert.match(action.error.value, /same file/); assert.equal(calls.length, 3); assert.equal(files.size, 0);
  await action.restoreFile(new File(['original'], 'renamed.txt'));
  assert.equal(action.needsOriginalFile.value, false); assert.equal(calls.length, 3, 'Restoring bytes never dispatches a new action.');
  files.clear();
  receipt = { callerPrincipalId: owner.callerPrincipalId, operationId: original.call.operationId, submittedHash: original.requestHash, phase: 'succeeded', outcome: { operationId: original.call.operationId } };
  await action.reconcile();
  assert.equal(action.saved.value.phase, 'succeeded'); assert.equal(calls.length, 3, 'The original receipt settles a lost file without sending it again.');
  assert.equal(ids, 2);
  const unavailable = t.mock.method(sessionStorage, 'setItem', () => { throw new Error('Fixture storage unavailable'); });
  await action.start('Retained before storage failure', { ...request, file: new File(['original'], 'small.txt') });
  assert.equal(calls.length, 3, 'Failure before dispatch sends no mutation.'); assert.equal(files.size, 1);
  const retainedId = action.saved.value.call.operationId;
  unavailable.mock.restore(); failWrite = false;
  await action.replay();
  assert.equal(calls.length, 4); assert.equal(calls[3].operationId, retainedId); assert.equal(ids, 3);
  assert.equal(action.saved.value.phase, 'succeeded'); assert.equal(action.canDismiss.value, true, 'Restored storage clears the latch after durable persistence.');
});

test('Deferred Markdown images require a trusted attachment and an explicit preview owner', () => {
  const { renderMarkdown } = load('packages/ui/src/lib/markdown.ts', {
    'markdown-it': { default: require('markdown-it') }, './attachments.js': { fileSize: bytes => bytes + ' B' },
  });
  const text = '![Saved](attachment:saved) ![Foreign](attachment:foreign) ![External](https://example.invalid/image.png)';
  const attachments = { 'attachment:saved': { name: 'saved.png', byteLength: 1 } };
  assert(!renderMarkdown(text, 262144, {}, attachments).html.includes('data-image-source'));
  const html = renderMarkdown(text, 262144, {}, attachments, true).html;
  assert.equal((html.match(/data-image-source=/g) ?? []).length, 1);
  assert(html.includes('data-image-source="attachment:saved"')); assert(!html.includes('<img'));
  assert(!html.includes('src="https:')); assert(!html.includes('data-image-source="attachment:foreign"'));
});

function attachmentPreviewComponent(read, save = () => {}) {
  const { attachmentView } = load('ui/task-board-ui/src/attachment-view.ts');
  return load('ui/task-board-ui/src/TaskAttachment.vue', {
    '@ivy/ui': {}, '@lucide/vue': {}, './attachment-view': { attachmentView },
    './attachment-images': { attachmentBytes: read, attachmentSource: id => 'attachment:' + id, saveBytes: save },
  }).default;
}
const previewProps = (filename, mediaType) => ({ taskId: 'task', attachment: {
  attachmentId: 'saved', filename, mediaType, object: { objectId: 'saved', revision: 1 },
}, loadImage: async () => { throw Error('A non-image must not enter the shared image cache.'); }, imageUrl: null, previewImage() {} });

test('Task attachment previews load video on demand and release the 8 MiB blob on close', async t => {
  const mount = setup(t), reads = [], created = [], revoked = [];
  t.mock.method(URL, 'createObjectURL', blob => { created.push(blob.size); return 'blob:preview-' + created.length; });
  t.mock.method(URL, 'revokeObjectURL', url => revoked.push(url));
  const component = attachmentPreviewComponent(async (_attachment, task, signal) => {
    reads.push({ task, signal }); return new Uint8Array(8 * 1024 * 1024);
  });
  const api = mount(component, previewProps('clip.mp4', 'video/mp4')); await settle();
  assert.equal(reads.length, 0, 'Listing videos retains no bytes and sends no read.');
  api.openPreview(); await settle();
  assert.equal(reads.length, 1); assert.deepEqual(created, [8388608]); assert.equal(api.previewUrl, 'blob:preview-1');
  api.dialogOpen = false; await settle();
  assert.deepEqual(revoked, ['blob:preview-1']); assert.equal(api.previewUrl, null); assert.equal(api.previewText, null);
  api.openPreview(); await settle();
  assert.equal(reads.length, 2, 'Reopening reads afresh instead of retaining the decoded body.');
  api.dialogOpen = false; await settle(); assert.deepEqual(revoked, ['blob:preview-1', 'blob:preview-2']);
});

test('Task attachment previews discard cancelled text and keep a later read and download independent', async t => {
  const mount = setup(t), reads = [], saved = [];
  const component = attachmentPreviewComponent((_attachment, _task, signal) => new Promise((resolve, reject) => reads.push({ signal, resolve, reject })),
    (bytes, name) => saved.push({ bytes, name }));
  const api = mount(component, previewProps('notes.txt', 'text/plain'));
  api.openPreview(); await settle(); assert.equal(reads.length, 1);
  api.dialogOpen = false; assert.equal(reads[0].signal.aborted, true);
  api.openPreview(); await settle(); assert.equal(reads.length, 2);
  reads[1].resolve(new TextEncoder().encode('Current text')); await settle();
  assert.equal(api.previewText, 'Current text');
  reads[0].reject(Error('Stale failure')); await settle();
  assert.equal(api.error, null); assert.equal(api.previewText, 'Current text'); assert.equal(api.busy, false);
  api.dialogOpen = false; await settle(); assert.equal(api.previewText, null);
  const download = api.download(); await settle(); assert.equal(reads.length, 3);
  await api.download(); assert.equal(reads.length, 3, 'The same card has only one active download.');
  const bytes = new Uint8Array([1, 2, 3]); reads[2].resolve(bytes); await download;
  assert.equal(saved.length, 1); assert.equal(saved[0].bytes, bytes); assert.equal(saved[0].name, 'notes.txt');
  assert.equal(api.previewText, null); assert.equal(api.previewUrl, null);
});

test('Task attachment previews abort on unmount and discard even a late successful body', async t => {
  setup(t); let complete, signal; const created = [];
  t.mock.method(URL, 'createObjectURL', blob => { created.push(blob.size); return 'blob:late'; });
  const component = attachmentPreviewComponent((_attachment, _task, work) => { signal = work; return new Promise(resolve => { complete = resolve; }); });
  const app = renderer.createApp({ ...component, render: () => null }, previewProps('report.pdf', 'application/pdf'));
  app.mount({}); const api = app._instance.setupState;
  let active = true;
  try {
    api.openPreview(); await settle(); app.unmount(); active = false;
    assert.equal(signal.aborted, true); complete(new Uint8Array([1])); await settle();
    assert.deepEqual(created, []); assert.equal(api.previewUrl, null); assert.equal(api.previewText, null); assert.equal(api.busy, false);
  } finally { if (active) app.unmount(); }
});

test('Task images share one active read and discard queued or late bytes after leaving the task', async t => {
  const mount = setup(t), id = vue.ref('task'), metadata = [], calls = [];
  const content = load('packages/ui-client/src/content.ts', { '../../sdk/src/client.js': { canonical } });
  const { BlobUrls } = load('packages/ui-client/src/blob-urls.ts');
  const bytes = new Uint8Array([42]), hash = await content.hashBytes(bytes);
  for (const name of ['one', 'two']) metadata.push({ attachmentId: name, filename: name + '.png', mediaType: 'image/png', byteLength: 1, contentHash: hash, object: { objectId: name, revision: 1 } });
  let release, running = 0, peak = 0;
  const { useAttachmentImages } = load('ui/task-board-ui/src/attachment-images.ts', {
    '@ivy/ui': { markdownImageSources: () => [] }, '../../../packages/ui-client/src/content': content,
    '../../../packages/ui-client/src/blob-urls': { BlobUrls }, './attachment-view': { attachmentView: () => 'image' },
    './runtime': { client: { request: async (_method, pin, options) => {
      calls.push({ pin, signal: options.signal }); running++; peak = Math.max(peak, running);
      if (pin.objectId === 'one') await new Promise(resolve => { release = resolve; });
      running--;
      return { content: { encoding: 'base64', value: 'Kg==' }, object: { contractKey: 'task-board/attachment', parentId: 'task' }, revision: { revision: 1, mediaType: 'application/octet-stream', byteLength: 1, contentHash: hash } };
    } } },
  });
  let images;
  mount({ setup() { images = useAttachmentImages(() => id.value, () => metadata, () => []); return {}; } });
  const first = images.load('attachment:one'), duplicate = images.load('attachment:one'), second = images.load('attachment:two');
  await settle(); assert.equal(calls.length, 1);
  assert.equal(await images.load('attachment:foreign'), null);
  id.value = 'another-task'; await settle();
  assert.equal(calls[0].signal.aborted, true);
  release(); assert.deepEqual(await Promise.all([first, duplicate, second]), [null, null, null]);
  assert.equal(calls.length, 1, 'Queued reads do not start after their task is left.'); assert.equal(peak, 1);
  assert.deepEqual(images.urls.value, {});
});

test('Attachment staging encodes bounded slices and waits for the original upload before preparing another', async t => {
  const backend = { backendId: 'primary', provider: 'codex', accountId: 'work' }, calls = [];
  let release;
  const api = attachmentsFixture(t, async (name, args) => {
    calls.push({ name, args });
    if (calls.length === 1) await new Promise(resolve => { release = resolve; });
    return { backend: args.backend, path: '/staged/' + args.name };
  }, () => backend);
  const bytes = Uint8Array.from({ length: 100001 }, (_, n) => n % 256);
  const first = new File([bytes], 'first.bin'), second = new File(['second bytes'], 'second.txt');
  const slice = first.slice.bind(first);
  let largest = 0, secondReads = 0;
  first.arrayBuffer = async () => { throw Error('The complete file must not be materialized.'); };
  first.slice = (start, end) => { const part = slice(start, end); largest = Math.max(largest, part.size); return part; };
  const nextSlice = second.slice.bind(second);
  second.slice = (...args) => { secondReads++; return nextSlice(...args); };
  api.add([first, second]); await settleFiles();
  assert.equal(calls.length, 1); assert.equal(secondReads, 0, 'No encoded backlog while the first RPC is unresolved.');
  assert.equal(largest, 24576);
  assert.equal(calls[0].args.dataBase64, Buffer.from(bytes).toString('base64'));
  assert.deepEqual(calls[0].args.backend, backend);
  release(); await settleFiles();
  assert.equal(calls.length, 2); assert(secondReads > 0);
  assert.equal(calls[1].args.dataBase64, Buffer.from('second bytes').toString('base64'));
  assert.deepEqual(api.staged.value.map(value => value.path), ['/staged/first.bin', '/staged/second.txt']);
});

test('Attachment staging rejects large or excessive admission and clearing drops queued file work', async t => {
  const backend = { backendId: 'primary', provider: 'codex', accountId: 'work' }, calls = [];
  let release;
  const api = attachmentsFixture(t, async (_name, args) => { calls.push(args); await new Promise(resolve => { release = resolve; }); return { backend, path: '/staged/file' }; }, () => backend);
  const oversized = new File(['x'], 'oversized.png', { type: 'image/png' });
  Object.defineProperty(oversized, 'size', { value: 8 * 1024 * 1024 + 1 });
  oversized.slice = () => { throw Error('Oversized input must fail before file reads.'); };
  api.add([oversized]); await settleFiles();
  assert.equal(calls.length, 0); assert.equal(api.items.value[0].previewUrl, null); assert.equal(api.items.value[0].state, 'failed');
  api.clear();
  api.add(Array.from({ length: 9 }, (_, n) => new File(['small'], 'file-' + n)));
  await settleFiles();
  assert.equal(api.items.value.length, 8); assert.match(api.error.value, /At most 8/); assert.equal(calls.length, 1);
  api.clear(); release(); await settleFiles();
  assert.equal(calls.length, 1); assert.equal(api.items.value.length, 0); assert.equal(api.error.value, null);
  let releasePreparation;
  const large = Array.from({ length: 5 }, (_, n) => {
    const file = new File(['small allocation'], 'large-' + n);
    Object.defineProperty(file, 'size', { value: 8 * 1024 * 1024 });
    file.slice = () => ({ arrayBuffer: () => new Promise(resolve => { releasePreparation = () => resolve(new ArrayBuffer(1)); }) });
    return file;
  });
  api.add(large); await settle();
  assert.equal(api.items.value.length, 4); assert.match(api.error.value, /At most 32 MiB/);
  api.clear(); releasePreparation(); await settleFiles();
  assert.equal(calls.length, 1, 'Clearing the bounded raw-file queue cancels preparation before another RPC.');
});

test('Attachment staging keeps the account captured before preparation and drops removed queued files', async t => {
  const original = { backendId: 'primary', provider: 'codex', accountId: 'work' }, backend = vue.ref(original), calls = [];
  const api = attachmentsFixture(t, async (_name, args) => { calls.push(args); return { backend: args.backend, path: '/staged/file' }; }, () => backend.value);
  let release;
  const first = new File(['held'], 'held.txt'), queued = new File(['queued'], 'queued.txt');
  first.slice = () => ({ arrayBuffer: () => new Promise(resolve => { release = () => resolve(new TextEncoder().encode('held').buffer); }) });
  queued.slice = () => { throw Error('A removed queue entry must not be read.'); };
  api.add([first, queued]); await settle();
  api.remove(api.items.value[1].id);
  backend.value = { ...original, accountId: 'personal' };
  release(); await settleFiles();
  assert.equal(calls.length, 0); assert.equal(api.items.value.length, 1); assert.match(api.items.value[0].detail, /account changed/);
});

test('Attachment staging does not adopt a newly selected account for queued preparation', async t => {
  const original = { backendId: 'primary', provider: 'codex', accountId: 'work' }, backend = vue.ref(original), calls = [];
  const api = attachmentsFixture(t, async (_name, args) => { calls.push(args); return { backend: args.backend, path: '/staged/file' }; }, () => backend.value);
  let release, queuedReads = 0;
  const first = new File(['held'], 'held.txt'), queued = new File(['queued'], 'queued.txt');
  first.slice = () => ({ arrayBuffer: () => new Promise(resolve => { release = () => resolve(new TextEncoder().encode('held').buffer); }) });
  queued.slice = () => { queuedReads++; throw Error('A stale account queue entry must not be read.'); };
  api.add([first, queued]); await settle();
  backend.value = { ...original, accountId: 'personal' };
  release(); await settleFiles();
  assert.equal(calls.length, 0); assert.equal(queuedReads, 0);
  assert(api.items.value.every(item => item.state === 'failed' && /account changed/.test(item.detail)));
});

test('live loaders take their initial snapshot after acknowledgement and retain bounded HTTP fallback', async t => {
  const mount = setup(t), notifications = new Notifications();
  notifications.connected = false;
  let reads = 0;
  mount({ setup() { remote.useRemote(async () => ++reads, 15000, ['objects/wiki/page']); return {}; } }, {}, notifications);
  await settle(); assert.equal(reads, 0);
  notifications.ready(true); await settle(); assert.equal(reads, 1);
  await advance(t, 250); assert.equal(reads, 1, 'connection setup does not trigger a second initial snapshot');
  const unavailable = new Notifications(); unavailable.connected = false;
  let fallback = 0;
  mount({ setup() { remote.useRemote(async () => ++fallback, 15000, ['objects/wiki/page']); return {}; } }, {}, unavailable);
  await settle(); assert.equal(fallback, 0);
  await advance(t, 250); assert.equal(fallback, 1);
  unavailable.ready(true); await advance(t, 250); assert.equal(fallback, 2, 'late connection setup reconciles the uncovered interval');
});

test('explicit invalidations and live hints share one read and retain one in-flight follow-up', async t => {
  const mount = setup(t), notifications = new Notifications(), signals = [];
  let api, release;
  mount({ setup() {
    api = remote.useRemote(async signal => {
      signals.push(signal);
      if (signals.length === 1) await new Promise(resolve => { release = resolve; });
      return signals.length;
    }, 15000, ['objects/task-board/task']);
    return {};
  } }, {}, notifications);
  await settle();
  for (let n = 0; n < 20; n++) { api.invalidate(); notifications.change('objects/task-board/task'); }
  await advance(t, 15000);
  assert.equal(signals.length, 1); assert.equal(signals[0].aborted, false);
  release(); await settle(); await advance(t, 250);
  assert.equal(signals.length, 2); assert.equal(api.value.value, 2);
  for (let n = 0; n < 20; n++) { api.invalidate(); notifications.change('objects/task-board/task'); }
  await advance(t, 249); assert.equal(signals.length, 2);
  await advance(t, 1); assert.equal(signals.length, 3);
  await advance(t, 15000); assert.equal(signals.length, 3, 'A healthy source needs no redundant poll.');
});

test('disabled and hidden views defer initial, explicit, event, reconnect and fallback reads', async t => {
  const mount = setup(t), notifications = new Notifications(), enabled = vue.ref(false);
  document.hidden = true;
  let reads = 0, api;
  mount({ setup() { api = remote.useRemote(async () => ++reads, 15000, ['objects/task-board/task'], () => enabled.value); return {}; } }, {}, notifications);
  await settle(); await api.refresh(); api.invalidate();
  notifications.change('objects/task-board/task'); notifications.ready(false); notifications.ready(true);
  await advance(t, 30000); assert.equal(reads, 0);
  document.hidden = false; document.dispatchEvent(new Event('visibilitychange'));
  await advance(t, 250); assert.equal(reads, 0);
  enabled.value = true; await settle();
  window.dispatchEvent(new Event('online')); notifications.change('objects/task-board/task');
  await advance(t, 250); assert.equal(reads, 1);
  enabled.value = false; await settle();
  notifications.ready(false); api.invalidate(); await api.refresh();
  await advance(t, 30000); assert.equal(reads, 1);
  enabled.value = true; await settle(); await advance(t, 250);
  assert.equal(reads, 2); assert.equal(api.error.value, null);
});

test('pausing a paged view aborts stale work, preserves its cursor and reconciles once on reopening', async t => {
  const mount = setup(t), notifications = new Notifications(), enabled = vue.ref(true), calls = [];
  let pages, release;
  mount({ setup() {
    pages = runtime.usePage(async (signal, cursor) => {
      const index = calls.push({ signal, cursor });
      if (index === 3) await new Promise(resolve => { release = resolve; });
      return { items: [index], nextCursor: cursor ? null : 'second-page' };
    }, 15000, () => 'paused-page', ['objects/task-board/task'], () => enabled.value);
    return {};
  } }, {}, notifications);
  await settle(); pages.next(); await settle();
  assert.equal(pages.page.value, 2); assert.equal(calls[1].cursor, 'second-page');
  pages.invalidate(); await advance(t, 250); assert.equal(calls.length, 3);
  enabled.value = false; await settle();
  assert.equal(calls[2].signal.aborted, true); assert.equal(pages.loading.value, false);
  notifications.change('objects/task-board/task'); pages.invalidate(); await advance(t, 30000);
  assert.equal(calls.length, 3); assert.deepEqual(pages.value.value.items, [2]);
  enabled.value = true; await settle(); await advance(t, 250);
  assert.equal(calls.length, 4); assert.equal(calls[3].cursor, 'second-page');
  release(); await settle(); assert.deepEqual(pages.value.value.items, [4], 'A transport ignoring abort cannot replace the reopened page.');
  enabled.value = false; await settle(); pages.reset(); await settle();
  assert.equal(calls.length, 4); assert.equal(pages.page.value, 1);
  enabled.value = true; await settle(); await advance(t, 250);
  assert.equal(calls.length, 5); assert.equal(calls[4].cursor, undefined);
});

test('hidden manual loaders resume their deferred initial read without acquiring background polling', async t => {
  const mount = setup(t); document.hidden = true;
  let reads = 0, api;
  mount({ setup() { api = remote.useRemote(async () => ++reads); return {}; } });
  await settle(); await advance(t, 30000); assert.equal(reads, 0);
  document.hidden = false; document.dispatchEvent(new Event('visibilitychange'));
  await advance(t, 250); assert.equal(reads, 1);
  document.dispatchEvent(new Event('visibilitychange')); window.dispatchEvent(new Event('online'));
  await advance(t, 30000); assert.equal(reads, 1);
  document.hidden = true; await api.refresh(); assert.equal(reads, 1);
  document.hidden = false; document.dispatchEvent(new Event('visibilitychange'));
  await advance(t, 250); assert.equal(reads, 2);
});

test("reactive live scopes follow their owner without refreshing for unrelated hosts", async t => {
  const mount = setup(t), notifications = new Notifications(), node = vue.ref("first");
  let reads = 0;
  mount({ setup() {
    remote.useRemote(async () => ++reads, 0, () => ["services/agent-manager/" + node.value]);
    return {};
  } }, {}, notifications);
  await settle();
  await advance(t, 250);
  const initial = reads;
  notifications.change("services/agent-manager/second");
  await advance(t, 250);
  assert.equal(reads, initial);
  node.value = "second";
  await settle(); await advance(t, 250);
  assert.equal(reads, initial + 1, "owner changes refresh and replace the subscription");
  notifications.change("services/agent-manager/first");
  await advance(t, 250);
  assert.equal(reads, initial + 1);
  notifications.change("services/agent-manager/second");
  await advance(t, 250);
  assert.equal(reads, initial + 2);
});

test("explicit live sources isolate reads and preserve polling for uncovered sources", async t => {
  const mount = setup(t), notifications = new Notifications();
  let liveReads = 0, pollingReads = 0, manualReads = 0;
  mount({ setup() {
    remote.useRemote(async () => ++liveReads, 15000, ["objects/task-board/task"]);
    remote.useRemote(async () => ++pollingReads, 15000);
    remote.useRemote(async () => ++manualReads);
    return {};
  } }, {}, notifications);
  await settle();
  notifications.change("objects/secretary/execution");
  notifications.emit("another-host", "notification", { method: "turn/completed", params: { threadId: "another-task" } });
  await advance(t, 250);
  assert.equal(liveReads, 1);
  notifications.change("objects/task-board/task");
  notifications.change("objects/task-board/task");
  await advance(t, 250);
  assert.equal(liveReads, 2, "a burst has one refresh owner");
  await advance(t, 15000); await advance(t, 250);
  assert.equal(liveReads, 2);
  assert.equal(pollingReads, 2, "a healthy unrelated socket cannot suppress polling");
  document.dispatchEvent(new Event("visibilitychange"));
  await advance(t, 250);
  assert.equal(manualReads, 1, "initial/manual loaders do not become background refreshers");
  notifications.ready(false);
  await advance(t, 15000); await advance(t, 250);
  assert.equal(liveReads, 4, "a disconnected live source resumes fallback reads");
});

for (const failedParent of [null, "parent"]) test("Hierarchy retries failed " + (failedParent === null ? "root" : "child") + " reads with a healthy socket", async t => {
  const mount = setup(t), notifications = new Notifications();
  const component = load("packages/ui/src/patterns/HierarchyTree.vue", {
    "../composables/remote": remote, "@lucide/vue": {},
  }).default;
  let reads = 0, fail = false, label = "Saved";
  const tree = mount(component, {
    label: "Pages", storageKey: "hierarchy-recovery", liveScopes: ["objects/wiki/page"],
    load: async parent => {
      reads++;
      if (fail && parent === failedParent) throw new Error("Temporarily unavailable");
      return { items: [{ id: parent === null ? "parent" : "child", label, href: "#/page" }], nextCursor: null };
    },
  }, notifications);
  await settle();
  await tree.toggle("parent");
  await settle();
  fail = true;
  label = "Updated";
  notifications.change("objects/wiki/page");
  await advance(t, 250);
  assert.equal(tree.branches.get(failedParent).error, "Temporarily unavailable");
  assert.equal(tree.branches.get(failedParent).items[0].label, "Saved", "failed reads keep the previous tree usable");
  const failedReads = reads;
  fail = false;
  await advance(t, 15000); await advance(t, 250);
  assert.ok(reads > failedReads, "retry without a second hint or reconnect");
  assert.equal(tree.branches.get(failedParent).error, null);
  assert.equal(tree.branches.get(failedParent).items[0].label, "Updated");
  const recoveredReads = reads;
  await advance(t, 30000); await advance(t, 250);
  assert.equal(reads, recoveredReads, "successful recovery stops fallback reads");

  if (failedParent !== null) {
    fail = true;
    notifications.change("objects/wiki/page");
    await advance(t, 250);
    assert.equal(tree.branches.get(failedParent).error, "Temporarily unavailable");
    await tree.toggle("parent");
    await advance(t, 15000); await advance(t, 250);
    const collapsedReads = reads;
    await advance(t, 30000); await advance(t, 250);
    assert.equal(reads, collapsedReads, "a hidden failed branch does not keep the whole tree polling");
  }
});

test("Hierarchy refreshes previously loaded descendants when a collapsed branch reopens", async t => {
  const mount = setup(t), notifications = new Notifications();
  const component = load("packages/ui/src/patterns/HierarchyTree.vue", {
    "../composables/remote": remote, "@lucide/vue": {},
  }).default;
  let label = "Saved", descendantReads = 0;
  const tree = mount(component, {
    label: "Pages", storageKey: "hierarchy-collapsed", liveScopes: ["objects/wiki/page"],
    load: async parent => {
      if (parent !== null) descendantReads++;
      const id = parent === null ? "parent" : parent === "parent" ? "child" : "grandchild";
      return { items: [{ id, label: parent === "child" ? label : id, href: "#/page" }], nextCursor: null };
    },
  }, notifications);
  await settle();
  await tree.toggle("parent");
  await tree.toggle("child");
  await tree.toggle("parent");
  const before = descendantReads;
  label = "Updated";
  notifications.change("objects/wiki/page");
  await advance(t, 250);
  assert.equal(descendantReads, before, "hidden branches stay lazy");
  await tree.toggle("parent");
  await settle();
  assert.equal(tree.branches.get("child").items[0].label, "Updated");
});

test("Secretary keeps dirty configuration and its save revision across push and fallback", async t => {
  const mount = setup(t), notifications = new Notifications();
  let revision = 1, reads = 0, finishSave, submitted, finishRead, holdRead = false;
  let saved = { rules: { main: { enabled: false }, voice: { enabled: false } }, execution: null };
  const component = load("ui/secretary-ui/src/RulesView.vue", {
    "@ivy/ui": remote,
    "./runtime": {
      base: new URL('https://fixture.invalid/'),
      client: { request: async () => ({ items: [] }) }, tr: (_de, en) => en,
      readDocument: async () => {
        reads++;
        const snapshot = { value: structuredClone(saved), pin: { objectId: "config", revision } };
        if (holdRead) { holdRead = false; await new Promise(resolve => { finishRead = resolve; }); }
        return snapshot;
      },
      saveConfiguration: async (_node, _pin, value) => {
        submitted = value;
        return new Promise(resolve => { finishSave = () => {
          saved = structuredClone(value); revision++;
          resolve({ objectId: "config", revision });
        }; });
      },
    },
  }).default;
  const state = mount(component, { node: "secretary", hostId: "host", root: "root", configuration: { objectId: "config", revision } }, notifications);
  await settle();
  state.draft.rules.main.enabled = true;
  notifications.change("objects/secretary/execution");
  await advance(t, 250);
  assert.equal(reads, 1, "execution updates do not reload configuration");
  revision = 2;
  notifications.change("objects/secretary/configuration");
  await advance(t, 250);
  assert.equal(reads, 2);
  assert.equal(state.draft.rules.main.enabled, true);
  assert.equal(state.pin.revision, 1, "dirty edits retain their original conflict base");
  notifications.ready(false);
  await advance(t, 15000); await advance(t, 250);
  assert.equal(reads, 3);
  assert.equal(state.draft.rules.main.enabled, true);
  state.draft.rules.main.enabled = false;
  saved.rules.main.enabled = true;
  notifications.ready(true);
  await advance(t, 250);
  assert.equal(state.draft.rules.main.enabled, true, "an unchanged draft adopts the latest saved snapshot");
  assert.equal(state.pin.revision, 2);
  assert.equal(submitted, undefined, "push and fallback never save a draft");
  holdRead = true;
  const oldRead = state.current.refresh();
  await settle();
  state.draft.rules.main.enabled = false;
  const saving = state.save();
  await settle();
  state.draft.rules.voice.enabled = true;
  finishSave(); await saving;
  assert.equal(saved.rules.voice.enabled, false, "save uses the snapshot submitted by the user");
  assert.equal(state.draft.rules.voice.enabled, true, "edits made during a save remain in the editor");
  assert.equal(state.pin.revision, 3);
  state.draft.rules.voice.enabled = false;
  finishRead(); await oldRead;
  assert.equal(state.pin.revision, 3, "a read started before saving cannot roll back the saved revision");
  assert.equal(state.draft.rules.main.enabled, false);
});

test("Native inputs receive only their owner and task hints and recover after reconnect", async t => {
  const mount = setup(t), notifications = new Notifications();
  let reads = 0;
  mount({ setup() {
    remote.useRemote(async () => ++reads, 15000, nativeInputUpdates(notifications, "owner", "task"));
    return {};
  } });
  await settle();
  notifications.emit("foreign-owner", "inputs", { threadId: "task" });
  notifications.emit("owner", "inputs", { threadId: "foreign-task" });
  notifications.emit("owner", "notification", { method: "turn/completed", params: { threadId: "task" } });
  await advance(t, 250);
  assert.equal(reads, 1);
  notifications.emit("owner", "inputs", { threadId: "task" });
  await advance(t, 250);
  assert.equal(reads, 2);
  notifications.emit("owner", "notification", { method: "serverRequest/resolved", params: { threadId: "task" } });
  await advance(t, 250);
  assert.equal(reads, 3);
  await advance(t, 15000); await advance(t, 250);
  assert.equal(reads, 3, "confirmed matching subscriptions replace input polling");
  notifications.ready(false);
  await advance(t, 15000); await advance(t, 250);
  assert.equal(reads, 4);
  notifications.ready(true);
  await advance(t, 250);
  assert.equal(reads, 5);
});

test('Native input live hints keep equal thread IDs and capabilities within the current account', async t => {
  const mount = setup(t), notifications = new Notifications();
  const account = { backendId: 'secondary', provider: 'claude', accountId: 'personal' };
  let selected = account, reads = 0;
  mount({ setup() { remote.useRemote(async () => ++reads, 15000, nativeInputUpdates(notifications, 'owner', 'same-thread', () => selected)); return {}; } });
  await settle();
  for (const backend of [undefined, { ...account, accountId: 'other' }, { ...account, provider: 'codex' }]) {
    notifications.emit('owner', 'inputs', { backend, threadId: 'same-thread' });
    notifications.emit('owner', 'capabilities', { backend });
  }
  await advance(t, 250); assert.equal(reads, 1);
  notifications.emit('owner', 'inputs', { backend: account, threadId: 'same-thread' });
  await advance(t, 250); assert.equal(reads, 2);
  selected = { ...account, accountId: 'other' };
  notifications.emit('owner', 'inputs', { backend: account, threadId: 'same-thread' });
  await advance(t, 250); assert.equal(reads, 2);
  notifications.emit('owner', 'notification', { backend: selected, method: 'serverRequest/resolved', params: { threadId: 'same-thread' } });
  await advance(t, 250); assert.equal(reads, 3);
});

function agentTask(t, connected = true, failures = new Set(), backend, mountView, holds = new Map()) {
  const mount = mountView ?? setup(t), notifications = new Notifications();
  notifications.connected = connected;
  const reads = { state: 0, goal: 0, turns: 0, output: 0, journal: 0 }, signals = [], outputSignals = [], releases = [];
  const journal = [];
  const read = async (kind, value, signal) => {
    reads[kind]++;
    if (kind === "state") signals.push(signal);
    if (kind === "output") outputSignals.push(signal);
    if (failures.has(kind)) throw new Error(kind + " unavailable");
    await holds.get(kind);
    return value;
  };
  const readProvider = async (_client, _node, method, args, signal) => {
    if (method === nativeBackends.nativeMethod(backend, 'thread/goal/get')) return read("goal", { goal: null }, signal);
    if (method === nativeBackends.nativeMethod(backend, 'thread/turns/list')) return read("turns", { data: [], nextCursor: null }, signal);
    assert.equal(method, "agent.notifications");
    return read("journal", { items: journal.filter(item => item.sequence > args.afterSequence),
      epoch: "epoch", throughSequence: journal.length, hasMore: false, gap: false }, signal);
  };
  const component = load("ui/agent-ui/src/AgentTask.vue", {
    "../../../packages/sdk/src/client.js": { canonical },
    "@lucide/vue": {}, "@ivy/ui": ui,
    "../../../packages/ui-client/src/runtime": runtime,
    "../../../packages/ui-client/src/native": {
      ...native,
      nativeModels: async () => ({ data: [] }),
      optionalTool: async (_client, _node, method) => method === nativeBackends.nativeMethod(backend, 'thread/goal/get') ? {} : undefined,
      nativeRead: readProvider,
    },
    "../../../packages/ui-client/src/native-backends": { ...nativeBackends,
      backendRead: (_client, scope, method, args, signal) => readProvider(_client, scope.serviceNodeId,
        method.startsWith('agent.') ? method : nativeBackends.nativeMethod(scope.backend, method), args, signal) },
    "../../../packages/ui-client/src/native-thread-control": {
      readNativeThreadControl: (_client, _node, _thread, signal) =>
        read("state", { attached: true, status: { epoch: "epoch", backend }, thread: { name: "Task", status: { type: "idle" } } }, signal),
    },
    "../../../packages/ui-client/src/native-output-page": {
      isUnmaterializedNativeHistory: () => false,
      releaseNativeOutput: (...args) => releases.push(args),
      readNativeOutputPage: (_client, _node, _thread, _turn, _cursor, signal) =>
        read("output", { mode: "items", items: [], nextCursor: null }, signal),
    },
    "../../../packages/ui-client/src/native-action": {
      nativeActionMethod, nativeActionParams,
      useNativeAction: () => ({ saved: vue.ref(null), locked: vue.ref(false), error: vue.ref(null) }),
    },
    "../../../packages/ui-client/src/native-modes": {},
    "../../../packages/ui-client/src/native-settings": {
      useNativeSettings: () => ({
        models: remote.useRemote(async () => ({ data: [] })),
        permissions: remote.useRemote(async () => null),
        modes: remote.useRemote(async () => null),
        config: remote.useRemote(async () => null),
        defaults: remote.useRemote(async () => null),
        choices: vue.computed(() => []),
        refresh() {},
      }),
      effectiveNativeSettings: () => ({}),
    },
    "../../../packages/ui-client/src/message-attachments": load("packages/ui-client/src/message-attachments.ts", { "../../sdk/src/client.js": {} }),
    "../../../packages/ui-client/src/NativeActionState.vue": {},
    "../../../packages/ui-client/src/NativeInputs.vue": {},
    "../../../packages/ui-client/src/TaskProjectDialog.vue": {},
    "../../../packages/ui-client/src/MessageImages.vue": {},
    "../../../packages/ui-client/src/native-images": {},
    "./TaskActions.vue": {},
    "./conversation": load("ui/agent-ui/src/conversation.ts", {
      "../../../packages/ui-client/src/native": native,
      "../../../packages/ui-client/src/message-images": load("packages/ui-client/src/message-images.ts", { "./native": native }),
    }),
    "./runtime": { base: location, client: { request: async () => ({ summary: {} }) }, notifications,
      tr: (_de, en) => en,
      outputCache: new (load("packages/ui-client/src/native-output-cache.ts").NativeOutputCache)(),
    },
  }).default;
  const state = mount(component, { node: "owner", backend, threadId: "task", turnId: "" }, notifications);
  return { state, notifications, reads, failures, holds, signals, outputSignals, releases,
    emit(method, params = {}) {
      const entry = { backend, method, params: { threadId: "task", ...params }, epoch: "epoch", sequence: journal.length + 1, observedAt: new Date().toISOString() };
      journal.push(entry);
      if (notifications.connected) notifications.emit("owner", "notification", entry);
    },
  };
}

test('Agent output subscribes before its first read and reconciles displayed output after reconnect', async t => {
  const f = agentTask(t);
  await settle(); await advance(t, 600);
  assert.equal(f.reads.output, 1, 'an already acknowledged subscription needs one initial snapshot');
  assert.equal(f.outputSignals[0].aborted, false);
  f.notifications.ready(false); f.notifications.ready(true);
  await advance(t, 600);
  assert.equal(f.reads.output, 2, 'displayed output still reconciles the uncovered interval');
});

test('Agent output replaces an unacknowledged initial read without losing completion or accepting its late reply', async t => {
  let finishInitial, finishAcknowledged;
  const initial = new Promise(resolve => { finishInitial = resolve; }), acknowledged = new Promise(resolve => { finishAcknowledged = resolve; });
  const f = agentTask(t, false, new Set(), undefined, undefined, new Map([['output', initial]]));
  await settle(); assert.equal(f.reads.output, 1);
  const initialSignal = f.outputSignals[0];
  f.holds.set('output', acknowledged); f.notifications.ready(true); await settle();
  assert.equal(initialSignal.aborted, true);
  assert.equal(f.reads.output, 2);
  assert.equal(f.outputSignals[1].aborted, false);
  f.emit('turn/completed'); await advance(t, 600);
  assert.equal(f.reads.output, 2, 'completion waits for the acknowledged read without discarding its reload');
  f.holds.delete('output'); finishAcknowledged(); await settle();
  assert.equal(f.reads.output, 3, 'completion reconciles once after the in-flight read');
  const current = f.state.outputPages;
  finishInitial(); await settle();
  assert.equal(f.state.outputPages, current, 'the cancelled initial reply cannot replace the newer result');
  assert.equal(f.state.outputLoading, false);
  assert.equal(f.state.outputError, null);
});

test('Agent output view releases its previous linked turn and clears large state and late reads on unmount', async t => {
  setup(t);
  let app, props, mounted = false;
  const mount = (component, initial, notifications) => {
    props = vue.reactive(initial);
    const view = { ...component, render: () => null };
    app = renderer.createApp({ render: () => vue.h(view, props) });
    app.use(liveUpdates(notifications)); app.mount({}); mounted = true;
    return app._instance.subTree.component.setupState;
  };
  try {
    const f = agentTask(t, true, new Set(), undefined, mount); await settle();
    props.turnId = 'linked-turn'; await settle();
    assert(f.releases.some(args => args[1] === 'owner' && args[2] === 'task' && args[3] === ''));
    let finish; f.holds.set('output', new Promise(resolve => { finish = resolve; }));
    const pending = f.state.loadOutput(true); await settle();
    const signal = f.outputSignals.at(-1); assert.equal(signal.aborted, false);
    f.state.outputPages = [{ mode: 'full', items: [], nextCursor: null, detail: '', download: { name: 'saved.json', raw: 'retained bytes' } }];
    app.unmount(); mounted = false;
    assert.equal(signal.aborted, true); assert.deepEqual(f.releases.at(-1).slice(1), ['owner', 'task', 'linked-turn', undefined]);
    assert.deepEqual(f.state.outputPages, []);
    finish(); await pending; await settle(); assert.deepEqual(f.state.outputPages, []);
  } finally { if (mounted) app.unmount(); }
});

test('Agent account hints cannot update an equal thread ID or trigger another account snapshot', async t => {
  const backend = { backendId: 'secondary', provider: 'claude', accountId: 'personal' }, f = agentTask(t, true, new Set(), backend);
  await settle(); await advance(t, 600);
  const before = { ...f.reads };
  for (const foreign of [undefined, { ...backend, provider: 'codex' }, { ...backend, accountId: 'work' }]) {
    f.notifications.emit('owner', 'notification', { backend: foreign, method: 'thread/name/updated', params: { threadId: 'task', threadName: 'Wrong account' },
      epoch: 'epoch', sequence: 1, observedAt: new Date().toISOString() });
  }
  await advance(t, 600); assert.deepEqual(f.reads, before); assert.equal(f.state.state.value.value.thread.name, 'Task');
  f.emit('thread/name/updated', { threadName: 'Personal task' });
  await advance(t, 600); assert.equal(f.state.state.value.value.thread.name, 'Personal task'); assert.deepEqual(f.reads, before);
});

test("Agent snapshots retry failed reads with a healthy socket and stop after recovery", async t => {
  const f = agentTask(t, true, new Set(["state", "goal", "turns", "output"]));
  await settle(); await advance(t, 600);
  const before = { ...f.reads };
  assert.equal(f.state.state.error.value, "state unavailable");
  assert.equal(f.state.goal.error.value, "goal unavailable");
  f.failures.clear();
  await advance(t, 14399);
  assert.deepEqual(f.reads, before, "failed snapshots do not cause a tight retry loop");
  await advance(t, 1); await advance(t, 600);
  for (const kind of ["state", "goal", "turns", "output"]) assert.equal(f.reads[kind], before[kind] + 1);
  assert.equal(f.state.state.error.value, null);
  assert.equal(f.state.goal.error.value, null);
  assert.equal(f.state.turns.error.value, null);
  assert.equal(f.state.outputError, null);
  assert.equal(f.state.attached, true);
  const recovered = { ...f.reads };
  await advance(t, 30000); await advance(t, 600);
  assert.deepEqual(f.reads, recovered, "healthy idle tasks have no periodic snapshot or journal reads");
});

test("Agent journal failures back off and stop retrying after push recovers", async t => {
  const f = agentTask(t, true, new Set(["journal"]));
  await settle();
  assert.equal(f.reads.journal, 1, "startup hints do not bypass a failed journal read's delay");
  await advance(t, 1999);
  assert.equal(f.reads.journal, 1);
  await advance(t, 1);
  assert.equal(f.reads.journal, 2);
  await advance(t, 3999);
  assert.equal(f.reads.journal, 2);
  await advance(t, 1);
  assert.equal(f.reads.journal, 3);
  f.failures.clear();
  await advance(t, 8000);
  const recovered = f.reads.journal;
  assert.equal(recovered, 5, "recovery reads the journal head and current page");
  await advance(t, 30000);
  assert.equal(f.reads.journal, recovered, "healthy push resumes without continuous retry polling");
});

test("Agent fallback keeps journal polling fast and snapshots slow without aborting in-flight reads", async t => {
  const f = agentTask(t, false);
  await settle();
  for (let i = 0; i < 10; i++) await advance(t, 1000);
  assert.ok(f.reads.journal >= 10);
  for (const kind of ["state", "goal", "turns", "output"]) assert.equal(f.reads[kind], 1, kind);
  for (let i = 0; i < 5; i++) await advance(t, 1000);
  await advance(t, 600);
  for (const kind of ["state", "goal", "turns", "output"]) assert.equal(f.reads[kind], 2, kind);
  let finish;
  f.holds.set("state", new Promise(resolve => { finish = resolve; }));
  f.emit("turn/completed");
  await advance(t, 1000); await advance(t, 600);
  assert.equal(f.reads.state, 3, "lifecycle events refresh immediately during fallback");
  for (let i = 0; i < 30; i++) await advance(t, 1000);
  assert.equal(f.reads.state, 3, "slow reads are not restarted by the fallback timer");
  assert.equal(f.signals.at(-1).aborted, false);
  f.holds.clear(); finish(); await settle();
  document.hidden = true;
  const hiddenReads = { ...f.reads };
  for (let i = 0; i < 20; i++) await advance(t, 1000);
  assert.deepEqual(f.reads, hiddenReads, "hidden tasks suspend background reads");
  document.hidden = false;
  const outputBeforeReconnect = f.reads.output;
  f.notifications.ready(true);
  await advance(t, 600);
  assert.equal(f.reads.state, 4, "reconnection refreshes immediately");
  assert.equal(f.reads.output, outputBeforeReconnect + 1, 'late acknowledgement still reconciles completed fallback output');
});

test("Agent events refresh only their affected panels and streaming deltas do not refresh snapshots", async t => {
  const f = agentTask(t);
  await settle(); await advance(t, 600);
  const before = { ...f.reads };
  for (const method of ["thread/realtime/outputAudio/delta", "thread/realtime/transcript/delta",
    "thread/realtime/item/transcript/delta", "thread/tokenUsage/updated", "item/agentMessage/delta"])
    f.emit(method);
  await advance(t, 1000);
  assert.deepEqual(f.reads, before);
  for (const [method, parts] of [['thread/name/updated', ['state']], ['thread/goal/updated', ['goal']],
    ['thread/goal/cleared', []], ['turn/completed', ['state', 'goal', 'turns', 'output']]]) {
    const count = { ...f.reads };
    f.emit(method); f.emit(method);
    await advance(t, 600);
    for (const kind of ['state', 'goal', 'turns', 'output']) assert.equal(f.reads[kind], count[kind] + Number(parts.includes(kind)), method + ' ' + kind);
  }
});

test('complete current-owner push data updates panels and running output without a history reread; completion reconciles and historical edits refresh', async t => {
  const f = agentTask(t);
  await settle(); await advance(t, 600);
  f.state.turns.value.value = { items: [{ id: 'active-turn', status: 'inProgress' }], nextCursor: null };
  const before = { ...f.reads };
  f.emit('thread/name/updated', { threadName: 'Pushed name' });
  f.emit('thread/status/changed', { status: { type: 'active', activeFlags: [] } });
  f.emit('thread/goal/updated', { goal: { threadId: 'task', objective: 'Pushed goal', status: 'active' } });
  f.emit('item/agentMessage/delta', { turnId: 'active-turn', itemId: 'answer', delta: 'Partial ' });
  f.emit('item/completed', { turnId: 'active-turn', item: { id: 'answer', type: 'agentMessage', text: 'Complete owner answer.' } });
  f.emit('item/completed', { turnId: 'active-turn', item: { id: 'command', type: 'commandExecution', command: 'checked' } });
  await advance(t, 1000);
  assert.deepEqual(f.reads, before);
  assert.equal(f.state.state.value.value.thread.name, 'Pushed name');
  assert.equal(f.state.threadStatus, 'active');
  assert.equal(f.state.goalRecord.objective, 'Pushed goal');
  assert.deepEqual(f.state.live, [{ id: 'answer', text: 'Complete owner answer.' }]);
  assert.equal(f.state.pendingSteps[0].running, false);
  f.emit('turn/completed', { turn: { id: 'active-turn', status: 'completed' } });
  await advance(t, 600);
  for (const kind of ['state', 'goal', 'turns', 'output']) assert.equal(f.reads[kind], before[kind] + 1);
  const completed = { ...f.reads };
  f.emit('item/completed', { turnId: 'active-turn', item: { id: 'answer', type: 'agentMessage', text: 'Historical edit.' } });
  await advance(t, 600);
  assert.equal(f.reads.output, completed.output + 1);
  assert.equal(f.reads.turns, completed.turns + 1);
  assert.equal(f.reads.state, completed.state);
});

test("Console app metadata and package releases follow their actual change sources", async t => {
  const mount = setup(t), notifications = new Notifications();
  let currentReleaseId = "r1", catalog = [], uiReads = 0, packageReads = 0;
  const consoleRuntime = load("services/hive/console/src/runtime.ts", {
    "@ivy/ui": ui, "../../../../packages/ui-client/src/live": { liveUpdates },
    "../../../../packages/contracts/src/ui-route.js": load("packages/contracts/src/ui-route.ts"),
    "../../../../packages/sdk/src/client.js": {
      browserNotifications: () => notifications,
      browserClient: () => ({ request: async method => {
        if (method === "uis.catalog") {
          uiReads++;
          return { items: [{ metadata: { uiId: "fixture" }, currentReleaseId }], nextCursor: null };
        }
        assert.equal(method, "packages.catalog"); packageReads++;
        return { catalog: { packages: [...catalog] } };
      } }),
    },
  });
  const views = ["UIsView", "ReleasesView"].map(name => mount(load("services/hive/console/src/views/" + name + ".vue", {
    "@ivy/ui": ui, "@lucide/vue": {}, "../runtime": consoleRuntime, "../../../../../packages/sdk/src/client": {},
  }).default, {}, notifications));
  await settle();
  assert.equal(uiReads, 2);
  assert.equal(packageReads, 1);
  currentReleaseId = "r2";
  notifications.change("services");
  await advance(t, 250);
  assert.equal(uiReads, 2, "service changes do not reload installed app metadata");
  notifications.change("uis");
  await advance(t, 250);
  for (const view of views) assert.equal(view.page.value.value.items[0].currentReleaseId, "r2");
  assert.equal(uiReads, 4);
  assert.equal(packageReads, 1);
  catalog = [{ componentId: "service-package", revision: 1 }];
  notifications.change("objects/ivy/package-catalog");
  await advance(t, 250);
  assert.equal(packageReads, 2);
  assert.deepEqual(views[1].packageItems, catalog);
  notifications.change("system"); notifications.change("objects/secretary/execution");
  await advance(t, 250);
  assert.equal(uiReads, 4);
  assert.equal(packageReads, 2, "unrelated system and object changes do not reload packages");
});
