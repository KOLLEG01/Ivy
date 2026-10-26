import assert from 'node:assert/strict';
import { parseArgs } from 'node:util';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { randomUUID, createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
import { artifactRoot, startNativeService, startTestHive } from '../../../tools/testing/native-service.mjs';

// Actual AgentManager + local App Server + AgentManager-managed Ivy MCP/skills, fresh Hive/home/project.
// Default is a no-model prerequisite probe. --auth-file enables exactly one synthetic Luna High
// CRUD turn with a three-minute deadline; source auth is read only and no external effect is requested.
const { values } = parseArgs({ options: Object.fromEntries(['distribution', 'native-config', 'evidence', 'auth-file', 'hive-artifact', 'agent-manager-artifact'].map(key => [key, { type: 'string' }])) });
for (const key of ['distribution', 'native-config', 'evidence']) assert.ok(values[key], 'Missing --' + key);
const distribution = resolve(values.distribution), root = resolve(values.evidence), load = path => import(pathToFileURL(join(distribution, 'dist', path)).href);
const { HiveClient, nativeServiceTools, serviceTools, newOperationId } = await load('packages/sdk/src/node.js');
const sourceBytes = await readFile(values['native-config']), sourceConfig = JSON.parse(sourceBytes);
const original = sourceConfig.instances.find(item => item.componentId === 'agent-manager'); assert.ok(original);
const artifact = artifactRoot('agent-manager', values['agent-manager-artifact'] ?? distribution);
const hiveArtifact = artifactRoot('hive', values['hive-artifact'] ?? distribution);
const build = JSON.parse(await readFile(join(artifact, 'dist/build-info.json'), 'utf8'));
await mkdir(root, { mode: 0o700 });
const exec = promisify(execFile);
if (values['auth-file'] && process.platform === 'win32') {
  assert.ok(process.env.SystemRoot);
  const system32 = join(process.env.SystemRoot, 'System32');
  const identity = await exec(join(system32, 'whoami.exe'), ['/user', '/fo', 'csv', '/nh'], { windowsHide: true });
  const sid = identity.stdout.match(/S-1-5-[0-9-]+/)?.[0]; assert.ok(sid);
  await exec(join(system32, 'icacls.exe'), [root, '/inheritance:r', '/grant:r', '*' + sid + ':(OI)(CI)F', '*S-1-5-18:(OI)(CI)F'], { windowsHide: true });
}
const dataRoot = join(root, 'agent'), home = join(dataRoot, 'native-home'), project = join(root, 'project');
await mkdir(home, { recursive: true }); await mkdir(project);
await writeFile(join(project, 'README.md'), '# Isolated acceptance project\nSynthetic tasks only.\n', { flag: 'wx' });
const token = randomUUID(), serviceToken = randomUUID();
const principal = 'acceptance-user', owner = 'acceptance-agent', hostId = 'isolated-agent-acceptance';
const settings = { nativeExecutable: original.settings.nativeExecutable, nativeVersion: original.settings.nativeVersion,
  nativeExecutableHash: original.settings.nativeExecutableHash, windowsShell: original.settings.windowsShell,
  nativeHome: home, appServer: { mode: 'owned-stdio' }, patcherEnabled: true,
  skillsRoot: join(root, 'skills'), projectRoot: join(root, 'projects'), internalProjectRoot: join(root, 'internal-projects'),
  limits: { maxOperations: 1000, maxJournalBytes: 256 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 } };
const report = { schemaVersion: 1, startedAt: new Date().toISOString(), build, root, hostId, nativeVersion: settings.nativeVersion,
  nativeExecutableHash: settings.nativeExecutableHash, modelTurns: 0, phase: 'starting', calls: [], cli: [] };
const save = async () => {
  const bytes = JSON.stringify(report, null, 2);
  assert.ok(!bytes.includes(token) && !bytes.includes(serviceToken), 'Credentials must not enter evidence');
  await writeFile(join(root, 'report.json'), bytes + '\n');
};
let server, manager, client, agentTools, nativeTools, threadId, authBytes;
const invoke = async (method, args) => {
  const operationId = await newOperationId(client), start = Date.now();
  try { const reply = await (method.startsWith('codex.') ? nativeTools : agentTools).call(method, args, operationId, { timeoutMs: 45000 });
    report.calls.push({ method, operationId, args, durationMs: Date.now() - start, reply }); await save(); return reply;
  } catch (error) { report.calls.push({ method, operationId, args, code: error.code ?? null, message: error.message }); await save(); throw error; }
};
try {
  server = await startTestHive({ root, artifact: hiveArtifact, credentials: [{ principalId: principal, token }, { principalId: owner, token: serviceToken }] });
  report.hiveBuild = { version: server.config.version, buildId: server.config.buildId };
  client = new HiveClient(server.config.publicBaseUrl, { credential: token });
  agentTools = serviceTools(client, owner, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]); nativeTools = nativeServiceTools(client, owner);
  manager = await startNativeService({ schemaVersion: 1, componentId: 'agent-manager', instanceId: 'agent', serviceNodeId: owner, hostId,
    dataRoot, publicBaseUrl: server.config.publicBaseUrl, credential: serviceToken, settings }, { artifact });
  const readyDeadline = Date.now() + 45000;
  for (;;) {
    const node = await client.request('serviceNodes.get', { serviceNodeId: owner });
    if (node.connected && node.synced && node.ready) break;
    assert.ok(Date.now() < readyDeadline, 'AgentManager public readiness deadline: ' + manager.logs);
    await delay(100);
  }
  report.status = await invoke('agent.status', {}); assert.equal(report.status.state, 'ready');
  const model = 'gpt-5.6-luna', effort = 'high';
  if (values['auth-file']) {
    authBytes = await readFile(values['auth-file']); const auth = JSON.parse(authBytes).tokens;
    assert.ok(auth?.access_token && auth.account_id);
    // Do not retain account credentials in acceptance reports; the owned runtime handles login.
    const login = await nativeTools.call('codex.account/login/start', { type: 'chatgptAuthTokens', accessToken: auth.access_token,
      chatgptAccountId: auth.account_id, chatgptPlanType: null }, await newOperationId(client), { timeoutMs: 45000 });
    assert.equal(login.type, 'chatgptAuthTokens');
    const models = await invoke('codex.model/list', { limit: 100 });
    assert.ok(models.data.some(item => item.model === model && !item.hidden && item.supportedReasoningEfforts.some(option => option.reasoningEffort === effort)), 'Required Luna High must be available; no fallback');
    report.budget = { maximumModelTurns: 1, model, effort, turnTimeoutMs: 180000, tokenLimit: null };
  }
  const thread = await invoke('codex.thread/start', { cwd: project, ephemeral: !values['auth-file'], permissions: ':danger-full-access', approvalPolicy: 'never',
    ...(values['auth-file'] ? { model, allowProviderModelFallback: false, config: { 'features.shell_tool': false, 'features.multi_agent': false, 'features.apps': false, web_search: 'disabled' } } : {}) });
  threadId = thread.thread.id; report.threadId = threadId;
  const status = await invoke('codex.mcpServerStatus/list', { threadId, detail: 'toolsAndAuthOnly', limit: 20 });
  const hive = status.data.find(item => item.name === 'ivy'); assert.ok(hive, 'AgentManager-managed Hive MCP must be visible');
  assert.ok(Object.hasOwn(hive.tools, 'wiki_search') && Object.hasOwn(hive.tools, 'hive_object_write'), 'Managed native context must authenticate and load direct MCP tools');
  const reply = await invoke('codex.mcpServer/tool/call', { threadId, server: hive.name, tool: 'hive_status', arguments: {} });
  assert.notEqual(reply.isError, true); assert.ok(reply.structuredContent?.result);
  assert.equal(reply.structuredContent.result.callerPrincipalId, owner, 'Managed MCP uses its explicit service principal, not an inherited user token.');
  if (values['auth-file']) {
    for (const [key, mediaType] of [['acceptance/note', 'text/plain'], ['acceptance/attachment', 'application/octet-stream']])
      await client.request('contracts.register', { mutationId: await newOperationId(client), definition: { key, version: '1.0.0', owner: { kind: 'agent' }, mediaType,
        retention: { objects: { mode: 'retain' }, revisions: { mode: 'all' } }, specMarkdown: 'Synthetic acceptance data only.' } });
    const marker = 'Grüße 世界 — ' + randomUUID(), name = 'Acceptance note ' + randomUUID();
    const original = marker + '\noriginal', revised = marker + '\nrevised', attachment = Buffer.from('Synthetic attachment\n').toString('base64');
    const prompt = `Use only the installed hive_ MCP tools. Do not use shell, files, other services or external actions.\n` +
      `Create exactly one root object named ${JSON.stringify(name)} with contract acceptance/note version 1.0.0 and text content ${JSON.stringify(original)}.\n` +
      `Read it, find it through objects.search, then update that same object using expectedRevision 1 to text ${JSON.stringify(revised)}.\n` +
      `Read its history using hive_object_read with view "history", then read the original revision. Create one child attachment named fixture.bin with contract acceptance/attachment version 1.0.0 and base64 content ${JSON.stringify(attachment)}; read its bytes back.\n` +
      `Archive only the note. Preserve operation identities on retries. Return a brief German summary with the two actual object IDs.`;
    report.scenario = { name, marker, original, revised, attachment, prompt }; await save();
    report.modelTurns = 1; await save();
    const started = await invoke('codex.turn/start', { threadId, model, effort, input: [{ type: 'text', text: prompt, text_elements: [] }] });
    const turnId = started.turn.id; report.turnId = turnId; const deadline = Date.now() + 180000;
    let final;
    while (Date.now() < deadline) {
      const read = await nativeTools.call('codex.thread/read', { threadId, includeTurns: true }, await newOperationId(client), { timeoutMs: 35000 });
      final = read.thread.turns.find(turn => turn.id === turnId);
      if (final && ['completed', 'failed', 'interrupted'].includes(final.status)) break;
      await delay(1000);
    }
    report.finalTurn = final;
    if (final?.status !== 'completed') {
      await invoke('codex.turn/interrupt', { threadId, turnId });
      assert.fail('Original synthetic turn did not complete within its deadline');
    }
    const items = final.items ?? [], actualTools = items.filter(item => item.type === 'mcpToolCall');
    report.actualMcpTools = actualTools.map(item => ({ server: item.server, tool: item.tool, status: item.status }));
    assert.ok(actualTools.length >= 7, 'Must observe real MCP calls, not a narrative claim');
    const direct = { hive_object_archive: 'hive.objects.archive', hive_status: 'hive.system.status', hive_schema_list: 'hive.contracts.list', hive_schema_get: 'hive.contracts.get', hive_object_write: 'hive.objects.write', hive_object_read: 'hive.objects.read', hive_object_search: 'hive.objects.search', hive_object_list: 'hive.objects.list' };
    const operation = item => item.tool === 'hive_object_read' && item.arguments?.view === 'history' ? 'hive.objects.history' : direct[item.tool];
    assert.ok(actualTools.every(item => item.server === hive.name && Object.hasOwn(direct, item.tool)), 'Only requested direct object operations are allowed');
    for (const tool of ['hive.objects.write', 'hive.objects.read', 'hive.objects.search', 'hive.objects.history', 'hive.objects.archive'])
      assert.ok(actualTools.some(item => operation(item) === tool), `Missing actual ${tool} call`);
    const roots = await client.request('objects.list', { parentId: null, includeArchived: true, limit: 100 });
    const matches = roots.items.filter(item => item.name === name); assert.equal(matches.length, 1, 'Exactly one original note must exist');
    const note = matches[0];
    report.verifiedNote = await client.request('objects.read', { objectId: note.id });
    assert.equal(report.verifiedNote.object.effectivelyArchived, true);
    assert.ok(report.verifiedNote.object.archivedAt);
    assert.equal(report.verifiedNote.content.value, revised);
    assert.equal(report.verifiedNote.revision.revision, 2, 'The original note must be revised exactly once');
    report.verifiedHistory = await client.request('objects.history', { objectId: note.id, limit: 100 });
    assert.deepEqual(report.verifiedHistory.items.map(item => item.revision), [2, 1]);
    const first = await client.request('objects.read', { objectId: note.id, revision: 1 }); assert.equal(first.content.value, original);
    const children = await client.request('objects.list', { parentId: note.id, includeArchived: true, limit: 100 });
    assert.equal(children.items.length, 1);
    assert.equal(children.items[0].name, 'fixture.bin');
    report.verifiedAttachment = await client.request('objects.read', { objectId: children.items[0].id });
    assert.equal(report.verifiedAttachment.content.value, attachment);
    const answer = items.filter(item => item.type === 'agentMessage' && item.phase === 'final_answer').map(item => item.text).join('\n');
    assert.ok(answer.includes(note.id) && answer.includes(children.items[0].id), 'Final answer must identify both actual saved objects');
  }
  report.phase = 'passed';
} catch (error) {
  report.phase = 'failed'; report.failure = { code: error.code ?? null, message: error.message }; process.exitCode = 1;
} finally {
  if (threadId && manager) try { await invoke('codex.thread/unsubscribe', { threadId }); } catch (error) { report.phase = 'failed'; report.cleanupFailure = error.code ?? error.message; process.exitCode = 1; }
  for (const [key, instance] of [['ownerStopFailure', manager], ['hiveStopFailure', server]]) {
    if (!instance) continue;
    try {
      assert.deepEqual(await instance.close(), { code: 0, signal: null });
      assert.equal(JSON.parse(await readFile(join(instance.config.dataRoot, 'health.json'), 'utf8')).ready, false);
    } catch (error) { report.phase = 'failed'; report[key] = error.code ?? error.message; process.exitCode = 1; }
  }
  report.sourceConfigUnchanged = createHash('sha256').update(sourceBytes).digest('hex') === createHash('sha256').update(await readFile(values['native-config'])).digest('hex');
  if (authBytes) { report.sourceAuthUnchanged = authBytes.equals(await readFile(values['auth-file'])); assert.ok(report.sourceAuthUnchanged); }
  assert.ok(report.sourceConfigUnchanged); report.finishedAt = new Date().toISOString(); await save();
  console.log(JSON.stringify({ phase: report.phase, failure: report.failure, report: join(root, 'report.json'), modelTurns: report.modelTurns }));
}
