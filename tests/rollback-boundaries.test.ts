import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:net';
import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { hostFixture, until } from './fixtures/host.js';
import { HiveServer } from '../services/hive/src/server.js';
import { HiveClient, scopedOperationId } from '../packages/sdk/src/client.js';
import { RuntimeOwner } from '../packages/host-runtime/src/runtime-owner.js';
import { HostExecutor } from '../packages/host-runtime/src/executor.js';
import { terminalPhases } from '../packages/host-runtime/src/journal.js';
import { atomicJson, jsonFile } from '../packages/host-runtime/src/config.js';
import { digest } from '../packages/contracts/src/canonical.js';
import type { Host } from '../packages/contracts/src/generated.js';

const distribution = resolve('.');
test('rollback refuses an older reader when a new contract version exists only in actual Hive revision history', { timeout: 75_000 }, async t => {
  const f = await hostFixture(t), listener = createServer();
  await new Promise<void>(resolve => listener.listen(0, '127.0.0.1', resolve));
  const port = (listener.address() as { port: number }).port; await new Promise<void>(resolve => listener.close(() => resolve()));
  const token = 'isolated-rollback-history-token', key = 'rollback/history';
  f.config.publicBaseUrl = `http://127.0.0.1:${port}/ivy`; f.config.instances[0]!.credential = token; await atomicJson(f.configPath, f.config);
  const hive = new HiveServer({ filename: join(f.root, 'hive.sqlite'), publicBaseUrl: f.config.publicBaseUrl, listenHost: '127.0.0.1', listenPort: port,
    version: 'test', buildId: digest('rollback-hive'), credentials: [{ principalId: 'rollback-user', digest: digest(token) }] });
  await hive.start(); f.cleanups.push(() => hive.close());
  const client = new HiveClient(f.config.publicBaseUrl, { credential: token });
  const mutation = (value: string) => scopedOperationId(client, ['rollback', value]);
  for (const version of ['1.0.0', '2.0.0', '3.0.0']) await client.request('contracts.register', { mutationId: await mutation('contract-' + version),
    definition: { key, version, owner: { kind: 'agent' }, mediaType: 'text/plain', retention: { objects: { mode: 'retain' }, revisions: { mode: 'all' } }, specMarkdown: 'Isolated compatibility revision ' + version } });
  const original = await client.request('objects.write', { mutationId: await mutation('first-version'), contractVersion: '1.0.0',
    references: {}, create: { contractKey: key, name: 'retained compatibility history', parentId: null, ownerObjectId: null }, content: { encoding: 'text', value: 'original' } });
  f.plan.requirements = { node: '>=24.18.0 <25.0.0', hiveProtocol: 1, contracts: [{ key, readVersions: ['1.0.0', '3.0.0'], writeVersions: ['1.0.0'] }] };
  f.plan.connectsToHive = true;
  const good = await f.prepare('old-reader');
  f.plan.requirements.contracts[0]!.readVersions.push('2.0.0');
  const bad = await f.prepare('new-reader-failed-health', false);
  const owner = new RuntimeOwner(f.config, 'first', distribution); owner.start(); f.cleanups.push(() => owner.close());
  const executor = new HostExecutor(f.config, f.configPath, distribution); executor.start(); f.cleanups.push(() => executor.close());
  const first = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: good.candidateId, operationId: 'first-reader' });
  await until(() => terminalPhases.has(f.journal.get(first.record.deploymentId).record.phase), 30_000);
  assert.equal(f.journal.get(first.record.deploymentId).record.phase, 'succeeded', JSON.stringify(f.journal.get(first.record.deploymentId)));
  const update = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: bad.candidateId, operationId: 'new-reader' });
  await until(() => f.journal.get(update.record.deploymentId).record.phase === 'verifying');
  // Another permitted writer changes actual stored history after preflight. Advancing to the
  // supported V3 must not conceal unsupported intermediate V2 from the older reader's inspection.
  await client.request('objects.write', { mutationId: await mutation('new-version'), objectId: original.object.id, expectedRevision: 1, contractVersion: '2.0.0', references: {}, content: { encoding: 'text', value: 'new history' } });
  await client.request('objects.write', { mutationId: await mutation('current-supported-version'), objectId: original.object.id, expectedRevision: 2, contractVersion: '3.0.0', references: {}, content: { encoding: 'text', value: 'current supported version' } });
  const expected = await client.request('objects.read', { objectId: original.object.id });
  const inspection = await client.request('system.inspectStorage', { contracts: [{ key }] });
  assert.deepEqual(inspection.contracts, [{ key, versions: ['1.0.0', '2.0.0', '3.0.0'] }]);
  await until(() => terminalPhases.has(f.journal.get(update.record.deploymentId).record.phase), 45_000);
  const outcome = f.journal.get(update.record.deploymentId);
  assert.equal(outcome.record.phase, 'needs_attention'); assert.equal(outcome.record.errorCode, 'incompatible_contract');
  assert.equal(outcome.activation.rollbackTargetRevision, null);
  await until(() => f.journal.observations()['first']?.state === 'stopped');
  const launches = (await readFile(join(f.config.runtimeRoot, 'instances/first/data/launches.txt'), 'utf8')).trim().split('\n');
  assert.equal(launches.filter(value => value.startsWith('old-reader ')).length, 1);
  assert.deepEqual(await client.request('objects.read', { objectId: original.object.id }), expected);
  assert.equal(f.journal.installed('first')?.candidateId, good.candidateId); assert.equal(f.journal.target('first')?.desired, 'stopped');
});

test('rollback rematerializes the previous binary from its acceptance snapshot instead of trusting an old target file', { timeout: 90_000 }, async t => {
  const f = await hostFixture(t), good = await f.prepare('original-config'), bad = await f.prepare('bad-health', false);
  const owner = new RuntimeOwner(f.config, 'first', distribution); owner.start(); f.cleanups.push(() => owner.close());
  const executor = new HostExecutor(f.config, f.configPath, distribution); executor.start(); f.cleanups.push(() => executor.close());
  const initial = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: good.candidateId, operationId: 'original' });
  await until(() => f.journal.get(initial.record.deploymentId).record.phase === 'succeeded');
  const originalTarget = f.journal.target('first')!, original = await jsonFile<Host.InstanceConfig>(originalTarget.configPath);
  f.config.instances[0]!.credential = 'new-credential';
  await atomicJson(f.configPath, f.config);
  const update = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: bad.candidateId, operationId: 'failed-update' });
  await until(() => f.journal.get(update.record.deploymentId).record.phase === 'verifying');
  await atomicJson(originalTarget.configPath, { ...original, settings: { damagedAfterAcceptance: true } });
  await until(() => terminalPhases.has(f.journal.get(update.record.deploymentId).record.phase), 65_000);
  assert.equal(f.journal.get(update.record.deploymentId).record.phase, 'rolled_back');
  await until(() => f.journal.observations()['first']?.state === 'ready');
  const launches = (await readFile(join(f.config.runtimeRoot, 'instances/first/data/launches.txt'), 'utf8')).trim().split('\n');
  assert.equal(launches.filter(value => value.startsWith('original-config ')).length, 2);
  assert.deepEqual((await jsonFile<Host.InstanceConfig>(originalTarget.configPath)).settings, { damagedAfterAcceptance: true });
  assert.notEqual(f.journal.target('first')?.configPath, originalTarget.configPath);
  const restored = await jsonFile<Host.InstanceConfig>(f.journal.target('first')!.configPath);
  assert.deepEqual(restored.settings, {});
  assert.equal(restored.serviceNodeId, original.serviceNodeId);
  assert.equal(restored.credential, undefined);
  assert.equal(f.journal.target('first')?.desired, 'running');
});
