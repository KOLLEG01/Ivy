import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import { HostExecutor, parallelDeployments } from '../packages/host-runtime/src/executor.js';
import type { Host } from '../packages/contracts/src/generated.js';

// Scheduling only consumes these fields; lifecycle tests cover journal validation.
const entry = (id: string, instanceId = id, componentId = 'worker') => ({ record: { deploymentId: id, instanceId, componentId, phase: 'checking' }, candidateId: null } as Host.JournalEntry);
test('parallel activation is bounded and preserves same-instance order', () => {
  const a = entry('a', 'first'), b = entry('b', 'first'), c = entry('c'), d = entry('d');
  assert.deepEqual(parallelDeployments([a, b, c, d], []), [a, c]);
  assert.deepEqual(parallelDeployments([a, b, c, d], [a]), [c]);
  assert.deepEqual(parallelDeployments([a, b, c, d], [a, c]), []);
});
test('infrastructure changes form fair exclusive barriers', () => {
  for (const component of ['host-executor', 'hive']) {
    const a = entry('a'), barrier = entry('infra', 'infra', component), b = entry('b');
    assert.deepEqual(parallelDeployments([a, barrier, b], []), [a]);
    assert.deepEqual(parallelDeployments([barrier, b], [a]), []);
    assert.deepEqual(parallelDeployments([barrier, b], []), [barrier]);
    assert.deepEqual(parallelDeployments([barrier, b], [barrier]), []);
  }
});
test('source builds do not compete with activation I/O', () => {
  const build = entry('build'); build.record.phase = 'preparing';
  assert.deepEqual(parallelDeployments([build, entry('next')], []), [build]);
  assert.deepEqual(parallelDeployments([entry('next')], [build]), []);
});
test('concurrent executor lanes isolate configuration changes and drain on close', { timeout: 10000 }, async () => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-executor-parallel-'));
  const config: Host.HostConfig = { schemaVersion: 1, hostId: 'parallel-test', runtimeRoot: join(root, 'state'), artifactRoot: join(root, 'artifacts'), stagingRoot: join(root, 'staging'), publicBaseUrl: 'http://127.0.0.1:39081/ivy',
    executables: { node: process.execPath }, instances: ['a', 'b'].map(instanceId => ({ instanceId, serviceNodeId: instanceId, componentId: 'worker', enabled: true, engine: 'process', settings: {} })) };
  const executor = new HostExecutor(config, join(root, 'config.json'), root);
  const entries = [entry('a'), entry('b')].map(value => ({ ...value, configurationPath: join(root, value.record.deploymentId + '.json') }));
  executor.journal.acceptedConfiguration = () => config;
  let release!: () => void; const gate = new Promise<void>(resolve => { release = resolve; });
  const started = new Set<string>(), completed = new Set<string>();
  executor.journal.unfinished = () => entries.filter(value => !completed.has(value.record.deploymentId));
  (executor as unknown as { execute(value: Host.JournalEntry): Promise<void> }).execute = async value => {
    const id = value.record.deploymentId;
    executor.journal.useConfiguration({ ...config, publicBaseUrl: `http://127.0.0.1:39081/${id}` });
    started.add(id); await gate;
    assert.equal(executor.config.publicBaseUrl, `http://127.0.0.1:39081/${id}`);
    completed.add(id);
  };
  try {
    executor.start();
    const deadline = Date.now() + 3000;
    while (started.size < 2 && Date.now() < deadline) await delay(10);
    assert.equal(started.size, 2, 'independent operations must overlap');
    assert.equal(executor.config.publicBaseUrl, config.publicBaseUrl, 'pulse/base context must remain unchanged');
    const closing = executor.close(); release(); await closing;
    assert.equal(completed.size, 2);
  } finally { release(); await executor.close(); await rm(root, { recursive: true, force: true }); }
});
