import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { readFile, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { hostFixture } from './fixtures/host.js';
import { bootstrapPlan, linuxUnit, retainedBootstrapPlans, retainBootstrapPlan } from '../packages/host-runtime/src/bootstrap.js';
import { linuxResourceScope } from '../packages/host-runtime/src/linux-resources.js';
import { bootstrapStatus, inspectBootstrap, updateBootstrap } from '../packages/host-runtime/src/bootstrap-maintenance.js';
import { AutomaticBootstrapUpdater, automaticBootstrapRecord, saveAutomaticBootstrapRecord } from '../packages/host-runtime/src/bootstrap-automatic.js';
import { runAutomaticBootstrapUpdate } from '../packages/host-runtime/src/bootstrap-auto-worker.js';
import { verifyCandidate } from '../packages/host-runtime/src/artifact.js';
import type { BootstrapDefinition, BootstrapOs, BootstrapOwner } from '../packages/host-runtime/src/bootstrap-os.js';
import { ExecutorLock } from '../packages/host-runtime/src/journal.js';
import { atomicJson, jsonFile } from '../packages/host-runtime/src/config.js';
import { backupHost } from '../packages/host-runtime/src/private-backup.js';
import { restoreHost, verifyBackup } from '../packages/host-runtime/src/restore.js';
import { exists } from '../packages/host-runtime/src/backup-files.js';
import { cli } from '../packages/cli/src/main.js';
import { canonical, digest, hashJson } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Host } from '../packages/contracts/src/generated.js';

const distribution = resolve('.');
const code = (expected: string) => (error: unknown) => error instanceof IvyError && error.code === expected;
/** Simulated OS boundary. Real SQLite locks, candidates, records, CLI and full backup/restore run below. */
class SimulatedOs implements BootstrapOs {
  owners: BootstrapOwner[]; writes = 0; inspections = 0; reloads = 0; launches: { requestPath: string; operationId: string }[] = [];
  failAfterWrite = false; forbidAccess = false;
  constructor(plan: Host.BootstrapPlan) {
    this.owners = plan.processes.map(instance => {
      const candidateId = instance.candidateId!;
      const content = JSON.stringify({ candidateId, instance, preserved: 'principal, triggers and settings' });
      return { ...instance, candidateId, definitionHash: digest(content), enabled: true, running: true, definition: { content, hash: digest(content) } };
    });
  }
  async pause(): Promise<void> { for (const owner of this.owners) { owner.enabled = false; owner.running = false; } }
  async resume(snapshot: Host.BootstrapSnapshot): Promise<void> {
    for (const owner of this.owners) { const state = snapshot.owners.find(value => value.instanceId === owner.instanceId)!;
      owner.enabled = state.enabled; owner.running = state.running; }
  }
  async launchAutomatic(_plan: Host.BootstrapPlan, requestPath: string, operationId: string): Promise<void> { this.launches.push({ requestPath, operationId }); }
  async inspect(): Promise<BootstrapOwner[]> { assert.equal(this.forbidAccess, false, 'historical replay accessed the OS'); this.inspections++; return structuredClone(this.owners); }
  async render(owner: BootstrapOwner, next: Host.BootstrapPlan): Promise<BootstrapDefinition> {
    const candidateId = next.processes.find(instance => instance.instanceId === owner.instanceId)!.candidateId!;
    const content = JSON.stringify({ ...JSON.parse(owner.definition.content), candidateId }); return { content, hash: digest(content) };
  }
  async check(definition: BootstrapDefinition): Promise<void> { if (digest(definition.content) !== definition.hash) throw new IvyError('storage_invalid', 'Changed simulated definition.'); }
  async apply(owner: BootstrapOwner, desired: BootstrapDefinition): Promise<void> {
    const current = this.owners.find(value => value.instanceId === owner.instanceId)!;
    if (current.running || current.enabled || ![owner.definitionHash, desired.hash].includes(current.definitionHash)) throw new IvyError('target_conflict', 'Simulated definition changed.');
    if (current.definitionHash === desired.hash) return;
    current.definition = structuredClone(desired); current.definitionHash = desired.hash; current.candidateId = JSON.parse(desired.content).candidateId; this.writes++;
    if (this.failAfterWrite) { this.failAfterWrite = false; throw new IvyError('outcome_unknown', 'Simulated process loss after the OS write and before its recorded outcome.', 'unknown'); }
  }
  async reload(): Promise<void> { this.reloads++; }
}

test('automatic bootstrap handoff retains one exact request and never retries a rolled-back release', { timeout: 60_000 }, async t => {
  const f = await fixture(t); await retainBootstrapPlan(f.oldPlan);
  await atomicJson(join(f.config.runtimeRoot, 'bootstrap', f.oldPlan.installationId + '.json'), f.oldPlan); f.journal.syncBootstrap(f.oldPlan);
  const updater = new AutomaticBootstrapUpdater(f.configPath, distribution, f.journal, f.os, inspectBootstrap);
  const selected = f.config.instances.map(instance => ({ instanceId: instance.instanceId, candidate: f.next, manifest: f.journal.manifest(f.next.candidateId) }));
  const before = f.os.inspections; assert.equal(await updater.reconcile(selected), true); assert.equal(f.os.inspections, before + 1);
  const launched = f.os.launches[0]!; const request = await jsonFile<Host.BootstrapMaintenanceRequest>(launched.requestPath);
  assert.equal(request.next.candidateId, f.next.candidateId); assert.equal(request.previous.owners[0]!.candidateId, f.old.candidateId);
  assert.equal(await updater.reconcile(selected), true); assert.equal(f.os.inspections, before + 1, 'replay reuses the retained ownership snapshot');
  assert.deepEqual(await jsonFile(launched.requestPath), request); assert.deepEqual(f.os.launches[1], launched);
  const record = await automaticBootstrapRecord(f.config, launched.operationId); assert.ok(record);
  record.phase = 'rolled_back'; record.errorCode = 'readiness_failed'; await saveAutomaticBootstrapRecord(f.config, record);
  assert.equal(await updater.reconcile(selected), false); assert.equal(f.os.launches.length, 2);
});

test('selected and retained copies of one bootstrap plan count as one authority', { timeout: 60_000 }, async t => {
  const f = await fixture(t);
  await retainBootstrapPlan(f.oldPlan);
  await atomicJson(join(f.config.runtimeRoot, 'bootstrap', f.oldPlan.installationId + '.json'), f.oldPlan);
  const plans = await retainedBootstrapPlans(f.config);
  assert.equal(plans.filter(plan => hashJson(plan) === hashJson(f.oldPlan)).length, 1);
});

test('automatic bootstrap worker activates through offline maintenance and rolls back failed readiness', { timeout: 90_000 }, async t => {
  for (const rollback of [false, true]) await t.test(rollback ? 'rollback' : 'success', async t => {
    const f = await fixture(t); await retainBootstrapPlan(f.oldPlan);
    await atomicJson(join(f.config.runtimeRoot, 'bootstrap', f.oldPlan.installationId + '.json'), f.oldPlan); f.journal.syncBootstrap(f.oldPlan);
    const updater = new AutomaticBootstrapUpdater(f.configPath, distribution, f.journal, f.os, inspectBootstrap);
    const selected = f.config.instances.map(instance => ({ instanceId: instance.instanceId, candidate: f.next, manifest: f.journal.manifest(f.next.candidateId) }));
    await updater.reconcile(selected); const launched = f.os.launches[0]!;
    const ready = async (_config: Host.HostConfig, plan: Host.BootstrapPlan) => {
      if (rollback && plan.candidateId === f.next.candidateId) throw new IvyError('readiness_failed', 'Simulated updated bootstrap readiness failure.');
      assert.equal(plan.candidateId, rollback ? f.old.candidateId : f.next.candidateId);
    };
    await runAutomaticBootstrapUpdate(f.configPath, launched.requestPath, distribution, f.os, ready);
    const record = await automaticBootstrapRecord(f.config, launched.operationId); assert.equal(record?.phase, rollback ? 'rolled_back' : 'succeeded');
    const expected = rollback ? f.old.candidateId : f.next.candidateId;
    assert.ok(f.os.owners.every(owner => owner.candidateId === expected && owner.enabled && owner.running));
    assert.ok(f.config.instances.every(instance => f.journal.installed(instance.instanceId)?.candidateId === expected));
  });
});
async function fixture(t: TestContext, domainEnabled=false) {
  const f = await hostFixture(t, 'host-executor');
  f.config.packageUpdates = { intervalSeconds: 60 }; await atomicJson(f.configPath, f.config); f.journal.useConfiguration(f.config);
  if(domainEnabled){f.config.instances.push({instanceId:'domain',serviceNodeId:'domain',componentId:'fixture-domain',enabled:true,engine:'process',settings:{}});await atomicJson(f.configPath,f.config);f.journal.useConfiguration(f.config);}
  const old = await f.prepare('old-bootstrap-fixture'), next = await f.prepare('next-bootstrap-fixture');
  const oldPlan = await bootstrapPlan(f.configPath, old.candidateId), nextPlan = await bootstrapPlan(f.configPath, next.candidateId), os = new SimulatedOs(oldPlan);
  const request: Host.BootstrapMaintenanceRequest = { schemaVersion: 1, operationId: 'maintenance-1', previous: await inspectBootstrap(f.configPath, distribution, os), next: nextPlan };
  return { ...f, old, next, oldPlan, os, request };
}

test('retained manifest extras use the same candidate/bootstrap path while required runtime fields remain mandatory', {timeout:60000},async t=>{
  const f=await fixture(t,true), minimal=f.journal.manifest(f.next.candidateId);
  const retained={...minimal,description:'retained description',prepare:[],checks:[],artifactHash:digest('old artifact inventory'),
    provenance:{source:'retained rollout'},verification:{status:'previously checked'}};
  await writeFile(f.next.manifestPath,canonical(retained)+'\n');
  f.journal.db.prepare('UPDATE candidates SET manifest_json=? WHERE candidate_id=?').run(canonical(retained),f.next.candidateId);
  assert.deepEqual(f.journal.manifest(f.next.candidateId),minimal);
  assert.deepEqual(await verifyCandidate(f.next,f.config),minimal);
  assert.deepEqual(await bootstrapPlan(f.configPath,f.next.candidateId),f.request.next);
  f.os.pause();
  assert.equal((await updateBootstrap(f.configPath,f.request,distribution,f.os)).phase,'succeeded');
  assert.equal(f.journal.config.instances[2]!.enabled,true);assert.ok(f.os.owners.every(owner=>!owner.running&&!owner.enabled));
  const invalid={...retained,restart:undefined};await writeFile(f.next.manifestPath,JSON.stringify(invalid));
  await assert.rejects(verifyCandidate(f.next,f.config),code('invalid_arguments'));
});

test('bootstrap snapshots retain exact ownership and maintenance does not rescan old payload contents', { timeout: 60_000 }, async t => {
  const f = await fixture(t);
  Object.assign(f.os.owners[0]!, { allowWindowsBreakaway: true });
  const observed = await inspectBootstrap(f.configPath, distribution, f.os);
  assert.deepEqual(observed.owners, f.request.previous.owners);
  assert.ok(!('allowWindowsBreakaway' in observed.owners[0]!));
  assert.equal(observed.owners[0]!.definitionHash, f.os.owners[0]!.definitionHash);
  // Status remains available even when a retained old payload was changed. The
  // new candidate's stamp and entrypoint are checked; no full old-file inventory
  // participates in maintenance of the replacement definitions.
  await writeFile(join(f.old.artifactRoot, 'dist/main.mjs'), 'damaged retained payload');
  assert.deepEqual((await inspectBootstrap(f.configPath, distribution, f.os)).owners, observed.owners);
  f.os.pause();
  const request = { ...f.request, previous: await inspectBootstrap(f.configPath, distribution, f.os) };
  const updated = await updateBootstrap(f.configPath, request, distribution, f.os);
  assert.equal(updated.phase, 'succeeded'); assert.equal(updated.errorCode, null);
  assert.equal(f.os.writes, f.request.next.processes.length);
});

test('bootstrap inspection ignores retained plans whose outer candidate is no longer executable',async t=>{
  const f=await fixture(t),stale={...f.oldPlan,candidateId:digest('missing historical bootstrap candidate'),artifactRoot:'/missing/bootstrap/artifact'};
  await atomicJson(join(f.config.runtimeRoot,'bootstrap-plans',hashJson(stale).slice(7)+'.json'),stale);
  const snapshot=await inspectBootstrap(f.configPath,distribution,f.os);
  assert.deepEqual(snapshot.owners.map(owner=>owner.instanceId),f.oldPlan.processes.map(process=>process.instanceId));
});

test('bootstrap inspection ignores obsolete candidates with invalid historical manifests while retaining the exact OS plan', async t => {
  const f = await fixture(t), stale = { ...f.oldPlan };
  const old = f.journal.manifest(f.old.candidateId);
  const obsolete = { ...old, requirements: { hiveProtocol: null, contracts: [] } };
  await writeFile(f.old.manifestPath, canonical(obsolete) + '\n');
  f.journal.db.prepare('UPDATE candidates SET manifest_json=? WHERE candidate_id=?').run(canonical(obsolete), f.old.candidateId);
  const snapshot = await inspectBootstrap(f.configPath, distribution, f.os);
  assert.deepEqual(snapshot.owners.map(owner => owner.instanceId), stale.processes.map(process => process.instanceId));
});

test('bootstrap interruption reconciles exact old/new definitions; replay cannot reverse a later maintenance', { timeout: 60_000 }, async t => {
  const f = await fixture(t); f.os.pause(); f.os.failAfterWrite = true;
  const priorCandidates = f.journal.db.prepare('SELECT * FROM candidates ORDER BY candidate_id').all(), oldOperations = f.journal.list();
  const interrupted = await updateBootstrap(f.configPath, f.request, distribution, f.os);
  assert.equal(interrupted.phase, 'needs_attention'); assert.equal(interrupted.errorCode, 'outcome_unknown'); assert.equal(f.os.writes, 1);
  assert.equal(interrupted.definitions[0]!.applied, false); assert.equal(f.os.owners[0]!.candidateId, f.next.candidateId);
  const completed = await updateBootstrap(f.configPath, f.request, distribution, f.os);
  assert.equal(completed.phase, 'succeeded'); assert.equal(f.os.writes, 2); assert.ok(completed.definitions.every(value => value.applied));
  assert.equal(f.journal.installed('first')?.candidateId, f.next.candidateId);
  assert.equal(f.os.reloads, 1); assert.ok(f.os.owners.every(value => !value.running && !value.enabled));
  assert.deepEqual(f.journal.list(), oldOperations); assert.deepEqual(f.journal.db.prepare('SELECT * FROM candidates ORDER BY candidate_id').all(), priorCandidates);
  const later: Host.BootstrapMaintenanceRequest = { ...f.request, operationId: 'maintenance-2', previous: await inspectBootstrap(f.configPath, distribution, f.os), next: f.oldPlan };
  assert.equal((await updateBootstrap(f.configPath, later, distribution, f.os)).phase, 'succeeded'); assert.equal(f.os.writes, 4);
  f.os.forbidAccess = true;
  assert.deepEqual(await updateBootstrap(f.configPath, f.request, distribution, f.os), completed); assert.equal(f.os.writes, 4);
  await assert.rejects(updateBootstrap(f.configPath, { ...f.request, next: f.oldPlan }, distribution, f.os), code('operation_conflict'));
  assert.deepEqual(await bootstrapStatus(f.config, f.request.operationId), completed);
  const path = join(f.root, 'request.json'); await atomicJson(path, f.request);
  const replay = await cli(['update-bootstrap', '--config', f.configPath, '--request', path, '--json']);
  assert.equal(replay.exitCode, 0); assert.deepEqual(replay.output.data, completed);
  const status = await cli(['bootstrap-status', '--config', f.configPath, '--operation-id', f.request.operationId, '--json']);
  assert.deepEqual(status.output.data, completed);
  assert.equal((await cli(['bootstrap-status', '--config', f.configPath, '--operation-id', 'missing', '--json'])).output.code, 'not_found');
});

test('bootstrap refuses a third OS state and altered retained evidence without overwriting either', { timeout: 60_000 }, async t => {
  const f = await fixture(t); f.os.pause(); f.os.failAfterWrite = true;
  await updateBootstrap(f.configPath, f.request, distribution, f.os);
  const genuine = structuredClone(f.os.owners[1]!);
  const foreign = JSON.stringify({ ...JSON.parse(genuine.definition.content), preserved: 'administrator changed settings' });
  f.os.owners[1]!.definition = { content: foreign, hash: digest(foreign) }; f.os.owners[1]!.definitionHash = digest(foreign);
  const conflict = await updateBootstrap(f.configPath, f.request, distribution, f.os);
  assert.equal(conflict.phase, 'needs_attention'); assert.equal(conflict.errorCode, 'target_conflict'); assert.equal(f.os.writes, 1);
  assert.equal(f.os.owners[1]!.definition.content, foreign);
  f.os.owners[1] = genuine;
  const path = join(f.config.runtimeRoot, 'bootstrap-maintenance', hashJson(f.request.operationId).slice(7), 'first.desired.json');
  const evidence = await readFile(path, 'utf8'); await writeFile(path, evidence.replace('principal, triggers and settings', 'tampered'));
  assert.equal((await updateBootstrap(f.configPath, f.request, distribution, f.os)).errorCode, 'storage_invalid'); assert.equal(f.os.writes, 1);
  await writeFile(path, evidence);
  assert.equal((await updateBootstrap(f.configPath, f.request, distribution, f.os)).phase, 'succeeded'); assert.equal(f.os.writes, 2);
});

test('bootstrap preflight binds configuration, scope, paused scheduling and actual SQLite ownership', { timeout: 60_000 }, async t => {
  const f = await fixture(t), before = f.os.inspections;
  await atomicJson(f.configPath, { ...f.config, publicBaseUrl: 'http://127.0.0.1:39888/changed' });
  await assert.rejects(updateBootstrap(f.configPath, f.request, distribution, f.os), code('configuration_changed')); assert.equal(f.os.inspections, before);
  await atomicJson(f.configPath, f.config);
  const otherScope = structuredClone(f.request); otherScope.previous.installationId = 'ivy-next-000000000000';
  await assert.rejects(updateBootstrap(f.configPath, otherScope, distribution, f.os), code('target_conflict')); assert.equal(f.os.inspections, before);
  assert.equal((await updateBootstrap(f.configPath, f.request, distribution, f.os)).errorCode, 'bootstrap_busy'); assert.equal(f.os.writes, 0);
  f.os.pause();
  for (const owner of ['executor', 'owners/first', 'bootstrap-maintenance-lock']) {
    const lock = new ExecutorLock(join(f.config.runtimeRoot, owner));
    try {
      if (owner === 'bootstrap-maintenance-lock') await assert.rejects(updateBootstrap(f.configPath, f.request, distribution, f.os), code('executor_already_running'));
      else assert.equal((await updateBootstrap(f.configPath, f.request, distribution, f.os)).errorCode, 'backup_busy');
    } finally { lock.close(); }
    assert.equal(f.os.writes, 0);
  }
  const pending = f.journal.accept({ action: 'deploy', instanceId: 'first', candidateId: f.old.candidateId, operationId: 'unfinished-activation' });
  assert.equal((await updateBootstrap(f.configPath, f.request, distribution, f.os)).errorCode, 'bootstrap_busy'); assert.equal(f.os.writes, 0);
  f.journal.advance(pending.record.deploymentId, 'prepared', 'failed', { readiness: { state: 'failed', message: 'Fixture activation failed before dispatch.' } });
  assert.equal((await updateBootstrap(f.configPath, f.request, distribution, f.os)).phase, 'succeeded');
});

test('bootstrap maintenance does not stop unrelated process owners', { timeout: 60_000 }, async t => {
  const f = await fixture(t, true); f.os.pause();
  const unrelated = new ExecutorLock(join(f.config.runtimeRoot, 'owners', 'domain'));
  try { assert.equal((await updateBootstrap(f.configPath, f.request, distribution, f.os)).phase, 'succeeded'); }
  finally { unrelated.close(); }
});

test('bootstrap maintenance waits briefly for a stopped executor to release its lock', { timeout: 60_000 }, async t => {
  const f = await fixture(t); f.os.pause();
  const prior = new ExecutorLock(join(f.config.runtimeRoot, 'executor'));
  let released = false;
  const release = setTimeout(() => { prior.close(); released = true; }, 250);
  try { assert.equal((await updateBootstrap(f.configPath, f.request, distribution, f.os)).phase, 'succeeded'); }
  finally { clearTimeout(release); if (!released) prior.close(); }
});

test('isolated restore preserves successful bootstrap outcomes and fences unfinished historical requests', { timeout: 90_000 }, async t => {
  const f = await fixture(t); f.os.pause();
  const complete = await updateBootstrap(f.configPath, f.request, distribution, f.os);
  // Retention leaves the original successful record, so backup/restore needs no compacted format.
  const completedRoot = join(f.config.runtimeRoot, 'bootstrap-maintenance', hashJson(f.request.operationId).slice(7));
  for (const definition of complete.definitions) for (const suffix of ['prior', 'desired'])
    await rm(join(completedRoot, definition.instanceId + '.' + suffix + '.json'));
  const second: Host.BootstrapMaintenanceRequest = { ...f.request, operationId: 'incomplete-before-backup', previous: await inspectBootstrap(f.configPath, distribution, f.os), next: f.oldPlan };
  f.os.failAfterWrite = true; const incomplete = await updateBootstrap(f.configPath, second, distribution, f.os);
  assert.equal(incomplete.phase, 'needs_attention');
  const relative = 'bootstrap-maintenance/' + hashJson(second.operationId).slice(7) + '/record.json', original = await readFile(join(f.config.runtimeRoot, relative));
  const backup = await backupHost(f.configPath, join(f.root, 'backup'), distribution);
  const manifest = await verifyBackup(backup.directory, backup.manifestHash);
  assert.equal(manifest.files.some(file => file.path.startsWith('state/bootstrap-maintenance-lock/')), false);
  const target: Host.HostConfig = { ...f.config, runtimeRoot: join(f.root, 'restored-state'), artifactRoot: join(f.root, 'restored-artifacts'), stagingRoot: join(f.root, 'restored-staging'),
    publicBaseUrl: 'http://127.0.0.1:39889/ivy', instances: f.config.instances.map(instance => ({ ...instance, enabled: false })) };
  for (const instance of target.instances) instance.settings = { hostConfigPath: join(target.runtimeRoot, 'host.json') };
  const template = join(f.root, 'target.json'); await atomicJson(template, target);
  const restored = await restoreHost(backup.directory, backup.manifestHash, template, distribution), config = await jsonFile<Host.HostConfig>(restored.configPath);
  const held = await bootstrapStatus(config, second.operationId);
  assert.equal(held.phase, 'needs_attention'); assert.equal(held.errorCode, 'restore_reconciliation_required'); assert.deepEqual(held.request, second);
  assert.deepEqual(await readFile(join(config.runtimeRoot, 'recovery-original', relative)), original);
  assert.deepEqual(await readFile(join(f.config.runtimeRoot, relative)), original);
  f.os.forbidAccess = true;
  assert.deepEqual(await updateBootstrap(restored.configPath, second, distribution, f.os), held);
  assert.deepEqual(await updateBootstrap(restored.configPath, f.request, distribution, f.os), complete);
  assert.equal((await cli(['bootstrap-status', '--config', restored.configPath, '--operation-id', second.operationId, '--json'])).exitCode, 3);
  await assert.rejects(updateBootstrap(restored.configPath, { ...second, operationId: 'different-id-for-old-installation' }, distribution, f.os), code('configuration_changed'));
  assert.equal(await exists(join(config.runtimeRoot, 'recovery-original/bootstrap-maintenance', hashJson(f.request.operationId).slice(7), 'record.json')), false);
  await verifyBackup(backup.directory, backup.manifestHash);
});

test('Linux resource maintenance retains both exact plans across partial OS publication and preserves the selected budget', { skip: process.platform !== 'linux', timeout: 60_000 }, async t => {
  const f = await fixture(t);
  const budget: Host.LinuxResourceBudget = { memoryHighBytes: 192 * 1024 * 1024, memoryMaxBytes: 256 * 1024 * 1024, tasksMax: 64 };
  const next = { ...f.request.next, linuxResources: linuxResourceScope(f.request.next.installationId, budget) };
  const owners: BootstrapOwner[] = f.oldPlan.processes.map(instance => {
    const content = linuxUnit(f.oldPlan, instance);
    return { ...instance, candidateId: f.old.candidateId, definitionHash: digest(content), enabled: false, running: false, definition: { content, hash: digest(content) } };
  });
  let failAfterWrite = true, writes = 0;
  const os: BootstrapOs = {
    async inspect(plans) {
      for (const owner of owners) assert.equal(plans.filter(plan => linuxUnit(plan, owner) === owner.definition.content).length, 1, 'actual old/new unit must match exactly one retained plan');
      return structuredClone(owners);
    },
    async render(owner, plan) { const content = linuxUnit(plan, owner); return { content, hash: digest(content) }; },
    async check(definition) { assert.equal(digest(definition.content), definition.hash); },
    async apply(owner, desired, plans) {
      const current = owners.find(value => value.instanceId === owner.instanceId)!;
      assert.ok([owner.definitionHash, desired.hash].includes(current.definitionHash));
      if (current.definitionHash === desired.hash) return;
      const plan = plans.find(value => linuxUnit(value, owner) === desired.content)!;
      assert.ok(plan); current.candidateId = plan.candidateId; current.definition = desired; current.definitionHash = desired.hash; writes++;
      if (failAfterWrite) { failAfterWrite = false; throw new IvyError('outcome_unknown', 'Simulated loss after one exact Linux unit publication.'); }
    },
    async reload() {},
  };
  const request: Host.BootstrapMaintenanceRequest = { ...f.request, previous: await inspectBootstrap(f.configPath, distribution, os), next };
  assert.equal((await updateBootstrap(f.configPath, request, distribution, os)).phase, 'needs_attention'); assert.equal(writes, 1);
  assert.ok((await retainedBootstrapPlans(f.config)).some(plan => hashJson(plan) === hashJson(next)));
  const unpublishedPath = join(f.config.runtimeRoot, 'bootstrap-plans', digest('unpublished plan').slice(7) + '.json.00000000-0000-0000-0000-000000000000.tmp');
  await writeFile(unpublishedPath, '{partial bytes');
  // A new read-only process can recognize mixed definitions without the original request argument.
  assert.equal((await inspectBootstrap(f.configPath, distribution, os)).owners.filter(owner => owner.candidateId === f.next.candidateId).length, 1);
  assert.equal((await updateBootstrap(f.configPath, request, distribution, os)).phase, 'succeeded'); assert.equal(writes, 2);
  assert.deepEqual((await bootstrapPlan(f.configPath, f.old.candidateId)).linuxResources, next.linuxResources);
  const remove: Host.BootstrapMaintenanceRequest = { schemaVersion: 1, operationId: 'remove-boundary', previous: await inspectBootstrap(f.configPath, distribution, os), next: f.oldPlan };
  assert.equal((await updateBootstrap(f.configPath, remove, distribution, os)).errorCode, 'resource_scope_required'); assert.equal(writes, 2);
  const changedBudget = { ...budget, tasksMax: 65 }, changed = await bootstrapPlan(f.configPath, f.old.candidateId, changedBudget);
  const change: Host.BootstrapMaintenanceRequest = { ...remove, operationId: 'change-boundary', next: changed };
  assert.equal((await updateBootstrap(f.configPath, change, distribution, os)).phase, 'succeeded'); assert.equal(writes, 4);
  assert.deepEqual((await bootstrapPlan(f.configPath, f.next.candidateId)).linuxResources?.budget, changedBudget);
  const retainedPath = join(f.config.runtimeRoot, 'bootstrap-plans', hashJson(next).slice(7) + '.json');
  await atomicJson(retainedPath, { ...next, linuxResources: changed.linuxResources });
  await assert.rejects(inspectBootstrap(f.configPath, distribution, os), code('storage_invalid')); assert.equal(writes, 4);
});
