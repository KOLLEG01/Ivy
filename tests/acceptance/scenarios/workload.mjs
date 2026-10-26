// Run from the repository: node tests/acceptance/scenarios/workload.mjs --root .local/workload/<name>
// --smoke checks the harness at a smaller scale; it never satisfies the normative workload.
// --resume keeps deterministic original mutations after a failed/interrupted attempt.
import assert from 'node:assert/strict';
import { createHash, randomUUID, randomBytes } from 'node:crypto';
import { fork, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:net';
import { createReadStream } from 'node:fs';
import { mkdir, readFile, writeFile, rename, readdir, copyFile, stat, statfs, cp } from 'node:fs/promises';
import { resolve, join, sep, relative, dirname } from 'node:path';
import { hostname, cpus, totalmem, freemem, release } from 'node:os';
import { DatabaseSync } from 'node:sqlite';
import { parseArgs } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from '@playwright/test';
import { HiveClient, deriveOperationId, newOperationId } from '../../../dist/packages/sdk/src/client.js';
import { ServiceClient } from '../../../dist/packages/sdk/src/service.js';
import { canonical, digest } from '../../../dist/packages/contracts/src/canonical.js';
import { requestPacer } from '../support/request-pacing.mjs';
import { artifactRoot, nativeExecutable } from '../../../tools/testing/native-service.mjs';

const args = parseArgs({ options: { root: { type: 'string' }, 'hive-artifact': { type: 'string' }, smoke: { type: 'boolean' }, resume: { type: 'boolean' } }, strict: true }).values;
assert.ok(args.root, 'An explicit isolated root is required.');
const root = resolve(args.root), allowed = resolve('.local/workload');
assert.ok(root.startsWith(allowed + sep) && root !== allowed, 'Workload data must stay under this checkout/.local/workload.');
const profile = args.smoke ? { objects: 200, revisions: 2000, bytes: 16 * 1024 ** 2 } : { objects: 10_000, revisions: 100_000, bytes: 1024 ** 3 };
const childFile = resolve('tests/acceptance/fixtures/workload-hive-child.mjs');
const now = () => new Date().toISOString();
async function atomic(path, value) { const temp = path + '.' + randomUUID() + '.tmp'; await writeFile(temp, JSON.stringify(value, null, 2) + '\n', { flag: 'wx', mode: 0o600 }); await rename(temp, path); }
async function fileHash(path) { const h = createHash('sha256'); for await (const bytes of createReadStream(path)) h.update(bytes); return 'sha256:' + h.digest('hex'); }
async function inventory(dir) {
  const entries = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) entries.push(...await inventory(path));
    else { assert.ok(entry.isFile(), 'Runtime inventory cannot contain symlinks.'); entries.push([relative(resolve('.'), path).replaceAll('\\', '/'), await fileHash(path)]); }
  }
  return entries.sort(([a], [b]) => a.localeCompare(b, 'en'));
}
const hiveSource = artifactRoot('hive', args['hive-artifact']);
const nativeNames = ['ivy-hive', 'ivy-health', ...(process.platform === 'win32' ? ['ivy-job'] : [])];
const nativeFiles = nativeNames.map(name => nativeExecutable(hiveSource, name));
const runtime = [
  ...await Promise.all(nativeFiles.map(async path => [relative(resolve('.'), path).replaceAll('\\', '/'), await fileHash(path)])),
  ...(await Promise.all([resolve(hiveSource, 'dist/console'), resolve('dist/packages/contracts'), resolve('dist/packages/sdk')].map(inventory))).flat(),
];
const identity = { runtimeHash: digest(canonical(runtime)), lockHash: await fileHash('package-lock.json'), runnerHash: await fileHash('tests/acceptance/scenarios/workload.mjs'), pacingHash: await fileHash('tests/acceptance/support/request-pacing.mjs'), childHash: await fileHash(childFile) };
let seed;
if (args.resume) {
  seed = JSON.parse(await readFile(join(root, 'seed.json'), 'utf8'));
  assert.deepEqual(seed.profile, profile); assert.deepEqual(seed.identity, identity, 'Resume requires the identical runner and runtime.');
} else {
  await mkdir(allowed, { recursive: true }); await mkdir(root, { mode: 0o700 });
  const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
  const port = reservation.address().port; await new Promise((resolve, reject) => reservation.close(error => error ? reject(error) : resolve()));
  seed = { id: randomUUID(), token: randomBytes(32).toString('base64url'), profile, identity, port, createdAt: now() };
  await atomic(join(root, 'seed.json'), seed);
}
// Runtime data and the immutable native artifact need disjoint roots.
const hiveArtifact = join(root, 'artifact');
if (!args.resume) {
  for (const file of [...nativeFiles, join(hiveSource, 'dist/build-info.json')]) {
    const destination = join(hiveArtifact, relative(hiveSource, file));
    await mkdir(dirname(destination), { recursive: true }); await copyFile(file, destination, 1);
  }
  await cp(join(hiveSource, 'dist/console'), join(hiveArtifact, 'dist/console'), { recursive: true, errorOnExist: true, force: false });
}
const base = `http://127.0.0.1:${seed.port}/ivy`, origin = new URL(base).origin, principalId = 'isolated-workload';
const attempt = randomUUID(), attemptRoot = join(root, 'attempts', attempt);
await mkdir(attemptRoot, { recursive: true });
const storage = await statfs(root);
let sourceCommit = null;
try { sourceCommit = execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim(); } catch {}
const report = { schemaVersion: 1, kind: 'isolated-reference-workload', smoke: !!args.smoke, attempt, datasetId: seed.id, startedAt: now(), ok: false,
  sourceCommit, identity, build: JSON.parse(await readFile(join(hiveArtifact, 'dist/build-info.json'), 'utf8')),
  profile, hardware: { host: hostname(), platform: process.platform, arch: process.arch, osRelease: release(), node: process.version,
    cpu: cpus()[0]?.model, logicalCpus: cpus().length, memoryBytes: totalmem(), freeMemoryBytes: freemem(), filesystemType: storage.type, freeDiskBytes: storage.bavail * storage.bsize },
  limitsUnchanged: true, transport: 'authenticated SDK WebSocket writes; four cookie-authenticated Chromium Console sessions',
  measurement: 'Native Hive browser RPC round trips, including transport, queue and storage worker processing; an upper bound on server latency',
  httpPacing: { sharedIntervalMs: 25, scope: 'SDK HTTP and browser RPC starts from the shared loopback address; pacing excluded from request latency; no retry', websocketWritesPaced: false },
  scope: 'Hive reference workload, abrupt owned Hive process loss and isolated SQLite online snapshot restore; not installed host recovery, remote latency or private integration acceptance' };
await atomic(join(attemptRoot, 'runtime-files.json'), runtime);
const persist = async () => atomic(join(attemptRoot, 'report.json'), report);
const log = (phase, values = {}) => process.stdout.write(JSON.stringify({ at: now(), phase, ...values }) + '\n');
const perObject = profile.revisions / profile.objects, objects = new Map(), services = [], pages = [], browserErrors = [], externalRequests = [];
const key = index => 'acceptance/load-' + (index % 4 < 2 ? 'binary' : index % 4 === 2 ? 'text' : 'json');
const name = index => 'Load ' + String(index).padStart(5, '0');
function content(index, revision) {
  const ordinal = index * perObject + revision - 1, size = Math.floor(profile.bytes / profile.revisions) + Number(ordinal < profile.bytes % profile.revisions);
  const material = seed.id + '/' + index + '/' + revision;
  const bytes = createHash('shake256', { outputLength: size }).update(material).digest();
  if (index % 4 < 2) return { encoding: 'base64', value: bytes.toString('base64') };
  // ASCII chunks have stable byte lengths and bounded word lengths for the real full-text index.
  let text = bytes.toString('base64').slice(0, size).replace(/.{16}/g, value => value.slice(0, 15) + ' ');
  if (index % 4 === 2) return { encoding: 'text', value: text };
  const value = { index, revision, payload: '' }, overhead = Buffer.byteLength(canonical(value));
  value.payload = text.slice(0, size - overhead); return { encoding: 'json', value };
}
function bytesOf(value) { return value.encoding === 'base64' ? Buffer.from(value.value, 'base64') : Buffer.from(value.encoding === 'json' ? canonical(value.value) : value.value); }
function mutation(index, revision, objectId) {
  return { mutationId: deriveOperationId(seed.operationId, ['object', index, revision]), contractVersion: '1.0.0', content: content(index, revision), references: {},
    ...(revision === 1 ? { create: { contractKey: key(index), parentId: null, ownerObjectId: null, name: name(index) } } : { objectId, expectedRevision: revision - 1 }) };
}
function stats(values) { assert.ok(values.length); const sorted = [...values].sort((a, b) => a - b); return { count: values.length, minMs: sorted[0], p50Ms: sorted[Math.ceil(sorted.length * .5) - 1], p95Ms: sorted[Math.ceil(sorted.length * .95) - 1], maxMs: sorted.at(-1) }; }
async function until(check, timeout = 30_000) { const end = performance.now() + timeout; while (!(await check())) { assert.ok(performance.now() < end, 'Acceptance convergence deadline exceeded.'); await delay(50); } }
let child, browser, sequence = 0;
const pending = new Map();
async function launch(filename, label) {
  const config = { filename, artifactRoot: hiveArtifact, publicBaseUrl: base,
    credentials: [{ principalId, token: seed.token }], listenPort: seed.port, captureOnlineBackup: label === 'after-crash' };
  const configPath = join(attemptRoot, label + '-instance.json'); await atomic(configPath, config);
  child = fork(childFile, [configPath], { stdio: ['ignore', 'pipe', 'pipe', 'ipc'], windowsHide: true });
  let tail = '';
  for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { tail = (tail + bytes).slice(-8192); });
  child.on('exit', (code, signal) => { for (const item of pending.values()) item.reject(new Error(`Owned Hive exited (${code}/${signal}): ${tail}`)); pending.clear(); });
  const ready = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('Owned Hive startup timed out: ' + tail)), 600_000);
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('exit', (code, signal) => { clearTimeout(timer); reject(new Error(`Owned Hive failed to start (${code}/${signal}): ${tail}`)); });
    child.on('message', message => {
      if (message.ready) { clearTimeout(timer); resolve(message); return; }
      const item = pending.get(message.id); if (!item) return; pending.delete(message.id);
      if (message.error) item.reject(Object.assign(new Error(message.error.message), { code: message.error.code })); else item.resolve(message.result);
    });
  });
  return ready;
}
async function control(action, values = {}) {
  const id = ++sequence;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { pending.delete(id); reject(new Error('Owned control timed out: ' + action)); }, 610_000);
    pending.set(id, { resolve: result => { clearTimeout(timer); resolve(result); }, reject: error => { clearTimeout(timer); reject(error); } });
    child.send({ id, action, ...values });
  });
}
async function stop(crash = false) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const exit = once(child, 'exit');
  await control(crash ? 'crash' : 'close');
  await exit;
}
const client = new HiveClient(base, { credential: seed.token });
const paceHttp = requestPacer();
const httpRequest = client.request.bind(client);
client.request = async (...args) => { await paceHttp(); return httpRequest(...args); };
async function nodes() {
  const found = await client.request('serviceNodes.list', { limit: 50 });
  assert.equal(found.items.length, 5); assert.equal(found.nextCursor, null);
  assert.ok(found.items.every(node => node.connected && node.synced && node.ready));
  return found.items.map(node => ({ serviceNodeId: node.serviceNodeId, connected: node.connected, synced: node.synced, ready: node.ready }));
}
const liveGenerations = () => services.map((service, n) => ({ serviceNodeId: 'workload-' + n, generation: service.connection.generation }));
async function browserRpc(page, method, params) {
  await paceHttp();
  return page.evaluate(async ({ method, params }) => {
    const start = performance.now();
    const response = await fetch('./api/v1/rpc', { method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: crypto.randomUUID(), method, params }), credentials: 'same-origin' });
    const frame = await response.json();
    if (!response.ok || frame.error) throw new Error(JSON.stringify({ status: response.status, error: frame.error }));
    return { result: frame.result, elapsedMs: performance.now() - start };
  }, { method, params });
}
async function measure(label) {
  await nodes();
  const samples = await Promise.all(pages.map(async page => {
    const durations = { 'objects.list': [], 'objects.query': [] }, cursors = {};
    let listPages = 0;
    for (let n = 0; n < 120; n++) {
      const method = n % 2 ? 'objects.query' : 'objects.list';
      // The Console may create catalog/settings roots. Measure full fifty-item pages;
      // verifyApi separately traverses every root, including the partial tail.
      if (method === 'objects.list' && listPages === Math.floor(profile.objects / 50)) { delete cursors[method]; listPages = 0; }
      const params = method === 'objects.list' ? { parentId: null, limit: 50 } : { contractKey: 'acceptance/load-binary', select: ['object.name', 'object.path'], limit: 50 };
      if (cursors[method]) params.cursor = cursors[method];
      const { result, elapsedMs } = await browserRpc(page, method, params);
      assert.equal(result.items.length, 50); assert.notEqual(result.nextCursor, cursors[method] ?? undefined);
      cursors[method] = result.nextCursor; durations[method].push(elapsedMs);
      if (method === 'objects.list') listPages++;
    }
    return durations;
  }));
  const observed = await control('samples');
  const combined = Object.fromEntries(['objects.list', 'objects.query'].map(method => [method, samples.flatMap(sample => sample[method])]));
  report[label] = { server: Object.fromEntries(Object.entries(combined).map(([method, values]) => [method, stats(values)])),
    clients: samples.map(sample => Object.fromEntries(Object.entries(sample).map(([method, values]) => [method, stats(values)]))),
    nativePid: observed.nativePid, coordinatorMemory: observed.coordinatorMemory, nodes: await nodes() };
  for (const method of ['objects.list', 'objects.query']) assert.ok(report[label].server[method].count >= 240);
  await atomic(join(attemptRoot, label + '-round-trip-samples.json'), combined); await persist(); log(label, { server: report[label].server });
}
async function verifyApi() {
  let cursor, count = 0; const seen = new Set();
  do {
    const page = await client.request('objects.list', { parentId: null, limit: 200, ...(cursor ? { cursor } : {}) });
    for (const item of page.items) {
      if (!item.contractKey.startsWith('acceptance/load-')) continue;
      assert.ok(!seen.has(item.id)); seen.add(item.id); assert.equal(item.currentRevision, perObject); count++;
    }
    assert.notEqual(page.nextCursor, cursor); cursor = page.nextCursor;
  } while (cursor);
  assert.equal(count, profile.objects);
  let through = 0, events = 0, previousSequence = 0;
  do {
    const batch = await client.request('events.read', { afterSequence: through, filter: { topics: ['hive.object.changed'] }, limit: 100 });
    for (const event of batch.items) {
      if (!seen.has(event.payload.objectId)) continue;
      assert.ok(event.sequence > previousSequence); previousSequence = event.sequence; events++;
    }
    assert.ok(batch.throughSequence > through || !batch.hasMore); through = batch.throughSequence;
    if (!batch.hasMore) break;
  } while (true);
  assert.equal(events, profile.revisions);
  // Both ends of history across all content kinds, plus original mutation receipts.
  for (const index of [...new Set([0, 1, 2, 3, Math.floor(profile.objects / 2), profile.objects - 1])]) {
    const objectId = objects.get(index);
    const history = await client.request('objects.history', { objectId, limit: 50 }); assert.equal(history.items.length, perObject); assert.equal(history.nextCursor, null);
    for (const revision of [1, perObject]) {
      const result = await client.request('objects.read', { objectId, revision }); assert.deepEqual(result.content, content(index, revision));
      const receipt = await client.request('objects.write', mutation(index, revision, objectId)); assert.equal(receipt.object.id, objectId); assert.equal(receipt.revision.revision, revision);
    }
  }
  return { objects: count, events, sampledHistoryObjects: 6, originalMutationReplay: true };
}
function audit(filename) {
  const start = performance.now(), db = new DatabaseSync(filename, { readOnly: true, timeout: 1000 });
  try {
    db.exec('PRAGMA query_only=ON; BEGIN');
    assert.deepEqual(db.prepare('PRAGMA quick_check').all().map(row => Object.values(row)[0]), ['ok']);
    assert.deepEqual(db.prepare('PRAGMA foreign_key_check').all(), []);
    assert.equal(db.prepare("SELECT count(*) AS n FROM objects WHERE contract_key LIKE 'acceptance/load-%'").get().n, profile.objects);
    assert.equal(db.prepare("SELECT count(*) AS n FROM events e JOIN objects o ON o.id=json_extract(e.payload,'$.objectId') WHERE o.contract_key LIKE 'acceptance/load-%'").get().n, profile.revisions);
    const hash = createHash('sha256'); let count = 0, bytes = 0;
    let previousSequence = 0;
    for (const row of db.prepare('SELECT o.name,r.*,e.sequence,e.mutation_id,e.payload FROM revisions r JOIN objects o ON o.id=r.object_id JOIN events e ON json_extract(e.payload,\'$.objectId\')=r.object_id AND json_extract(e.payload,\'$.revision\')=r.revision WHERE o.contract_key LIKE \'acceptance/load-%\' ORDER BY e.sequence').iterate()) {
      const index = Number(row.name.slice(5)), expected = bytesOf(content(index, row.revision));
      const actual = row.encoding === 'base64' ? Buffer.from(row.content) : Buffer.from(row.content);
      assert.equal(actual.compare(expected), 0); assert.equal(row.content_hash, digest(expected)); assert.equal(row.byte_length, expected.length);
      assert.equal(row.mutation_id, deriveOperationId(seed.operationId, ['object', index, row.revision]));
      assert.equal(JSON.parse(row.payload).revision, row.revision);
      assert.ok(row.sequence > previousSequence); previousSequence = row.sequence; count++; bytes += expected.length;
      hash.update(canonical([row.object_id, row.revision, row.content_hash, row.sequence, row.mutation_id]) + '\n');
    }
    assert.equal(count, profile.revisions); assert.equal(bytes, profile.bytes);
    assert.equal(db.prepare("SELECT count(*) AS n FROM mutations m JOIN objects o ON o.id=json_extract(m.result_json,'$.object.id') WHERE m.result_kind='snapshot' AND o.contract_key LIKE 'acceptance/load-%'").get().n, profile.revisions);
    return { objects: profile.objects, revisions: count, events: count, contentBytes: bytes, logicalHash: 'sha256:' + hash.digest('hex'), elapsedMs: performance.now() - start };
  } finally { if (db.isTransaction) db.exec('ROLLBACK'); db.close(); }
}
try {
  assert.ok(report.hardware.freeDiskBytes >= profile.bytes * 6, 'Insufficient space for the isolated workload and retained copies.');
  log('starting', { root, profile }); const startup = performance.now();
  await launch(join(root, 'hive-data', 'hive.sqlite'), 'original'); report.startupMs = performance.now() - startup;
  if (!seed.operationId) {
    seed.operationId = await newOperationId(client, seed.id);
    await atomic(join(root, 'seed.json'), seed);
  }
  for (const [kind, mediaType] of [['binary', 'application/octet-stream'], ['text', 'text/plain'], ['json', 'application/json']]) {
    await client.request('contracts.register', { mutationId: deriveOperationId(seed.operationId, ['contract', kind]), definition: { key: 'acceptance/load-' + kind, version: '1.0.0', owner: { kind: 'agent' }, mediaType,
      retention: { objects: { mode: 'retain' }, revisions: { mode: 'all' } },
      specMarkdown: 'Synthetic mixed-content M7 reference workload; no private data.', ...(kind === 'json' ? { jsonSchema: { type: 'object', properties: { index: { type: 'integer' }, revision: { type: 'integer' }, payload: { type: 'string' } }, required: ['index', 'revision', 'payload'], additionalProperties: false } } : {}) } });
  }
  for (let n = 0; n < 5; n++) {
    const service = new ServiceClient({ publicBaseUrl: base, credential: () => seed.token,
      identity: { serviceNodeId: 'workload-' + n, hostId: 'workload-reference', serviceName: 'workload', version: '1.0.0', buildId: report.build.buildId, hiveProtocol: 1 },
      registry: () => ({ namespaces: [], contracts: [], requiredContracts: [] }), handlers: {}, reconcile: async () => {} });
    service.start(); services.push(service);
  }
  await Promise.all(services.map(service => service.waitReady())); report.initialNodes = await nodes();
  browser = await chromium.launch({ ...(process.platform === 'win32' ? { channel: 'msedge' } : {}), headless: true, timeout: 30_000 });
  report.browser = browser.version();
  for (let n = 0; n < 4; n++) {
    const context = await browser.newContext();
    await context.route('**/*', route => { if (new URL(route.request().url()).origin === origin) return route.continue(); externalRequests.push(route.request().url()); return route.abort(); });
    const page = await context.newPage(); page.on('pageerror', error => browserErrors.push(error.message));
    await page.goto(base + '/'); await page.getByLabel('Hive credential').fill(seed.token); await page.getByRole('button', { name: 'Sign in', exact: true }).click();
    await page.getByRole('heading', { name: 'UIs', exact: true }).waitFor(); pages.push(page);
  }
  const cookies = await Promise.all(pages.map(async page => (await page.context().cookies()).find(cookie => cookie.name === 'ivy_session')?.value));
  assert.ok(cookies.every(Boolean)); assert.equal(new Set(cookies).size, 4); report.distinctBrowserSessions = 4;
  const population = performance.now(); let completed = 0, writes = 0;
  await Promise.all(services.map(async (service, lane) => {
    for (let index = lane; index < profile.objects; index += 5) {
      let objectId, last = 0;
      if (args.resume) {
        try { const found = await service.connection.request('objects.stat', { path: '/' + name(index) }); objectId = found.id; last = found.currentRevision; assert.ok(last <= perObject); }
        catch (error) { if (error.code !== 'not_found') throw error; }
      }
      // Replay the last accepted identity as well as any unfinished next revision on resume.
      for (let revision = Math.max(1, last); revision <= perObject; revision++) {
        const result = await service.connection.request('objects.write', mutation(index, revision, objectId));
        assert.equal(result.revision.revision, revision); assert.equal(result.revision.contentHash, digest(bytesOf(content(index, revision))));
        objectId = result.object.id; writes++;
      }
      objects.set(index, objectId); completed++;
      if (completed % 100 === 0) { report.population = { completed, writes, elapsedMs: performance.now() - population }; await persist(); log('population', report.population); }
    }
  }));
  report.population = { completed, writes, elapsedMs: performance.now() - population }; await persist();
  report.initialApi = await verifyApi(); await measure('loadedReads');
  await nodes(); const oldNodes = liveGenerations(), oldConnections = services.map(service => service.connection), lost = performance.now(); await stop(true);
  await until(() => services.every(service => !service.ready));
  const dependencyStart = performance.now(); await launch(join(root, 'hive-data', 'hive.sqlite'), 'after-crash'); const dependencyReady = performance.now();
  await until(() => services.every(service => service.ready)); await nodes(); const recovered = liveGenerations();
  // Generation numbers belong to one network owner and can restart at one.
  // Prove replacement and a fresh accepted handshake on every SDK connection.
  for (const [index, service] of services.entries()) {
    assert.notEqual(service.connection, oldConnections[index]);
    assert.equal(oldConnections[index].signal.aborted, true);
    const heartbeat = await service.connection.request('service.heartbeat', { ready: true, diagnostics: [] });
    assert.equal(heartbeat.generation, recovered[index].generation);
    assert.ok(Number.isSafeInteger(heartbeat.generation) && heartbeat.generation > 0);
  }
  report.recovery = { processLossToReadyMs: performance.now() - lost, hiveRestartMs: dependencyReady - dependencyStart,
    dependenciesReadyToRegistryMs: performance.now() - dependencyReady, nodes: recovered, previousNodes: oldNodes,
    replacedConnections: oldConnections.length, api: await verifyApi() };
  for (const page of pages) { await page.reload(); await page.getByRole('heading', { name: 'UIs', exact: true }).waitFor(); }
  await measure('recoveredReads');
  const backupFile = join(attemptRoot, 'online-backup.sqlite'), backupStart = performance.now();
  report.backup = { ...(await control('backup', { destination: backupFile })), elapsedMs: performance.now() - backupStart };
  assert.equal(await fileHash(backupFile), report.backup.bytesHash); report.backup.bytes = (await stat(backupFile)).size;
  report.snapshotAudit = audit(backupFile); await persist();
  await Promise.all(services.map(service => service.stop())); await stop();
  const restoreStart = performance.now(), restoredFile = join(attemptRoot, 'restore-data', 'hive.sqlite');
  await mkdir(dirname(restoredFile));
  await copyFile(backupFile, restoredFile, 1); assert.equal(await fileHash(restoredFile), report.backup.bytesHash);
  report.restoreAudit = audit(restoredFile); assert.equal(report.restoreAudit.logicalHash, report.snapshotAudit.logicalHash);
  await launch(restoredFile, 'restored'); report.restoredApi = await verifyApi();
  for (const page of pages) { await page.reload(); await page.getByRole('heading', { name: 'UIs', exact: true }).waitFor(); }
  report.restore = { elapsedMs: performance.now() - restoreStart, sqliteBytes: report.backup.bytes, restoredContentBytes: report.restoreAudit.contentBytes, originalBrowserSessionsRecovered: 4 };
  assert.deepEqual(browserErrors, []); assert.deepEqual(externalRequests, []);
  report.targets = { fiftyItemReadP95Below250ms: ['loadedReads', 'recoveredReads'].every(phase => Object.values(report[phase].server).every(value => value.p95Ms < 250)),
    registryWithin30s: report.recovery.dependenciesReadyToRegistryMs <= 30_000, hiveRecoveryWithin10min: report.recovery.processLossToReadyMs <= 600_000,
    isolatedHiveRestoreWithin10min: report.restore.elapsedMs <= 600_000 };
  assert.ok(Object.values(report.targets).every(Boolean), 'A measured acceptance timing target was missed.');
  report.ok = true; log('passed', { targets: report.targets });
} catch (error) { report.error = { code: error.code ?? error.name, message: error.message, stack: error.stack }; log('failed', { error: report.error }); process.exitCode = 1; }
finally {
  await browser?.close().catch(() => {}); await Promise.allSettled(services.map(service => service.stop()));
  try { await stop(); } catch (error) { report.cleanupError = error.message; child?.kill('SIGKILL'); process.exitCode = 1; report.ok = false; }
  report.finishedAt = now(); await persist(); log('evidence', { report: join(attemptRoot, 'report.json'), ok: report.ok });
}
