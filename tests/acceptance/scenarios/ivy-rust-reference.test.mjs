import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { inspectReference, inspectReferenceSurface } from './ivy-rust-reference.mjs';

async function fixture(t, components = ['service-manager', 'hive', 'data-collector']) {
  const root = await mkdtemp(join(tmpdir(), 'ivy-rust-reference-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const paths = [], calls = [];
  const buildId = 'sha256:' + 'a'.repeat(64);
  for (const componentId of components) {
    const dataRoot = join(root, componentId);
    await mkdir(dataRoot);
    const config = { schemaVersion: 1, hostId: 'fixture', componentId, instanceId: componentId,
      serviceNodeId: 'fixture.' + componentId, publicBaseUrl: 'https://fixture.invalid/ivy',
      dataRoot, artifactRoot: dataRoot, buildId, version: '1.0.0', settings: componentId === 'hive' ? {
        listenHost: '127.0.0.1', listenPort: 12345, trustedProxyAddresses: [],
        credentials: [{ principalId: 'fixture', token: 'fixture-credential' }], packagePublisherPrincipalIds: [],
        backup: { enabled: false, directory: join(root, 'backups'), intervalHours: 24, retain: 1 },
      } : {} };
    const path = join(dataRoot, 'instance.json');
    paths.push(path);
    await writeFile(path, JSON.stringify(config));
    await writeFile(join(dataRoot, 'health.json'), JSON.stringify({ schemaVersion: 1,
      instanceId: config.instanceId, componentId, buildId, pid: process.pid, bootId: 'fixture-boot',
      startedAt: new Date().toISOString(), observedAt: new Date().toISOString(), ready: true, generation: 1, details: '' }));
  }
  const client = { async request(method, params) {
    calls.push({ method, params });
    if (method === 'system.status') return { ready: true, buildId };
    if (method === 'serviceNodes.get') return { connected: true, synced: true, ready: true, buildId, hostId: 'fixture' };
    if (method === 'tools.list') return { items: [{ qualifiedName: 'data-collector.read', definitionHash: 'fixture-definition',
      definition: { annotations: { readOnlyHint: true } } }], provider: { node: { serviceNodeId: 'fixture.data-collector' } }, nextCursor: null };
    if (method === 'tools.call') return { items: [] };
    throw Error('Unexpected request');
  } };
  return { root, configPaths: paths, components, client, calls, surface: async () => ({ mcp: true, consoleHtml: true }) };
}

test('reference smoke checks health and current registration before a bound collector read', async t => {
  const f = await fixture(t);
  const report = await inspectReference({ ...f, components: undefined });
  assert.equal(report.ready, true);
  assert.equal(report.services.length, 3);
  assert.equal(report.collectorTasks, 0);
  assert.deepEqual(f.calls.map(c => c.method), ['system.status', 'serviceNodes.get', 'serviceNodes.get', 'tools.list', 'tools.call']);
  assert.deepEqual(f.calls.at(-1).params, { qualifiedName: 'data-collector.read', serviceNodeId: 'fixture.data-collector',
    expectedDefinitionHash: 'fixture-definition', arguments: { view: 'tasks' } });
  assert.ok(!JSON.stringify(report).includes('fixture-credential'));
});

test('escaped data and stale health refuse before any network request', async t => {
  const f = await fixture(t), path = f.configPaths[0], config = JSON.parse(await readFile(path, 'utf8'));
  await writeFile(path, JSON.stringify({ ...config, dataRoot: tmpdir() }));
  await assert.rejects(inspectReference(f), /escapes the isolated root/);
  await writeFile(path, JSON.stringify(config));
  const healthPath = join(config.dataRoot, 'health.json');
  const health = JSON.parse(await readFile(healthPath, 'utf8'));
  await writeFile(healthPath, JSON.stringify({ ...health, observedAt: '2000-01-01T00:00:00.000Z' }));
  await assert.rejects(inspectReference(f), /not freshly ready/);
  assert.deepEqual(f.calls, []);
});

test('registration mismatch and routed errors cannot produce a successful smoke result', async t => {
  const f = await fixture(t), original = f.client.request.bind(f.client);
  f.client.request = async (method, params) => method === 'serviceNodes.get' ? { connected: false } : original(method, params);
  await assert.rejects(inspectReference(f), /registration is not ready/);
  f.client.request = async (method, params) => {
    if (method === 'tools.call') throw Error('collector unavailable');
    return original(method, params);
  };
  await assert.rejects(inspectReference(f), /collector unavailable/);
});

test('whole-stack readiness requires its complete inventory and every remote service, including Phone', async t => {
  const components = ['host-executor', 'service-manager', 'hive', 'agent-manager', 'chat-bridge',
    'dashboards', 'data-collector', 'phone-bridge', 'secretary', 'task-board'];
  const f = await fixture(t, components);
  const report = await inspectReference(f);
  assert.equal(report.services.length, components.length);
  assert.deepEqual(f.calls.filter(call => call.method === 'serviceNodes.get').map(call => call.params.serviceNodeId).sort(),
    components.filter(component => !['host-executor', 'hive'].includes(component)).map(component => 'fixture.' + component).sort());
  f.calls.length = 0;
  await assert.rejects(inspectReference({ ...f, configPaths: f.configPaths.slice(0, -1) }));
  await assert.rejects(inspectReference({ ...f, components: [...components.slice(0, -1), 'phone-bridge'] }), /distinct expected/);
  assert.deepEqual(f.calls, []);
  const original = f.client.request.bind(f.client);
  f.client.request = async (method, params) => method === 'serviceNodes.get' && params.serviceNodeId === 'fixture.phone-bridge'
    ? { connected: false } : original(method, params);
  await assert.rejects(inspectReference(f), /phone-bridge registration is not ready/);
});

test('surface smoke uses the authenticated Console and public MCP tools and closes a failed session', async () => {
  const calls = [], credential = 'fixture-credential';
  let closed = 0, fail = false;
  const mcp = { async connect() {}, async close() { closed++; }, async callTool(call) {
    calls.push(call);
    return call.name === 'hive_status' ? { structuredContent: { result: { ready: true } } } : { isError: fail };
  } };
  const input = { url: new URL('http://127.0.0.1:12345/ivy'), credential, collectorNode: 'fixture.data-collector', mcp,
    fetchPage: async (url, options) => {
      assert.equal(url, 'http://127.0.0.1:12345/ivy/');
      assert.equal(options.headers.Authorization, 'Bearer ' + credential);
      assert.equal(options.redirect, 'error');
      return new Response('<html><body>Fixture</body></html>', { headers: { 'content-type': 'text/html' } });
    } };
  assert.deepEqual(await inspectReferenceSurface(input), { mcp: true, consoleHtml: true });
  assert.deepEqual(calls, [{ name: 'hive_status', arguments: {} },
    { name: 'data_collector_read', arguments: { serviceNodeId: 'fixture.data-collector', input: { view: 'tasks' } } }]);
  fail = true;
  await assert.rejects(inspectReferenceSurface(input), /MCP Collector read failed/);
  assert.equal(closed, 2);
});
