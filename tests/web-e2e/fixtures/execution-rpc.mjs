// Inert transport for the real TaskBoard SFCs and shared approval components. No provider or external service is started.
import { hashJson } from '../../../packages/ui-client/src/hash.ts';
export const backends = [
  { backendId: 'primary', provider: 'codex', accountId: 'work' },
  { backendId: 'secondary', provider: 'claude', accountId: 'personal' },
  { backendId: 'other', provider: 'codex', accountId: 'personal' },
];
export const hostId = 'fixture-host', serviceNodeId = hostId + '.agent-manager', taskNode = 'fixture-task-board';
const hash = 'sha256:' + '1'.repeat(64), time = '2026-01-01T00:00:00.000Z';
const staleActivity = new URLSearchParams(location.search).get('mode') === 'agents-freshness';
const retiredInventory = new URLSearchParams(location.search).get('mode') === 'agents-origins';
export const workspace = { serviceNodeId: taskNode, generation: 1, principalId: 'fixture-user', callerPrincipalId: 'fixture-user',
  rootObjectId: null, role: 'user', recovered: true, observedAt: time, scheduler: { enabled: false, intervalMs: 1000, pageSize: 10 }, phoneTarget: null };
const nativeOptions = { backend: backends[0], model: 'codex-only', reasoningEffort: 'high', serviceTier: 'fast' };
export const pendingInput = { identity: { serviceNodeId, backend: backends[1], epoch: 'secondary-epoch', requestId: 7 },
  method: 'item/commandExecution/requestApproval', params: { availableDecisions: ['accept', 'decline'], command: 'Inert command fixture' },
  observedAt: time, updatedAt: time, threadId: 'same-thread', turnId: 'same-turn', state: 'pending', answerOperationId: null, answerCallerPrincipalId: null, reply: null, code: null };
export const state = { calls: [], actions: [], directories: [], selectedDirectory: '', heldDirectory: null, wrongInput: false,
  threadNames: {}, threadTiers: {}, threadRunning: staleActivity ? { primary: true } : {}, sequence: {}, defaultWrites: [], defaultCommits: [], accountDefaults: null, defaultsRevision: 0,
  legacyDefaults: null, loseDefaultReplyOnce: false, historyEpochs: {}, historyReads: [],
  configuration: { schemaVersion: 1, defaults: { executionRequirement: { kind: 'host', hostId }, nativeOptions } } };
const fields = { title: 'Scoped task', description: '', acceptanceCriteria: [], category: null, control: 'agent', priority: 2,
  executionRequirement: { kind: 'host', hostId }, nativeOptions: { backend: backends[1], model: null, reasoningEffort: null, serviceTier: null },
  workspaceRequirement: { kind: 'existing_project', projectId: 'same-project', path: '/personal', useWorktree: false },
  userContact: 'ticket', allowParallel: false, dueAt: null };
export const task = { schemaVersion: 1, taskKey: 'TASK-0001', fields, workflowState: 'backlog', wakeAt: null, runtimeWait: null,
  claim: null, publication: null, attemptCount: 0, lastRun: null, primaryResourceRef: null, latestResult: null, latestReview: null,
  comments: [], attachments: [], dependencies: [], project: null, createdAt: time, updatedAt: time, nextCommentSequence: 1 };
const backendStatus = backend => ({ serviceNodeId, hostId, backend, nativeVersion: '0.163.0', nativeExecutableHash: hash, catalogHash: hash,
  epoch: state.historyEpochs[backend.backendId] ?? backend.backendId + '-epoch', state: 'ready', pid: null, observedAt: time, code: null, initialized: { codexHome: '/account/' + backend.accountId }, pendingInputs: 0,
  operations: { retained: 0, maximum: 100, bytes: 0, maximumBytes: 1048576 }, observedMethods: [] });
const project = backend => ({ nativeId: 'same-project', source: 'native', name: backend.provider === 'claude' ? 'Personal project' : 'Work project',
  paths: [backend.provider === 'claude' ? '/personal' : '/work'] });
const models = backend => [backend === backends[0] ? 'codex-only' : 'same-model', ...(backend === backends[0] ? ['same-model'] : []), 'alternate-model'].map(model => ({
  id: 'id-' + model, model, displayName: model === 'same-model' ? 'Shared model' : model, description: '', isDefault: model === 'same-model',
  supportedReasoningEfforts: (backend.provider === 'claude' ? ['medium', 'max'] : backend === backends[0] ? ['low', 'high'] : ['minimal']).map(reasoningEffort => ({ reasoningEffort })),
  defaultReasoningEffort: backend.provider === 'claude' ? 'medium' : 'low',
  serviceTiers: backend.provider === 'codex' ? [{ id: 'fast' }, { id: 'flex' }] : [],
}));
const binding = (qualifiedName, owner = serviceNodeId) => {
  const separator = qualifiedName.indexOf('.'), namespace = qualifiedName.slice(0, separator), name = qualifiedName.slice(separator + 1);
  const native = backends.find(backend => backend.backendId === namespace);
  return { qualifiedName, serviceNodeId: owner, definitionHash: hash, definition: { namespace, name, interfaceVersion: native ? '0.163.0' : '1.0.0', description: 'Inert browser fixture.',
    annotations: { readOnlyHint: native ? !['thread/start', 'thread/resume', 'turn/start', 'turn/steer', 'turn/interrupt', 'thread/name/set', 'thread/archive', 'thread/unarchive', 'thread/delete', 'thread/metadata/update', 'thread/goal/set', 'thread/goal/clear'].includes(name) : !['answer', 'invoke', 'stageFile', 'resolveProject'].includes(name) },
    inputSchema: { type: 'object', properties: { serviceTier: native?.provider === 'claude' ? { enum: ['flex', null] } : { type: ['string', 'null'] },
      cwd: true, projectId: true, model: true, config: true, permissions: true, threadId: true, effort: true, name: true,
      ...(name === 'thread/list' ? { archived: true, limit: true, cursor: true, sourceKinds: { type: 'array', items: { enum: ['cli', 'vscode', 'exec', 'appServer', 'subAgent', 'subAgentReview', 'subAgentCompact', 'subAgentThreadSpawn', 'subAgentOther', 'unknown'] } },
        modelProviders: true, sortKey: true, sortDirection: true, useStateDbOnly: true } : {}),
      input: { type: 'array', items: { type: 'object', properties: { type: { enum: ['text', 'localImage'] }, text: true, path: true } } } } }, outputSchema: true,
    ...(native ? { nativeMethod: name, nativeSchemaIdentity: hash } : {}) } };
};
const agentMode = () => new URLSearchParams(location.search).get('mode')?.startsWith('agents');
const thread = backend => ({ id: 'same-thread', nativeId: 'same-thread', name: state.threadNames[backend.backendId] ?? `${backend.provider} ${backend.accountId} task`,
  preview: '', cwd: project(backend).paths[0], projectId: 'same-project', model: 'same-model', serviceTier: state.threadTiers[backend.backendId] ?? null, status: { type: state.threadRunning[backend.backendId] ? 'active' : 'idle' }, canAcceptDirectInput: true, updatedAt: 1 });
const defaultReceipts = new Map();
function objectRead(key, value, version) {
  return { object: { id: key, contractKey: 'task-board/' + key, parentId: null }, revision: { revision: 1, contractVersion: version, mediaType: 'application/json' },
    content: { encoding: 'json', value } };
}
export const fixtureClient = { async request(method, params, options = {}) {
  options.signal?.throwIfAborted(); params = JSON.parse(JSON.stringify(params)); state.calls.push({ method, params });
  if (method === 'serviceNodes.list') return { items: [{ serviceNodeId, hostId, serviceName: 'agent-manager', connected: true, synced: true, ready: true, desiredEnabled: true },
    ...(agentMode() ? [{ serviceNodeId: hostId + '.offline-manager', hostId, serviceName: 'agent-manager', nativeVersion: retiredInventory ? '0.163.0' : null, connected: false, synced: false, ready: false, desiredEnabled: true }] : [])], nextCursor: null };
  if (method === 'serviceNodes.get') return { serviceName: params.serviceNodeId === taskNode ? 'task-board' : 'agent-manager', connected: true, synced: true, ready: true, desiredEnabled: true };
  if (method === 'system.status') return { ready: true, callerPrincipalId: 'fixture-user', runtimeEpoch: '11111111-1111-4111-8111-111111111111' };
  if (method === 'uis.catalog') return { items: [], nextCursor: null };
  if (method === 'tools.list') return { provider: { node: { serviceNodeId: params.serviceNodeId } }, items: [binding(params.namespace + '.' + params.namePrefix, params.serviceNodeId)], nextCursor: null };
  if (method === 'discovery.describe') return { provider: { serviceNodeId: params.serviceNodeId }, items: params.tools.map(name => binding(name, params.serviceNodeId)) };
  if (method === 'objects.query') return { items: [], nextCursor: null };
  if (method === 'objects.stat') {
    if (params.path === '/ivy-agent-account-execution-defaults' && state.accountDefaults) return { id: 'account-defaults', currentRevision: state.defaultsRevision, effectivelyArchived: false };
    if (params.path === '/ivy-agent-execution-defaults' && state.legacyDefaults) return { id: 'legacy-defaults', currentRevision: 3, effectivelyArchived: false };
    throw Object.assign(new Error('No saved override'), { code: 'not_found' });
  }
  if (method === 'objects.read' && ['account-defaults', 'legacy-defaults'].includes(params.objectId)) return {
    object: { id: params.objectId, contractKey: params.objectId === 'account-defaults' ? 'agent/account-execution-defaults' : 'agent/execution-defaults' },
    revision: { contractVersion: '1.0.0', revision: params.revision }, content: { encoding: 'json', value: structuredClone(params.objectId === 'account-defaults' ? state.accountDefaults : state.legacyDefaults) } };
  if (method === 'objects.write' && (params.objectId === 'account-defaults' || params.create?.contractKey === 'agent/account-execution-defaults')) {
    state.defaultWrites.push(params);
    if (defaultReceipts.has(params.mutationId)) { const original = defaultReceipts.get(params.mutationId); if (JSON.stringify(original.params) !== JSON.stringify(params)) throw Error('Changed original default write'); return original.result; }
    if (params.objectId && params.expectedRevision !== state.defaultsRevision) throw Object.assign(new Error('Defaults changed; your draft is retained. Review and save again.'), { code: 'revision_conflict' });
    if (params.create && state.accountDefaults) throw Object.assign(new Error('Defaults already exist'), { code: 'name_conflict' });
    state.accountDefaults = structuredClone(params.content.value); state.defaultsRevision++;
    const result = { objectId: 'account-defaults', revision: state.defaultsRevision };
    state.defaultCommits.push(params); defaultReceipts.set(params.mutationId, { params, result });
    if (state.loseDefaultReplyOnce) { state.loseDefaultReplyOnce = false; throw Error('Lost default write response'); }
    return result;
  }
  if (method === 'objects.read') return params.objectId === 'run' ? objectRead('run', { target: { hostId, serviceNodeId, backend: backends[1] },
    primaryResourceRef: { serviceNodeId, namespace: backends[1].backendId, kind: 'thread', nativeId: new URLSearchParams(location.search).get('mode') === 'linked-wrong' ? 'foreign' : 'retained' },
    plan: { turnStart: { model: 'same-model', effort: 'max', serviceTier: 'flex' } } }, '3.1.0') : objectRead('task', task, '3.3.0');
  if (method === 'inventory.get') return { summary: thread(backends.find(backend => backend.backendId === params.resourceRef.namespace)) };
  if (method === 'inventory.list') return { items: [...backends.map(backend => ({ resourceRef: { serviceNodeId, namespace: backend.backendId, kind: params.kind ?? 'project', nativeId: params.kind === 'thread' ? params.cursor ? 'older-thread' : 'same-thread' : 'same-project' },
    summary: params.kind === 'thread' ? { ...thread(backend), ...(params.cursor ? { name: 'Older ' + backend.provider + ' task' } : {}) } : project(backend),
    schemaVersion: '1.0.0', observedAt: time, stale: params.kind === 'thread' && staleActivity, snapshotRevision: 1 })),
    ...(retiredInventory ? [{ resourceRef: { serviceNodeId: hostId + '.offline-manager', namespace: 'codex', kind: params.kind ?? 'project', nativeId: params.kind === 'thread' ? 'same-thread' : 'same-project' },
      summary: params.kind === 'thread' ? thread(backends[1]) : project(backends[1]), schemaVersion: '1.0.0', observedAt: time, stale: true, snapshotRevision: 1 }] : [])],
    nextCursor: agentMode() && params.kind === 'thread' && !params.cursor ? 'next-tasks' : null };
  if (method !== 'tools.call') throw Error('Unexpected fixture RPC: ' + method);
  const name = params.qualifiedName, args = params.arguments;
  if (name === 'task-board.workspace') return workspace;
  if (name === 'task-board.configuration') return { configuration: structuredClone(state.configuration), object: { objectId: 'configuration', revision: 1 } };
  if (name === 'task-board.categories') return { categories: [] };
  if (name === 'task-board.read') return { task, object: { objectId: 'task', revision: 1 }, archived: false, executionCondition: { state: 'idle', detail: '' } };
  if (name === 'task-board.create' || name === 'task-board.configure' || name === 'task-board.edit') {
    state.actions.push(structuredClone(args));
    if (name === 'task-board.configure') state.configuration = structuredClone(args.value);
    return { operationId: params.operationId, ...(name === 'task-board.configure' ? { configuration: { objectId: 'configuration', revision: 2 } } : { task: { objectId: 'saved-task', revision: 2 } }) };
  }
  if (name === 'agent.backends') return { backends: backends.map(backendStatus) };
  if (name === 'agent.environmentDefaults') return { instructions: { schemaVersion: 1, hostId: null, enabled: true, includeLocal: true, text: '' },
    mcp: { schemaVersion: 1, hostId: null, enabled: true, servers: [] }, skills: { schemaVersion: 1, hostId: null, enabled: true, files: [] } };
  const backend = name.startsWith('agent.') ? backends.find(value => value.backendId === args.backend?.backendId && value.provider === args.backend?.provider && value.accountId === args.backend?.accountId) : backends.find(value => name.startsWith(value.backendId + '.'));
  if (!backend) throw Error('Missing or wrong account selector: ' + name);
  if (name === 'agent.status') return backendStatus(backend);
  if (name === 'agent.storageIdentity') return { serviceNodeId, backend, provider: backend.provider,
    originalServiceNodeId: backend.provider === 'claude' ? hostId + '.offline-manager' : serviceNodeId };
  if (name === 'agent.inputDefinition') return { backend, identity: pendingInput.identity, method: pendingInput.method, nativeVersion: '0.163.0', catalogHash: hash,
    responseSchema: { type: 'object', properties: { decision: { enum: ['accept', 'decline'] } }, required: ['decision'] } };
  if (name === 'agent.inputs') return { backend, epoch: backend.backendId + '-epoch', truncated: false, items: agentMode() ? [] : [new URLSearchParams(location.search).get('mode') === 'input-history' ?
    { ...pendingInput, identity: { serviceNodeId: 'original-owner', epoch: 'secondary-epoch', requestId: 7 } } : args.identity && state.wrongInput ?
      { ...pendingInput, identity: { ...pendingInput.identity, backend: backends[0] } } : pendingInput] };
  if (name === 'agent.answer') {
    state.actions.push(args);
    const original = { identity: args.identity, reply: args.reply };
    pendingInput.state = 'answered';
    return { schemaVersion: 1, backend, serviceNodeId, nativeVersion: '0.163.0', nativeExecutableHash: hash, callerPrincipalId: 'fixture-user',
      operationId: params.operationId, method: 'agent.answer', params: original, requestHash: await hashJson({ method: 'agent.answer', params: original }),
      phase: 'succeeded', createdAt: time, updatedAt: time, epoch: 'secondary-epoch', requestId: 7, reply: { result: null }, code: null };
  }
  if (name === 'agent.capabilities') return { backend, serviceNodeId, hostId, revision: 1, capabilities: [], updatedAt: time, operationId: null };
  if (name === 'agent.projects') return { backend, source: 'native', observedAt: time, projects: [project(backend)], defaults: { projectRoot: project(backend).paths[0], internalProjectRoot: '/internal' } };
  if (name === 'agent.listDirectories') {
    state.directories.push(structuredClone(args));
    const value = { backend, path: project(backend).paths[0], parent: '/', directories: [] };
    if (backend === backends[0] && new URLSearchParams(location.search).get('mode') === 'directory') return new Promise(resolve => { state.heldDirectory = () => resolve(value); });
    return value;
  }
  if (name.endsWith('.model/list')) return { data: models(backend), nextCursor: null };
  if (name === 'agent.resolveProject') return { backend, kind: args.selection.kind, cwd: args.selection.cwd ?? project(backend).paths[0], project: args.selection.kind === 'projectless' ? null : project(backend) };
  if (name === 'agent.notifications') return { backend, epoch: backend.backendId + '-epoch', firstAvailableSequence: 1, throughSequence: state.sequence[backend.backendId] ?? 0, items: [], hasMore: false, gap: false };
  if (name === 'agent.stageFile') { state.actions.push(args); return { backend, path: project(backend).paths[0] + '/' + args.name, name: args.name, byteLength: atob(args.dataBase64).length, contentHash: hash }; }
  if (name === 'agent.invoke') {
    state.actions.push(args);
    let result = null;
    if (args.method === 'thread/start' || args.method === 'thread/resume') { if ('serviceTier' in args.params) state.threadTiers[backend.backendId] = args.params.serviceTier; result = { thread: thread(backend) }; }
    else if (args.method === 'turn/start') { if ('serviceTier' in args.params) state.threadTiers[backend.backendId] = args.params.serviceTier; result = { turn: { id: 'same-turn', status: 'completed' } }; }
    else if (args.method === 'thread/name/set') state.threadNames[backend.backendId] = args.params.name;
    return { schemaVersion: 1, backend, serviceNodeId, nativeVersion: '0.163.0', nativeExecutableHash: hash, callerPrincipalId: 'fixture-user', operationId: params.operationId,
      method: args.method, params: args.params, requestHash: await hashJson({ method: args.method, params: args.params }), phase: 'succeeded', createdAt: time, updatedAt: time,
      epoch: backend.backendId + '-epoch', requestId: state.actions.length, reply: { result }, code: null };
  }
  if (name.endsWith('.thread/read')) return { thread: thread(backend) };
  if (name.endsWith('.thread/list')) {
    state.historyReads.push({ backend, args });
    const prefix = backend.backendId + ':' + !!args.archived + ':', total = args.archived ? 51 : 5101;
    if (args.limit !== 50 || args.useStateDbOnly !== true || args.cursor && !args.cursor.startsWith(prefix)) throw Error('Wrong native history request');
    const offset = args.cursor ? Number(args.cursor.slice(prefix.length)) : 0;
    return { data: Array.from({ length: Math.min(args.limit, total - offset) }, (_, i) => ({
      id: 'history-' + (offset + i), name: `${args.archived ? 'Archived' : 'Current'} ${backend.provider} ${backend.accountId} task ${offset + i}`,
      preview: '', cwd: project(backend).paths[0], ephemeral: false,
    })), nextCursor: offset + args.limit < total ? prefix + (offset + args.limit) : null };
  }
  if (name.endsWith('.thread/goal/get')) return { goal: null };
  if (name.endsWith('.thread/turns/list')) return { data: [], nextCursor: null };
  if (name.endsWith('.thread/items/list')) return { data: [{ turnId: 'same-turn', item: { id: 'same-answer', type: 'agentMessage', text: `Saved answer from ${backend.provider} ${backend.accountId}\n\n![Owned image](shot.png)` } }], nextCursor: null };
  if (name.endsWith('.config/read')) return { config: { model: 'same-model', model_reasoning_effort: backend.provider === 'claude' ? 'medium' : 'low', default_permissions: ':workspace' } };
  if (name.endsWith('.permissionProfile/list')) return { data: [{ id: ':workspace', description: 'Workspace', allowed: true }], nextCursor: null };
  if (name.endsWith('.collaborationMode/list')) return { data: [] };
  if (name.endsWith('.fs/readFile')) return { dataBase64: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jwXcAAAAASUVORK5CYII=' };
  if (name.endsWith('.account/rateLimits/read')) return { rateLimits: { limitId: backend.backendId, limitName: backend.accountId, primary: { usedPercent: 25, windowDurationMins: 60 } } };
  throw Error('Unexpected fixture tool: ' + name);
} };
const subscriptions = new Set();
export const fixtureNotifications = { connected: true, subscribe: (filter, callback) => { const entry = { filter, callback }; subscriptions.add(entry); return () => subscriptions.delete(entry); },
  subscribeChanges: () => () => {}, onChange: () => () => {}, onStatus: callback => { callback(true); return () => {}; }, reconnectNow() {} };
state.notify = (backend, method, params) => {
  const sequence = state.sequence[backend.backendId] = (state.sequence[backend.backendId] ?? 0) + 1;
  for (const { filter, callback } of subscriptions) if (filter.namespace === 'agent' && filter.name === 'notification') callback({ params: { serviceNodeId, payload: {
    backend, epoch: backend.backendId + '-epoch', sequence, observedAt: time, method, params,
  } } });
};
window.executionFixture = state;
