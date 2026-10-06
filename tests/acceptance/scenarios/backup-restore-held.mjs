import assert from 'node:assert/strict';
import { readFile, realpath, access } from 'node:fs/promises';
import { resolve, join, dirname, basename } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { DatabaseSync } from 'node:sqlite';

// Complete offline backup and isolated restore, ending with every service disabled.
// The caller stops its owned installation first. This runner never stops or starts processes.
const { values } = parseArgs({ options: {
  ...Object.fromEntries(['distribution', 'config', 'source-root', 'template', 'target-root', 'backup', 'evidence'].map(key => [key, { type: 'string' }])),
  execute: { type: 'boolean' }
} });
for (const key of ['distribution', 'config', 'source-root', 'template', 'target-root', 'backup', 'evidence']) assert.ok(values[key], 'Missing --' + key);
const distribution = await realpath(resolve(values.distribution));
const load = path => import(pathToFileURL(join(distribution, 'dist', path)).href);
const { inside, atomicJson } = await load('packages/host-runtime/src/config.js');
const { privateDirectory } = await load('packages/host-runtime/src/backup-files.js');
const { validateHost } = await load('packages/contracts/src/validation.js');
const { hashJson } = await load('packages/contracts/src/canonical.js');
const { resolveHostConfiguration, hostConfigurationPath, hostStorageAreas, resolvedFuturePath } = await load('packages/host-runtime/src/layout.js');
const local = await realpath(resolve('.local'));
const sourceRoot = await realpath(resolve(values['source-root'])), targetRoot = await realpath(resolve(values['target-root']));
assert.ok(inside(local, sourceRoot) && inside(local, targetRoot));
assert.ok(!inside(sourceRoot, targetRoot) && !inside(targetRoot, sourceRoot));
const configPath = await realpath(resolve(values.config)), templatePath = await realpath(resolve(values.template));
assert.ok(inside(sourceRoot, configPath) && inside(targetRoot, templatePath));
const sourceBytes = await readFile(configPath), templateBytes = await readFile(templatePath);
const source = resolveHostConfiguration(JSON.parse(sourceBytes), configPath), template = resolveHostConfiguration(JSON.parse(templateBytes), templatePath);
for (const config of [source, template]) {
  validateHost('HostConfig', config);
  assert.ok(config.hostId.endsWith('-acceptance') && !config.restoredFrom);
  assert.ok(config.instances.length && config.instances.every(instance => instance.engine === 'process'));
  assert.equal(new URL(config.publicBaseUrl).hostname, '127.0.0.1');
  assert.equal(new URL(config.publicBaseUrl).protocol, 'http:');
}
assert.equal(template.hostId, source.hostId);
assert.ok(template.instances.every(instance => !instance.enabled));
assert.notEqual(template.publicBaseUrl, source.publicBaseUrl);
const absent = async path => {
  await assert.rejects(access(path), error => error.code === 'ENOENT', 'Path must not already exist: ' + path);
};
for (const path of [...hostStorageAreas(source).map(area => area.path), source.artifactRoot, source.stagingRoot]) assert.ok(inside(sourceRoot, await resolvedFuturePath(path)));
for (const path of [...hostStorageAreas(template).map(area => area.path), template.artifactRoot, template.stagingRoot]) {
  assert.ok(inside(targetRoot, await resolvedFuturePath(path)));
  await absent(path);
}
const freshPath = async value => {
  const requested = resolve(value), path = join(await realpath(dirname(requested)), basename(requested));
  assert.ok(inside(local, path));
  for (const root of [sourceRoot, targetRoot]) assert.ok(!inside(root, path) && !inside(path, root));
  await absent(path); return path;
};
const backup = await freshPath(values.backup), evidence = await freshPath(values.evidence);
assert.ok(!inside(backup, evidence) && !inside(evidence, backup));
await privateDirectory(evidence);
const report = { schemaVersion: 1, startedAt: new Date().toISOString(), sourceRoot, targetRoot,
  sourceConfigurationHash: hashJson(source), templateConfigurationHash: hashJson(template),
  execute: Boolean(values.execute), phase: 'preflight_passed', backupPath: backup,
  scope: 'Complete offline backup and isolated disabled restore; later deployment and enablement are explicit operations.' };
const save = async phase => {
  report.phase = phase; await atomicJson(join(evidence, 'report.json'), report);
  console.log(JSON.stringify({ phase, reportPath: join(evidence, 'report.json') }));
};
await save('preflight_passed');
try {
  if (values.execute) {
    const { cli } = await load('packages/cli/src/main.js');
    await save('backup_intent');
    const saved = await cli(['backup', '--config', configPath, '--destination', backup, '--json']);
    assert.equal(saved.exitCode, 0, 'Backup failed: ' + saved.output.code);
    assert.equal(saved.output.code, 'backup_completed');
    validateHost('BackupResult', saved.output.data); report.backup = saved.output.data;
    assert.equal(report.backup.hostId, source.hostId);
    assert.equal(resolve(report.backup.directory), backup);
    await save('backup_completed');
    const restored = await cli(['restore', '--config', templatePath, '--backup', backup, '--backup-hash', report.backup.manifestHash, '--json']);
    assert.equal(restored.exitCode, 0, 'Restore failed: ' + restored.output.code);
    assert.equal(restored.output.code, 'restore_completed');
    validateHost('RestoreResult', restored.output.data); report.restore = restored.output.data;
    assert.equal(report.restore.backupId, report.backup.backupId);
    assert.equal(report.restore.manifestHash, report.backup.manifestHash);
    assert.equal(report.restore.state, 'disabled');
    assert.equal(resolve(report.restore.configPath), hostConfigurationPath(template));
    const config = JSON.parse(await readFile(report.restore.configPath, 'utf8')); validateHost('HostConfig', config);
    assert.deepEqual(config, { ...template, restoredFrom: { backupId: report.backup.backupId,
      manifestHash: report.backup.manifestHash, restoreId: report.restore.restoreId } });
    assert.ok(config.instances.every(instance => !instance.enabled));
    await absent(join(config.runtimeRoot, 'restore.json'));
    const db = new DatabaseSync(join(config.runtimeRoot, 'deployments.sqlite'), { readOnly: true });
    try {
      assert.ok(db.prepare('PRAGMA quick_check').all().every(row => Object.values(row)[0] === 'ok'));
      assert.deepEqual(db.prepare('PRAGMA foreign_key_check').all(), []);
      for (const table of ['observations', 'runtime_targets']) assert.equal(db.prepare('SELECT count(*) AS n FROM ' + table).get().n, 0);
      assert.equal(db.prepare('SELECT count(*) AS n FROM installed').get().n, 0);
      report.uncertainDeployments = db.prepare("SELECT deployment_id FROM operations WHERE phase='needs_attention'").all().map(row => row.deployment_id);
    } finally { db.close(); }
    report.disabledInstanceIds = config.instances.map(instance => instance.instanceId);
    assert.deepEqual(await readFile(configPath), sourceBytes);
    if (resolve(report.restore.configPath) !== templatePath) assert.deepEqual(await readFile(templatePath), templateBytes);
    report.completedAt = new Date().toISOString(); await save('passed_disabled_restore');
  }
} catch (error) {
  // Assertion diffs can contain configuration credentials; retain only a safe phase and code.
  report.failure = { code: error.code ?? error.name, phase: report.phase };
  await save('failed'); process.exitCode = 1;
}
