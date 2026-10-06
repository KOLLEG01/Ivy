import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { linuxUnit, configurationBootstrapPlan } from '../packages/host-runtime/src/bootstrap.js';
import { atomicJson, jsonFile } from '../packages/host-runtime/src/config.js';
import { linuxResourceScope, validateLinuxResourceScope, linuxSlice, assertLinuxResourceMembership, verifyLinuxResourceMembership } from '../packages/host-runtime/src/linux-resources.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { validateHost } from '../packages/contracts/src/validation.js';
import type { Host } from '../packages/contracts/src/generated.js';

const installationId = 'ivy-next-111111111111', unit = installationId + '-222222222222.service';
const budget: Host.LinuxResourceBudget = { memoryHighBytes: 192 * 1024 * 1024, memoryMaxBytes: 256 * 1024 * 1024, tasksMax: 64 };
const scope = linuxResourceScope(installationId, budget);
const errorCode = (code: string) => (error: unknown) => error instanceof IvyError && error.code === code;

test('Linux scope validates a bounded budget and its exact installation-derived immutable name', () => {
  validateHost('LinuxResourceScope', scope);
  validateLinuxResourceScope(scope, installationId);
  assert.match(scope.slice, /^ivynext111111111111[a-f0-9]{12}\.slice$/);
  assert.notEqual(linuxResourceScope(installationId, { ...budget, tasksMax: 65 }).slice, scope.slice);
  assert.throws(() => validateLinuxResourceScope(scope, 'ivy-next-000000000000'), errorCode('target_conflict'));
  assert.throws(() => validateLinuxResourceScope({ ...scope, budget: { ...budget, tasksMax: 65 } }), errorCode('target_conflict'));
  for (const value of [0, budget.memoryMaxBytes, budget.memoryMaxBytes + 1, Number.NaN, Number.POSITIVE_INFINITY])
    assert.throws(() => linuxResourceScope(installationId, { ...budget, memoryHighBytes: value }));
  for (const value of ['system.slice', '../foreign.slice', scope.slice + '\n', 'ivy-next-111111111111.slice'])
    assert.throws(() => validateLinuxResourceScope({ ...scope, slice: value }));
  assert.throws(() => linuxResourceScope('foreign', budget));
  assert.throws(() => linuxResourceScope(installationId, { ...budget, tasksMax: 15 }));
  assert.throws(() => validateHost('LinuxResourceBudget', { ...budget, memorySwapBytes: 1 }));
});

test('kernel verification refuses a neighboring group, legacy hierarchy and changed effective controllers', () => {
  const membership = '0::/' + scope.slice + '/' + unit + '\n';
  const values = { 'memory.high': String(budget.memoryHighBytes), 'memory.max': String(budget.memoryMaxBytes), 'memory.swap.max': '0', 'pids.max': String(budget.tasksMax) };
  assertLinuxResourceMembership(scope, unit, membership, values);
  for (const changed of ['0::/system.slice/' + unit, membership.replace(unit, 'neighbor.service'), membership + '1:memory:/legacy', membership.replace('0::', '2:memory:')])
    assert.throws(() => assertLinuxResourceMembership(scope, unit, changed, values), errorCode('resource_scope_mismatch'));
  for (const controller of Object.keys(values)) for (const value of ['max', '', '1'])
    assert.throws(() => assertLinuxResourceMembership(scope, unit, membership, { ...values, [controller]: value }), errorCode('resource_scope_mismatch'));
  assert.throws(() => assertLinuxResourceMembership(scope, '../foreign', membership, values), errorCode('resource_scope_mismatch'));
});

test('Linux resource plans preserve the owned slice contract and reject Windows scope selection', () => {
  const plan: Host.BootstrapPlan = { schemaVersion: 1, hostId: 'fixture', os: 'linux', installationId, candidateId: digest('candidate'), artifactRoot: '/tmp/ivy-artifact', configPath: '/tmp/ivy-host.json',
    configHash: digest('config'), runtimeRoot: '/tmp/ivy-runtime', nodeExecutable: '/usr/bin/node', processes: [{ instanceId: 'fixture', componentId: 'fixture', name: unit.slice(0, -8) }] };
  validateHost('BootstrapPlan', plan);
  const legacy = linuxUnit(plan, plan.processes[0]!);
  assert.equal(legacy.includes('Slice='), false); assert.equal(legacy.includes('--linux-resources'), false);
  const bounded = linuxUnit({ ...plan, linuxResources: scope }, plan.processes[0]!);
  assert.ok(bounded.includes('Slice=' + scope.slice + '\n'));
  assert.equal(bounded.includes('--linux-resources'), false);
  assert.ok(linuxSlice(scope).includes('MemorySwapMax=0\n'));
  assert.throws(() => linuxUnit({ ...plan, linuxResources: { ...scope, slice: 'system.slice' } }, plan.processes[0]!));
  const config: Host.HostConfig = { schemaVersion: 1, hostId: 'fixture', runtimeRoot: '/tmp/runtime', artifactRoot: '/tmp/artifacts', stagingRoot: '/tmp/staging', publicBaseUrl: 'http://127.0.0.1:39001/ivy', executables: { node: process.execPath }, instances: [] };
  assert.throws(() => configurationBootstrapPlan(config, '/tmp/host.json', { platform: { os: 'win32' } } as Host.Candidate, budget), errorCode('unsupported_runtime'));
  const native = { ...config, instances: [
    { instanceId: 'host-executor', serviceNodeId: 'executor', componentId: 'host-executor', enabled: true, engine: 'process', settings: {} },
    { instanceId: 'service-manager', serviceNodeId: 'manager', componentId: 'service-manager', enabled: true, engine: 'process', settings: {} },
    { instanceId: 'agent-manager', serviceNodeId: 'agent', componentId: 'agent-manager', enabled: true, engine: 'process', settings: {} },
  ] } as Host.HostConfig;
  const root = { candidateId: digest('executor'), componentId: 'host-executor', artifactRoot: '/tmp/executor', platform: { os: 'linux', arch: 'x64', node: process.version } } as Host.Candidate;
  assert.deepEqual(configurationBootstrapPlan(native, '/tmp/host.json', root).processes.map(value => value.instanceId), ['host-executor']);
});

test('actual systemd parser accepts the owned resource slice and direct service argv', { skip: process.platform !== 'linux', timeout: 15_000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-resource-parser-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const artifactRoot = join(root, 'space % literal $directory'); await mkdir(artifactRoot);
  const plan: Host.BootstrapPlan = { schemaVersion: 1, os: 'linux', hostId: 'fixture', installationId, candidateId: digest('candidate'), artifactRoot, configPath: join(root, 'private config.json'),
    configHash: digest('config'), runtimeRoot: join(root, 'state'), nodeExecutable: process.execPath, processes: [{ instanceId: 'fixture', componentId: 'fixture', name: unit.slice(0, -8) }], linuxResources: scope };
  await writeFile(join(root, scope.slice), linuxSlice(scope)); await writeFile(join(root, unit), linuxUnit(plan, plan.processes[0]!));
  await promisify(execFile)('/usr/bin/systemd-analyze', ['verify', join(root, scope.slice), join(root, unit)], { timeout: 10_000, maxBuffer: 65536 });
  // The test runner is not an owned runtime owner and must fail even if the selected group is absent.
  await assert.rejects(verifyLinuxResourceMembership({ hostId: 'fixture' } as Host.HostConfig, 'fixture', scope));
});

test('persisted bootstrap plans keep identical unit bytes after canonical JSON reorders resource keys', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-resource-plan-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const plan: Host.BootstrapPlan = { schemaVersion: 1, hostId: 'fixture', os: 'linux', installationId, candidateId: digest('candidate'), artifactRoot: '/tmp/ivy-artifact', configPath: '/tmp/ivy-host.json',
    configHash: digest('config'), runtimeRoot: '/tmp/ivy-runtime', nodeExecutable: '/usr/bin/node', processes: [{ instanceId: 'fixture', componentId: 'fixture', name: unit.slice(0, -8) }], linuxResources: scope };
  const original = linuxUnit(plan, plan.processes[0]!);
  assert.ok(original.includes('Slice=' + scope.slice + '\n'), 'Keep the already installed resource boundary.');
  await atomicJson(join(root, 'plan.json'), plan);
  const restored = await jsonFile<Host.BootstrapPlan>(join(root, 'plan.json'));
  assert.deepEqual(Object.keys(restored.linuxResources!), ['budget', 'slice'], 'Exercise actual canonical persistence ordering.');
  assert.equal(linuxUnit(restored, restored.processes[0]!), original);
  const reordered = { ...restored, linuxResources: { budget: { tasksMax: 64, memoryMaxBytes: 268435456, memoryHighBytes: 201326592 }, slice: scope.slice } };
  assert.equal(linuxUnit(reordered, reordered.processes[0]!), original);
});
