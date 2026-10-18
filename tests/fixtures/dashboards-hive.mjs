import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { createServer } from 'node:net';
import { createHash, randomUUID } from 'node:crypto';
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client';
import { PNG } from 'pngjs';
import { HiveServer } from '../../services/hive/src/server.js';
import { HiveClient, newOperationId } from '../../packages/sdk/src/client.js';
import { digest } from '../../packages/contracts/src/canonical.js';
import { atomicJson } from '../../packages/host-runtime/src/config.js';
import { startDashboards } from '../../services/dashboards/src/main.js';

const sha = value => createHash('sha256').update(value).digest('hex');
async function until(check, timeout = 30000) {
  const end = Date.now() + timeout;
  let last;
  while (Date.now() < end) {
    try { const value = await check(); if (value) return value; }
    catch (error) { last = error; }
    await delay(50);
  }
  throw new Error('Fixture readiness timed out', { cause: last });
}
function pixels(image) {
  const bytes = Buffer.from(image.data, 'base64'), png = PNG.sync.read(bytes);
  assert.equal(png.width, 120);
  assert.equal(png.height, 100);
  return { width: png.width, height: png.height, pixels: sha(png.data) };
}

async function exercise(kind) {
  const root = await mkdtemp(join(tmpdir(), 'ivy-dashboards-hive-'));
  const reservation = createServer();
  reservation.listen(0, '127.0.0.1');
  await once(reservation, 'listening');
  const port = reservation.address().port;
  await new Promise((yes, no) => reservation.close(error => error ? no(error) : yes()));
  const base = `http://127.0.0.1:${port}/ivy/`, credential = randomUUID();
  const build = { version: '1.0.0-fixture', buildId: digest('dashboards-hive-fixture') };
  const serviceNodeId = 'fixture-host.renamed-dashboards';
  const server = new HiveServer({ filename: join(root, 'hive.sqlite'), publicBaseUrl: base,
    ...build, credentials: [{ principalId: 'fixture-caller', digest: digest(credential) }],
    listenHost: '127.0.0.1', listenPort: port, consoleRoot: root });
  const client = new HiveClient(base, { credential });
  const config = { schemaVersion: 1, componentId: 'dashboards', instanceId: 'renamed-dashboard',
    serviceNodeId, hostId: 'fixture-host', publicBaseUrl: base, ...build, credential,
    artifactRoot: join(root, 'artifact'), dataRoot: join(root, 'data'),
    settings: { chromiumExecutable: process.env.DASHBOARDS_TEST_BROWSER } };
  let original, native, mcp, nativeLog = '', stopped = false;
  const trace = [];
  const closeService = async () => {
    if (stopped) return;
    stopped = true;
    if (original) await original.close();
    if (native && native.exitCode === null && native.signalCode === null) {
      const exited = once(native, 'exit');
      const timer = new AbortController();
      try {
        const health = JSON.parse(await readFile(join(config.dataRoot, 'health.json'), 'utf8'));
        await atomicJson(join(config.dataRoot, 'control.json'), { schemaVersion: 1,
          instanceId: health.instanceId, bootId: health.bootId, launchId: health.launchId,
          action: 'shutdown', requestedAt: new Date().toISOString() });
        await Promise.race([exited, delay(10000, undefined, { signal: timer.signal }).then(() => { throw Error('Native shutdown timed out'); })]);
      }
      finally {
        timer.abort();
        if (native.exitCode === null && native.signalCode === null) {
          native.kill();
          await exited;
        }
      }
      assert.equal(native.exitCode, 0, nativeLog);
    }
  };
  try {
    await server.start();
    await atomicJson(join(config.artifactRoot, 'dist/build-info.json'), build);
    await atomicJson(join(root, 'instance.json'), config);
    if (kind === 'node') original = await startDashboards(join(root, 'instance.json'));
    else {
      if (process.platform === 'win32') {
        await mkdir(join(config.artifactRoot, 'dist/native'), { recursive: true });
        await copyFile(process.env.DASHBOARDS_TEST_JOB, join(config.artifactRoot, 'dist/native/ivy-job.exe'));
      }
      native = spawn(process.env.DASHBOARDS_TEST_SERVICE, ['--config', join(root, 'instance.json')], {
        windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
        env: { ...process.env, IVY_NODE_EXECUTABLE: process.execPath },
      });
      native.stdout.on('data', bytes => { nativeLog = (nativeLog + bytes).slice(-8000); });
      native.stderr.on('data', bytes => { nativeLog = (nativeLog + bytes).slice(-8000); });
      native.on('error', error => { nativeLog += String(error); });
    }
    await until(async () => {
      if (native && (native.exitCode !== null || native.signalCode !== null)) throw Error(nativeLog);
      const node = await client.request('serviceNodes.get', { serviceNodeId });
      return node.connected && node.synced && node.ready;
    });
    const catalog = await client.request('tools.list', { namespace: 'dashboards', serviceNodeId });
    assert.equal(catalog.items.length, 7);
    trace.push(['registry', catalog.items.map(row => [row.qualifiedName, row.definitionHash])]);
    const definitions = new Map(catalog.items.map(row => [row.qualifiedName, row.definitionHash]));
    const call = (name, args, operationId, extra = {}) => client.request('tools.call', {
      serviceNodeId, qualifiedName: 'dashboards.' + name, arguments: args,
      expectedDefinitionHash: definitions.get('dashboards.' + name), ...(operationId ? { operationId } : {}), ...extra,
    });
    const error = async (label, work) => {
      await assert.rejects(work, failure => {
        assert.ok(failure.code, String(failure));
        trace.push([label, failure.code, failure.outcome]); return true;
      });
    };
    await error('definition fence', () => call('list', {}, undefined, { expectedDefinitionHash: digest('wrong') }));
    await error('missing write identity', () => call('save', { value: { title: 'A', html: '<p>A</p>', sources: {} } }));
    await error('strict arguments', () => call('list', { extra: true }));
    await client.request('contracts.register', { mutationId: await newOperationId(client), definition: {
      key: 'fixture/value', version: '1.0.0', owner: { kind: 'agent' }, mediaType: 'application/json',
      retention: { objects: { mode: 'retain' }, revisions: { mode: 'all' } }, specMarkdown: 'Isolated fixture data.', jsonSchema: true,
    } });
    const legacy = await client.request('objects.write', { mutationId: await newOperationId(client),
      contractVersion: '1.0.0', references: {}, create: { contractKey: 'dashboards/dashboard',
        parentId: null, ownerObjectId: null, name: 'legacy' },
      content: { encoding: 'json', value: { title: 'Legacy', html: '<p>Legacy</p>', sources: {}, refreshSeconds: 5 } } });
    const normalized = await call('read', { id: legacy.object.id });
    assert.deepEqual(normalized.value.metadata, {});
    assert.equal(normalized.value.imageToken, null);
    const retained = await client.request('objects.read', { objectId: legacy.object.id });
    assert.equal(retained.revision.revision, 1);
    assert.equal(retained.revision.contractVersion, '1.0.0');
    assert.equal(Object.hasOwn(retained.content.value, 'metadata'), false);
    trace.push(['legacy read without migration', normalized.value]);
    await call('delete', { id: legacy.object.id, expectedRevision: 1 }, await newOperationId(client));
    const source = await client.request('objects.write', { mutationId: await newOperationId(client),
      contractVersion: '1.0.0', references: {}, create: { contractKey: 'fixture/value',
        parentId: null, ownerObjectId: null, name: 'source' },
      content: { encoding: 'json', value: { counter: 1, color: [240, 20, 40] } } });
    const token = 'a'.repeat(40);
    const value = { title: '  Native integration  ', html: `<style>html,body{margin:0}</style><script>
dashboard.onUpdate(async ({data,output})=>{await new Promise(r=>setTimeout(r,20));
if(output.mode!=='image'||data.saved.counter!==data.all[0].counter)throw Error('bad snapshot');
document.body.style.background='rgb('+data.saved.color.join(',')+')';});</script>`,
      sources: { saved: { objectId: source.object.id }, all: { contractKey: 'fixture/value', limit: 1 } },
      metadata: { display: 'Touch ä', nested: { enabled: true } }, imageToken: token, refreshSeconds: 5 };
    const operation = await newOperationId(client);
    const created = await call('save', { value }, operation);
    assert.deepEqual(await call('save', { value }, operation), created);
    const row = await call('read', { id: created.id });
    assert.equal(row.value.title, 'Native integration');
    assert.equal(new URL(created.imageUrl).pathname, '/ivy/api/v1/dashboards/image.png');
    assert.equal(new URL(created.url).pathname, '/ivy/dashboards/');
    assert.equal((await call('list', {})).items[0].id, created.id);
    trace.push(['created', created.revision, row.value.title]);
    await client.request('objects.write', { mutationId: await newOperationId(client),
      objectId: source.object.id, expectedRevision: 1, contractVersion: '1.0.0', references: {},
      content: { encoding: 'json', value: { counter: 2, color: [20, 40, 240] } } });
    const data = await call('data', { id: created.id });
    assert.equal(data.data.saved.counter, 2);
    assert.equal(data.data.all[0].counter, 2);
    assert.ok(Number.isFinite(Date.parse(data.observedAt)));
    trace.push(['snapshot', data.data]);
    const output = { id: created.id, width: 120, height: 100 };
    for (const colorMode of ['color', 'grayscale', 'monochrome']) {
      const image = await call('render', { ...output, colorMode });
      assert.deepEqual(image.metadata, { ...value.metadata, refreshSeconds: 5 });
      trace.push(['render', colorMode, pixels(image)]);
    }
    const endpoint = new URL(created.imageUrl);
    endpoint.searchParams.set('width', '120'); endpoint.searchParams.set('height', '100');
    const response = await fetch(endpoint);
    assert.equal(response.status, 200);
    const bytes = Buffer.from(await response.arrayBuffer()), etag = '"' + sha(bytes) + '"';
    assert.equal(response.headers.get('etag'), etag);
    assert.equal(response.headers.get('content-type'), 'image/png');
    assert.equal(response.headers.get('access-control-allow-origin'), '*');
    assert.equal(response.headers.get('cache-control'), 'private, no-cache');
    const imageStatus = async (label, url = endpoint, options = {}, status = 200) => {
      const result = await fetch(url, options);
      assert.equal(result.status, status, label);
      trace.push([label, result.status]);
      return result;
    };
    const wrong = new URL(endpoint); wrong.searchParams.set('token', 'wrong');
    await imageStatus('wrong token', wrong, {}, 401);
    wrong.searchParams.delete('token'); await imageStatus('missing token', wrong, {}, 401);
    wrong.searchParams.set('token', token); wrong.searchParams.set('id', 'missing');
    await imageStatus('missing object', wrong, {}, 404);
    const duplicate = new URL(endpoint); duplicate.searchParams.append('width', '120');
    await imageStatus('duplicate query', duplicate, {}, 400);
    const invalid = new URL(endpoint); invalid.searchParams.set('width', '120x');
    await imageStatus('invalid dimensions', invalid, {}, 400);
    const leading = new URL(endpoint); leading.searchParams.set('width', '00120');
    await imageStatus('leading zeros', leading);
    const post = await imageStatus('post', endpoint, { method: 'POST' }, 405);
    assert.equal(post.headers.get('allow'), 'GET, HEAD');
    const head = await imageStatus('head', endpoint, { method: 'HEAD' });
    assert.equal((await head.arrayBuffer()).byteLength, 0);
    assert.equal(head.headers.get('content-length'), String(bytes.length));
    await imageStatus('weak tag', endpoint, { headers: { 'If-None-Match': 'W/' + etag } });
    await imageStatus('wildcard tag', endpoint, { headers: { 'If-None-Match': '*' } });
    const updatedValue = { ...value, refreshSeconds: 60, metadata: { ...value.metadata, changed: true },
      sources: { ...value.sources, mirror: { objectId: created.id }, dashboards: { contractKey: 'dashboards/dashboard', limit: 1 } } };
    const updated = await call('save', { id: created.id, expectedRevision: 1, value: updatedValue }, await newOperationId(client));
    assert.equal(updated.revision, 2);
    await error('revision conflict', async () => call('save', { id: created.id, expectedRevision: 1, value }, await newOperationId(client)));
    const unchanged = await imageStatus('metadata-only conditional', endpoint, { headers: { 'If-None-Match': '"other", ' + etag } }, 304);
    assert.equal(unchanged.headers.get('x-dashboard-refresh-seconds'), '60');
    assert.deepEqual(JSON.parse(decodeURIComponent(unchanged.headers.get('x-dashboard-metadata'))), { ...updatedValue.metadata, refreshSeconds: 60 });
    assert.equal((await unchanged.arrayBuffer()).byteLength, 0);
    const scrubbed = await call('data', { id: created.id });
    assert.equal(Object.hasOwn(scrubbed.data.mirror, 'imageToken'), false);
    assert.equal(Object.hasOwn(scrubbed.data.dashboards[0], 'imageToken'), false);
    trace.push(['source tokens removed', true]);

    mcp = new Client({ name: 'dashboards-native-fixture', version: '1' });
    await mcp.connect(new StreamableHTTPClientTransport(new URL(base + 'mcp'), {
      requestInit: { headers: { Authorization: 'Bearer ' + credential } },
    }));
    const publicTools = (await mcp.listTools()).tools.filter(tool => tool.name.startsWith('dashboard_'));
    assert.equal(publicTools.length, 5);
    trace.push(['mcp tools', publicTools.map(tool => tool.name).sort()]);
    const projected = await mcp.callTool({ name: 'dashboard_render', arguments: { serviceNodeId, input: output } });
    assert.notEqual(projected.isError, true, JSON.stringify(projected));
    const blocks = projected.content.filter(block => block.type === 'image');
    assert.equal(blocks.length, 1);
    assert.ok(!JSON.stringify(projected.structuredContent).includes(blocks[0].data));
    assert.ok(!JSON.stringify(projected.content.filter(block => block.type !== 'image')).includes(blocks[0].data));
    trace.push(['mcp image', pixels(blocks[0])]);

    const rotatedValue = { ...updatedValue, imageToken: 'b'.repeat(40) };
    const rotated = await call('save', { id: created.id, expectedRevision: 2, value: rotatedValue }, await newOperationId(client));
    await imageStatus('rotated old token', endpoint, {}, 401);
    const rotatedEndpoint = new URL(endpoint); rotatedEndpoint.searchParams.set('token', rotatedValue.imageToken);
    await imageStatus('rotated new token', rotatedEndpoint, { method: 'HEAD' });
    await call('save', { id: created.id, expectedRevision: rotated.revision, value: { ...rotatedValue, imageToken: null } }, await newOperationId(client));
    await imageStatus('disabled token', rotatedEndpoint, {}, 401);
    await client.request('objects.archive', { objectId: source.object.id, archived: true, mutationId: await newOperationId(client) });
    await error('archived source', () => call('data', { id: created.id }));
    const deletion = await newOperationId(client);
    const removed = await call('delete', { id: created.id, expectedRevision: 4 }, deletion);
    assert.equal(removed.deleted, true);
    assert.deepEqual(await call('delete', { id: created.id, expectedRevision: 4 }, deletion), removed);
    assert.equal((await call('list', {})).items.length, 0);
    trace.push(['deleted', true]);
    await mcp.close(); mcp = null;
    await closeService();
    const offline = await imageStatus('offline provider', endpoint, {}, 503);
    assert.equal(offline.headers.get('retry-after'), '5');
    assert.deepEqual(await offline.json(), { error: 'image_unavailable' });
    return trace;
  } catch (error) {
    throw new Error(kind + ' Hive integration failed: ' + error.message + (nativeLog ? '\n' + nativeLog : ''), { cause: error });
  } finally {
    await mcp?.close();
    try { await closeService(); }
    finally { await server.close(); await rm(root, { recursive: true, force: true, maxRetries: 3 }); }
  }
}

const node = await exercise('node');
const rust = await exercise('rust');
assert.deepEqual(rust, node, 'Rust and original Node service traces differ');
process.stdout.write(JSON.stringify({ cases: rust.length, trace: rust }) + '\n');
