import assert from 'node:assert/strict';
import { readdir, lstat, realpath, readFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { parseArgs, promisify } from 'node:util';
import { execFile } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';

// Recovery acceptance for one explicitly selected fresh local installation.
// No native account/login mutation; exact inspected OS definitions remain the action boundary.
const keys = ['config', 'distribution', 'candidate', 'evidence', 'operation-id', 'config-hash', 'installation-root'];
const { values } = parseArgs({ options: { ...Object.fromEntries(keys.map(key => [key, { type: 'string' }])), execute: { type: 'boolean' } } });
for (const key of keys) assert.ok(values[key], 'Missing --' + key);
assert.equal(process.platform, 'win32');
assert.match(values['operation-id'], /^[a-z0-9-]{1,60}$/);
assert.match(values['config-hash'], /^sha256:[0-9a-f]{64}$/);
const configPath = resolve(values.config), distribution = resolve(values.distribution);
const module = path => import(pathToFileURL(join(distribution, 'dist', path)).href);
const { hostConfig } = await module('packages/host-runtime/src/host-config.js');
const { HostJournal } = await module('packages/host-runtime/src/journal.js');
const { atomicJson, inside } = await module('packages/host-runtime/src/config.js');
const { privateDirectory, exists } = await module('packages/host-runtime/src/backup-files.js');
const { verifyCandidate, fileHash } = await module('packages/host-runtime/src/artifact.js');
const { inspectBootstrap, updateBootstrap, bootstrapStatus } = await module('packages/host-runtime/src/bootstrap-maintenance.js');
const { configurationBootstrapPlan } = await module('packages/host-runtime/src/bootstrap.js');
const { runtimeEnvironment } = await module('packages/host-runtime/src/process.js');
const { hashJson } = await module('packages/contracts/src/canonical.js');
const { HiveClient } = await module('packages/sdk/src/client.js');
const installationRoot = await realpath(resolve(values['installation-root']));
assert.ok(inside(await realpath(resolve('.local')), installationRoot), 'Select a fresh installation under this checkout/.local.');
assert.ok(inside(installationRoot, await realpath(configPath)));
const selected = JSON.parse(await readFile(configPath, 'utf8'));
// hostConfig creates missing runtime directories. Validate the existing owned roots first,
// before calling it, so a wrong configuration cannot create directories elsewhere.
for (const path of [selected.runtimeRoot, selected.artifactRoot, selected.stagingRoot]) assert.ok(inside(installationRoot, await realpath(path)));
const config = await hostConfig(configPath);
assert.ok(config.hostId.endsWith('-acceptance'));
const components = ['agent-manager', 'automation-example', 'host-executor', 'hive', 'service-manager', 'task-board'];
assert.deepEqual(config.instances.map(x => x.componentId).sort(), [...components].sort());
const instanceFor = component => config.instances.find(x => x.componentId === component).instanceId;
const ids = config.instances.map(x => x.instanceId).sort(), journal = new HostJournal(config);
const bootstrapIds = config.instances.filter(x => x.engine === 'process' && ['host-executor', 'service-manager'].includes(x.componentId)).map(x => x.instanceId).sort();
assert.equal(hashJson(config), values['config-hash']);
const candidate = journal.candidate(values.candidate);
assert.equal(candidate.componentId, 'host-executor');
assert.equal(resolve(candidate.artifactRoot), distribution);
assert.ok(inside(config.artifactRoot, distribution));
const root = resolve(values.evidence);
assert.ok(inside(installationRoot, root) && !inside(config.runtimeRoot, root) && !inside(config.artifactRoot, root) && !inside(config.stagingRoot, root));
assert.equal(await exists(root), false, 'Keep original evidence; do not restart an interrupted run under a new identity.');
await privateDirectory(root);
const reportPath = join(root, 'report.json'), operationId = values['operation-id'];
const report = { schemaVersion: 1, hostId: config.hostId, operationId, candidateId: candidate.candidateId, buildId: candidate.buildId,
  startedAt: new Date().toISOString(), phase: 'preflight', execute: Boolean(values.execute), steps: [] };
const secrets = config.instances.flatMap(x => [x.credential, ...(x.componentId === 'hive' ? x.settings.credentials.map(y => y.token) : [])]).filter(Boolean);
const save = async () => { const text = JSON.stringify(report); for (const secret of secrets) assert.ok(!text.includes(secret)); await atomicJson(reportPath, report); };
const step = async (name, details = {}) => { report.steps.push({ name, at: new Date().toISOString(), ...details }); await save(); console.log(JSON.stringify({ name, ...details })); };
const failure = error => ({ code: typeof error.code === 'string' ? error.code : error.name });
const exec = promisify(execFile);
const owners = async (snapshot, action) => {
  const path = join(root, 'os-' + action + '.json'); await atomicJson(path, snapshot);
  const result = await exec(join(process.env.SystemRoot, 'System32/WindowsPowerShell/v1.0/powershell.exe'),
    ['-NoProfile', '-NonInteractive', '-File', fileURLToPath(new URL('../fixtures/windows-bootstrap-owners.ps1', import.meta.url)), '-PlanPath', path, '-Action', action,
      '-ExpectedHostId', config.hostId, '-ExpectedInstallationId', report.baseline.before.installationId,
      '-ExecutorInstanceId', instanceFor('host-executor'), '-ExpectedOwnerCount', String(bootstrapIds.length)],
    { windowsHide: true, timeout: 90000, maxBuffer: 131072, env: runtimeEnvironment() });
  const value = JSON.parse(result.stdout); assert.equal(value.ok, true); return value;
};
const until = async (check, code, timeout = 240000) => {
  const deadline = Date.now() + timeout;
  while (!await check()) { if (Date.now() >= deadline) throw Object.assign(new Error(code), { code }); await delay(500); }
};
const fresh = value => value && Date.now() - Date.parse(value.observedAt) < 5000;
const lifecycle = async (action, instanceId) => {
  // CLI timeout is only an observation failure. Retain its exact operation ID; never redispatch
  // under a different ID or infer that its accepted deployment was cancelled.
  const id = operationId + '-' + action + '-' + instanceId;
  await step('lifecycle_intent', { action, instanceId, operationId: id });
  const result = await exec(config.executables.node, [join(distribution, 'dist/packages/cli/src/main.js'), action, '--instance', instanceId,
    '--operation-id', id, '--wait-ms', '600000', '--config', configPath, '--json'],
    { windowsHide: true, timeout: 650000, maxBuffer: 1048576, env: runtimeEnvironment() });
  const value = JSON.parse(result.stdout); assert.equal(value.code, 'deployment_succeeded');
  await step('lifecycle_completed', { action, instanceId, operationId: id, deploymentId: value.deploymentId });
};
const candidatesHash = () => hashJson(journal.db.prepare('SELECT candidate_json,manifest_json FROM candidates ORDER BY candidate_id').all());
const nativeFiles = async () => {
  const { instanceOwnedCodexHome } = await module('packages/host-runtime/src/codex-home.js');
  const { servicePaths } = await module('packages/host-runtime/src/layout.js');
  const instance = config.instances.find(x => x.componentId === 'agent-manager');
  assert.equal(instance.settings.appServer?.mode, 'owned-stdio', 'Destructive acceptance requires an explicitly owned test server.');
  const home = instanceOwnedCodexHome(instance.settings, servicePaths(config, instance).data);
  assert.ok(home && await exists(home));
  const files = [];
  const visit = async path => {
    for (const entry of await readdir(path, { withFileTypes: true })) {
      assert.ok(files.length < 10000, 'Native history exceeds this acceptance bound.');
      assert.equal(entry.isSymbolicLink(), false, 'Native history cannot traverse links.');
      const full = join(path, entry.name);
      if (entry.isDirectory()) await visit(full);
      else if (entry.name.endsWith('.jsonl')) { assert.ok((await lstat(full)).isFile()); files.push({ path: full, hash: await fileHash(full) }); }
    }
  };
  for (const name of ['sessions', 'archived_sessions']) if (await exists(join(home, name))) await visit(join(home, name));
  return files.sort((a, b) => a.path.localeCompare(b.path));
};
let pauseStarted = false, osStopStarted = false;
try {
  await verifyCandidate(candidate, config);
  assert.equal(journal.unfinished().length, 0);
  const original = ids.map(instanceId => ({ instanceId, installed: journal.installed(instanceId), target: journal.target(instanceId) }));
  assert.ok(original.every(x => x.installed?.enabled && x.target?.desired === 'running'));
  const before = await inspectBootstrap(configPath, distribution);
  assert.deepEqual(before.owners.map(x => x.instanceId).sort(), bootstrapIds);
  assert.ok(before.owners.every(x => x.enabled && x.running));
  report.baseline = { before, original, observations: journal.observations(), candidatesHash: candidatesHash(), nativeFiles: await nativeFiles(),
    operations: journal.db.prepare('SELECT deployment_id,entry_json FROM operations ORDER BY deployment_id').all()
      .map(x => ({ deploymentId: x.deployment_id, hash: hashJson(JSON.parse(x.entry_json)) })) };
  assert.equal(hashJson(await hostConfig(configPath)), values['config-hash']);
  await step('preflight_verified');
  if (values.execute) {
    pauseStarted = true;
    for (const component of ['agent-manager', 'task-board', 'automation-example', 'service-manager', 'hive']) await lifecycle('disable', instanceFor(component));
    assert.equal(journal.unfinished().length, 0);
    assert.equal(hashJson(await hostConfig(configPath)), values['config-hash']);
    osStopStarted = true; await step('os_stop_intent'); await owners(before, 'stop');
    const stopped = await inspectBootstrap(configPath, distribution);
    assert.ok(stopped.owners.every(x => !x.enabled && !x.running));
    const request = { schemaVersion: 1, operationId, previous: stopped, next: configurationBootstrapPlan(config, configPath, candidate) };
    await atomicJson(join(root, 'request.json'), request); await step('bootstrap_update_intent');
    report.maintenance = await updateBootstrap(configPath, request, distribution);
    assert.equal(report.maintenance.phase, 'succeeded');
    assert.deepEqual(await bootstrapStatus(config, operationId), report.maintenance);
    assert.deepEqual(await updateBootstrap(configPath, request, distribution), report.maintenance);
    await step('bootstrap_verified');
  } else report.phase = 'preflight_passed';
} catch (error) { report.failure = failure(error); report.phase = 'failed'; process.exitCode = 1; }
finally {
  if (pauseStarted) {
    try {
      // An unresolved lifecycle operation must be reconciled before any inverse action.
      assert.equal(journal.unfinished().length, 0, 'Original lifecycle operation is still authoritative.');
      for (const intent of report.steps.filter(x => x.name === 'lifecycle_intent')) {
        const originalOperation = journal.operation(intent.operationId);
        if (originalOperation) assert.ok(['succeeded', 'failed', 'rolled_back'].includes(originalOperation.record.phase), 'Reconcile the original uncertain lifecycle outcome before resuming.');
      }
      const original = report.baseline.before;
      if (osStopStarted) {
        const seen = await inspectBootstrap(configPath, distribution);
        let maintenance = null;
        try { maintenance = await bootstrapStatus(config, operationId); } catch (error) { if (error.code !== 'not_found') throw error; }
        const resume = { ...seen, owners: seen.owners.map(owner => {
          const prior = original.owners.find(x => x.instanceId === owner.instanceId);
          const updated = maintenance?.definitions.find(x => x.instanceId === owner.instanceId);
          assert.ok([prior.definitionHash, updated?.desiredHash].includes(owner.definitionHash));
          return { ...owner, enabled: prior.enabled, running: prior.running };
        }) };
        await step('resume_os_intent'); await owners(resume, 'start');
      }
      await until(() => { const x = journal.observations()[instanceFor('host-executor')]; return fresh(x) && x.state === 'ready'; }, 'executor_resume_deadline');
      for (const id of ['hive', 'service-manager', 'agent-manager', 'task-board', 'automation-example'].map(instanceFor)) {
        if (!journal.installed(id).enabled || journal.target(id)?.desired !== 'running') await lifecycle('enable', id);
      }
      await until(() => ids.every(id => { const x = journal.observations()[id]; return fresh(x) && x.state === 'ready'; }), 'services_resume_deadline');
      assert.equal(hashJson(await hostConfig(configPath)), values['config-hash']);
      assert.equal(candidatesHash(), report.baseline.candidatesHash);
      assert.deepEqual(await nativeFiles(), report.baseline.nativeFiles);
      for (const x of report.baseline.operations) assert.equal(hashJson(journal.get(x.deploymentId)), x.hash);
      for (const x of report.baseline.original) {
        const after = journal.installed(x.instanceId);
        for (const key of ['candidateId', 'buildId', 'enabled']) assert.equal(after[key], x.installed[key]);
      }
      const hive = config.instances.find(x => x.componentId === 'hive');
      const client = new HiveClient(config.publicBaseUrl, { credential: hive.settings.credentials[0].token });
      report.hive = await client.request('system.status', {});
      report.after = await inspectBootstrap(configPath, distribution);
      if (report.maintenance?.phase === 'succeeded') assert.ok(report.after.owners.every(x => x.candidateId === candidate.candidateId));
      const seen = journal.observations();
      if (osStopStarted) for (const id of ids) assert.notEqual(seen[id].ownerBootId, report.baseline.observations[id].ownerBootId);
      report.sourceResumed = true;
      if (!report.failure) report.phase = 'bootstrap_installed_and_ready';
    } catch (error) { report.resumeFailure = failure(error); report.phase = 'needs_attention'; process.exitCode = 1; }
  }
  report.completedAt = new Date().toISOString(); await save(); journal.close();
  console.log(JSON.stringify({ reportPath, phase: report.phase, failure: report.failure ?? report.resumeFailure ?? null }));
}
