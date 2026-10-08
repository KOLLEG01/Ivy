import { randomUUID } from 'node:crypto';
import { readFile, mkdir } from 'node:fs/promises';
import { readdirSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { PassThrough } from 'node:stream';
import { fixture, fixtureToken } from './fixture.mjs';
import { NativeRpc } from '../../../dist/services/agent-manager/src/rpc.js';
import { agentRegistry } from '../../../dist/services/agent-manager/src/registry.js';
import { startAgentManager } from '../../../dist/services/agent-manager/src/main.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';
import { publishUi } from '../../../dist/packages/cli/src/publish-ui.js';
import { newOperationId } from '../../../dist/packages/sdk/src/client.js';

// Real Hive, AgentManager, journal and native JSON-RPC validation. Only the external native process
// is simulated; model execution and installed target behavior have separate acceptance gates.
export async function agentFixture(t, viewport, completeCatalog = false, nativeVersion = '0.154.0', manageInstructions = false, serviceTasks = false) {
  const f = await fixture(t, false, viewport), build = JSON.parse(await readFile('dist/build-info.json', 'utf8'));
  const workerOperations = [], requestWorker = f.server.worker.request.bind(f.server.worker);
  f.server.worker.request = async (...args) => {
    const row = { action: args[0].action, method: args[0].request?.method ?? null, startedAt: new Date().toISOString() };
    if (workerOperations.length < 256) workerOperations.push(row);
    const began = performance.now();
    try { return await requestWorker(...args); }
    catch (error) { row.code = error.code ?? error.name; throw error; }
    finally { row.durationMs = performance.now() - began; }
  };
  const full = JSON.parse(await readFile('specs/native/codex-' + nativeVersion + '/catalog.json', 'utf8'));
  const methods = ['thread/list', 'thread/loaded/list', 'thread/read', 'thread/start', 'thread/resume', 'thread/metadata/update', 'thread/turns/list', 'thread/items/list', 'thread/goal/get', 'thread/goal/set', 'thread/goal/clear', 'turn/start', 'turn/steer', 'turn/interrupt', 'model/list', 'collaborationMode/list', 'permissionProfile/list', 'account/rateLimits/read', 'project/list', 'project/create', 'project/update', 'project/delete'];
  methods.push('config/read');
  const catalog = completeCatalog ? full : { ...full, clientRequests: full.clientRequests.filter(m => methods.includes(m.method) || m.method === 'fs/readFile') };
  const settings = { nativeExecutable: process.execPath, nativeHome: join(f.root, 'native'), nativeVersion: full.version, nativeExecutableHash: full.nativeExecutableHash,
    skillsRoot: join(f.root, '.agents', 'skills'), projectRoot: join(f.root, 'normal-projects'), internalProjectRoot: join(f.root, 'internal-projects'),
    capabilities: [],
    limits: { maxOperations: 10000, maxJournalBytes: 256 * 1024 * 1024, maxPendingInputs: 32, maxNotificationBytes: 1048576 } };
  const config = { schemaVersion: 1, componentId: 'agent-manager', instanceId: 'agent', serviceNodeId: 'browser-agent', hostId: 'isolated-agent-host',
    publicBaseUrl: f.base, artifactRoot: join(f.root, 'agent-artifact'), dataRoot: join(f.root, 'agent-data'), buildId: build.buildId, version: build.version, credential: fixtureToken, settings };
  await mkdir(join(config.artifactRoot, 'dist'), { recursive: true }); await atomicJson(join(config.artifactRoot, 'dist/build-info.json'), build);
  const configFile = join(f.root, 'agent.json'); await atomicJson(configFile, config);
  if (manageInstructions) await mkdir(settings.nativeHome, { recursive: true });
  const nativeProjects = [{ id: 'fixture-project', name: 'Isolated project', path: resolve(f.root, 'project') }];
  const projectCreations = new Map();
  let emptyModelPage = false;
  let heldMethod = null, heldReply = null;
  if (serviceTasks) nativeProjects.push({ id: 'fixture-internal-project', name: 'IvyInternal', path: settings.internalProjectRoot });
  for (const project of nativeProjects) await mkdir(project.path, { recursive: true });
  const threads = new Map(), goals = new Map(), owners = [], managers = [], managerObservations = []; let unsupportedItems = false, unmaterializedHistory = false;
  const newThread = (id, loaded = false) => ({ cliVersion: full.version, createdAt: 1788690000, cwd: nativeProjects[0].path, ephemeral: false, archived: false, id, modelProvider: 'openai', preview: 'An isolated saved native task', projectId: nativeProjects[0].id, recencyAt: 1788690000000, sessionId: id, source: 'appServer', status: { type: loaded ? 'idle' : 'notLoaded' }, turns: [], updatedAt: 1788690000, canAcceptDirectInput: loaded, name: id });
  const wireThread = thread => ({ ...thread });
  threads.set('saved-task', newThread('saved-task'));
  if (serviceTasks) threads.set('internal-task', { ...newThread('internal-task'), name: 'Internal service work', cwd: settings.internalProjectRoot, projectId: 'fixture-internal-project' });
  const launcher = async (options, modelCatalog) => {
    const input = new PassThrough(), output = new PassThrough(), epoch = randomUUID(), sent = [];
    options.beginEpoch(epoch);
    const rpc = new NativeRpc(catalog, input, output, { onClose: code => options.onClose(epoch, code), onRequest: value => options.onRequest(epoch, value), onNotification: value => options.onNotification(epoch, value) });
    const owner = { rpc, epoch, sent, emit: frame => { output.write(JSON.stringify(frame) + '\n'); } }; owners.push(owner);
    const startResult = thread => ({ thread: wireThread(thread), cwd: thread.cwd, model: 'fixture-model', modelProvider: 'openai', approvalPolicy: 'on-request', approvalsReviewer: 'user', sandbox: { type: 'readOnly' }, reasoningEffort: 'high' });
    input.on('data', bytes => {
      const frame = JSON.parse(bytes.toString('utf8')); sent.push(frame);
      if (!frame.method) return;
      const p = frame.params ?? {}, thread = threads.get(p.threadId); let result;
      if (frame.method === 'model/list' && emptyModelPage) { emptyModelPage = false; owner.emit({ id: frame.id, result: { data: [], nextCursor: null } }); return; }
      switch (frame.method) {
        case 'project/list': result = { data: nativeProjects.map((project, position) => ({ id: project.id, name: project.name, roots: [{ path: project.path }], metadata: {}, position, recencyAt: 1788690000000, createdAt: 1788690000, updatedAt: 1788690000 })), nextCursor: null }; break;
        case 'project/create': {
          let project = projectCreations.get(p.idempotencyKey);
          if (!project) { project = { id: randomUUID(), name: p.name, path: p.roots[0].path }; nativeProjects.push(project); projectCreations.set(p.idempotencyKey, project); }
          result = { project: { id: project.id, name: project.name, roots: [{ path: project.path }], metadata: {}, position: nativeProjects.indexOf(project), createdAt: 1788690000, updatedAt: 1788690000 } };
          owner.emit({ method: 'project/changed', params: { projectId: project.id, changeType: 'created' } }); break;
        }
        case 'project/update': {
          const project = nativeProjects.find(value => value.id === p.projectId); if (p.name) project.name = p.name;
          result = { project: { id: project.id, name: project.name, roots: [{ path: project.path }], metadata: {}, position: nativeProjects.indexOf(project), createdAt: 1788690000, updatedAt: 1788690000 } };
          owner.emit({ method: 'project/changed', params: { projectId: project.id, changeType: 'updated' } }); break;
        }
        case 'project/delete': {
          const index = nativeProjects.findIndex(value => value.id === p.projectId); if (index >= 0) nativeProjects.splice(index, 1);
          for (const thread of threads.values()) if (thread.projectId === p.projectId) thread.projectId = null;
          result = {}; owner.emit({ method: 'project/changed', params: { projectId: p.projectId, changeType: 'deleted' } }); break;
        }
        case 'collaborationMode/list': result = { data: [{ mode: 'plan', name: 'Plan', model: null, reasoning_effort: 'high' }, { mode: 'default', name: 'Default', model: null, reasoning_effort: null }] }; break;
        case 'config/read': result = { config: { model: modelCatalog?.find(model => model.isDefault)?.model ?? 'fixture-model', model_reasoning_effort: 'high', sandbox_mode: 'read-only' }, origins: {}, layers: null }; break;
        case 'permissionProfile/list': result = { data: [{ id: ':read-only', allowed: true, description: 'Inspect the workspace without changing files.' }, { id: ':workspace', allowed: true, description: 'Read and write inside the active workspace.' }, { id: ':danger-full-access', allowed: true, description: 'Run without local sandbox restrictions.' }], nextCursor: null }; break;
        case 'thread/list': result = { data: [...threads.values()].filter(v => v.archived === !!p.archived).sort((a, b) => b.recencyAt - a.recencyAt).map(v => wireThread({ ...v, turns: [] })), nextCursor: null }; break;
        case 'thread/loaded/list': result = { data: [...threads.values()].filter(t => t.canAcceptDirectInput).map(t => t.id), nextCursor: null }; break;
        case 'thread/read': result = { thread: wireThread({ ...thread, turns: p.includeTurns ? thread.turns : [] }) }; break;
        case 'thread/start': { if (p.projectId === '') { owner.emit({ id: frame.id, error: { code: -32600, message: 'projectId must not be empty' } }); return; } const next = newThread('created-' + randomUUID(), true); next.cwd = p.cwd; next.projectId = p.projectId ?? null; threads.set(next.id, next); result = startResult({ ...next, turns: [] }); setImmediate(() => owner.emit({ method: 'thread/started', params: { thread: wireThread({ ...next, turns: [] }) } })); break; }
        case 'thread/metadata/update': { thread.projectId = p.projectId || null; result = wireThread({ ...thread, turns: [] }); owner.emit({ method: 'thread/project/updated', params: { threadId: thread.id, projectId: thread.projectId } }); break; }
        case 'thread/name/set': thread.name = p.name; result = {}; setImmediate(() => owner.emit({ method: 'thread/name/updated', params: { threadId: thread.id, name: thread.name } })); break;
        case 'thread/archive': thread.archived = true; result = {}; setImmediate(() => owner.emit({ method: 'thread/archived', params: { threadId: thread.id } })); break;
        case 'thread/delete': threads.delete(thread.id); result = {}; setImmediate(() => owner.emit({ method: 'thread/deleted', params: { threadId: thread.id } })); break;
        case 'thread/unarchive': thread.archived = false; result = { thread: wireThread(thread) }; setImmediate(() => owner.emit({ method: 'thread/unarchived', params: { threadId: thread.id } })); break;
        case 'thread/resume': thread.canAcceptDirectInput = true; thread.status = { type: 'idle' }; result = startResult(thread); break;
        case 'thread/goal/get': result = { goal: goals.get(p.threadId) ?? null }; break;
        case 'thread/goal/set': {
          const now = 1788690000, previous = goals.get(p.threadId), next = { threadId: p.threadId, objective: p.objective ?? previous?.objective ?? '', status: p.status ?? previous?.status ?? 'active', tokenBudget: p.tokenBudget ?? previous?.tokenBudget ?? null, tokensUsed: previous?.tokensUsed ?? 0, timeUsedSeconds: previous?.timeUsedSeconds ?? 0, createdAt: previous?.createdAt ?? now, updatedAt: now };
          goals.set(p.threadId, next); result = { goal: next }; break;
        }
        case 'thread/goal/clear': result = { cleared: goals.delete(p.threadId) }; break;
        case 'thread/turns/list': {
          if (unmaterializedHistory && thread.turns.length === 0) { owner.emit({ id: frame.id, error: { code: -32600, message: `thread ${p.threadId} is not materialized yet; thread/turns/list is unavailable before first user message` } }); return; }
          const offset = p.cursor ? Number(p.cursor.slice('fixture-turns-'.length)) : 0, all = [...thread.turns].reverse(), limit = p.limit ?? 100;
          result = { data: all.slice(offset, offset + limit).map(turn => ({ ...turn, items: p.itemsView === 'full' ? turn.items : [], itemsView: p.itemsView === 'full' ? 'full' : 'notLoaded' })), nextCursor: offset + limit < all.length ? 'fixture-turns-' + (offset + limit) : null }; break;
        }
        case 'thread/items/list': {
          if (unsupportedItems) { owner.emit({ id: frame.id, error: { code: -32601, message: 'thread/items/list is not supported yet' } }); return; }
          const turns = p.sortDirection === 'asc' ? thread.turns : [...thread.turns].reverse();
          const data = turns.filter(t => !p.turnId || p.turnId === t.id).flatMap(t => (p.sortDirection === 'asc' ? t.items : [...t.items].reverse()).map(item => ({ item, turnId: t.id })));
          const offset = p.cursor ? Number(p.cursor.slice('fixture-items-'.length)) : 0;
          const wireItems = data;
          result = completeCatalog ? { data: wireItems.slice(offset, offset + 1), nextCursor: offset + 1 < wireItems.length ? 'fixture-items-' + (offset + 1) : null } : { data: wireItems, nextCursor: null }; break;
        }
        case 'model/list': result = modelCatalog ? { data: modelCatalog, nextCursor: null } : { data: [{ id: p.cursor ? 'second-model-id' : 'fixture-model-id', model: p.cursor ? 'second-native-model' : 'fixture-model', displayName: p.cursor ? 'Second native page model' : 'Native fixture model', description: 'Protocol simulation only', hidden: false, isDefault: !p.cursor, defaultReasoningEffort: 'high', supportedReasoningEfforts: [{ reasoningEffort: 'high', description: 'Native high effort' }, { reasoningEffort: 'low', description: 'Native low effort' }, { reasoningEffort: 'max', description: 'Native max effort' }] }], nextCursor: p.cursor ? null : 'models-page-2' }; break;
        case 'turn/start': {
          thread.model = p.collaborationMode?.settings.model ?? p.model ?? thread.model ?? 'fixture-model';
          thread.reasoningEffort = p.collaborationMode?.settings.reasoning_effort ?? p.effort ?? thread.reasoningEffort ?? 'high';
          if (p.collaborationMode) thread.collaborationMode = p.collaborationMode;
          if (p.permissions) thread.activePermissionProfile = p.permissions.id;
          if (!thread?.canAcceptDirectInput || !['idle', 'systemError'].includes(thread.status.type)) { owner.emit({ id: frame.id, error: { code: -32000, message: 'The actual native fixture refuses concurrent or unattached input.' } }); return; }
          const turn = { id: randomUUID(), status: 'inProgress', items: [{ id: randomUUID(), type: 'userMessage', content: p.input }] };
          thread.turns.push(turn); thread.status = { type: 'active', activeFlags: [] }; result = { turn }; break;
        }
        case 'turn/steer': {
          const turn = thread?.turns.at(-1);
          if (!turn || turn.status !== 'inProgress' || turn.id !== p.expectedTurnId) { owner.emit({ id: frame.id, error: { code: -32000, message: 'The fixture has no matching active turn.' } }); return; }
          turn.items.push({ id: randomUUID(), type: 'userMessage', content: p.input }); result = { turnId: turn.id }; break;
        }
        case 'fs/readFile': try { result = { dataBase64: readFileSync(p.path).toString('base64') }; } catch (error) { owner.emit({ id: frame.id, error: { code: -32603, message: error.message } }); return; } break;
        case 'turn/interrupt': result = {}; break; // Receipt alone never claims cancellation.
        case 'fs/readDirectory': try { result = { entries: readdirSync(p.path, { withFileTypes: true }).map(entry => ({ fileName: entry.name, isDirectory: entry.isDirectory(), isFile: entry.isFile() })) }; } catch (error) { owner.emit({ id: frame.id, error: { code: -32603, message: error.message } }); return; } break;
        case 'account/rateLimits/read': result = { rateLimits: { limitId: 'codex', limitName: 'Codex', primary: { usedPercent: 12, windowDurationMins: 300, resetsAt: 1788700000 }, secondary: null, credits: null, planType: null } }; break;
        default: throw new Error('Unexpected native fixture method ' + frame.method);
      }
      if (frame.method === heldMethod) {
        heldMethod = null;
        heldReply = () => owner.emit({ id: frame.id, result });
      } else owner.emit({ id: frame.id, result });
    });
    return { rpc, epoch, catalog, initialized: { codexHome: settings.nativeHome }, launcherPid: null,
      ...(manageInstructions ? { nativeHome: settings.nativeHome, connectionMode: 'owned-stdio', ownsNativeHome: false } : {}),
      close: async () => { rpc.close('fixture_stop'); input.destroy(); output.destroy(); } };
  };
  const startManager = async () => {
    const manager = await startAgentManager(configFile, launcher); managers.push(manager);
    const states = [], onState = manager.service.options.onState;
    const observation = { states, closedCode: null }; managerObservations.push(observation);
    void manager.closed.then(code => { observation.closedCode = code; });
    manager.service.options.onState = state => { states.push({ ...state, at: new Date().toISOString() }); onState?.(state); };
    try { await manager.service.waitReady({ timeoutMs: 25000 }); await atomicJson(join(f.root, 'agent-startup-observed.json'), { states, workerOperations }); }
    catch (cause) {
      await atomicJson(join(f.root, 'agent-startup-failure.json'), { states, workerOperations, health: JSON.parse(await readFile(join(config.dataRoot, 'health.json'), 'utf8')), nativeMethods: owners.at(-1)?.sent.map(frame => frame.method ?? 'answer') ?? [] });
      throw cause;
    }
    return manager;
  };
  t.after(async () => { for (const handle of managers) await handle.close(); });
  let manager = await startManager();
  const definition = JSON.parse(await readFile('ui/agent-ui/ui.json', 'utf8'));
  await publishUi(f.client, { directory: resolve('dist/apps/agent-ui'), definition, mutationId: await newOperationId(f.client, 'agent-publication-' + randomUUID()), expectedReleaseId: null });
  const url = f.base + '/ui/agent-ui/';
  const open = async hash => { await f.login(); await f.page.goto(url + (hash ?? '#/home')); };
  const current = () => owners.at(-1);
  const complete = (threadId, output = 'A saved native result.', status = 'completed') => {
    const thread = threads.get(threadId), turn = thread.turns.at(-1); turn.status = status; turn.items.push({ id: randomUUID(), type: 'agentMessage', text: output }); thread.status = { type: 'idle' }; thread.recencyAt++;
    current().emit({ method: 'turn/completed', params: { threadId, turn } });
  };
  const restart = async () => { await manager.close(); for (const thread of threads.values()) { thread.canAcceptDirectInput = false; thread.status = { type: 'notLoaded' }; } manager = await startManager(); };
  const registryChange = async change => { const registry = agentRegistry(catalog); change(registry); await manager.service.connection.request('registry.sync', registry); await manager.service.connection.request('service.heartbeat', { ready: true, diagnostics: [] }); };
  const addAgent = async (modelCatalog, hostId = config.hostId) => {
    const extra = { ...config, instanceId: 'second-agent', serviceNodeId: 'browser-second-agent', hostId,
      dataRoot: join(f.root, 'second-agent-data'), settings: { ...settings, nativeHome: join(f.root, 'second-native') } };
    const path = join(f.root, 'second-agent.json'); await atomicJson(path, extra);
    const handle = await startAgentManager(path, options => launcher(options, modelCatalog));
    managers.push(handle); await handle.service.waitReady();
    return handle;
  };
  return { ...f, url, open, nativeProjects, threads, owners, managerObservations, current, complete, restart, registryChange, addAgent, config, catalog,
    holdNativeOnce: method => { heldMethod = method; return () => { assertHeld(); heldReply(); heldReply = null; }; },
    emptyModelsOnce: () => { emptyModelPage = true; }, unsupportedItems: () => { unsupportedItems = true; }, unmaterializedHistory: () => { unmaterializedHistory = true; } };
  function assertHeld() { if (!heldReply) throw new Error('The selected native request has not been held.'); }
}
