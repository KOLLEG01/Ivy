import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { hostFixture, until } from './fixtures/host.js';
import { RuntimeOwner } from '../packages/host-runtime/src/runtime-owner.js';
import { HostExecutor } from '../packages/host-runtime/src/executor.js';
import { terminalPhases } from '../packages/host-runtime/src/journal.js';
import { captureSource } from '../packages/host-runtime/src/source.js';
import { jsonFile } from '../packages/host-runtime/src/config.js';
import { atomicJson } from '../packages/host-runtime/src/config.js';
import { DatabaseSync } from 'node:sqlite';
import { mkdir } from 'node:fs/promises';
import { verifyServiceStorage } from '../packages/host-runtime/src/service-storage.js';
import type { Host } from '../packages/contracts/src/generated.js';

const distribution = resolve('.');
test('incompatible local service storage rejects source deployment before building or touching the target', async t => {
  const f = await hostFixture(t, 'phone-bridge');
  f.plan.storage = { minReadableFormat: 3, maxReadableFormat: 3, writeFormat: 3 };
  await atomicJson(join(f.source, 'services/phone-bridge/deploy.json'), f.plan);
  const root = join(f.config.runtimeRoot, 'instances/first/data'); await mkdir(root, { recursive: true });
  const path = join(root, 'phone-commands.sqlite'), db = new DatabaseSync(path);
  db.exec('PRAGMA user_version=2'); db.close();
  const snapshot = await captureSource(f.source, f.config);
  const entry = f.journal.accept({ action: 'deploy', instanceId: 'first', source: f.source, operationId: 'storage-preflight' }, snapshot);
  const executor = new HostExecutor(f.config, f.configPath, distribution); executor.start(); f.cleanups.push(() => executor.close());
  await until(() => terminalPhases.has(f.journal.get(entry.record.deploymentId).record.phase), 3000);
  const result = f.journal.get(entry.record.deploymentId);
  assert.equal(result.record.phase, 'failed'); assert.equal(result.record.errorCode, 'incompatible_storage');
  assert.match(result.record.readiness.message, /format 2.*format 3/);
  assert.equal(result.candidateId, null); assert.equal(f.journal.target('first'), null);
  const after = new DatabaseSync(path, { readOnly: true });
  try { assert.equal(after.prepare('PRAGMA user_version').get()!['user_version'], 2); } finally { after.close(); }
  const owner = new DatabaseSync(path);
  try {
    owner.exec('PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE');
    assert.throws(() => verifyServiceStorage(f.config, 'first', f.plan, 2), { code: 'incompatible_storage' });
    assert.doesNotThrow(() => verifyServiceStorage(f.config, 'first', { ...f.plan, storage: { minReadableFormat: 2, maxReadableFormat: 2, writeFormat: 2 } }, 2));
  } finally { owner.exec('ROLLBACK'); owner.close(); }
});

test('failed manifest readiness remains bounded in the runtime observation without recording child output', { timeout: 55_000 }, async t => {
  const f = await hostFixture(t);
  f.plan.readiness!.command.args = ['-e', 'console.error("private readiness diagnostic");process.exit(23)'];
  const candidate = await f.prepare('ready-health-failed-command');
  const owner = new RuntimeOwner(f.config, 'first', distribution); owner.start(); f.cleanups.push(() => owner.close());
  const executor = new HostExecutor(f.config, f.configPath, distribution); executor.start(); f.cleanups.push(() => executor.close());
  f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: candidate.candidateId, operationId: 'manifest-readiness-failure' });
  let failure: Host.RuntimeObservation | null = null;
  await until(async () => {
    const value = f.journal.observations()['first']; if (value?.state === 'starting' && value.code === 'service_not_ready') failure = value;
    return failure !== null;
  }, 30_000);
  const saved = failure!;
  assert.equal(saved.code, 'service_not_ready'); assert.ok(saved.message.length <= 4096);
  assert.equal(saved.message.includes('private readiness diagnostic'), false);
});

test('rapid child exits retain bounded backoff while an unrelated healthy process and runtime owner survive', { timeout: 55_000 }, async t => {
  const f = await hostFixture(t), healthy = await f.prepare('unrelated-survivor');
  f.plan.entrypoint!.args = ['-e', 'console.error(JSON.stringify({code:"unsupported_storage"}));process.exit(7)'];
  const crashing = await f.prepare('crash-before-health');
  for (const instance of f.config.instances) { const owner = new RuntimeOwner(f.config, instance.instanceId, distribution); owner.start(); f.cleanups.push(() => owner.close()); }
  const executor = new HostExecutor(f.config, f.configPath, distribution); executor.start(); f.cleanups.push(() => executor.close());
  const initial = f.journal.accept({ action: 'deploy', instanceId: 'unrelated', candidateId: healthy.candidateId, operationId: 'healthy' });
  await until(() => f.journal.get(initial.record.deploymentId).record.phase === 'succeeded');
  const before = f.journal.observations()['unrelated']!;
  const failed = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: crashing.candidateId, operationId: 'rapid-failure' });
  await until(() => terminalPhases.has(f.journal.get(failed.record.deploymentId).record.phase), 10_000);
  assert.equal(f.journal.get(failed.record.deploymentId).record.phase, 'needs_attention');
  assert.equal(f.journal.get(failed.record.deploymentId).record.errorCode, 'unsupported_storage');
  await until(() => f.journal.observations()['first']?.state === 'stopped');
  const after = f.journal.observations()['unrelated']!;
  assert.equal(after.ownerBootId, before.ownerBootId); assert.equal(after.health!.bootId, before.health!.bootId); assert.equal(after.state, 'ready');
  assert.equal((await readFile(join(f.config.runtimeRoot, 'instances/unrelated/data/launches.txt'), 'utf8')).trim().split('\n').length, 1);
});

test('a malformed captured manifest fails its durable source operation without disturbing the current launch', { timeout: 35_000 }, async t => {
  const f = await hostFixture(t), good = await f.prepare('healthy-during-malformed-source');
  const owner = new RuntimeOwner(f.config, 'first', distribution); owner.start(); f.cleanups.push(() => owner.close());
  const executor = new HostExecutor(f.config, f.configPath, distribution); executor.start(); f.cleanups.push(() => executor.close());
  const initial = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: good.candidateId, operationId: 'healthy' });
  await until(() => f.journal.get(initial.record.deploymentId).record.phase === 'succeeded');
  const before = f.journal.observations()['first']!;
  await writeFile(join(f.source, 'services/fixture/deploy.json'), '{ malformed component manifest');
  const snapshot = await captureSource(f.source, f.config);
  const failed = f.journal.accept({ action: 'deploy', instanceId: 'first', source: f.source, operationId: 'malformed-source' }, snapshot);
  await until(() => terminalPhases.has(f.journal.get(failed.record.deploymentId).record.phase));
  assert.equal(f.journal.get(failed.record.deploymentId).record.phase, 'failed');
  assert.equal(f.journal.get(failed.record.deploymentId).activation.rollbackTargetRevision, null);
  const after = f.journal.observations()['first']!;
  assert.equal(after.ownerBootId, before.ownerBootId); assert.equal(after.health!.bootId, before.health!.bootId); assert.equal(after.state, 'ready');
  assert.equal(f.journal.installed('first')?.candidateId, good.candidateId);
});
