import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdtemp, mkdir, readFile, readdir, realpath, rm, stat, utimes, writeFile } from 'node:fs/promises';
import { get as httpGet } from 'node:http';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { isAbsolute, join, relative, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import test from 'node:test';
import WebSocket from 'ws';

// The native artifact is explicit: this check never builds or starts the old
// backend, substitutes a storage worker, or contacts a configured installation.
test('native Hive starts storage, serves authenticated Console/MCP and restarts', { timeout: 180000 }, async () => {
  assert.ok(process.env.IVY_HIVE_REFERENCE_ARTIFACT, 'Set IVY_HIVE_REFERENCE_ARTIFACT to the built native Hive artifact.');
  const artifactRoot = await realpath(resolve(process.env.IVY_HIVE_REFERENCE_ARTIFACT));
  const executable = join(artifactRoot, 'dist', 'native', process.platform + '-' + process.arch, process.platform === 'win32' ? 'ivy-hive.exe' : 'ivy-hive');
  const identity = JSON.parse(await readFile(join(artifactRoot, 'dist/build-info.json'), 'utf8'));
  const temporary = await realpath(tmpdir());
  const root = await mkdtemp(join(temporary, 'ivy-hive-reference-'));
  let token = randomUUID();
  let child, closed, logs = '', config;
  const json = async file => JSON.parse(await readFile(file, 'utf8'));
  const poll = async (check, timeout = 15000) => {
    const end = Date.now() + timeout;
    let last;
    do {
      if (child?.exitCode !== null && child?.exitCode !== undefined) throw Error('Hive exited: ' + logs);
      try { const value = await check(); if (value) return value; } catch (error) { last = error; }
      await delay(50);
    } while (Date.now() < end);
    throw Error('Native Hive did not reach the expected condition: ' + (last?.message ?? '') + '\n' + logs);
  };
  const request = async (path, options = {}) => {
    try {
      return await fetch(config.publicBaseUrl + path, {
        redirect: 'manual', ...options, signal: AbortSignal.timeout(10000),
        headers: { Connection: 'close', ...options.headers },
      });
    } catch (cause) {
      throw Error(`Native request ${options.method ?? 'GET'} ${path.split('?')[0]} failed.\n${logs}`, { cause });
    }
  };
  const rpc = async (method, params = {}) => {
    const response = await request('/api/v1/rpc', { method: 'POST', headers: { Authorization: 'Bearer ' + token, 'Content-Type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: randomUUID(), method, params }) });
    const text = await response.text();
    const value = JSON.parse(response.headers.get('content-type')?.includes('text/event-stream') ? text.match(/^data: (.+)$/m)?.[1] : text);
    assert.equal(response.status, 200, JSON.stringify(value)); assert.equal(value.error, undefined, JSON.stringify(value));
    return value.result;
  };
  const mcp = async (method, params = {}, modern = false, bearer = token) => {
    const headers = { Authorization: 'Bearer ' + bearer, 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream', 'MCP-Protocol-Version': modern ? '2026-07-28' : '2025-11-25' };
    if (modern) {
      headers['MCP-Method'] = method;
      if (params.name) headers['MCP-Name'] = params.name;
      params = { ...params, _meta: { 'io.modelcontextprotocol/protocolVersion': '2026-07-28', 'io.modelcontextprotocol/clientCapabilities': {} } };
    }
    const response = await request('/mcp', { method: 'POST', headers, body: JSON.stringify({ jsonrpc: '2.0', id: randomUUID(), method, params }) });
    const text = await response.text();
    const value = JSON.parse(response.headers.get('content-type')?.includes('text/event-stream') ? text.match(/^data: (.+)$/m)?.[1] : text);
    assert.equal(response.status, 200, JSON.stringify(value)); assert.equal(value.error, undefined, JSON.stringify(value));
    return value.result;
  };
  const start = async () => {
    let port = config?.settings.listenPort;
    if (!port) {
      const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
      port = reservation.address().port; await new Promise((yes, no) => reservation.close(error => error ? no(error) : yes()));
    }
    config = { schemaVersion: 1, componentId: 'hive', instanceId: 'hive', serviceNodeId: 'reference.hive', hostId: 'reference',
      artifactRoot, dataRoot: join(root, 'data'), workRoot: join(root, 'work'), logsRoot: join(root, 'logs'),
      publicBaseUrl: `http://127.0.0.1:${port}/ivy`, version: identity.version, buildId: identity.buildId,
      settings: { listenHost: '127.0.0.1', listenPort: port, credentials: [{ principalId: 'reference-user', token }],
        packagePublisherPrincipalIds: ['reference-user'],
        backup: { enabled: true, directory: join(root, 'backups'), intervalHours: 24, retain: 2 } } };
    await writeFile(join(root, 'instance.json'), JSON.stringify(config));
    child = spawn(executable, ['--config', join(root, 'instance.json')], { windowsHide: true,
      env: { ...process.env, IVY_LAUNCH_ID: randomUUID() }, stdio: ['ignore', 'pipe', 'pipe'] });
    closed = new Promise((yes, no) => { child.once('error', no); child.once('close', (code, signal) => yes({ code, signal })); });
    for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { logs = (logs + bytes).slice(-64000); });
    return poll(async () => { const health = await json(join(config.dataRoot, 'health.json')); return health.ready && health.pid === child.pid ? health : null; }, 30000);
  };
  const stop = async () => {
    if (!child || child.exitCode !== null) return;
    const health = await json(join(config.dataRoot, 'health.json'));
    await writeFile(join(config.dataRoot, 'control.json'), JSON.stringify({ schemaVersion: 1, instanceId: config.instanceId,
      launchId: health.launchId, bootId: health.bootId, action: 'shutdown', requestedAt: new Date().toISOString() }));
    const result = await Promise.race([closed, delay(18000, undefined, { ref: false }).then(() => { throw Error('Native Hive shutdown timed out.\n' + logs); })]);
    assert.equal(result.code, 0, logs); assert.equal(result.signal, null, logs);
  };
  const snapshots = async () => {
    const names = await readdir(join(root, 'backups'));
    return names.filter(name => name.endsWith('.sqlite') && names.includes(name + '.json') && names.includes(name.replace(/\.sqlite$/, '.ui'))).sort();
  };
  const diagnostics = async code => (await rpc('system.diagnostics', { limit: 100 })).items.find(item => item.code === code);
  const credentialStatus = async bearer => {
    const response = await request('/api/v1/rpc', { method: 'POST', headers: { Authorization: 'Bearer ' + bearer, 'Content-Type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: randomUUID(), method: 'system.status', params: {} }) });
    await response.arrayBuffer(); return response.status;
  };
  try {
    await mkdir(join(root, 'data'));
    const first = await start();
    assert.equal(first.componentId, 'hive'); assert.equal(first.buildId, identity.buildId);
    const firstSnapshot = await poll(async () => (await snapshots())[0]);
    const firstRecord = await json(join(root, 'backups', firstSnapshot + '.json'));
    assert.equal(firstRecord.schemaVersion, 1); assert.equal(firstRecord.buildId, identity.buildId); assert.ok(firstRecord.pages > 0);
    assert.equal(firstRecord.bytesHash, 'sha256:' + createHash('sha256').update(await readFile(join(root, 'backups', firstSnapshot))).digest('hex'));
    await poll(async () => (await diagnostics('retention_failed'))?.status === 'resolved');
    assert.deepEqual(await (await request('/health/live')).json(), { live: true });
    assert.deepEqual(await (await request('/health/ready')).json(), { ready: true });
    const denied = await request('/api/v1/rpc', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' });
    assert.equal(denied.status, 401);
    assert.equal((await request('/')).status, 303);
    const loginPage = await request('/login'); assert.equal(loginPage.status, 200); assert.match(await loginPage.text(), /<form/);
    const login = await request('/login', { method: 'POST', headers: { Origin: new URL(config.publicBaseUrl).origin, 'Content-Type': 'application/x-www-form-urlencoded' }, body: new URLSearchParams({ token, returnTo: '/ivy/' }) });
    assert.equal(login.status, 303); const cookie = login.headers.get('set-cookie'); assert.ok(cookie);
    const consolePage = await request('/', { headers: { Cookie: cookie.split(';')[0] } });
    assert.equal(consolePage.status, 200); const html = await consolePage.text(); assert.match(html, /<script[^>]+src=/);
    const script = html.match(/<script[^>]+src="([^"]+)"/)?.[1]; assert.ok(script);
    const asset = await fetch(new URL(script, config.publicBaseUrl + '/'), { signal: AbortSignal.timeout(10000), headers: { Authorization: 'Bearer ' + token, Connection: 'close' } });
    assert.equal(asset.status, 200); assert.match(asset.headers.get('content-type'), /javascript/); assert.ok((await asset.arrayBuffer()).byteLength > 0);
    if (process.env.IVY_HIVE_REFERENCE_BROWSER) {
      const { chromium } = await import('playwright-core');
      const browser = await chromium.launch({ executablePath: process.env.IVY_HIVE_REFERENCE_BROWSER, headless: true });
      try {
        for (const viewport of [{ width: 1280, height: 800 }, { width: 390, height: 844 }]) {
          const context = await browser.newContext({ viewport, isMobile: viewport.width < 600 });
          try {
            const page = await context.newPage(), errors = []; page.setDefaultTimeout(15000);
            page.on('pageerror', error => errors.push(error.message));
            await page.goto(config.publicBaseUrl + '/login');
            await page.locator('input[name=token]').fill(token);
            const statusResponse = page.waitForResponse(response => response.url().endsWith('/api/v1/rpc') && response.request().postData()?.includes('"method":"system.status"'));
            await Promise.all([page.waitForURL(url => url.pathname === '/ivy/'), page.locator('button[type=submit]').click()]);
            const response = await statusResponse;
            const sent = JSON.parse(response.request().postData());
            const id = (Array.isArray(sent) ? sent : [sent]).find(item => item.method === 'system.status').id;
            const received = await response.json();
            const status = (Array.isArray(received) ? received : [received]).find(item => item.id === id);
            assert.equal(response.status(), 200); assert.equal(status.error, undefined, JSON.stringify(status));
            assert.equal(status.result.ready, true); assert.equal(status.result.callerPrincipalId, 'reference-user');
            await page.getByText('Your workspace starts here', { exact: true }).waitFor();
            assert.deepEqual(errors, []);
            assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), 'Console must fit the desktop/mobile viewport.');
          } finally { await context.close(); }
        }
      } finally { await browser.close(); }
    }
    const firstStatus = await rpc('system.status');
    assert.equal(firstStatus.callerPrincipalId, 'reference-user');
    const ws = new WebSocket(config.publicBaseUrl.replace(/^http/, 'ws') + '/ws', { headers: { Authorization: 'Bearer ' + token } });
    try {
      await once(ws, 'open', { signal: AbortSignal.timeout(10000) });
      const response = once(ws, 'message', { signal: AbortSignal.timeout(10000) });
      ws.send(JSON.stringify({ jsonrpc: '2.0', id: 'websocket-status', method: 'system.status', params: {} }));
      const [bytes] = await response; const status = JSON.parse(bytes.toString());
      assert.equal(status.id, 'websocket-status'); assert.equal(status.result.callerPrincipalId, 'reference-user');
      const closed = once(ws, 'close', { signal: AbortSignal.timeout(10000) }); ws.close(); await closed;
    } finally { ws.terminate(); }
    const initialized = await mcp('initialize', { protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'native-reference-check', version: '1' } });
    assert.equal(initialized.protocolVersion, '2025-11-25');
    const catalog = await mcp('tools/list'); assert.ok(catalog.tools.some(tool => tool.name === 'hive_status'));
    const status = await mcp('tools/call', { name: 'hive_status', arguments: {} });
    assert.equal(status.isError, undefined); assert.equal(status.structuredContent.result.callerPrincipalId, 'reference-user');
    const discovered = await mcp('server/discover', {}, true);
    assert.deepEqual(discovered.supportedVersions, ['2026-07-28']); assert.equal(discovered.resultType, 'complete');
    assert.equal((await mcp('tools/call', { name: 'hive_status', arguments: {} }, true)).structuredContent.result.callerPrincipalId, 'reference-user');
    const resource = config.publicBaseUrl + '/mcp', redirectUri = 'http://127.0.0.1/callback';
    const registered = await request('/oauth/register', { method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ client_name: 'Native PKCE check', redirect_uris: [redirectUri] }) });
    assert.equal(registered.status, 201); const client = await registered.json();
    const verifier = randomBytes(48).toString('base64url'), state = randomUUID();
    const authorization = await request('/oauth/authorize?' + new URLSearchParams({ response_type: 'code', client_id: client.client_id,
      redirect_uri: redirectUri, state, code_challenge: createHash('sha256').update(verifier).digest('base64url'),
      code_challenge_method: 'S256', resource, scope: 'hive' }));
    assert.equal(authorization.status, 200);
    const transaction = (await authorization.text()).match(/name="transaction" value="([^"]+)"/)?.[1]; assert.ok(transaction);
    const authorized = await request('/oauth/authorize', { method: 'POST', headers: { Origin: new URL(config.publicBaseUrl).origin,
      'Content-Type': 'application/x-www-form-urlencoded' }, body: new URLSearchParams({ transaction, credential: token }) });
    assert.equal(authorized.status, 303); const callback = new URL(authorized.headers.get('location'));
    assert.equal(callback.searchParams.get('state'), state); assert.ok(callback.searchParams.get('iss'));
    const exchange = async form => request('/oauth/token', { method: 'POST', headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({ client_id: client.client_id, resource, ...form }) });
    const codeGrant = { grant_type: 'authorization_code', code: callback.searchParams.get('code'), redirect_uri: redirectUri, code_verifier: verifier };
    const issued = await exchange(codeGrant); assert.equal(issued.status, 200); const tokens = await issued.json();
    assert.equal(tokens.token_type, 'Bearer'); assert.ok(tokens.refresh_token); assert.ok(tokens.expires_in > 0);
    assert.equal((await mcp('tools/call', { name: 'hive_status', arguments: {} }, false, tokens.access_token)).structuredContent.result.callerPrincipalId, 'reference-user');
    assert.notEqual((await exchange(codeGrant)).status, 200, 'An authorization code can be exchanged only once.');
    const refreshGrant = refresh_token => ({ grant_type: 'refresh_token', refresh_token });
    const refreshed = await exchange(refreshGrant(tokens.refresh_token)); assert.equal(refreshed.status, 200); const rotated = await refreshed.json();
    assert.notEqual(rotated.refresh_token, tokens.refresh_token);
    const hash = bytes => 'sha256:' + createHash('sha256').update(bytes).digest('hex');
    const authenticated = { Authorization: 'Bearer ' + token };
    const mutation = (method, params) => rpc(method, { mutationId: firstStatus.runtimeEpoch + ':' + Date.now() + ':' + randomUUID(), ...params });
    const wikiDefinition = JSON.parse(await readFile(new URL('../../ui/wiki-ui/ui.json', import.meta.url), 'utf8')).dataContracts.find(contract => contract.key === 'wiki/page');
    await mutation('contracts.register', { definition: wikiDefinition });
    const journalStart = await rpc('events.read', { afterSequence: 0, filter: { topics: ['reference.no-match'] } });
    const createPage = { mutationId: firstStatus.runtimeEpoch + ':' + Date.now() + ':' + randomUUID(), title: 'ReferencePagingWord Alpha', markdown: 'ReferencePagingWord revision one.' };
    const createdPage = await rpc('wiki.create', createPage);
    assert.deepEqual(await rpc('wiki.create', createPage), createdPage, 'Retained mutation receipts replay without creating another object.');
    const otherPage = await mutation('wiki.create', { title: 'ReferencePagingWord Beta', markdown: 'ReferencePagingWord second object.' });
    for (let revision = 1; revision < 3; revision++) {
      const updated = await mutation('wiki.update', { objectId: createdPage.object.id, expectedRevision: revision, markdown: 'ReferencePagingWord revision ' + (revision + 1) + '.' });
      assert.equal(updated.revision.revision, revision + 1);
    }
    let historyCursor;
    for (const revision of [3, 2, 1]) {
      const history = await rpc('wiki.history', { objectId: createdPage.object.id, limit: 1, ...(historyCursor ? { cursor: historyCursor } : {}) });
      assert.deepEqual(history.items.map(item => item.revision), [revision]);
      historyCursor = history.nextCursor; assert.equal(typeof historyCursor, revision > 1 ? 'string' : 'object');
    }
    assert.equal(historyCursor, null);
    const searched = await rpc('objects.search', { text: 'ReferencePagingWord', limit: 1 });
    assert.equal(searched.items.length, 1); assert.ok(searched.nextCursor);
    const searchedNext = await rpc('objects.search', { text: 'ReferencePagingWord', limit: 1, cursor: searched.nextCursor });
    assert.equal(searchedNext.nextCursor, null);
    assert.deepEqual(new Set([...searched.items, ...searchedNext.items].map(item => item.id)), new Set([createdPage.object.id, otherPage.object.id]));
    const query = { contractKey: 'wiki/page', select: ['object.name'], orderBy: [{ field: 'object.name', direction: 'asc' }], limit: 1 };
    const queried = await rpc('objects.query', query); assert.equal(queried.items.length, 1); assert.ok(queried.nextCursor);
    const queriedNext = await rpc('objects.query', { ...query, cursor: queried.nextCursor });
    assert.equal(queriedNext.nextCursor, null);
    assert.deepEqual(new Set([...queried.items, ...queriedNext.items].map(item => item.objectId)), new Set([createdPage.object.id, otherPage.object.id]));
    const contracts = await rpc('contracts.list', { limit: 1 }); assert.ok(contracts.nextCursor);
    const contractsNext = await rpc('contracts.list', { limit: 1, cursor: contracts.nextCursor });
    assert.equal(contracts.items.length, 1); assert.equal(contractsNext.items.length, 1); assert.notDeepEqual(contractsNext.items, contracts.items);
    const capabilities = await rpc('discovery.list', { serviceName: 'hive', query: 'object', limit: 1 });
    assert.equal(capabilities.items.length, 1); assert.ok(capabilities.nextCursor);
    const capabilitiesNext = await rpc('discovery.list', { serviceName: 'hive', query: 'object', limit: 1, cursor: capabilities.nextCursor });
    assert.equal(capabilitiesNext.items.length, 1); assert.notDeepEqual(capabilitiesNext.items, capabilities.items);
    let throughSequence = journalStart.throughSequence;
    for (const revision of [1, 2, 3]) {
      const events = await rpc('events.read', { afterSequence: throughSequence, filter: { topics: ['hive.object.changed'], objectIds: [createdPage.object.id] }, limit: 1 });
      assert.deepEqual(events.items.map(item => item.payload.revision), [revision]);
      assert.equal(events.items[0].payload.objectId, createdPage.object.id); assert.equal(events.gap, null);
      throughSequence = events.throughSequence;
    }
    const excludedEvents = await rpc('events.read', { afterSequence: journalStart.throughSequence, filter: { topics: ['reference.no-match'] } });
    assert.deepEqual(excludedEvents.items, []); assert.ok(excludedEvents.throughSequence >= throughSequence);
    const retention = await rpc('retention.preview');
    const wikiRetention = retention.families.find(family => family.contractKey === 'wiki/page');
    assert.equal(wikiRetention.objectCount, 2); assert.equal(wikiRetention.revisionCount, 4); assert.ok(retention.mutations.count >= 4);
    const metadata = { uiId: 'reference-ui', slug: 'reference', displayName: 'Reference UI', description: 'Isolated native route check.', iconKey: 'ui' };
    const requirements = { hiveProtocol: 1, contracts: [], services: [] };
    const files = [
      ['index.html', 'text/html', '<!doctype html><h1>Published reference UI</h1>'],
      ['demo.sandbox.html', 'text/html', '<!doctype html><p>Isolated sandbox</p>'],
      ['a+b.txt', 'text/plain', 'Literal plus'],
      ['caf\u00e9.txt', 'text/plain', 'Unicode asset'],
    ];
    const assets = files.map(([path, mediaType, bytes]) => ({ path, mediaType, contentHash: hash(bytes), byteLength: Buffer.byteLength(bytes) }));
    for (let i = 0; i < files.length; i++) await mutation('uis.stageAsset', { uiId: metadata.uiId, releaseId: 'r1', asset: assets[i], base64: Buffer.from(files[i][2]).toString('base64') });
    await mutation('uis.deploy', { metadata, release: { releaseId: 'r1', entryPath: 'index.html', requirements, assets }, expectedReleaseId: null });
    const mounted = await request('/ui/reference-ui/', { headers: authenticated });
    assert.equal(mounted.status, 200); assert.match(await mounted.text(), /Published reference UI/);
    assert.equal(mounted.headers.get('etag'), '"' + assets[0].contentHash + '"');
    assert.equal(mounted.headers.get('vary'), 'Cookie, Authorization');
    assert.equal(mounted.headers.get('cache-control'), 'private, no-cache');
    assert.match(mounted.headers.get('content-security-policy'), /frame-src 'self'/);
    assert.equal(mounted.headers.get('x-frame-options'), 'DENY');
    const conditional = await request('/reference/', { headers: { ...authenticated, 'If-None-Match': mounted.headers.get('etag') } });
    assert.equal(conditional.status, 304); assert.equal(await conditional.text(), '');
    const canonical = await request('/reference?selected=1', { headers: authenticated });
    assert.equal(canonical.status, 303); assert.equal(canonical.headers.get('location'), '/ivy/reference/?selected=1');
    for (const path of ['/reference/a+b.txt', '/reference/a%2Bb.txt', '/ui/reference-ui/caf%C3%A9.txt']) {
      const response = await request(path, { headers: authenticated }); assert.equal(response.status, 200);
      assert.equal(await response.text(), path.includes('caf') ? 'Unicode asset' : 'Literal plus');
    }
    const sandbox = await request('/reference/demo.sandbox.html', { headers: authenticated });
    assert.equal(sandbox.status, 200); assert.match(sandbox.headers.get('content-security-policy'), /^sandbox allow-scripts;/);
    assert.equal(sandbox.headers.get('x-frame-options'), null); await sandbox.arrayBuffer();
    const historical = await request('/ui/reference-ui/releases/r1/index.html', { headers: authenticated });
    assert.equal(historical.status, 200); assert.match(historical.headers.get('cache-control'), /immutable/); await historical.arrayBuffer();
    const navigation = { 'Sec-Fetch-Mode': 'navigate', 'Sec-Fetch-Dest': 'document' };
    // Fetch supplies its own Sec-Fetch-Mode; use an actual HTTP request for
    // browser navigation metadata rather than asking Fetch to impersonate it.
    const navigate = (path, headers) => new Promise((resolve, reject) => {
      const get = httpGet(config.publicBaseUrl + path, { headers: { Connection: 'close', ...headers }, signal: AbortSignal.timeout(10000) }, response => {
        const received = new Headers();
        for (let i = 0; i < response.rawHeaders.length; i += 2) received.append(response.rawHeaders[i], response.rawHeaders[i + 1]);
        response.on('error', reject); response.resume();
        response.on('end', () => resolve(new Response(null, { status: response.statusCode, headers: received })));
      });
      get.on('error', reject);
    });
    const resume = await navigate('/reference/?selected=1', navigation);
    assert.equal(resume.status, 303); const target = new URL(resume.headers.get('location'), config.publicBaseUrl);
    assert.equal(target.pathname, '/ivy/login'); assert.equal(target.searchParams.get('returnTo'), '/ivy/reference/?selected=1');
    assert.equal((await request('/reference/index.html')).status, 401);
    const oldNavigation = await navigate('/reference/releases/r1/index.html?selected=1', { ...authenticated, ...navigation });
    assert.equal(oldNavigation.status, 303); assert.equal(oldNavigation.headers.get('location'), '/ivy/reference/?selected=1');
    const traversal = await request('/reference/%2e%2e%2foutside', { headers: authenticated });
    assert.notEqual(traversal.status, 200); await traversal.arrayBuffer();
    // Publish a real immutable v1 archive through the one-time upload route.
    // Its inert files are never executed and belong only to this test root.
    const candidate = join(root, 'package-candidate'), dist = join(candidate, 'artifact/dist');
    await mkdir(dist, { recursive: true });
    const buildId = hash('native-route-fixture');
    const manifest = { schemaVersion: 1, componentId: 'reference-fixture', kind: 'service', version: '1.0.0', buildId, connectsToHive: true,
      requirements: { node: '>=24.18.0 <25.0.0', hiveProtocol: 1, contracts: [] },
      entrypoint: { executable: 'node', args: ['dist/main.js'], timeoutMs: 30000 },
      readiness: { timeoutMs: 30000, command: { executable: 'node', args: ['dist/health.js'], timeoutMs: 5000 } },
      shutdown: { timeoutMs: 15000 }, restart: { policy: 'always', minimumDelayMs: 1000, maximumDelayMs: 60000 } };
    await writeFile(join(candidate, 'component.json'), JSON.stringify(manifest));
    await writeFile(join(dist, 'build-info.json'), JSON.stringify({ schemaVersion: 1, componentId: manifest.componentId, version: manifest.version, buildId }));
    for (const file of ['main.js', 'health.js']) await writeFile(join(dist, file), 'throw Error("Inert archive fixture");\n');
    await writeFile(join(dist, 'payload.bin'), randomBytes(384 * 1024));
    const archivePath = join(root, 'package.tar.gz');
    const tar = spawn('tar', ['-czf', archivePath, '-C', candidate, 'artifact', 'component.json'], { windowsHide: true, stdio: 'ignore' });
    const [tarCode] = await once(tar, 'close'); assert.equal(tarCode, 0);
    const archive = await readFile(archivePath);
    const packageInput = { componentId: manifest.componentId, version: manifest.version, buildId, archiveHash: hash(archive), bytes: archive.length, manifest };
    const grant = await rpc('packages.authorizeUpload', packageInput);
    const uploadPath = grant.uploadPath.slice('/ivy'.length), uploadHeaders = { Authorization: 'Bearer ' + grant.uploadToken };
    // Exercise the early-rejection/ongoing-upload race on separate connections.
    for (let attempt = 0; attempt < 12; attempt++) {
      const selected = attempt === 0 ? grant : await rpc('packages.authorizeUpload', packageInput);
      const mismatched = await request(selected.uploadPath.slice('/ivy'.length), { method: 'PUT',
        headers: { Authorization: 'Bearer ' + selected.uploadToken, 'Content-Length': String(archive.length - 1) }, body: archive.subarray(1) });
      assert.equal(mismatched.status, 400); await mismatched.arrayBuffer();
    }
    const consumed = await request(uploadPath, { method: 'PUT', headers: uploadHeaders, body: archive });
    assert.equal(consumed.status, 401); await consumed.arrayBuffer();
    const fresh = await rpc('packages.authorizeUpload', packageInput);
    const uploaded = await request(fresh.uploadPath.slice('/ivy'.length), { method: 'PUT', headers: { Authorization: 'Bearer ' + fresh.uploadToken }, body: archive });
    assert.equal(uploaded.status, 201, await uploaded.clone().text());
    assert.equal((await uploaded.json()).archiveHash, packageInput.archiveHash);
    const catalogResponse = await request('/api/v1/packages/catalog?after=0', { headers: authenticated });
    assert.equal(catalogResponse.status, 200); const packageCatalog = await catalogResponse.json();
    assert.deepEqual(packageCatalog.packages.map(item => item.componentId), [manifest.componentId]);
    assert.ok(catalogResponse.headers.get('x-ivy-object-id')); assert.ok(Number(catalogResponse.headers.get('x-ivy-object-revision')) > 0);
    const unchanged = await request('/api/v1/packages/catalog?after=' + packageCatalog.revision, { headers: authenticated });
    assert.equal(unchanged.status, 204); assert.equal(unchanged.headers.get('etag'), '"' + packageCatalog.revision + '"');
    const artifactPath = '/api/v1/packages/reference-fixture/1.0.0/artifact';
    const downloaded = await request(artifactPath, { headers: authenticated });
    assert.equal(downloaded.status, 200); assert.equal(downloaded.headers.get('content-type'), 'application/gzip');
    assert.equal(Number(downloaded.headers.get('content-length')), archive.length);
    assert.equal(downloaded.headers.get('etag'), '"' + packageInput.archiveHash + '"');
    assert.deepEqual(Buffer.from(await downloaded.arrayBuffer()), archive);
    const abandoned = await request(artifactPath, { headers: authenticated }); await abandoned.body.cancel();
    assert.equal((await rpc('system.status')).ready, true);
    const logout = await request('/logout', { method: 'POST', headers: { Origin: new URL(config.publicBaseUrl).origin, Cookie: cookie.split(';')[0] } });
    assert.equal(logout.status, 303); assert.match(logout.headers.get('set-cookie'), /Max-Age=0/);
    const revoked = await request('/api/v1/rpc', { method: 'POST', headers: { Origin: new URL(config.publicBaseUrl).origin, Cookie: cookie.split(';')[0], 'Content-Type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 'revoked', method: 'system.status', params: {} }) });
    assert.equal(revoked.status, 401);
    await mkdir(join(config.dataRoot, 'ui-releases', '.incoming'), { recursive: true });
    await mkdir(join(config.dataRoot, 'ui-releases', '.staging'), { recursive: true });
    await writeFile(join(config.dataRoot, 'ui-releases', '.incoming', 'excluded'), 'In-flight upload');
    await writeFile(join(config.dataRoot, 'ui-releases', '.staging', 'excluded'), 'In-flight publish');
    await stop();
    await utimes(join(root, 'backups', firstSnapshot), new Date(0), new Date(0));
    const orphan = 'hive-2000-01-01T00-00-00.000Z-00000000-0000-0000-0000-000000000000.partial';
    await writeFile(join(root, 'backups', orphan), 'Interrupted snapshot'); await utimes(join(root, 'backups', orphan), new Date(0), new Date(0));
    await writeFile(join(root, 'backups', 'unrelated.txt'), 'Keep unrelated files');
    const second = await start(); assert.notEqual(second.bootId, first.bootId);
    // The preceding shutdown request is still on disk, but belongs to the old
    // boot. The new process must retain storage and ignore that stale control.
    await delay(650);
    const secondStatus = await rpc('system.status');
    assert.equal(secondStatus.callerPrincipalId, 'reference-user');
    assert.equal(secondStatus.runtimeEpoch, firstStatus.runtimeEpoch);
    const retainedPage = await rpc('wiki.read', { objectId: createdPage.object.id });
    assert.equal(retainedPage.object.currentRevision, 3); assert.equal(retainedPage.content.value, 'ReferencePagingWord revision 3.');
    assert.deepEqual(await rpc('wiki.create', createPage), createdPage, 'Mutation receipts survive the process restart.');
    const renewed = await exchange(refreshGrant(rotated.refresh_token)); assert.equal(renewed.status, 200); const afterRestart = await renewed.json();
    assert.equal((await mcp('tools/call', { name: 'hive_status', arguments: {} }, true, afterRestart.access_token)).structuredContent.result.callerPrincipalId, 'reference-user');
    assert.equal((await exchange(refreshGrant(tokens.refresh_token))).status, 401, 'Reusing an earlier token revokes its durable family.');
    assert.equal((await exchange(refreshGrant(afterRestart.refresh_token))).status, 401, 'A revoked family cannot rotate again.');
    const secondSnapshot = await poll(async () => (await snapshots()).find(name => name !== firstSnapshot));
    const secondRecord = await json(join(root, 'backups', secondSnapshot + '.json'));
    assert.equal(secondRecord.bytesHash, hash(await readFile(join(root, 'backups', secondSnapshot))));
    const uiSnapshot = join(root, 'backups', secondRecord.uiFilesDirectory);
    const copiedUi = await readdir(uiSnapshot, { recursive: true });
    assert.ok(copiedUi.some(name => name.endsWith('index.html')));
    assert.ok(copiedUi.every(name => !name.startsWith('.incoming') && !name.startsWith('.staging')));
    const { DatabaseSync } = await import('node:sqlite');
    const database = new DatabaseSync(join(root, 'backups', secondSnapshot), { readOnly: true });
    try {
      assert.equal(Object.values(database.prepare('PRAGMA quick_check').get())[0], 'ok');
      assert.equal(database.prepare('SELECT MAX(revision) AS revision FROM revisions WHERE object_id=?').get(createdPage.object.id).revision, 3);
    } finally { database.close(); }
    assert.equal(await readFile(join(root, 'backups', 'unrelated.txt'), 'utf8'), 'Keep unrelated files');
    await assert.rejects(stat(join(root, 'backups', orphan)), { code: 'ENOENT' });

    const rejectedToken = randomUUID(), configPath = join(root, 'instance.json');
    const changed = structuredClone(config); changed.settings.listenPort = changed.settings.listenPort === 65535 ? 65534 : changed.settings.listenPort + 1;
    changed.settings.credentials = [{ principalId: 'reference-user', token: rejectedToken }];
    await writeFile(configPath, JSON.stringify(changed));
    await poll(async () => (await diagnostics('configuration_reload_failed'))?.status === 'current');
    assert.equal(await credentialStatus(token), 200); assert.equal(await credentialStatus(rejectedToken), 401);
    await writeFile(configPath, '{'); await delay(2200);
    assert.equal(await credentialStatus(token), 200);
    await writeFile(configPath, JSON.stringify(config));
    await poll(async () => (await diagnostics('configuration_reload_failed'))?.status === 'resolved');

    const oldToken = token, replacement = randomUUID();
    const oldSocket = new WebSocket(config.publicBaseUrl.replace(/^http/, 'ws') + '/ws', { headers: { Authorization: 'Bearer ' + oldToken } });
    try {
      await once(oldSocket, 'open'); const retired = once(oldSocket, 'close');
      config.settings.credentials = [{ principalId: 'reference-user', token: replacement }];
      await writeFile(configPath, JSON.stringify(config));
      await poll(async () => await credentialStatus(replacement) === 200);
      token = replacement;
      assert.equal(await credentialStatus(oldToken), 401);
      await Promise.race([retired, delay(10000, undefined, { ref: false }).then(() => { throw Error('Old credential socket was not retired.'); })]);
    } finally { oldSocket.terminate(); }
    await stop();
    await utimes(join(root, 'backups', secondSnapshot), new Date(0), new Date(0));
    await start();
    const thirdSnapshot = await poll(async () => (await snapshots()).find(name => name !== firstSnapshot && name !== secondSnapshot));
    await poll(async () => { const retained = await snapshots(); return retained.length === 2 && !retained.includes(firstSnapshot); });
    await assert.rejects(stat(join(root, 'backups', firstSnapshot)), { code: 'ENOENT' });
    await stop();
    const retainedSnapshots = await snapshots(); await start(); await delay(2500);
    assert.deepEqual(await snapshots(), retainedSnapshots, 'Restarting with a recent complete snapshot must preserve the backup schedule.');
    assert.ok((await snapshots()).includes(thirdSnapshot)); assert.equal((await rpc('system.status')).callerPrincipalId, 'reference-user');
    await stop();
  } finally {
    if (child && child.exitCode === null) {
      try { await stop(); } catch { child.kill(); await closed; }
    }
    const actual = await realpath(root), local = relative(temporary, actual);
    assert.ok(!isAbsolute(local) && local.startsWith('ivy-hive-reference-') && !local.includes('/') && !local.includes('\\'));
    await rm(actual, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
  }
});
