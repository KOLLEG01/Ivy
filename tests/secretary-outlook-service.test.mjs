import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { HiveClient, ServiceClient, hashJson, IvyError, validateAgent, callBound, discover } from '../dist/packages/sdk/src/node.js';
import { atomicJson } from '../dist/packages/sdk/src/host.js';
import { rootObject, settingsFor, principal } from './secretary-fixture.mjs';
import { mail, page, profile } from './secretary-outlook-fixture.mjs';

test('Secretary ignores legacy Microsoft connector configuration and never dispatches Outlook source calls', { timeout: 90000 }, async t => {
  const base = process.env.IVY_TEST_HIVE_URL, credential = process.env.IVY_TEST_HIVE_CREDENTIAL;
  assert.ok(base && credential); assert.equal(new URL(base).hostname, '127.0.0.1');
  const client = new HiveClient(base, { credential }); assert.equal((await client.request('system.status', {})).version, 'isolated-consumer-test');
  const id = randomUUID(), serviceNodeId = 'secretary-outlook-process-' + id, directory = await mkdtemp(join(tmpdir(), 'ivy-outlook-process-'));
  const root = await rootObject(client), target = { serviceNodeId: 'outlook-provider-' + id, hostId: 'fixture', nativeVersion: '0.149.1', nativeExecutableHash: hashJson('executable'), catalogHash: hashJson('catalogue') };
  const account = { account: { type: 'chatgpt', email: 'native@example.test', planType: 'pro' }, requiresOpenaiAuth: true }, epoch = randomUUID(), operations = new Map(); let nativeCalls = 0;
  const schema = { type: 'object', additionalProperties: true }, methods = { agent: ['status', 'operation', 'invoke'], codex: ['account/read', 'thread/start', 'mcpServer/tool/call', 'thread/unsubscribe'] };
  const registry = { namespaces: Object.entries(methods).map(([namespace, names]) => ({ namespace, description: 'Isolated controlled native provider', guideMarkdown: 'Test-only provider; no external account or model.', topics: [], inventoryKinds: [],
    tools: names.map(name => ({ namespace, name, interfaceVersion: '1.0.0', description: 'Controlled original operation', inputSchema: schema, outputSchema: schema, annotations: { readOnlyHint: namespace === 'agent' && name !== 'invoke', idempotentHint: true } })) })), contracts: [], requiredContracts: [] };
  const native = new ServiceClient({ publicBaseUrl: base, credential: () => credential, identity: { serviceNodeId: target.serviceNodeId, serviceName: 'agent', hostId: target.hostId, version: 'test', buildId: hashJson(target), hiveProtocol: 1 }, registry: () => registry, heartbeatMs: 2000, reconcile: async () => {},
    handlers: Object.fromEntries(Object.entries(methods).flatMap(([namespace, names]) => names.map(name => [namespace + '.' + name, (args, context) => {
      const receipt = namespace === 'agent' && name === 'invoke';
      let nativeMethod = name;
      if (receipt) { assert.equal(args.operationId, context.operationId); nativeMethod = args.method; args = args.params; }
      const now = new Date().toISOString();
      if (namespace === 'agent' && !receipt) {
        if (name === 'status') { const value = { ...target, state: 'ready', epoch, pid: process.pid, observedAt: now, code: null, initialized: {}, pendingInputs: 0,
          operations: { retained: operations.size, maximum: 1000, bytes: 1000, maximumBytes: 1048576 }, observedMethods: [] }; validateAgent('Status', value); return value; }
        const value = operations.get(args.operationId); if (value) return value;
        throw new IvyError('not_found', 'Original operation absent.', 'not_executed', { kind: 'agent_operation_absent', serviceNodeId: target.serviceNodeId, operationId: args.operationId, epoch });
      }
      nativeCalls++; assert.equal(context.callerPrincipalId, principal); assert.ok(context.operationId);
      const old = operations.get(context.operationId); if (old) { assert.deepEqual(old.params, args); assert.equal(old.method, nativeMethod); return receipt ? old : old.reply.result; }
      let result;
      if (nativeMethod === 'account/read') result = account;
      else if (nativeMethod === 'thread/start') { assert.equal(args.ephemeral, true); assert.equal(args.permissions, ':danger-full-access'); result = { thread: { id: 'original-' + context.operationId } }; }
      else if (nativeMethod === 'thread/unsubscribe') result = { status: 'unsubscribed' };
      else {
        assert.equal(args.server, 'codex_apps'); assert.ok(args.threadId.startsWith('original-'));
        assert.ok(['microsoft_outlook_email.get_profile', 'microsoft_outlook_email.list_messages'].includes(args.tool));
        if (args.tool.endsWith('list_messages')) { assert.equal(args.arguments.folder_id, 'inbox'); assert.equal(args.arguments.skip, 0); assert.match(args.arguments.filter, / and isRead eq false$/); }
        result = { isError: false, content: [], structuredContent: args.tool.endsWith('list_messages') ? page([mail()]) : profile };
      }
      const operation = { schemaVersion: 1, operationId: context.operationId, callerPrincipalId: context.callerPrincipalId, serviceNodeId: target.serviceNodeId,
        nativeVersion: target.nativeVersion, nativeExecutableHash: target.nativeExecutableHash, method: nativeMethod, params: args, requestHash: hashJson({ method: nativeMethod, params: args }),
        phase: 'succeeded', createdAt: now, updatedAt: now, epoch, requestId: operations.size, reply: { result }, code: null };
      validateAgent('Operation', operation); operations.set(context.operationId, structuredClone(operation)); return receipt ? operation : result;
    }]))),
  });
  t.after(() => native.stop()); native.start(); await native.waitReady({ timeoutMs: 20000 });
  const settings = settingsFor(root, serviceNodeId, principal, { sources: [{ sourceId: 'mail', accountId: profile.id, kind: 'email', producerPrincipalIds: [principal], ownSenderIds: [profile.email], allowedSenderIds: null }] });
  const build = JSON.parse(await readFile('dist/build-info.json', 'utf8')), dataRoot = join(directory, 'data'), path = join(directory, 'instance.json');
  const config = { schemaVersion: 1, instanceId: id, componentId: 'secretary', serviceNodeId, hostId: 'fixture', publicBaseUrl: base, credential, dataRoot, artifactRoot: resolve('.'), version: build.version, buildId: build.buildId, settings };
  await atomicJson(path, config); await atomicJson(join(directory, 'config.json'), {schemaVersion:1,microsoft:{ schemaVersion: 1, collectors: [{ sourceId: 'mail', target, expectedAccountHash: hashJson({ type: 'chatgpt', email: account.account.email }), profile,
    since: '2026-09-01T00:00:00.000Z', pageSize: 8, maximumPages: 10, pollMs: 600000, threadCwd: directory }] }});
  let child, stopped, output = '';
  const start = () => { child = spawn(process.execPath, ['dist/services/secretary/src/main.js', '--config', path], { cwd: resolve('.'), windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, IVY_LAUNCH_ID: randomUUID() } });
    stopped = once(child, 'exit'); for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { output = (output + bytes).slice(-65536); }); };
  const stop = async () => { if (child?.exitCode === null && child?.signalCode === null) { child.kill('SIGTERM'); await stopped; } };
  t.after(async () => { await stop(); });
  const until = async (read, label) => { const end = Date.now() + 25000; while (Date.now() < end) { if (child.exitCode !== null || child.signalCode !== null) throw Error('Secretary exited before ' + label + ': ' + output); const value = await read(); if (value) return value; await new Promise(resolve => setTimeout(resolve, 50)); } throw Error('Missing ' + label + ': ' + JSON.stringify({ nativeCalls, operations: operations.size, output })); };
  const owner = () => client.request('serviceNodes.get', { serviceNodeId }).catch(error => { if (error.code === 'not_found') return null; throw error; });
  const objects = key => client.request('objects.query', { contractKey: key, limit: 10, where: { op: 'eq', field: 'object.parentId', value: root } });
  start(); await until(async () => { const value = await owner(), health = await readFile(join(dataRoot, 'health.json'), 'utf8').then(JSON.parse).catch(() => null); return value?.ready && health?.ready && health.pid === child.pid ? health : null; }, 'initial readiness');
  const initialStatus = await callBound(client, await discover(client, 'secretary.status', { serviceNodeId }), { expectedScope: settings.identity.scope });
  assert.equal(nativeCalls, 0); assert.equal(operations.size, 0); assert.equal((await objects('secretary/item')).items.length, 0);
  assert.equal(initialStatus.sources[0].data.cursor, null); assert.deepEqual(initialStatus.recoveryIssues, []);
  await stop(); await native.stop(); start();
  await until(async () => { const value = await owner(), health = await readFile(join(dataRoot, 'health.json'), 'utf8').then(JSON.parse).catch(() => null); return value?.ready && health?.ready && health.pid === child.pid ? health : null; }, 'offline-provider restarted readiness');
  // The separate source test verifies offline proof replay; this checks restarted process health.
  const healthPath = join(dataRoot, 'health.json'); await until(async () => { const health = JSON.parse(await readFile(healthPath, 'utf8')); return health.ready && health.pid === child.pid; }, 'new process health');
  const status = await callBound(client, await discover(client, 'secretary.status', { serviceNodeId }), { expectedScope: settings.identity.scope });
  assert.deepEqual(status.sources, initialStatus.sources); assert.deepEqual(status.recoveryIssues, []);
  assert.equal((await objects('secretary/item')).items.length, 0); assert.equal(nativeCalls, 0);
  t.diagnostic(JSON.stringify({ directory, buildId: build.buildId, operations: operations.size, legacyMicrosoftConnector: 'disabled', actualMicrosoft: false, installedRuntimeOwner: false }));
});
