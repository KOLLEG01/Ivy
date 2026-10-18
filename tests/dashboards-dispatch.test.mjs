import test from 'node:test';
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { mkdtemp, readFile, rm, rename, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { createServer } from 'node:net';
import { randomUUID } from 'node:crypto';
import { PNG } from 'pngjs';
import { HiveServer } from '../dist/services/hive/src/server.js';
import { HiveClient, newOperationId } from '../dist/packages/sdk/src/client.js';
import { digest } from '../dist/packages/contracts/src/canonical.js';
import { atomicJson } from '../dist/packages/host-runtime/src/config.js';
import { packageHealth } from './fixtures/dashboards-package.mjs';
import { nativeDashboards } from './web-e2e/fixtures/dashboards.mjs';

test('Dashboards legacy dispatchers preserve native health identity and retire the owned browser tree', {
  timeout: 120000,
  skip: process.platform !== 'win32' || !process.env.DASHBOARDS_TEST_DISPATCH,
}, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-dashboards-dispatch-'));
  const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
  const port = reservation.address().port;
  await new Promise((yes, no) => reservation.close(error => error ? no(error) : yes()));
  const base = `http://127.0.0.1:${port}/ivy/`, credential = randomUUID();
  const server = new HiveServer({ filename: join(root, 'hive.sqlite'), publicBaseUrl: base,
    version: 'dispatch-fixture', buildId: digest('hive-dispatch-fixture'),
    credentials: [{ principalId: 'dispatch-fixture', digest: digest(credential) }],
    listenHost: '127.0.0.1', listenPort: port, consoleRoot: root });
  const client = new HiveClient(base, { credential });
  const plan = JSON.parse(await readFile('services/dashboards/deploy.json', 'utf8'));
  const build = { version: plan.version, buildId: digest('dashboards-dispatch-fixture') };
  const path = join(root, 'instance.json');
  const config = { schemaVersion: 1, componentId: 'dashboards', instanceId: 'renamed-dashboards',
    serviceNodeId: 'fixture-host.dashboards', hostId: 'fixture-host', publicBaseUrl: base,
    artifactRoot: join(root, 'artifact'), dataRoot: join(root, 'data'), ...build, credential,
    settings: { chromiumExecutable: process.env.DASHBOARDS_TEST_BROWSER } };
  let runtime;
  const proofs = [];
  try {
    await server.start();
    await atomicJson(path, config);
    await atomicJson(join(config.artifactRoot, 'dist/build-info.json'), build);
    runtime = await nativeDashboards(path, client);
    await runtime.service.waitReady();
    const before = await runtime.inspect();
    proofs.push({ finish: 'cooperative', ...before });
    const tools = await client.request('tools.list', { namespace: 'dashboards', serviceNodeId: config.serviceNodeId });
    const invoke = (name, args, operationId) => client.request('tools.call', {
      serviceNodeId: config.serviceNodeId, qualifiedName: 'dashboards.' + name, arguments: args,
      expectedDefinitionHash: tools.items.find(tool => tool.qualifiedName === 'dashboards.' + name).definitionHash,
      ...(operationId ? { operationId } : {}),
    });
    const saved = await invoke('save', { value: { title: 'Dispatched',
      html: '<style>body{background:rgb(20,40,60)}</style>', sources: {} } }, await newOperationId(client));
    const image = await invoke('render', { id: saved.id, width: 100, height: 100 });
    const png = PNG.sync.read(Buffer.from(image.data, 'base64'));
    assert.deepEqual([...png.data.subarray(0, 4)], [20, 40, 60, 255]);
    await atomicJson(path, { ...config, buildId: digest('wrong-artifact') });
    assert.deepEqual(await runtime.probe(), { exitCode: 1, value: { ok: false, code: 'build_mismatch' } });
    await atomicJson(path, config);
    assert.deepEqual(await runtime.probe(), { exitCode: 0, value: { ok: true, buildId: build.buildId, generation: before.health.generation } });
    await runtime.close();
    assert.deepEqual(await runtime.probe(), { exitCode: 1, value: { ok: false, code: 'service_not_ready' } });
    const stopped = JSON.parse(await readFile(join(config.dataRoot, 'health.json'), 'utf8'));
    assert.equal(stopped.ready, false);
    const priorBoot = before.health.bootId;
    runtime = await nativeDashboards(path, client);
    await runtime.service.waitReady();
    const forced = await runtime.inspect();
    assert.notEqual(forced.health.bootId, priorBoot);
    proofs.push({ finish: 'forced-job-retirement', ...forced });
    await runtime.crash();
    assert.deepEqual(await runtime.probe(), { exitCode: 1, value: { ok: false, code: 'service_not_ready' } });
    const missing = runtime.packaged.nativeHealth;
    await rename(missing, missing + '.saved');
    try { assert.deepEqual(await packageHealth(runtime.packaged.health, path), { exitCode: 1, value: { ok: false, code: 'artifact_missing' } }); }
    finally { await rename(missing + '.saved', missing); }
    const evidence = { cooperativeAndForcedTreesRetired: true, imagePixel: [20, 40, 60, 255], proofs };
    if (process.env.DASHBOARDS_TEST_DISPATCH_EVIDENCE) await writeFile(process.env.DASHBOARDS_TEST_DISPATCH_EVIDENCE, JSON.stringify(evidence));
    t.diagnostic(JSON.stringify(evidence));
  } finally {
    try { await runtime?.close(); }
    finally { await server.close(); await rm(root, { recursive: true, force: true, maxRetries: 3 }); }
  }
});
