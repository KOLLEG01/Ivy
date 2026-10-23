import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import ts from 'typescript';
import {NativeInventory, publishNativeThreads, publishNativeProjects} from '../../dist/services/agent-manager/src/inventory.js';
import {NativeNoticePolicy} from '../../dist/services/agent-manager/src/notifications.js';
import {NativeJournal} from '../../dist/services/agent-manager/src/journal.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {digest} from '../../dist/packages/contracts/src/canonical.js';

const source = ts.createSourceFile('main.ts', readFileSync('services/agent-manager/src/main.ts', 'utf8'), ts.ScriptTarget.Latest, true);
function extract(kind, name) {
  const found = [];
  function visit(node) {
    if (kind === 'variable' && ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.name.text === name) found.push(node);
    if (kind === 'property' && ts.isPropertyAssignment(node) && node.name.getText(source).replaceAll('"', '') === name) found.push(node.initializer);
    if (kind === 'assignment' && ts.isBinaryExpression(node) && node.left.getText(source) === name && node.operatorToken.kind === ts.SyntaxKind.EqualsToken) found.push(node.right);
    ts.forEachChild(node, visit);
  }
  visit(source); assert.equal(found.length, 1, name); return found[0].getText(source);
}
const variables = ['projectSnapshot', 'projectObserved', 'threadRefreshAt', 'projectRefreshAt', 'threadRefreshWork', 'projectRefreshWork',
  'threadRefreshQueued', 'projectRefreshQueued', 'threadInventoryCode', 'projectInventoryCode'];
const expressions = Object.fromEntries([...variables.map(name => [name, extract('variable', name)]),
  ...['refreshThreads', 'refreshProjects', 'freshProjects'].map(name => [name, extract('variable', name)]),
  ['projects', extract('property', 'agent.projects')], ['readiness', extract('property', 'readiness')],
  ...['requestThreadRefresh', 'requestProjectRefresh'].map(name => [name, extract('assignment', name)])]);
const globals = {IvyError, publishNativeThreads, publishNativeProjects};
const setup = 'let ' + variables.map(name => expressions[name]).join(',') + ';' +
  ['refreshThreads', 'refreshProjects', 'freshProjects'].map(name => 'const ' + expressions[name] + ';').join('') +
  'return {refreshThreads,refreshProjects,freshProjects,projects:' + expressions.projects + ',readiness:' + expressions.readiness +
  ',requestThreads:' + expressions.requestThreadRefresh + ',requestProjects:' + expressions.requestProjectRefresh +
  ',snapshot:()=>({projectSnapshot,projectObserved,code:threadInventoryCode??projectInventoryCode})' +
  ',bind:(connection)=>{hiveConnection=connection;projectSnapshot=inventory.currentProjects();locations.setNativeProjects(projectSnapshot.projects.projects);}};';
const create = Function('env', ...Object.keys(globals), 'with(env){' + ts.transpileModule('const setup=()=>{' + setup + '};',
  {compilerOptions: {target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext}}).outputText + 'return setup();}');
const c = (action, fields = {}) => ({action, ...fields});
const failure = (code, outcome = 'not_executed') => ({code, message: 'Fixture failure\ud800', outcome, details: {exact: '\udfff'}});
const abortReason = failure('fixture_cancelled\ud800', 'unknown');
const error = value => new IvyError(value.code, value.message, value.outcome, value.details);
const event = (method, params) => ({method, params});
const start = (handle, method, extra = {}) => c('start', {handle, method, ...extra});
const wait = handle => c('wait', {handle}), entered = name => c('entered', {name}), release = name => c('release', {name});
const snap = c('snapshot'), drain = c('drain'), bind = name => c('bind', {name});
const project = (id = 'p', name = 'Project', path = '/fixture') => ({id, name, roots: [{path}], position: 0, recencyAt: null});
const thread = (id = 't', extra = {}) => ({id, preview: 'Original ' + id, cwd: '/fixture', name: null, status: {type: 'idle'}, source: 'appServer', updatedAt: 1788600000, ephemeral: false, ...extra});
const page = (method, data = [], extra = {}) => ({method, reply: {result: {data, nextCursor: null}}, ...extra});
const p = (id = 'p', extra = {}, name = 'Project') => page('project/list', [project(id, name)], extra);
const t = (id = 't', extra = {}, metadata = {}) => [page('thread/loaded/list', [], extra), page('thread/list', [thread(id, metadata)]), page('thread/list'), page('thread/loaded/list')];
const rows = [];
function add(name, commands, threads = [], projects = [], hive = {}, options = {}) {
  rows.push({name, commands, native: {thread: threads, project: projects}, hive, ...options});
}
add('projects-first-read-then-cached-ignore-arguments-and-cancellation', [bind('one'), c('projects', {cancelled: true}), c('projects'), snap], [], [p()], {}, {defaults: {taskRoot: '/tasks', nested: ['retained']}});
add('project-publication-failure-retains-complete-snapshot-and-new-native-cache', [bind('one'), c('projects'), c('refreshProjects'), c('projects'), snap], [], [p('retained'), p('new', {}, 'IvyInternal')], {project: [{}, {throw: failure('publish_failed', 'unknown')}]});
add('first-publication-failure-remains-unobserved-and-next-tool-retries', [bind('one'), c('projects'), snap, c('projects'), snap], [], [p('first'), p('second')], {project: [{throw: failure('publish_failed')}, {}]});
add('cached-project-publication-does-not-read-native', [bind('one'), c('refreshProjects', {nativeRead: false}), c('projects'), snap]);
add('thread-overlap-coalesces-one-follow-up-on-latest-generation', [bind('one'), start('first', 'threads'), entered('read'), start('second', 'threads'), start('third', 'threads'), bind('two'), release('read'), wait('first'), wait('second'), wait('third'), drain, snap], [...t('first', {gate: 'read'}), ...t('second')]);
add('thread-queued-follow-up-is-suppressed-on-close', [bind('one'), start('first', 'threads'), entered('read'), start('second', 'threads'), c('closing', {value: true}), release('read'), wait('first'), wait('second'), drain, snap], t('first', {gate: 'read'}));
add('thread-queued-follow-up-is-suppressed-for-aborted-current-generation', [bind('one'), start('first', 'threads'), entered('read'), start('second', 'threads'), bind('two'), c('abort', {name: 'two'}), release('read'), wait('first'), wait('second'), drain, snap], t('first', {gate: 'read'}));
add('aborted-old-read-can-follow-up-on-live-new-generation', [bind('one'), start('first', 'threads'), entered('read'), start('second', 'threads'), bind('two'), c('abort', {name: 'one'}), release('read'), wait('first'), wait('second'), drain, snap], [page('thread/loaded/list', [], {gate: 'read'}), ...t('second')]);
add('cached-project-overlap-does-not-queue-native-read', [bind('one'), start('first', 'projects'), entered('read'), start('second', 'projects', {nativeRead: false}), release('read'), wait('first'), wait('second'), drain, snap], [], [p('first', {gate: 'read'})]);
add('native-project-overlap-queues-one-follow-up', [bind('one'), start('first', 'projects', {nativeRead: false}), entered('publish'), start('second', 'projects'), start('third', 'projects'), release('publish'), wait('first'), wait('second'), wait('third'), drain, snap], [], [p('new')], {project: [{gate: 'publish'}, {}]});
add('fresh-projects-waits-then-reads-again', [bind('one'), start('first', 'projects'), entered('publish'), start('fresh', 'fresh'), release('publish'), wait('first'), wait('fresh'), drain, snap], [], [p('first'), p('fresh')], {project: [{gate: 'publish'}, {}]});
add('fresh-projects-propagates-existing-failure-without-second-read', [bind('one'), start('first', 'projects'), entered('publish'), start('fresh', 'fresh'), release('publish'), wait('first'), wait('fresh'), drain, snap], [], [p('first')], {project: [{gate: 'publish', throw: failure('publish_failed')}]});
add('project-overlap-follows-current-generation', [bind('one'), start('first', 'projects'), entered('read'), start('second', 'projects'), bind('two'), release('read'), wait('first'), wait('second'), drain, snap], [], [p('first', {gate: 'read'}), p('second')]);
add('post-publication-abort-refuses-observed-snapshot', [bind('one'), c('refreshProjects'), snap], [], [p('cached')], {project: [{abort: true}]});
add('thread-publication-failure-keeps-policy-metadata', [bind('one'), c('refreshThreads'), snap], t('t', {}, {projectId: 'p'}), [], {thread: [{throw: failure('publish_failed', 'unknown')}]});
add('project-sink-failure-occurs-after-policy-before-publication', [bind('one'), c('sinkFailure'), c('refreshProjects'), snap], [], [p('p', {}, 'IvyInternal')]);
add('rebind-retains-observed-flag-and-uses-current-native-cache', [bind('one'), c('projects'), c('refreshProjects'), bind('two'), c('projects'), snap], [], [p('old'), p('cached')], {project: [{}, {throw: failure('publish_failed')}]});
add('abort-before-refresh-preserves-exact-generation-reason', [bind('one'), c('abort', {name: 'one'}), c('refreshThreads'), c('refreshProjects', {nativeRead: false}), snap]);
add('independent-failures-prioritize-thread-code-until-recovered', [bind('one'), c('refreshThreads'), c('refreshProjects'), snap, c('refreshThreads'), snap, c('refreshProjects'), snap], [{method: 'thread/loaded/list', throw: failure('thread_failure\ud800', 'unknown')}, ...t()], [{method: 'project/list', throw: failure('project_failure\udfff')}, p()]);
add('refresh-periods-use-start-time-and-separate-lanes', [bind('one'), c('time', {value: 59999}), c('due'), drain, c('time', {value: 60000}), c('due'), drain, c('time', {value: 300000}), c('due'), drain, c('time', {value: 300001}), c('due'), drain, snap], [...t('one'), ...t('two')], [p()], {}, {initialTime: 0});
add('readiness-does-not-queue-a-running-thread-refresh', [bind('one'), start('first', 'threads'), entered('read'), c('time', {value: 60000}), c('due'), release('read'), wait('first'), drain, snap], t('one', {gate: 'read'}), [], {}, {initialTime: 0});
add('failed-start-time-still-bounds-periodic-retry', [bind('one'), c('time', {value: 60000}), c('due'), drain, snap, c('time', {value: 60001}), c('due'), drain, c('time', {value: 120000}), c('due'), drain, snap], [{method: 'thread/loaded/list', throw: failure('read_failed')}, ...t()], [], {}, {initialTime: 0});
add('push-hooks-require-ready-current-connection-and-have-no-extra-close-guard', [c('requestThreads'), c('requestProjects'), bind('one'), c('ready', {value: false}), c('requestThreads'), c('requestProjects'), c('ready', {value: true}), c('requestThreads'), c('requestProjects'), drain, c('closing', {value: true}), c('requestThreads'), c('requestProjects'), drain, snap], [...t(), ...t()], [p(), p()]);
add('newer-push-metadata-survives-inflight-thread-snapshot', [bind('one'), c('refreshProjects'), start('first', 'threads'), entered('read'), c('observe', {event: event('thread/started', {thread: {id: 't', projectId: 'p'}})}), c('observe', {event: event('thread/started', {thread: {id: 'new', projectId: 'p'}})}), release('read'), wait('first'), drain, snap], t('t', {gate: 'read'}, {projectId: null}), [p('p', {}, 'IvyInternal')]);

const policyRows = [];
const summary = (nativeId, name, paths) => ({nativeId, source: 'native', name, paths, position: 0, recencyAt: null});
const projects = [summary('internal', 'IvyInternal', ['C:\\Root', '/work']), summary('external', 'Project', ['C:\\Root\\child', '/work/deeper'])];
const checks = ['t', 'new', '\ud800'];
const policy = (name, commands) => policyRows.push({name, commands});
const set = (id, value, revision) => c('setThread', {id, value, ...(revision === undefined ? {} : {revision})});
const entries = (values, revision) => c('setThreads', {entries: values.map(([id, value]) => ({nativeId: id, summary: value})), revision});
policy('unknown-and-empty-known-thread-are-distinct', [snap, set('t', {}), snap]);
policy('projects-arrive-without-native-thread-reread', [set('t', {cwd: '/work/task'}), snap, c('setProjects', {projects}), snap]);
for (const value of [{cwd: 'c:\\ROOT\\task'}, {cwd: 'C:/root/child/task'}, {cwd: '/work/task'}, {cwd: '/workshop'}, {cwd: '/WORK/task'}, {cwd: '/work/deeper/task'}, {cwd: '/work/task', projectId: 'external'}, {cwd: '/work/task', projectId: null}, {cwd: '/work/task', projectId: ''}, {cwd: '/work/task', projectId: 7}, {cwd: '/work/task', projectId: 'missing'}, {cwd: 7, projectId: 'internal'}]) policy('membership-' + policyRows.length, [c('setProjects', {projects}), set('t', value), snap]);
policy('newer-push-wins-over-old-bulk-snapshot-and-absent-ids', [c('setProjects', {projects}), set('t', {projectId: 'external'}, 0), set('new', {projectId: 'internal'}), set('t', {projectId: 'internal'}), entries([['t', {projectId: 'external'}]], 0), snap, entries([], 1), snap, entries([], 2), snap]);
policy('equal-revision-replaces-and-deletes', [c('setProjects', {projects}), set('t', {projectId: 'internal'}, 3), set('t', {projectId: 'external'}, 3), snap, entries([], 3), snap]);
policy('explicit-revision-does-not-advance-push-revision', [c('setProjects', {projects}), set('t', {projectId: 'internal'}, 100), set('t', {projectId: 'external'}), snap]);
policy('observe-preserves-cwd-project-property-and-utf16-identity', [c('setProjects', {projects}), c('observe', {event: event('thread/started', {thread: {id: '\ud800', cwd: '/work/task', ignored: true}})}), c('observe', {event: event('thread/project/updated', {threadId: '\ud800', projectId: null})}), snap, c('observe', {event: event('thread/project/updated', {threadId: 'new', projectId: 'internal'})}), snap]);
policy('deletion-does-not-increment-revision-or-create-a-tombstone', [c('setProjects', {projects}), set('t', {projectId: 'internal'}), c('observe', {event: event('thread/deleted', {threadId: 't'})}), snap, entries([['t', {projectId: 'internal'}]], 0), snap]);
policy('irrelevant-and-malformed-events-do-not-mutate-policy', [set('t', {}), ...[event('other', null), event('thread/started', []), event('thread/started', {thread: {id: 7}}), event('thread/project/updated', {threadId: 't'}), event('thread/deleted', {threadId: 7})].map(event => c('observe', {event})), snap]);
policy('bulk-summary-is-an-object-only-and-removes-stale-ids', [set('new', {projectId: 'internal'}, 0), entries([['t', null], ['\ud800', []]], 0), snap]);

const RealDate = Date; let now;
globalThis.Date = class extends RealDate {constructor(...args) {super(...(args.length ? args : [now]));} static now() {return now;}};
const capture = async work => {try {return {result: (await work()) ?? null};} catch (e) {return {error: IvyError.from(e).toWire()};}};
try {
  for (const row of policyRows) {
    const owner = new NativeNoticePolicy(); row.expected = [];
    for (const step of row.commands) row.expected.push(await capture(() => {
      if (step.action === 'setProjects') owner.setProjects(step.projects);
      else if (step.action === 'setThread') owner.setThread(step.id, step.value, step.revision);
      else if (step.action === 'setThreads') owner.setThreads(step.entries, step.revision);
      else if (step.action === 'observe') owner.observe(step.event);
      else if (step.action === 'snapshot') return {revision: owner.revision, internal: Object.fromEntries(checks.map(id => [id, owner.isInternal(id)]))};
      else throw Error(step.action);
    }));
  }
  const catalog = JSON.parse(readFileSync('specs/native/codex-0.154.0/catalog.json', 'utf8'));
  for (const row of rows) {
    now = row.initialTime ?? 1788600000000;
    const noticePolicy = new NativeNoticePolicy(), connections = new Map(), barriers = new Map(), handles = new Map(), work = new Set();
    const nativeCalls = {thread: [], project: []}, hiveCalls = {thread: [], project: []}, consumed = {thread: 0, project: 0}, published = {thread: 0, project: 0}, locations = [];
    let failSink = false, last;
    function barrier(name) { if (!barriers.has(name)) { let enter, release; const entered = new Promise(r => {enter = r;}), opened = new Promise(r => {release = r;}); barriers.set(name, {enter, release, entered, opened}); } return barriers.get(name); }
    async function gate(name) { if (name) {const b = barrier(name); b.enter(); await b.opened;} }
    const rpc = {catalog, request: async (method, params, options, budget) => {
      const lane = method.startsWith('project/') ? 'project' : 'thread', step = row.native[lane][consumed[lane]++];
      assert(step, row.name + ' unexpected native ' + method); assert.equal(method, step.method);
      nativeCalls[lane].push({method, params: structuredClone(params), detachOnTimeout: options.detachOnTimeout, budget});
      await gate(step.gate); if (step.throw) throw error(step.throw); return structuredClone(step.reply);
    }};
    const inventory = new NativeInventory(rpc, 'epoch');
    const journal = new NativeJournal({serviceNodeId: 'owner', nativeVersion: 'fixture', nativeExecutableHash: digest('fixture')}, {maxOperations: 100, maxJournalBytes: 64 * 1024 * 1024, maxPendingInputs: 10});
    const env = {inventory, journal, noticePolicy, hiveConnection: null, closing: false, service: {ready: true, connection: null},
      locations: {defaults: row.defaults ?? {}, setNativeProjects: values => {locations.push(structuredClone(values)); if (failSink) {failSink = false; throw error(failure('locations_failed'));}}},
      track: promise => {work.add(promise); void promise.then(() => work.delete(promise), () => work.delete(promise)); return promise;},
      instructions: null, mcp: null, skills: null, environmentAt: 0, owner: {rpc: {connected: true}}, config: {serviceNodeId: 'owner'}, writeHealth: async () => {}};
    const runtime = create(env, ...Object.values(globals));
    function connection(name) {
      if (!connections.has(name)) {
        const controller = new AbortController();
        connections.set(name, {controller, signal: controller.signal, generation: connections.size + 1,
          request: async (method, params, options) => {
            assert.equal(method, 'inventory.sync'); assert.equal(options.signal, controller.signal);
            const lane = params.kind, step = row.hive[lane]?.[published[lane]++] ?? {};
            hiveCalls[lane].push({connection: name, method, params: structuredClone(params)});
            await gate(step.gate); if (step.abort) controller.abort(error(abortReason)); if (step.throw) throw error(step.throw); return {};
          }});
      }
      return connections.get(name);
    }
    async function invoke(method, step = {}) {
      if (method === 'threads') return runtime.refreshThreads(env.service.connection);
      if (method === 'projects') return runtime.refreshProjects(env.service.connection, step.nativeRead ?? true);
      if (method === 'fresh') return runtime.freshProjects(env.service.connection);
      if (method === 'tool') return runtime.projects(step.input ?? null, {signal: AbortSignal.abort(error(abortReason))});
      throw Error(method);
    }
    row.expected = [];
    for (const step of row.commands) {
      const outcome = await capture(async () => {
        switch (step.action) {
          case 'bind': env.service.connection = connection(step.name); runtime.bind(env.service.connection); break;
          case 'refreshThreads': await invoke('threads', step); break;
          case 'refreshProjects': await invoke('projects', step); break;
          case 'projects': last = await invoke('tool', step); return last;
          case 'mutateResult': last.projects.length = 0; if (last.defaults.nested) last.defaults.nested[0] = 'changed'; break;
          case 'start': {const handle = {settled: false}; handle.promise = capture(() => invoke(step.method, step)).then(value => {handle.settled = true; return value;}); handles.set(step.handle, handle); break;}
          case 'wait': return handles.get(step.handle).promise;
          case 'entered': await barrier(step.name).entered; break;
          case 'release': barrier(step.name).release(); break;
          case 'abort': connection(step.name).controller.abort(error(abortReason)); break;
          case 'closing': env.closing = step.value; break;
          case 'ready': env.service.ready = step.value; break;
          case 'requestThreads': runtime.requestThreads(); break;
          case 'requestProjects': runtime.requestProjects(); break;
          case 'due': await runtime.readiness(env.service.connection); break;
          case 'time': now = step.value; break;
          case 'sinkFailure': failSink = true; break;
          case 'observe': noticePolicy.observe(step.event); break;
          case 'drain': while (work.size) await Promise.allSettled([...work]); break;
          case 'snapshot': return {...structuredClone(runtime.snapshot()), currentProjects: inventory.currentProjects(),
            policy: {revision: noticePolicy.revision, internal: Object.fromEntries(checks.map(id => [id, noticePolicy.isInternal(id)]))},
            nativeCalls: structuredClone(nativeCalls), hiveCalls: structuredClone(hiveCalls), locations: structuredClone(locations)};
          default: throw Error(step.action);
        }
        return null;
      });
      // JSON snapshots must not accidentally share mutable tool return references.
      row.expected.push(structuredClone(outcome));
    }
    while (work.size) await Promise.allSettled([...work]);
    assert.deepEqual(consumed, {thread: row.native.thread.length, project: row.native.project.length}, row.name);
  }
  console.log(JSON.stringify({expressions, abortReason, checks, policyRows, rows}));
} finally {globalThis.Date = RealDate;}
