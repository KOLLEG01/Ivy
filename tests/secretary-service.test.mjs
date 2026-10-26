import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { DatabaseSync } from 'node:sqlite';
import { HiveClient, discover, callBound, newOperationId } from '../dist/packages/sdk/src/node.js';
import { atomicJson, validateHost, validateComponent } from '../dist/packages/sdk/src/host.js';
import { settingsFor, rootObject, message, assessment } from './secretary-fixture.mjs';
import { startNativeService, nativeExecutable, artifactRoot } from '../tools/testing/native-service.mjs';

test('a separate Secretary process exposes actual tools, retains source and decision through kill/restart and honors exact host shutdown', { timeout: 70000 }, async t => {
  const base = process.env.IVY_TEST_HIVE_URL, credential = process.env.IVY_TEST_HIVE_CREDENTIAL;
  assert.ok(base && credential); assert.equal(new URL(base).hostname, '127.0.0.1'); const client = new HiveClient(base, { credential }); assert.equal((await client.request('system.status', {})).version, process.env.IVY_TEST_HIVE_VERSION);
  const root = await rootObject(client), directory = await mkdtemp(join(tmpdir(), 'ivy-secretary-process-')), id = randomUUID(), serviceNodeId = 'secretary-process-' + id;
  const artifact = artifactRoot('secretary'), settings = settingsFor(root, serviceNodeId), build = JSON.parse(await readFile(join(artifact, 'dist/build-info.json'), 'utf8'));
  const config = { schemaVersion: 1, hostId: 'fixture', instanceId: id, serviceNodeId, componentId: 'secretary', publicBaseUrl: base, dataRoot: join(directory, 'data'), artifactRoot: artifact, buildId: build.buildId, version: build.version, credential, settings };
  // Historical triage configuration remains readable, with no automatic worker.
  await atomicJson(join(directory, 'config.json'), { schemaVersion: 1, triage: {
    schemaVersion: 1, target: { serviceNodeId: 'unavailable-triage-agent', hostId: 'fixture', nativeVersion: '0.159.2',
      nativeExecutableHash: 'sha256:' + '0'.repeat(64), catalogHash: 'sha256:' + '1'.repeat(64) },
    model: 'historical-model', effort: 'high', instructions: 'Retain historical intent.', threadCwd: directory, maximumConcurrent: 1,
  } });
  validateHost('InstanceConfig', config);
  const manifest = JSON.parse(await readFile('services/secretary/deploy.json', 'utf8'));
  validateComponent(manifest);
  let owner, child, stopped, launch, output = ''; const launches = [];
  const start = async () => {
    owner = await startNativeService(config, { artifact }); child = owner.child; launch = owner.launchId;
    launches.push({ pid: child.pid, launch }); stopped = once(child, 'exit'); child.stdout.on('data', bytes => { output = (output + bytes).slice(-65536); }); child.stderr.on('data', bytes => { output = (output + bytes).slice(-65536); });
  };
  t.after(async () => { try { await owner?.close(); } finally { await rm(directory, { recursive: true, force: true }); } });
  const until = async (observe, label) => {
    const end = Date.now() + 20000; while (Date.now() < end) { if (child.exitCode !== null || child.signalCode !== null) throw Error('Secretary exited before ' + label + ': ' + output); const value = await observe(); if (value) return value; await new Promise(resolve => setTimeout(resolve, 50)); }
    throw Error('Secretary did not reach ' + label + ': ' + output);
  };
  const ready = () => until(async () => { try { const value = await client.request('serviceNodes.get', { serviceNodeId }); const health = await readFile(join(config.dataRoot, 'health.json'), 'utf8').then(JSON.parse).catch(() => null); return value.ready && health?.ready && health.pid === child.pid ? health : null; } catch (error) { if (error.code === 'not_found') return null; throw error; } }, 'live process readiness');
  const call = async (name, args, operationId) => callBound(client, await discover(client, 'secretary.' + name, { serviceNodeId }), args, operationId);
  await start(); await ready();
  const binding = await call('binding', {});
  assert.equal(binding.serviceNodeId, serviceNodeId);
  assert.equal(binding.hostId, 'fixture');
  assert.equal(binding.available, true);
  assert.deepEqual(binding.expectedScope, settings.identity.scope);
  assert.match(binding.bindingHash, /^sha256:[0-9a-f]{64}$/);
  const initialAssignments = await call('listAssignments', { expectedScope: settings.identity.scope });
  assert.equal(initialAssignments.assignments.length, 0);
  const media = { action: 'capture', operationId: await newOperationId(client), expectedScope: settings.identity.scope,
    sourceId: 'personal-inbox', message: message({ attachments: 'expected', text: 'The unavailable attachment must remain unassessed.' }) };
  const pending = await call('capture', media, media.operationId); assert.equal(pending.phase, 'succeeded');
  assert.deepEqual(await call('capture', media, media.operationId), pending);
  const capture = { action: 'capture', operationId: await newOperationId(client), expectedScope: settings.identity.scope, sourceId: 'personal-inbox', message: message() };
  const saved = await call('capture', capture, capture.operationId); assert.equal(saved.phase, 'succeeded');
  const assess = { action: 'assess', operationId: await newOperationId(client), expectedScope: settings.identity.scope, item: saved.effect, assessment: assessment() };
  const decided = await call('assess', assess, assess.operationId); assert.equal(decided.phase, 'succeeded');
  await owner.kill(); await stopped; await start();
  await ready();
  const restoredAssignments = await call('listAssignments', { expectedScope: settings.identity.scope });
  assert.deepEqual(restoredAssignments.assignments.map(entry => entry.pin), initialAssignments.assignments.map(entry => entry.pin));
  assert.deepEqual(await call('capture', media, media.operationId), pending);
  assert.equal((await client.request('objects.read', { objectId: pending.effect.objectId })).content.value.decision, null);
  assert.deepEqual(await call('capture', capture, capture.operationId), saved); assert.deepEqual(await call('assess', assess, assess.operationId), decided);
  const current = await client.request('objects.read', { objectId: saved.effect.objectId }); assert.equal(current.content.value.decision.operationId, assess.operationId); assert.equal(current.content.value.notice.state, 'queued');
  assert.deepEqual(await call('operation', { expectedScope: settings.identity.scope, operationId: assess.operationId }), decided);
  const outcome = await call('operationRead', { expectedScope: settings.identity.scope, operationId: assess.operationId });
  assert.deepEqual({ action: outcome.action, phase: outcome.phase, effect: outcome.effect }, { action: 'assess', phase: 'succeeded', effect: decided.effect });
  const checkpoint = { action: 'checkpoint', operationId: await newOperationId(client), expectedScope: settings.identity.scope, sourceId: 'personal-inbox', previousCursor: null, nextCursor: 'source-page-1', captures: [capture.operationId] };
  assert.equal((await call('checkpoint', checkpoint, checkpoint.operationId)).phase, 'succeeded');
  // The real scheduler also visits the queued notice. This fixture deliberately
  // has no Chat provider; it must retain the item and report that exact missing destination.
  const expectedIssues = [{ objectId: saved.effect.objectId, code: 'service_unavailable' }];
  const status = await until(async () => { const value = await call('status', { expectedScope: settings.identity.scope }); return value.recoveryIssues.some(issue => issue.objectId === saved.effect.objectId) ? value : null; }, 'retained notice recovery issue');
  assert.equal(status.sources[0].data.cursor, 'source-page-1'); assert.equal(status.noticeConfigured, false); assert.deepEqual(status.recoveryIssues, expectedIssues);
  await assert.rejects(call('capture', { ...capture, operationId: await newOperationId(client) }, 'different-domain-id'), { code: 'invalid_arguments' });
  await assert.rejects(call('status', { expectedScope: { ...settings.identity.scope, rootObjectId: randomUUID() } }), { code: 'secretary_scope_conflict' });
  const check = spawnSync(nativeExecutable(artifact, 'ivy-health'), ['--config', owner.path], { windowsHide: true, encoding: 'utf8', timeout: 10000 }); assert.equal(check.status, 0, check.stderr + check.stdout);
  const health = JSON.parse(await readFile(join(config.dataRoot, 'health.json'), 'utf8')); assert.equal(health.pid, child.pid); assert.equal(health.launchId, launch);
  await atomicJson(join(config.dataRoot, 'control.json'), { schemaVersion: 1, instanceId: id, launchId: launch, bootId: health.bootId, action: 'shutdown', requestedAt: new Date().toISOString() });
  const timer = setTimeout(() => child.kill('SIGKILL'), 10000), [code, signal] = await stopped; clearTimeout(timer); assert.equal(code, 0); assert.equal(signal, null);
  assert.equal(JSON.parse(await readFile(join(config.dataRoot, 'health.json'), 'utf8')).ready, false);
  const journal = new DatabaseSync(join(config.dataRoot, 'secretary.sqlite'), { readOnly: true });
  try {
    for (const table of ['technical_documents', 'technical_blobs'])
      assert.equal(journal.prepare('SELECT count(*) AS count FROM ' + table + ' WHERE key LIKE ?').get('secretary/triage-%').count, 0);
  } finally { journal.close(); }
  t.diagnostic(JSON.stringify({ buildId: build.buildId, launches, captured: saved.effect, decision: decided.effect, native: true, installedRuntimeOwner: false }));
});
