import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { HiveClient, newOperationId } from '../dist/packages/sdk/src/node.js';
import { artifactRoot, writeJson } from '../tools/testing/native-service.mjs';
import { syntheticNative } from './fixtures/synthetic-native.mjs';
import { catalogArtifact } from './fixtures/native-artifact.mjs';

// Real native Hive/Agent ownership and public MCP transport. Only the external
// native protocol/account/model is scripted; this does not qualify a live model.
for (const withAuth of [false, true]) test('isolated Agent MCP CLI ' + (withAuth ? 'verifies one synthetic CRUD turn' : 'probes without a model turn'), { timeout: 240000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-agent-mcp-acceptance-'));
  const evidence = join(root, 'evidence'), home = join(evidence, 'agent/native-home');
  const requests = [], errors = [], threads = new Map();
  let peer, child, outcome, mcp, epochClient, output = '', closed = Promise.resolve();
  t.after(async () => {
    if (child?.pid && !outcome) {
      // Stop the exact owned instances before terminating a stuck CLI.
      for (const instance of ['agent', 'hive']) {
        try {
          const config = JSON.parse(await readFile(join(evidence, instance + '.json'), 'utf8'));
          const health = JSON.parse(await readFile(join(config.dataRoot, 'health.json'), 'utf8'));
          await writeJson(join(config.dataRoot, 'control.json'), { schemaVersion: 1, instanceId: config.instanceId,
            launchId: health.launchId, bootId: health.bootId, action: 'shutdown', requestedAt: new Date().toISOString() });
        } catch {}
      }
      await Promise.race([closed, delay(2000)]);
      if (!outcome) {
        if (process.platform === 'win32') spawnSync('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore', timeout: 15000 });
        else process.kill(-child.pid, 'SIGKILL');
      }
    }
    await closed;
    try { await mcp?.close(); } finally { await peer?.close(); await rm(root, { recursive: true, force: true, maxRetries: 3 }); }
  });
  const managedMcp = async () => {
    if (mcp) return mcp;
    const text = await readFile(join(home, 'config.toml'), 'utf8');
    const section = text.match(/\[mcp_servers\."ivy"\]([\s\S]*?)(?=\n\[|$)/)?.[1];
    assert.ok(section, 'The owned native home must contain the managed Hive MCP block.');
    const url = JSON.parse(section.match(/^url = (".*")$/m)?.[1]);
    const authorization = JSON.parse(section.match(/^http_headers = \{ Authorization = (".*") \}$/m)?.[1]);
    assert.equal(new URL(url).hostname, '127.0.0.1');
    const config = JSON.parse(await readFile(join(evidence, 'agent.json'), 'utf8'));
    const hive = JSON.parse(await readFile(join(evidence, 'hive.json'), 'utf8'));
    assert.ok(authorization === 'Bearer ' + config.credential && config.credential !== hive.settings.credentials[0].token,
      'Managed MCP must use its service credential rather than the CLI user credential.');
    epochClient = new HiveClient(config.publicBaseUrl, { credential: config.credential });
    mcp = new Client({ name: 'isolated-native-peer', version: '1.0.0' });
    await mcp.connect(new StreamableHTTPClientTransport(new URL(url), { requestInit: { headers: { Authorization: authorization } } }));
    return mcp;
  };
  const runCrud = async (thread, turn) => {
    const scenario = JSON.parse(await readFile(join(evidence, 'report.json'), 'utf8')).scenario;
    const call = async (tool, args) => {
      const result = await (await managedMcp()).callTool({ name: tool, arguments: args });
      assert.notEqual(result.isError, true, tool + ': ' + JSON.stringify(result));
      turn.items.push({ id: randomUUID(), type: 'mcpToolCall', server: 'ivy', tool, arguments: args,
        status: 'completed', durationMs: 0, error: null, result: { content: result.content, structuredContent: result.structuredContent } });
      return result.structuredContent.result;
    };
    const created = await call('hive_object_write', { mutationId: await newOperationId(epochClient), contractVersion: '1.0.0', references: {},
      create: { contractKey: 'acceptance/note', parentId: null, ownerObjectId: null, name: scenario.name }, content: { encoding: 'text', value: scenario.original } });
    const id = created.object.id;
    assert.equal((await call('hive_object_read', { objectId: id })).content.value, scenario.original);
    const found = await call('hive_object_search', { text: scenario.marker, limit: 100 });
    assert.ok(found.items.some(item => item.id === id || item.objectId === id));
    await call('hive_object_write', { mutationId: await newOperationId(epochClient), objectId: id, expectedRevision: 1, contractVersion: '1.0.0', references: {}, content: { encoding: 'text', value: scenario.revised } });
    assert.deepEqual((await call('hive_object_read', { view: 'history', objectId: id, limit: 100 })).items.map(item => item.revision), [2, 1]);
    assert.equal((await call('hive_object_read', { objectId: id, revision: 1 })).content.value, scenario.original);
    const attachment = await call('hive_object_write', { mutationId: await newOperationId(epochClient), contractVersion: '1.0.0', references: {},
      create: { contractKey: 'acceptance/attachment', parentId: id, ownerObjectId: null, name: 'fixture.bin' }, content: { encoding: 'base64', value: scenario.attachment } });
    assert.equal((await call('hive_object_read', { objectId: attachment.object.id })).content.value, scenario.attachment);
    await call('hive_object_archive', { mutationId: await newOperationId(epochClient), objectId: id, archived: true, expectedRevision: 2 });
    turn.items.push({ id: randomUUID(), type: 'agentMessage', phase: 'final_answer', text: 'Gespeichert: ' + id + ' und ' + attachment.object.id });
    turn.status = 'completed'; thread.status = { type: 'idle' };
  };
  const version = '0.154.0';
  peer = await syntheticNative(join(root, 'peer'), version, emit => frame => {
    requests.push(frame);
    if (!frame.method || frame.method === 'initialized') return;
    const respond = async () => {
      const p = frame.params ?? {}; let result;
      switch (frame.method) {
        case 'initialize': result = { codexHome: home, userAgent: 'codex/' + version }; break;
        case 'project/list': result = { data: [], nextCursor: null }; break;
        case 'thread/loaded/list': result = { data: [...threads.values()].filter(thread => thread.canAcceptDirectInput).map(thread => thread.id), nextCursor: null }; break;
        case 'thread/list': result = { data: [...threads.values()].filter(thread => !thread.ephemeral && thread.archived === !!p.archived).map(thread => ({ ...thread, turns: [] })), nextCursor: null }; break;
        case 'thread/start': {
          const thread = { id: randomUUID(), cliVersion: version, cwd: p.cwd, ephemeral: !!p.ephemeral, archived: false,
            createdAt: 1788690000, updatedAt: 1788690000, recencyAt: 1788690000000, projectId: null, sessionId: randomUUID(),
            modelProvider: 'openai', preview: 'Isolated MCP acceptance', source: 'appServer', status: { type: 'idle' }, turns: [], canAcceptDirectInput: true };
          threads.set(thread.id, thread);
          result = { thread, cwd: p.cwd, model: p.model ?? 'fixture-model', modelProvider: 'openai', reasoningEffort: 'high',
            approvalPolicy: p.approvalPolicy, approvalsReviewer: 'user', sandbox: { type: 'dangerFullAccess' } }; break;
        }
        case 'mcpServerStatus/list': {
          const tools = []; let cursor;
          do { const page = await (await managedMcp()).listTools(cursor ? { cursor } : {}); tools.push(...page.tools); cursor = page.nextCursor; } while (cursor);
          result = { data: [{ name: 'ivy', authStatus: 'bearerToken', tools: Object.fromEntries(tools.map(tool => [tool.name, tool])), resources: [], resourceTemplates: [] }], nextCursor: null }; break;
        }
        case 'mcpServer/tool/call': result = await (await managedMcp()).callTool({ name: p.tool, arguments: p.arguments }); break;
        case 'account/login/start': assert.equal(p.accessToken, 'synthetic-no-provider-token'); result = { type: 'chatgptAuthTokens' }; break;
        case 'model/list': result = { data: [{ id: 'synthetic-luna', model: 'gpt-5.6-luna', displayName: 'Synthetic model protocol', description: 'No model execution', hidden: false,
          isDefault: true, defaultReasoningEffort: 'high', supportedReasoningEfforts: [{ reasoningEffort: 'high', description: 'Synthetic high effort' }] }], nextCursor: null }; break;
        case 'turn/start': {
          assert.equal(withAuth, true); assert.equal(p.model, 'gpt-5.6-luna'); assert.equal(p.effort, 'high');
          const thread = threads.get(p.threadId), turn = { id: randomUUID(), status: 'inProgress', items: [{ id: randomUUID(), type: 'userMessage', content: p.input }], error: null };
          thread.turns.push(turn); thread.status = { type: 'active', activeFlags: [] }; result = { turn };
          emit({ id: frame.id, result });
          try { await runCrud(thread, turn); } catch (error) { errors.push(error); turn.status = 'failed'; turn.error = { message: error.message }; }
          emit({ method: 'turn/completed', params: { threadId: thread.id, turn } }); return;
        }
        case 'thread/read': { const thread = threads.get(p.threadId); result = { thread: { ...thread, turns: p.includeTurns ? thread.turns : [] } }; break; }
        case 'turn/interrupt': { const turn = threads.get(p.threadId).turns.find(turn => turn.id === p.turnId); if (turn.status === 'inProgress') turn.status = 'interrupted'; result = {}; break; }
        case 'thread/unsubscribe': threads.get(p.threadId).canAcceptDirectInput = false; result = { status: 'unsubscribed' }; break;
        default: assert.fail('Unexpected external native method: ' + frame.method);
      }
      emit({ id: frame.id, result });
    };
    void respond().catch(error => { errors.push(error); emit({ id: frame.id, error: { code: -32603, message: error.message } }); });
  });
  const catalog = JSON.parse(await readFile('specs/native/codex-' + version + '/catalog.json', 'utf8'));
  catalog.nativeExecutableHash = peer.settings.nativeExecutableHash;
  const artifact = await catalogArtifact('agent-manager', root, catalog), sourceConfig = join(root, 'source.json'), authFile = join(root, 'auth.json');
  await writeJson(sourceConfig, { instances: [{ componentId: 'agent-manager', settings: peer.settings }] });
  await writeJson(authFile, { tokens: { access_token: 'synthetic-no-provider-token', account_id: 'synthetic-account' } });
  const configBytes = await readFile(sourceConfig), authBytes = await readFile(authFile);
  const cli = fileURLToPath(new URL('./acceptance/scenarios/isolated-agent-mcp.mjs', import.meta.url));
  child = spawn(process.execPath, [cli, '--distribution', resolve('.'), '--native-config', sourceConfig, '--evidence', evidence,
    '--hive-artifact', artifactRoot('hive'), '--agent-manager-artifact', artifact, ...(withAuth ? ['--auth-file', authFile] : [])],
  { windowsHide: true, detached: process.platform !== 'win32', stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, ...peer.env } });
  for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { output = (output + bytes).slice(-64000); });
  closed = new Promise(resolve => { child.once('error', error => resolve({ error })); child.once('close', (code, signal) => resolve({ code, signal })); })
    .then(value => { outcome = value; return value; });
  const deadline = Date.now() + 210000;
  while (!outcome && Date.now() < deadline) await delay(100);
  assert.ok(outcome, 'CLI deadline: ' + output);
  assert.deepEqual(outcome, { code: 0, signal: null }, output + '\n' + errors.map(error => error.stack).join('\n'));
  assert.equal(errors.length, 0);
  const reportText = await readFile(join(evidence, 'report.json'), 'utf8'), report = JSON.parse(reportText);
  assert.equal(report.phase, 'passed'); assert.equal(report.modelTurns, withAuth ? 1 : 0);
  assert.equal(requests.filter(frame => frame.method === 'turn/start').length, withAuth ? 1 : 0);
  assert.equal(requests.filter(frame => frame.method === 'account/login/start').length, withAuth ? 1 : 0);
  assert.equal(requests.filter(frame => frame.method === 'thread/unsubscribe').length, 1);
  assert.equal(report.sourceConfigUnchanged, true); assert.deepEqual(await readFile(sourceConfig), configBytes); assert.deepEqual(await readFile(authFile), authBytes);
  assert.equal(reportText.includes('synthetic-no-provider-token'), false);
  for (const component of ['agent', 'hive']) assert.equal(JSON.parse(await readFile(join(evidence, component === 'agent' ? 'agent/health.json' : 'hive-data/health.json'), 'utf8')).ready, false);
  if (withAuth) { assert.equal(report.sourceAuthUnchanged, true); assert.ok(report.actualMcpTools.length >= 7); assert.equal(report.verifiedNote.revision.revision, 2); }
});
