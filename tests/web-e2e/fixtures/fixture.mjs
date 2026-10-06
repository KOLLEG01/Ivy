import { chromium } from '@playwright/test';
import { createServer } from 'node:net';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { after } from 'node:test';
import { randomUUID } from 'node:crypto';
import { resolve, sep } from 'node:path';
import { HiveServer } from '../../../dist/services/hive/src/server.js';
import { HiveClient, newOperationId } from '../../../dist/packages/sdk/src/client.js';
import { ServiceClient } from '../../../dist/packages/sdk/src/service.js';
import { digest } from '../../../dist/packages/contracts/src/canonical.js';

export const fixtureToken = 'isolated-browser-contract-fixture';
const roots = [];
after(async () => { for (const root of roots) await rm(root, { recursive: true, force: true, maxRetries: 3 }); });
export async function fixture(t, populated = false, viewport = { width: 1440, height: 1000 }, serverOptions = {}) {
  const root = await mkdtemp(resolve(tmpdir(), 'ivy-browser-')); roots.push(root);
  const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
  const port = reservation.address().port; await new Promise((resolve, reject) => reservation.close(error => error ? reject(error) : resolve()));
  const base = `http://127.0.0.1:${port}/ivy`, origin = new URL(base).origin;
  const server = new HiveServer({ filename: resolve(root, 'hive.sqlite'), publicBaseUrl: base, version: '0.1.0-browser-fixture', buildId: digest('browser-fixture'),
    credentials: [{ principalId: 'browser-fixture', digest: digest(fixtureToken) }], listenHost: '127.0.0.1', listenPort: port, consoleRoot: resolve('dist/console'), ...serverOptions });
  let browser, service;
  t.after(async () => { await browser?.close(); await service?.stop(); await server.close(); });
  await server.start();
  const client = new HiveClient(base, { credential: fixtureToken });
  let firstObjectId, firstContent;
  if (populated) {
    for (const [key, mediaType] of [['fixture/markdown', 'text/markdown'], ['fixture/html', 'text/html']]) {
      await client.request('contracts.register', { mutationId: await newOperationId(client), definition: { key, version: '1.0.0', owner: { kind: 'agent' }, mediaType,
        retention: { objects: { mode: 'retain' }, revisions: { mode: 'all' } }, specMarkdown: 'Isolated browser fixture, not a product integration.' } });
    }
    firstContent = '# Isolated note 1\n\nOriginal saved text.\n\n<script>alert("unsafe")</script>\n\n![No automatic image](https://invalid.test/pixel)\n';
    for (let index = 0; index < 56; index++) {
      const item = await client.request('objects.write', { mutationId: await newOperationId(client), contractVersion: '1.0.0', references: {}, create: { contractKey: 'fixture/markdown', parentId: null, ownerObjectId: null, name: 'Note ' + String(index + 1).padStart(2, '0') },
        content: { encoding: 'text', value: index === 0 ? firstContent : '# Isolated note ' + (index + 1) + '\n\nSaved browser test content.' } });
      if (index === 0) firstObjectId = item.object.id;
    }
    await client.request('objects.write', { mutationId: await newOperationId(client), objectId: firstObjectId, expectedRevision: 1, contractVersion: '1.0.0', references: {}, content: { encoding: 'text', value: '# Isolated note 1\n\nRevised saved result.\n\n- A material outcome\n- Exact history\n' } });
    for (const releaseId of ['r1', 'r2']) {
      const bytes = Buffer.from('<!doctype html><html lang="en"><meta charset="utf-8"><title>Fixture ' + releaseId + '</title><h1>Isolated ui ' + releaseId + '</h1><a href="/ivy/">Return to Console</a></html>');
      const asset = { path: 'index.html', mediaType: 'text/html', contentHash: digest(bytes), byteLength: bytes.length };
      await client.request('uis.stageAsset', { uiId: 'fixture', releaseId, asset, base64: bytes.toString('base64'), mutationId: await newOperationId(client) });
      await client.request('uis.deploy', { mutationId: await newOperationId(client), expectedReleaseId: releaseId === 'r1' ? null : 'r1',
        metadata: { uiId: 'fixture', displayName: 'Isolated test ui', description: 'Real static releases for browser pointer-selection testing.', iconKey: 'ui' },
        release: { releaseId, entryPath: 'index.html', requirements: { hiveProtocol: 1, contracts: [], services: [] }, assets: [asset] } });
    }
    let ready;
    const readyPromise = new Promise(resolve => { ready = resolve; });
    service = new ServiceClient({ publicBaseUrl: base, credential: () => fixtureToken,
      identity: { serviceNodeId: 'browser-fixture-service', hostId: 'browser-fixture-host', serviceName: 'browser-fixture', version: '1.0.0', buildId: digest('fixture-service'), hiveProtocol: 1 },
      registry: () => ({ namespaces: [{ namespace: 'fixture', description: 'Browser fixture', guideMarkdown: '# Actual SDK connection\n\nIsolated test data only.',
        tools: [{ namespace: 'fixture', name: 'read', interfaceVersion: '1.0.0', description: 'Read a constant fixture marker.', inputSchema: { type: 'object', properties: {}, additionalProperties: false }, outputSchema: { type: 'string' } }], topics: [], inventoryKinds: [] }], contracts: [], requiredContracts: [{ key: 'fixture/markdown', readVersions: ['1.0.0'], writeVersions: [] }] }),
      handlers: { 'fixture.read': () => 'fixture' }, reconcile: async () => {}, onState: value => { if (value.status === 'ready') ready(); } });
    service.start();
    let timer;
    try { await Promise.race([readyPromise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Fixture SDK readiness timed out.')), 15000); })]); }
    finally { clearTimeout(timer); }
    const now = new Date().toISOString();
    await server.worker.request({ action: 'diagnostic', value: { code: 'browser_fixture_observation', source: 'browser-fixture', severity: 'info', status: 'resolved',
      message: 'A retained, resolved browser fixture diagnostic.', resource: { serviceNodeId: 'browser-fixture-service' }, firstObservedAt: now, lastObservedAt: now } });
  }
  browser = await chromium.launch({ ...(process.platform === 'win32' ? { channel: 'msedge' } : {}), headless: true, timeout: 20000 });
  const context = await browser.newContext({ viewport, acceptDownloads: true, colorScheme: 'light' });
  const page = await context.newPage(), pageErrors = [], externalRequests = [];
  page.on('pageerror', error => pageErrors.push(error.message));
  await context.route('**/*', request => {
    if (new URL(request.request().url()).origin === origin) return request.continue();
    externalRequests.push(request.request().url()); return request.abort();
  });
  const login = async () => {
    await page.goto(base + '/');
    await page.getByLabel('Hive credential').fill(fixtureToken);
    const request = page.waitForRequest(request => request.method() === 'POST' && request.url() === base + '/login');
    await page.getByRole('button', { name: 'Sign in', exact: true }).click();
    const submitted = await request;
    if (submitted.headers()['origin'] !== origin) throw new Error('Real login form did not retain its exact Origin.');
    await page.getByRole('heading', { name: 'UIs', exact: true }).waitFor();
  };
  return { root, base, origin, client, server, service, browser, context, page, pageErrors, externalRequests, login, firstObjectId, firstContent };
}
