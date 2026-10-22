import assert from 'node:assert/strict';
import { readFile, realpath } from 'node:fs/promises';
import { isAbsolute, join, parse, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { HiveClient, discover, callBound } from '../../../dist/packages/sdk/src/client.js';
import { validateHost } from '../../../dist/packages/contracts/src/host-validation.js';

const components = ['service-manager', 'hive', 'data-collector'];
const json = async path => JSON.parse(await readFile(path, 'utf8'));

// Run after HostExecutor starts the isolated reference. This checks the live
// public path; process ancestry, restart and shutdown belong to the launch run.
export async function inspectReference({ root, configPaths, endpoint, client, surface = inspectReferenceSurface }) {
  root = await realpath(resolve(root));
  assert.notEqual(root, parse(root).root, 'Use an explicit isolated installation root');
  const inside = async path => {
    assert.ok(isAbsolute(path), 'Reference paths must be absolute');
    const actual = await realpath(path), fromRoot = relative(root, actual);
    assert.ok(fromRoot && fromRoot !== '..' && !fromRoot.startsWith('..' + (process.platform === 'win32' ? '\\' : '/')) && !isAbsolute(fromRoot),
      'Reference path escapes the isolated root');
    return actual;
  };
  assert.equal(configPaths.length, components.length);
  const configs = await Promise.all(configPaths.map(async path => {
    const config = await json(await inside(resolve(path)));
    validateHost('InstanceConfig', config);
    for (const key of ['dataRoot', 'workRoot', 'logsRoot']) {
      if (config[key]) await inside(config[key]);
    }
    return config;
  }));
  assert.deepEqual(configs.map(c => c.componentId).sort(), [...components].sort());
  assert.equal(new Set(configs.map(c => c.hostId)).size, 1, 'Reference services must share their isolated host');
  for (const key of ['instanceId', 'serviceNodeId', 'dataRoot']) {
    assert.equal(new Set(configs.map(c => c[key])).size, components.length, 'Reference services need distinct ' + key);
  }
  const health = [];
  for (const config of configs) {
    const value = await json(await inside(join(config.dataRoot, 'health.json')));
    validateHost('Health', value);
    for (const key of ['instanceId', 'componentId', 'buildId']) assert.equal(value[key], config[key], 'Health ' + key + ' mismatch');
    const age = Date.now() - Date.parse(value.observedAt);
    assert.ok(value.ready && age >= -1000 && age < 15000, config.componentId + ' is not freshly ready');
    process.kill(value.pid, 0);
    health.push({ componentId: value.componentId, instanceId: value.instanceId, buildId: value.buildId, pid: value.pid, bootId: value.bootId });
  }
  const hive = configs.find(c => c.componentId === 'hive');
  validateHost('HiveSettings', hive.settings);
  assert.ok(hive.settings.credentials[0]?.token, 'Hive needs its isolated test credential');
  const url = new URL(endpoint ?? hive.publicBaseUrl);
  if (!endpoint) {
    url.protocol = 'http:';
    url.hostname = hive.settings.listenHost === '::1' ? '[::1]' : hive.settings.listenHost;
    url.port = String(hive.settings.listenPort);
  }
  assert.ok(['http:', 'https:'].includes(url.protocol) && ['127.0.0.1', '[::1]'].includes(url.hostname), 'Reference endpoint must be loopback');
  assert.ok(!url.username && !url.password && !url.search && !url.hash);
  assert.equal(url.pathname, '/ivy');
  client ??= new HiveClient(url.href, { credential: hive.settings.credentials[0].token, batchReads: false });
  const status = await client.request('system.status', {}, { timeoutMs: 5000 });
  assert.equal(status.ready, true);
  assert.equal(status.buildId, hive.buildId);
  for (const config of configs.filter(c => c.componentId !== 'hive')) {
    const node = await client.request('serviceNodes.get', { serviceNodeId: config.serviceNodeId }, { timeoutMs: 5000 });
    assert.ok(node.connected && node.synced && node.ready, config.componentId + ' registration is not ready');
    assert.equal(node.buildId, config.buildId);
    assert.equal(node.hostId, config.hostId);
  }
  const collector = configs.find(c => c.componentId === 'data-collector');
  const binding = await discover(client, 'data-collector.read', { serviceNodeId: collector.serviceNodeId });
  assert.equal(binding.definition.annotations?.readOnlyHint, true, 'Collector read must remain read-only');
  const tasks = await callBound(client, binding, { view: 'tasks' }, undefined, { timeoutMs: 5000 });
  assert.ok(Array.isArray(tasks.items), 'Collector did not return its public task list');
  const surfaces = await surface({ url, credential: hive.settings.credentials[0].token, collectorNode: collector.serviceNodeId });
  return { ready: true, hostId: hive.hostId, services: health, collectorTasks: tasks.items.length, ...surfaces };
}

export async function inspectReferenceSurface({ url, credential, collectorNode,
  mcp = new Client({ name: 'ivy-rust-reference', version: '1.0.0' }), fetchPage = fetch }) {
  const headers = { Authorization: 'Bearer ' + credential };
  const page = await fetchPage(url.href + '/', { headers, redirect: 'error', signal: AbortSignal.timeout(5000) });
  assert.equal(page.status, 200, 'Reference Console is unavailable');
  assert.match(page.headers.get('content-type') ?? '', /text\/html/i);
  assert.match(await page.text(), /<html[\s>]/i);
  try {
    await mcp.connect(new StreamableHTTPClientTransport(new URL(url.href + '/mcp'), { requestInit: { headers } }));
    const status = await mcp.callTool({ name: 'hive_status', arguments: {} });
    assert.notEqual(status.isError, true, 'MCP Hive status failed');
    assert.equal(status.structuredContent?.result?.ready, true);
    const read = await mcp.callTool({ name: 'data_collector_read', arguments: { serviceNodeId: collectorNode, input: { view: 'tasks' } } });
    assert.notEqual(read.isError, true, 'MCP Collector read failed');
    return { mcp: true, consoleHtml: true };
  } finally {
    await mcp.close();
  }
}

async function main() {
  const { values } = parseArgs({ options: {
    root: { type: 'string' }, config: { type: 'string', multiple: true }, endpoint: { type: 'string' },
  } });
  assert.ok(values.root && values.config?.length === 3,
    'Usage: node tests/acceptance/scenarios/ivy-rust-reference.mjs --root PATH --config MANAGER --config HIVE --config COLLECTOR [--endpoint http://127.0.0.1:PORT/ivy]');
  console.log(JSON.stringify(await inspectReference({ root: values.root, configPaths: values.config, endpoint: values.endpoint })));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
