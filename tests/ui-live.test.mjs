import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import ts from "typescript";
import * as vue from "vue";
import { parse, compileScript } from "@vue/compiler-sfc";

// Exercise the actual composable and SFC setup with controlled transports and time.
const require = createRequire(import.meta.url);
function load(file, bindings = {}) {
  let source = readFileSync(file, "utf8");
  if (file.endsWith(".vue")) source = compileScript(parse(source).descriptor, { id: file }).content;
  const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", code)(name => name in bindings ? bindings[name] : require(name), module, module.exports);
  return module.exports;
}
const remote = load("packages/ui/src/composables/remote.ts");
const ui = { ...remote, ...load("packages/ui/src/composables/page-position.ts") };
const { liveUpdates, nativeInputUpdates } = load("packages/ui-client/src/live.ts", { "@ivy/ui": remote });
const runtime = load("packages/ui-client/src/runtime.ts", {
  "@ivy/ui": ui, "../../sdk/src/client.js": { browserClient: () => ({}), browserNotifications: () => ({}) }, "./live": { liveUpdates },
  "../../contracts/src/ui-route.js": load("packages/contracts/src/ui-route.ts"),
});
const native = load("packages/ui-client/src/native.ts", { "../../sdk/src/client.js": {} });
const { IvyError } = load("packages/contracts/src/errors.ts");

test("native panels share owner readiness, suppress outage calls and recover without rerouting", async t => {
  t.mock.timers.enable({ apis: ["Date"], now: 100000 });
  const owners = new Map([
    ["offline", { connected: true, synced: true, ready: false, desiredEnabled: true }],
    ["other", { connected: true, synced: true, ready: true, desiredEnabled: true }],
  ]);
  const checks = [], calls = [], discoveries = [];
  let failure = null;
  const client = { request: async (method, params) => {
    assert.equal(method, "serviceNodes.get"); checks.push(params.serviceNodeId);
    return { ...owners.get(params.serviceNodeId) };
  } };
  const { nativeRead } = load("packages/ui-client/src/native.ts", { "../../sdk/src/client.js": {
    IvyError,
    canonical: JSON.stringify,
    discover: async (_client, method, target) => { discoveries.push(target.serviceNodeId); return { method, ...target }; },
    callBound: async (_client, binding) => { calls.push(binding.serviceNodeId); if (failure) throw failure; return {}; },
    newOperationId: async () => "read-operation",
  } });
  const burst = () => Promise.allSettled(["agent.status", "codex.model/list", "agent.inputs", "agent.notifications"].map(method => nativeRead(client, "offline", method, {})));
  const unavailable = await burst();
  assert.ok(unavailable.every(result => result.status === "rejected" && result.reason.code === "service_not_ready"));
  await burst();
  assert.deepEqual(checks, ["offline"], "panels share one owner check during an outage");
  assert.equal(discoveries.length, 0);
  assert.equal(calls.length, 0, "known unavailable owners receive no failing provider calls");
  await nativeRead(client, "other", "agent.status", {});
  assert.deepEqual(calls, ["other"], "another owner remains independently usable");
  owners.get("offline").ready = true;
  t.mock.timers.tick(5000);
  assert.ok((await burst()).every(result => result.status === "fulfilled"));
  assert.equal(checks.filter(node => node === "offline").length, 2);
  failure = new IvyError("service_unavailable", "Provider connection is closed.");
  await assert.rejects(nativeRead(client, "offline", "agent.status", {}), { code: "service_unavailable" });
  const sent = calls.length;
  await burst();
  assert.equal(calls.length, sent, "a disconnect after a ready observation suppresses follow-up calls");
  failure = null;
  t.mock.timers.tick(5000);
  await nativeRead(client, "offline", "agent.status", {});
  assert.equal(calls.at(-1), "offline", "recovery retains the original owner");
  const cancelled = new AbortController(); cancelled.abort();
  const before = checks.length;
  await assert.rejects(nativeRead(client, "offline", "agent.status", {}, cancelled.signal));
  assert.equal(checks.length, before);
  assert.equal(calls.length, sent + 1, "cancelled observations create no native work");
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
      return { method, ...target };
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

function agentTask(t, connected = true, failures = new Set()) {
  const mount = setup(t), notifications = new Notifications();
  notifications.connected = connected;
  const reads = { state: 0, goal: 0, turns: 0, output: 0, journal: 0 }, holds = new Map(), signals = [];
  const journal = [];
  const read = async (kind, value, signal) => {
    reads[kind]++;
    if (kind === "state") signals.push(signal);
    if (failures.has(kind)) throw new Error(kind + " unavailable");
    await holds.get(kind);
    return value;
  };
  const component = load("ui/agent-ui/src/AgentTask.vue", {
    "@lucide/vue": {}, "@ivy/ui": ui,
    "../../../packages/ui-client/src/runtime": runtime,
    "../../../packages/ui-client/src/native": {
      ...native,
      nativeModels: async () => ({ data: [] }),
      optionalTool: async (_client, _node, method) => method === "codex.thread/goal/get" ? {} : undefined,
      nativeRead: async (_client, _node, method, args, signal) => {
        if (method === "codex.thread/goal/get") return read("goal", { goal: null }, signal);
        if (method === "codex.thread/turns/list") return read("turns", { data: [], nextCursor: null }, signal);
        assert.equal(method, "agent.notifications");
        return read("journal", { items: journal.filter(item => item.sequence > args.afterSequence),
          epoch: "epoch", throughSequence: journal.length, hasMore: false, gap: false }, signal);
      },
    },
    "../../../packages/ui-client/src/native-thread-control": {
      readNativeThreadControl: (_client, _node, _thread, signal) =>
        read("state", { attached: true, status: { epoch: "epoch" }, thread: { name: "Task", status: { type: "idle" } } }, signal),
    },
    "../../../packages/ui-client/src/native-output-page": {
      isUnmaterializedNativeHistory: () => false,
      readNativeOutputPage: (_client, _node, _thread, _turn, _cursor, signal) =>
        read("output", { mode: "items", items: [], nextCursor: null }, signal),
    },
    "../../../packages/ui-client/src/native-action": {
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
    "../../../packages/ui-client/src/object-archive": load("packages/ui-client/src/object-archive.ts", { "../../sdk/src/client.js": {} }),
    "../../../packages/ui-client/src/message-attachments": load("packages/ui-client/src/message-attachments.ts", { "../../sdk/src/client.js": {} }),
    "../../../packages/ui-client/src/NativeActionState.vue": {},
    "../../../packages/ui-client/src/NativeInputs.vue": {},
    "../../../packages/ui-client/src/TaskProjectDialog.vue": {},
    "../../../packages/ui-client/src/MessageImages.vue": {},
    "../../../packages/ui-client/src/native-images": {},
    "./conversation": load("ui/agent-ui/src/conversation.ts", {
      "../../../packages/ui-client/src/native": native,
      "../../../packages/ui-client/src/message-images": load("packages/ui-client/src/message-images.ts", { "./native": native }),
    }),
    "./runtime": { base: location, client: { request: async () => ({ summary: {} }) }, notifications,
      tr: (_de, en) => en,
      outputCache: new (load("packages/ui-client/src/native-output-cache.ts").NativeOutputCache)(),
    },
  }).default;
  const state = mount(component, { node: "owner", threadId: "task", turnId: "" }, notifications);
  return { state, notifications, reads, failures, holds, signals,
    emit(method) {
      const entry = { method, params: { threadId: "task" }, epoch: "epoch", sequence: journal.length + 1, observedAt: new Date().toISOString() };
      journal.push(entry);
      if (notifications.connected) notifications.emit("owner", "notification", entry);
    },
  };
}

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
  f.notifications.ready(true);
  await advance(t, 600);
  assert.equal(f.reads.state, 4, "reconnection refreshes immediately");
});

test("Agent streaming deltas do not refresh snapshots but lifecycle and goal changes do", async t => {
  const f = agentTask(t);
  await settle(); await advance(t, 600);
  const before = { ...f.reads };
  for (const method of ["thread/realtime/outputAudio/delta", "thread/realtime/transcript/delta",
    "thread/realtime/item/transcript/delta", "thread/tokenUsage/updated", "item/agentMessage/delta"])
    f.emit(method);
  await advance(t, 1000);
  assert.deepEqual(f.reads, before);
  for (const method of ["thread/name/updated", "thread/goal/updated", "thread/goal/cleared", "turn/completed"]) {
    const count = f.reads.state;
    f.emit(method); f.emit(method);
    await advance(t, 600);
    assert.equal(f.reads.state, count + 1, method + " coalesces a burst into one refresh");
  }
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
