import { PreparationProgress, preparationProgress } from '../packages/host-runtime/src/preparation-progress.js';
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { join, relative, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { HostJournal } from '../packages/host-runtime/src/journal.js';
import { atomicJson } from '../packages/host-runtime/src/config.js';
import { runCommand } from '../packages/host-runtime/src/process.js';
import { cli } from '../packages/cli/src/main.js';
import { digest } from '../packages/contracts/src/canonical.js';
import type { Host } from '../packages/contracts/src/generated.js';

test('CLI JSON observes the same durable ID after caller/source loss, and reports unknown outcomes explicitly', { timeout: 15_000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-cli-test-'));
  const config: Host.HostConfig = { schemaVersion: 1, hostId: 'cli-fixture', runtimeRoot: join(root, 'state'), artifactRoot: join(root, 'artifacts'), stagingRoot: join(root, 'staging'), publicBaseUrl: 'http://127.0.0.1:39081/ivy', executables: { node: process.execPath },
    instances: [{ instanceId: 'fixture', serviceNodeId: 'fixture', componentId: 'fixture', enabled: false, engine: 'process', settings: {} }] };
  const path = join(root, 'config.json'); await atomicJson(path, config);
  const journal = new HostJournal(config);
  t.after(async () => { journal.close(); assert.ok(relative(tmpdir(), root).startsWith('ivy-cli-test-')); await rm(root, { recursive: true, force: true }); });
  // Prepared-source metadata is a journal fixture; this test never executes it.
  const source = join(root, 'original-checkout-no-longer-present');
  const request: Host.LocalRequest = { action: 'deploy', operationId: 'accepted-before-caller-exit', instanceId: 'fixture', source };
  const entry = journal.accept(request, { snapshotId: digest('fixture-inputs'), originalRoot: source, sourceRoot: join(config.stagingRoot, 'snapshot'), manifestPath: join(config.stagingRoot, 'snapshot.json'), capturedAt: new Date().toISOString() });
  const argv = ['deploy', '--config', path, '--json', '--instance', 'fixture', '--source', source, '--operation-id', request.operationId];
  const replay = await cli(argv); assert.equal(replay.exitCode, 0); assert.equal(replay.output.deploymentId, entry.record.deploymentId); assert.deepEqual(replay.output.data, entry);
  const actual = await runCommand({ executable: 'node', args: ['dist/packages/cli/src/main.js', 'status', '--config', path, '--deployment', entry.record.deploymentId, '--json'], timeoutMs: 5000 }, resolve('.'), config.executables);
  assert.equal(JSON.parse(actual.stdout).deploymentId, entry.record.deploymentId);
  const archive = join(root, 'in-progress.tar.gz'); await writeFile(archive, 'partial archive');
  const progress = new PreparationProgress(config, 'fixture', entry.sourceSnapshot!.snapshotId);
  try {
    await progress.step('archiving', { command: 'tar -czf artifact.tar.gz', filePath: archive });
    const detailed = await cli(['status', '--config', path, '--deployment', entry.record.deploymentId]);
    const data = detailed.output.data as unknown as { progress: { step: string; fileBytes: number; stepElapsedMs: number } };
    assert.equal(await preparationProgress(config, { ...entry, record: { ...entry.record, createdAt: new Date(Date.now() + 60000).toISOString() } }), null, 'An older attempt must not supply progress for a newer deployment.');
    assert.equal(data.progress.step, 'archiving'); assert.equal(data.progress.fileBytes, 15);
    assert.ok(data.progress.stepElapsedMs >= 0);
    const messages: string[] = [];
    const waiting = await cli([...argv, '--wait-ms', '10'], process.env, message => messages.push(message));
    assert.equal(waiting.output.deploymentId, entry.record.deploymentId);
    assert.match(messages[0]!, /archiving.*15 bytes/);
    progress.output('stdout', 'payload must not appear in progress');
  } finally { await progress.close(); }
  const invalid = await cli([...argv, '--wait-ms', '-1']); assert.equal(invalid.exitCode, 2); assert.equal(journal.list().length, 1);
  const changed = await cli(argv.map(value => value === source ? join(root, 'different-source') : value)); assert.equal(changed.output.code, 'mutation_conflict');
  journal.advance(entry.record.deploymentId, 'preparing', 'needs_attention', { readiness: { state: 'unknown', message: 'Fixture owner cannot establish the result.' } });
  const unknown = await cli(argv); assert.equal(unknown.exitCode, 3); assert.equal(unknown.output.code, 'deployment_needs_attention');
  const status = await cli(['status', '--config', path, '--json']); assert.equal(status.exitCode, 0); assert.equal(status.output.code, 'host_status');
  const collection = await cli(['collect-storage', '--config', path, '--json']);
  assert.equal(collection.output.code, 'storage_retention_succeeded');
  assert.ok(journal.storageRetentionStatus()?.completedAt);
  assert.equal(Object.hasOwn(status.output.data as object, 'storageRetention'), false);
  const retention: Host.StorageRetentionStatus = { schemaVersion: 1, hostId: config.hostId, state: 'partial', trigger: 'scheduled',
    startedAt: new Date().toISOString(), completedAt: new Date().toISOString(), lastSuccessAt: null,
    removed: { candidates: 1, preparations: 0, payloads: 0, snapshots: 0, maintenance: 0, bootstrapPlans: 0 },
    availableBytes: { runtime: 1024, artifacts: 1024, staging: 1024 }, lowSpace: true,
    skipped: [{ area: 'snapshots', reason: 'unknown_preparation_ownership', count: 1, requiresAttention: true }], errorCode: null };
  journal.saveStorageRetentionStatus(retention);
  const retainedStatus = await cli(['status', '--config', path, '--json']);
  assert.equal(retainedStatus.exitCode, 0);
  assert.deepEqual((retainedStatus.output.data as unknown as { storageRetention: Host.StorageRetentionStatus }).storageRetention, retention);
  const preparations = await cli(['preparations', '--config', path, '--json']); assert.equal(preparations.exitCode, 0);
  assert.deepEqual((preparations.output.data as Host.PreparationInventory).entries, []);
  const compact = await cli(['compact-preparations', '--config', path, '--json']); assert.equal(compact.exitCode, 0);
  assert.deepEqual((compact.output.data as Host.PreparationCompaction).entries, []);
});
