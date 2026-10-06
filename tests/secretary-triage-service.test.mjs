import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { discover, callBound } from '../dist/packages/sdk/src/node.js';
import { atomicJson } from '../dist/packages/sdk/src/host.js';
import { startSecretary } from '../dist/services/secretary/src/main.js';
import { triageFixture } from './secretary-triage-fixture.mjs';

test('Secretary initializes assignments and does not automatically triage an unrelated capture across service generations', { timeout: 70000 }, async t => {
  const f = await triageFixture(t); await f.startNativeTransport(); await f.f.service.stop();
  const root = await mkdtemp(join(tmpdir(), 'ivy-secretary-assignment-service-')), dataRoot = join(root, 'data'); await mkdir(dataRoot);
  const build = JSON.parse(await readFile('dist/build-info.json', 'utf8')), serviceNodeId = f.f.settings.identity.serviceNodeId;
  const config = { schemaVersion: 1, hostId: 'fixture', instanceId: 'assignment-fixture', serviceNodeId, componentId: 'secretary',
    publicBaseUrl: process.env.IVY_TEST_HIVE_URL, credential: process.env.IVY_TEST_HIVE_CREDENTIAL, dataRoot, artifactRoot: resolve('.'),
    buildId: build.buildId, version: build.version, settings: { ...f.f.settings, pollMs: 100 } };
  const path = join(root, 'instance.json'); await atomicJson(path, config);
  await writeFile(join(root, 'config.json'), JSON.stringify({ schemaVersion: 1, triage: f.settings }), { mode: 0o600 });
  const handles = []; t.after(async () => { for (const handle of handles) await handle.close(); await rm(root, { recursive: true, force: true }); });
  const start = async () => { const handle = await startSecretary(path); handles.push(handle); await handle.service.waitReady({ timeoutMs: 20000 }); return handle; };
  const list = async () => callBound(f.f.client, await discover(f.f.client, 'secretary.listAssignments', { serviceNodeId }), { expectedScope: f.f.settings.identity.scope });

  const first = await start(), initial = await list();
  assert.equal(initial.assignments.length, 0);
  const capture = f.f.captureRequest();
  const saved = await callBound(f.f.client, await discover(f.f.client, 'secretary.capture', { serviceNodeId }), capture, capture.operationId);
  assert.equal(saved.phase, 'succeeded');
  await delay(350);
  assert.equal(f.state.dispatches.length, 0);
  assert.equal((await f.f.client.request('objects.read', saved.effect)).content.value.decision, null);

  const previous = first.engine;
  first.service.connection.close();
  await first.service.waitReady({ timeoutMs: 20000 });
  assert.notEqual(first.engine, previous);
  assert.throws(() => previous.store.db.prepare('SELECT 1'), /not open|closed/i);
  assert.deepEqual((await list()).assignments.map(entry => entry.pin), initial.assignments.map(entry => entry.pin));

  await first.close(); handles.splice(handles.indexOf(first), 1);
  await start();
  const restored = await list();
  assert.deepEqual(restored.assignments.map(entry => entry.pin), initial.assignments.map(entry => entry.pin));
  assert.equal(f.state.dispatches.length, 0);
});
