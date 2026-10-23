import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { cp, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { isAbsolute, join, relative, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

// Explicit built artifacts and a synthetic definition only. No account, pairing,
// message dispatch, native model execution or configured installation is used.
test('prepared native Chat registers with Hive, serves public history and restarts without Node', { timeout: 180000 }, async () => {
  for (const name of ['IVY_CHAT_PACKAGE_ARTIFACT', 'IVY_HIVE_REFERENCE_ARTIFACT', 'IVY_CHAT_PACKAGE_DEFINITION'])
    assert.ok(process.env[name] && isAbsolute(process.env[name]), `Supply ${name}.`);
  const source = await realpath(resolve(process.env.IVY_CHAT_PACKAGE_ARTIFACT));
  const hiveArtifact = await realpath(resolve(process.env.IVY_HIVE_REFERENCE_ARTIFACT));
  const definition = JSON.parse(await readFile(process.env.IVY_CHAT_PACKAGE_DEFINITION, 'utf8'));
  assert.deepEqual([...definition.nativePlan.turnStart.reservedFields].sort(), ['clientUserMessageId', 'input', 'threadId'], 'Supply a Chat plan, including its reserved input fields.');
  const temporary = await realpath(tmpdir()), root = await mkdtemp(join(temporary, 'ivy-chat-package-'));
  const artifact = join(root, 'artifact'), children = [], report = { artifact: source, hiveArtifact, registrations: [], exits: [] };
  let primaryError, cleanupError;
  const json = async path => JSON.parse(await readFile(path, 'utf8'));
  const environment = { ...Object.fromEntries(Object.entries(process.env).filter(([key]) => key.toUpperCase() !== 'PATH')),
    PATH: '', IVY_NODE_EXECUTABLE: join(root, 'missing-node'), IVY_INSTANCE_CONFIG: '' };
  const platform = process.platform + '-' + process.arch, suffix = process.platform === 'win32' ? '.exe' : '';
  const native = (distribution, name) => join(distribution, 'dist/native', platform, name + suffix);
  const start = async (config, binary) => {
    const path = join(root, config.instanceId + '.json'); await writeFile(path, JSON.stringify(config));
    const launcher = process.platform === 'win32' ? native(artifact, 'ivy-job') : binary;
    const args = process.platform === 'win32' ? ['--parent', String(process.pid), binary, '--config', path] : ['--config', path];
    const child = spawn(launcher, args, { env: { ...environment, IVY_LAUNCH_ID: randomUUID() }, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    const state = { child, config, path, logs: '' };
    state.closed = new Promise((yes, no) => { child.once('error', no); child.once('close', (code, signal) => yes({ code, signal })); });
    for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { state.logs = (state.logs + bytes).slice(-16000); });
    children.push(state); return state;
  };
  const poll = async check => {
    const deadline = Date.now() + 45000; let last;
    do {
      try { const value = await check(); if (value) return value; } catch (error) { last = error; }
      await delay(100);
    } while (Date.now() < deadline);
    throw Error('Native condition timed out: ' + last?.message + '\n' + children.map(state => state.logs).join('\n'));
  };
  const stop = async state => {
    if (state.child.exitCode !== null) return;
    const health = await json(join(state.config.dataRoot, 'health.json'));
    await writeFile(join(state.config.dataRoot, 'control.json'), JSON.stringify({ schemaVersion: 1, instanceId: state.config.instanceId,
      bootId: health.bootId, launchId: health.launchId, action: 'shutdown', requestedAt: new Date().toISOString() }));
    const result = await Promise.race([state.closed, delay(18000, null, { ref: false }).then(() => { throw Error('Native shutdown timed out: ' + state.logs); })]);
    assert.deepEqual(result, { code: 0, signal: null }, state.logs); report.exits.push({ component: state.config.componentId, ...result });
  };
  const run = async (binary, args) => {
    const child = spawn(binary, args, { env: environment, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let text = ''; child.stdout.on('data', bytes => { text += bytes; }); child.stderr.on('data', bytes => { text += bytes; });
    const deadline = setTimeout(() => child.kill(), 15000);
    try { const [code] = await once(child, 'close'); return { code, text }; } finally { clearTimeout(deadline); }
  };
  try {
    await cp(join(source, 'dist'), join(artifact, 'dist'), { recursive: true, errorOnExist: true, force: false });
    const identity = await json(join(artifact, 'dist/build-info.json'));
    report.identity = identity;
    report.nativeEnvironment = { PATH: '', IVY_NODE_EXECUTABLE: 'unavailable' };
    report.binaries = {};
    for (const name of ['ivy-chat-bridge', 'ivy-health', ...(process.platform === 'win32' ? ['ivy-job'] : [])]) {
      const bytes = await readFile(native(artifact, name));
      report.binaries[name] = { bytes: bytes.length, hash: 'sha256:' + createHash('sha256').update(bytes).digest('hex') };
    }
    for (const file of ['instructions/chat-main-instructions.json', 'instructions/chat-notice-prompt.json',
      'specs/native/codex-0.154.0/catalog.json', 'licenses/chat-bridge/libopus-1.6.1.txt'])
      assert.ok((await readFile(join(artifact, 'dist', file))).length > 0, file);
    const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
    const port = reservation.address().port; await new Promise(yes => reservation.close(yes));
    const token = randomUUID(), serviceToken = randomUUID(), base = `http://127.0.0.1:${port}/ivy`;
    const hive = { schemaVersion: 1, componentId: 'hive', instanceId: 'hive', serviceNodeId: 'reference.hive', hostId: 'reference',
      artifactRoot: hiveArtifact, dataRoot: join(root, 'hive-data'), publicBaseUrl: base, ...await json(join(hiveArtifact, 'dist/build-info.json')),
      settings: { listenHost: '127.0.0.1', listenPort: port, credentials: [{ principalId: 'reference-user', token }, { principalId: definition.principalId, token: serviceToken }],
        backup: { enabled: false, directory: join(root, 'backups'), intervalHours: 24, retain: 2 } } };
    const hiveState = await start(hive, native(hiveArtifact, 'ivy-hive'));
    await poll(async () => (await json(join(hive.dataRoot, 'health.json'))).ready);
    const rpc = async (method, params = {}, mcp = false) => {
      const response = await fetch(base + (mcp ? '/mcp' : '/api/v1/rpc'), { method: 'POST',
        headers: { Authorization: 'Bearer ' + token, 'Content-Type': 'application/json', Connection: 'close',
          ...(mcp ? { Accept: 'application/json, text/event-stream', 'MCP-Protocol-Version': '2025-11-25' } : {}) },
        body: JSON.stringify({ jsonrpc: '2.0', id: randomUUID(), method, params }), signal: AbortSignal.timeout(10000) });
      const text = await response.text(), value = JSON.parse(response.headers.get('content-type')?.includes('text/event-stream') ? text.match(/^data: (.+)$/m)?.[1] : text);
      assert.equal(response.status, 200, text); assert.equal(value.error, undefined, text); return value.result;
    };
    const config = { schemaVersion: 1, componentId: 'chat-bridge', instanceId: 'chat', serviceNodeId: 'reference.chat', hostId: 'reference',
      artifactRoot: artifact, dataRoot: join(root, 'chat-data'), publicBaseUrl: base + '/', credential: serviceToken,
      version: identity.version, buildId: identity.buildId, settings: { definition, pollMs: 60000 } };
    const canonical = value => value && typeof value === 'object' ? Array.isArray(value) ? value.map(canonical)
      : Object.fromEntries(Object.keys(value).sort().map(key => [key, canonical(value[key])])) : value;
    const expectedBridge = { principalId: definition.principalId, callerPrincipalId: 'reference-user', workspaceId: definition.workspaceId,
      definitionHash: 'sha256:' + createHash('sha256').update(JSON.stringify(canonical(definition))).digest('hex') };
    let chat = await start(config, native(artifact, 'ivy-chat-bridge'));
    for (let attempt = 0; attempt < 2; attempt++) {
      const registration = await poll(async () => { const node = await rpc('serviceNodes.get', { serviceNodeId: config.serviceNodeId }); report.lastRegistration = node; return node.ready && node.synced && node.connected ? node : null; });
      assert.equal(registration.buildId, identity.buildId); report.registrations.push(registration);
      const health = await run(native(artifact, 'ivy-health'), ['--config', chat.path]);
      assert.equal(health.code, 0, health.text);
      const catalog = await rpc('tools.list', { namespace: 'chat', serviceNodeId: config.serviceNodeId });
      assert.equal(catalog.items.length, 12);
      const history = catalog.items.find(tool => tool.qualifiedName === 'chat.history'); assert.ok(history);
      const page = await rpc('tools.call', { qualifiedName: history.qualifiedName, serviceNodeId: config.serviceNodeId,
        expectedDefinitionHash: history.definitionHash, arguments: { expectedBridge, channel: definition.channels[0].channel, afterSequence: 0, limit: 8 } });
      assert.deepEqual(page, { entries: [], throughSequence: 0, hasMore: false });
      const tools = (await rpc('tools/list', {}, true)).tools.filter(tool => tool.name.startsWith('chat_bridge_')).map(tool => tool.name).sort();
      assert.deepEqual(tools, ['chat_bridge_read', 'chat_bridge_send']);
      report.history = page; report.tools = catalog.items.map(tool => tool.qualifiedName); report.mcpTools = tools;
      await stop(chat);
      const stopped = await run(native(artifact, 'ivy-health'), ['--config', chat.path]); assert.notEqual(stopped.code, 0);
      if (attempt === 0) chat = await start(config, native(artifact, 'ivy-chat-bridge'));
    }
    await stop(hiveState); report.completed = true;
  } catch (error) {
    primaryError = error; report.error = error.stack;
  } finally {
    report.processes = await Promise.all(children.map(async state => ({ component: state.config.componentId,
      logs: state.logs, health: await json(join(state.config.dataRoot, 'health.json')).catch(() => null) })));
    try { for (const state of children.toReversed()) await stop(state); }
    catch (error) { cleanupError = error; report.cleanupError = error.stack; }
    finally {
      for (const state of children.toReversed()) if (state.child.exitCode === null) { state.child.kill(); await state.closed; }
      const actual = await realpath(root), local = relative(temporary, actual);
      assert.ok(!isAbsolute(local) && /^ivy-chat-package-[^/\\]+$/.test(local));
      await rm(actual, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
      if (process.env.IVY_CHAT_PACKAGE_RESULT) await writeFile(process.env.IVY_CHAT_PACKAGE_RESULT, JSON.stringify(report, null, 2) + '\n');
    }
  }
  if (primaryError) throw primaryError;
  if (cleanupError) throw cleanupError;
});
