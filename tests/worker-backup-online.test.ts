import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { Worker } from 'node:worker_threads';
import { WorkerClient } from '../services/hive/src/worker-client.js';
import { digest } from '../packages/contracts/src/canonical.js';

test('backup progress extends only its own deadline while a backup without progress retires its worker', { timeout: 20000 }, async t => {
  for (const progressing of [false, true]) {
    const root = await mkdtemp(join(tmpdir(), 'ivy-backup-progress-')), hook = join(root, 'backup-hook.mjs');
    await writeFile(hook, `import { HiveStore } from ${JSON.stringify(new URL('../services/hive/src/store.js', import.meta.url).href)};
import { setTimeout as delay } from 'node:timers/promises';
const original = HiveStore.prototype.backupTo;
HiveStore.prototype.backupTo = async function (path, progress) {
  if (!${progressing}) return new Promise(() => {});
  for (let i=0;i<25;i++) { progress(); await delay(100); }
  return original.call(this,path,progress);
};`);
    const worker = new WorkerClient({ filename: join(root, 'hive.sqlite'), publicBaseUrl: 'http://127.0.0.1/ivy', version: 'test', buildId: digest('test'), credentials: [] },
      (file, options) => new Worker(new URL('data:text/javascript,' + encodeURIComponent(`await import(${JSON.stringify(pathToFileURL(hook).href)}); await import(${JSON.stringify(file.href)});`)), options));
    t.after(async () => { await worker.close(); await rm(root, { recursive: true, force: true }); });
    let failures = 0; worker.onFailure = () => { failures++; }; await worker.start();
    const pending = worker.request({ action: 'backup', destination: join(root, 'copy.sqlite') }, 1500);
    if (progressing) { await pending; assert.equal(failures, 0); assert.equal(worker.ready, true); }
    else { await assert.rejects(pending, { code: 'deadline_exceeded', outcome: 'unknown' }); assert.equal(failures, 1); }
    await worker.close();
  }
});

test('a backup longer than normal request deadlines permits requests, health and credential revocation without worker restart', { timeout: 50_000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-online-backup-'));
  const hook = join(root, 'slow-backup.mjs');
  const gate = new Int32Array(new SharedArrayBuffer(4));
  // Hold the completed real snapshot/hash in this worker only. No product delay/test switch.
  await writeFile(hook, `import { HiveStore } from ${JSON.stringify(new URL('../services/hive/src/store.js', import.meta.url).href)};
import { parentPort, workerData } from 'node:worker_threads';
const original = HiveStore.prototype.backupTo; HiveStore.prototype.backupTo = async function (...args) { const result = await original.apply(this, args); parentPort.postMessage({ slowBackup: true }); await Atomics.waitAsync(new Int32Array(workerData.testBackupGate), 0, 0).value; return result; };`);
  const credential = { principalId: 'original', digest: digest('original') };
  let entered!: () => void;
  const backupEntered = new Promise<void>(resolve => { entered = resolve; });
  const worker = new WorkerClient({ filename: join(root, 'hive.sqlite'), publicBaseUrl: 'http://127.0.0.1/ivy', version: 'test', buildId: digest('test'), credentials: [credential] },
    (file, options) => {
      const bootstrap = `await import(${JSON.stringify(pathToFileURL(hook).href)}); await import(${JSON.stringify(file.href)});`;
      const instance = new Worker(new URL('data:text/javascript,' + encodeURIComponent(bootstrap)), { ...options, workerData: { ...options.workerData, testBackupGate: gate.buffer } });
      instance.on('message', message => { if (message.slowBackup) entered(); }); return instance;
    });
  t.after(async () => { Atomics.store(gate, 0, 1); Atomics.notify(gate, 0); await worker.close(); await rm(root, { recursive: true, force: true }); });
  let failures = 0; worker.onFailure = () => { failures++; }; await worker.start();
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const backup = worker.request<{ pages: number; bytesHash: string }>({ action: 'backup', destination: join(root, 'snapshot.sqlite') }, 45_000);
  await backupEntered;
  const response = await worker.request<{ kind: string }>({ action: 'execute', context: { credentialDigest: credential.digest, principalId: credential.principalId, transport: 'http' }, request: { jsonrpc: '2.0', id: 'status', method: 'system.status', params: {} } });
  assert.equal(response.kind, 'result');
  assert.deepEqual(await worker.request({ action: 'health' }, 1000, false), { ready: true });
  await worker.request({ action: 'credentials.configure', credentials: [] });
  await assert.rejects(worker.request({ action: 'authenticate', context: { credentialDigest: credential.digest } }), { code: 'unauthenticated' });
  await assert.rejects(worker.request({ action: 'backup', destination: join(root, 'other.sqlite') }), { code: 'limit_exceeded' });
  // Cross the normal 30-second deadline without spending 35 seconds asleep.
  t.mock.timers.tick(35_000);
  Atomics.store(gate, 0, 1); Atomics.notify(gate, 0);
  const saved = await backup;
  assert.ok(saved.pages > 0); assert.match(saved.bytesHash, /^sha256:[a-f0-9]{64}$/);
  assert.deepEqual(await worker.request({ action: 'health' }), { ready: true }); assert.equal(failures, 0); assert.equal(worker.ready, true);
});
