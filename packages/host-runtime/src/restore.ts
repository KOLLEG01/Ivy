import { DatabaseSync } from 'node:sqlite';
import { lstat, mkdir, realpath, unlink } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { atomicJson, inside, jsonFile } from './config.js';
import { copyVerified, exists, inventoryFiles, privateDirectory, relativeFile, syncDirectories } from './backup-files.js';
import { fileHash } from './artifact.js';
import { installationRecord } from './accepted-configuration.js';
import { terminalPhases } from './journal.js';
import { nativeBackupExclusions, sharedNativeHomes } from './native-backup-paths.js';
import { instanceOwnedCodexHome, usesExternalAppServer } from './codex-home.js';
import { servicePaths, hostStorageAreas, storageFile, hostConfigurationPath, resolveHostConfiguration, resolvedFuturePath } from './layout.js';
import { relocateNativeStorage } from './native-restore.js';
import { relocateProjects } from './project-restore.js';
import { restoredStorageAreas } from './project-storage.js';
import { holdRestoredBootstrapMaintenance } from './bootstrap-maintenance.js';
import { requireStorageSpace } from './storage-space.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import { validateShared } from '../../contracts/src/core-validation.js';
import { canonical, hashJson } from '../../contracts/src/canonical.js';
import { requireThat } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';

export async function verifyBackup(directory: string, expectedHash: string): Promise<Host.BackupManifest> {
  validateShared('Hash', expectedHash);
  const path = join(directory, 'backup.json'), info = await lstat(path);
  requireThat(info.isFile() && !info.isSymbolicLink() && info.size <= 12 * 1024 * 1024 && !await exists(join(directory, 'backup-failure.json')),
    'backup_invalid', 'Backup publication is incomplete or invalid.');
  requireThat(await fileHash(path) === expectedHash, 'backup_changed', 'Backup manifest does not match the selected immutable identity.');
  const manifest = await jsonFile<Host.BackupManifest>(path, 12 * 1024 * 1024); validateHost('BackupManifest', manifest);
  const names = manifest.files.map(file => process.platform === 'win32' ? file.path.toLowerCase() : file.path);
  requireThat(new Set(names).size === names.length && manifest.files.length > 0, 'backup_invalid', 'Backup file names must be distinct.');
  for (const file of manifest.files) relativeFile(directory, file.path);
  const actual = await inventoryFiles(directory, path => path === 'backup.json');
  const identity = (files: Host.BackupFile[]) => files.map(({ path, hash, bytes }) => ({ path, hash, bytes })).sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
  requireThat(hashJson(identity(actual)) === hashJson(identity(manifest.files)), 'backup_changed', 'Backup payload is missing, extra or changed.');
  const configuration = await jsonFile<Host.HostConfig>(join(directory, 'configuration.json')); validateHost('HostConfig', configuration);
  requireThat(hashJson(configuration) === manifest.configurationHash && configuration.hostId === manifest.hostId, 'backup_invalid', 'Backup configuration identity differs from its manifest.');
  const shared = manifest.sharedNativeHomes ?? [];
  const sharedInstances = configuration.instances.filter(instance => instance.componentId === 'agent-manager' &&
    (usesExternalAppServer(instance.settings) ||
      !instanceOwnedCodexHome(instance.settings, servicePaths(configuration, instance).data)));
  requireThat(shared.length === sharedInstances.length && new Set(shared.map(home => home.instanceId)).size === shared.length && shared.every(home => {
    const instance = sharedInstances.find(value => value.instanceId === home.instanceId);
    if (!instance || !isAbsolute(home.path) || inside(configuration.runtimeRoot, home.path)) return false;
    const connection = instance.settings['appServer'] as { mode?: string; expectedCodexHome?: string } | undefined;
    // A captured environment-derived home is identified by the manifest, not by the
    // account/environment currently verifying this backup. Target validation below
    // still requires the original absolute assignment before restore can proceed.
    const explicit = connection?.mode === 'external-proxy' ? connection.expectedCodexHome ?? instance.settings['codexHome'] ?? instance.settings['nativeHome']
      : instance.settings['codexHome'] ?? instance.settings['nativeHome'];
    return explicit === undefined || typeof explicit === 'string' && resolve(explicit) === resolve(home.path);
  }), 'backup_invalid', 'Shared Codex home references do not match the captured instance ownership.');
  const capturedAreas = manifest.storageAreas ?? [{ key: 'state', path: configuration.runtimeRoot }];
  requireThat(hashJson(capturedAreas.filter(area => !area.key.startsWith('retained-projects/'))) === hashJson(hostStorageAreas(configuration)),
    'backup_invalid', 'Captured service and project storage areas differ from the configuration.');
  for (const area of capturedAreas.filter(area => area.key.startsWith('retained-projects/'))) {
    const [, instanceId, hash, extra] = area.key.split('/');
    requireThat(!extra && configuration.instances.some(instance => instance.instanceId === instanceId && instance.componentId === 'agent-manager') &&
      isAbsolute(area.path) && hash === hashJson(area.path).slice(7) && capturedAreas.every(other => other === area || !inside(other.path, area.path) && !inside(area.path, other.path)),
      'backup_invalid', 'Retained project storage identity is invalid or overlapping.');
  }
  if (manifest.ephemeralNativePaths) {
    requireThat(hashJson(manifest.ephemeralNativePaths) === hashJson(nativeBackupExclusions(configuration)), 'backup_invalid', 'Native cache exclusions differ from the captured configuration.');
    requireThat(manifest.ephemeralNativePaths.every(entry => !manifest.files.some(file => { const path = entry.path.startsWith('services/') ? entry.path : 'state/' + entry.path; return file.path === path || file.path.startsWith(path + '/'); })),
      'backup_invalid', 'Backup contains a declared ephemeral native cache.');
  }
  return manifest;
}
async function targetConfiguration(templatePath: string, backupRoot: string, manifest: Host.BackupManifest, manifestHash: string, restoreId: string): Promise<Host.HostConfig> {
  const original = await jsonFile<Host.HostConfig>(join(backupRoot, 'configuration.json'));
  const raw = await jsonFile<Host.HostConfigInput>(templatePath); validateHost('HostConfigInput', raw);
  const input = resolveHostConfiguration(raw, resolve(templatePath)); validateHost('HostConfig', input);
  requireThat(!input.restoredFrom && input.hostId === manifest.hostId && input.instances.every(instance => !instance.enabled), 'target_conflict', 'Restore needs the original host identity, a fresh configuration and every instance disabled.');
  const identity = (host: Host.HostConfig) => host.instances.map(({ instanceId, componentId, serviceNodeId }) => ({ instanceId, componentId, serviceNodeId })).sort((a, b) => a.instanceId.localeCompare(b.instanceId));
  requireThat(hashJson(identity(input)) === hashJson(identity(original)) && input.publicBaseUrl !== original.publicBaseUrl,
    'target_conflict', 'Isolated restore retains instance identities and needs a distinct explicit Hive endpoint.');
  requireThat(hashJson(input.executables) === hashJson(original.executables), 'target_conflict', 'This restore requires its recorded host executable mappings.');
  for (const prerequisite of manifest.externalFiles) {
    requireThat(isAbsolute(prerequisite.path) && await fileHash(prerequisite.path) === prerequisite.hash, 'runtime_changed', 'A recorded runtime/native executable prerequisite is missing or changed.');
  }
  const roots = [input.runtimeRoot, input.artifactRoot, input.stagingRoot];
  const sourceAreas = manifest.storageAreas ?? [{ key: 'state', path: original.runtimeRoot }], targetAreas = restoredStorageAreas(input, sourceAreas);
  const targetConfig = hostConfigurationPath(input);
  const actualRoots = await Promise.all(roots.map(resolvedFuturePath));
  const actualAreas = await Promise.all(targetAreas.map(area => resolvedFuturePath(area.path)));
  const protectedPaths = await Promise.all([...sourceAreas.map(area => area.path), original.artifactRoot, original.stagingRoot, backupRoot, ...sharedNativeHomes(input).map(home => home.path)].map(resolvedFuturePath));
  const actualConfig = await resolvedFuturePath(targetConfig);
  requireThat(actualConfig !== await resolvedFuturePath(hostConfigurationPath(original)) && protectedPaths.every(path => !inside(path, actualConfig)) &&
    [...actualRoots.slice(1), ...actualAreas.slice(1)].every(path => !inside(path, actualConfig)) &&
    (targetConfig === resolve(templatePath) || !await exists(targetConfig)), 'target_conflict', 'Restore configuration must be a fresh target, never the original user configuration.');
  requireThat(hashJson(sourceAreas.map(area => area.key)) === hashJson(targetAreas.map(area => area.key)), 'target_conflict', 'Restore must explicitly preserve the captured service/project layout.');
  for (const area of targetAreas.filter(area => area.key !== 'state')) {
    const actual = await resolvedFuturePath(area.path);
    requireThat(isAbsolute(area.path) && !await exists(area.path) && [...protectedPaths, ...actualRoots]
      .every(root => !inside(root, actual) && !inside(actual, root)), 'target_conflict', 'Restored service and project areas must be fresh, disjoint and outside original data.');
  }
  const baseAreas = actualAreas.filter((_, index) => !targetAreas[index]!.key.startsWith('retained-projects/'));
  requireThat(baseAreas.every((area, index) => baseAreas.every((other, otherIndex) => index === otherIndex || !inside(area, other))), 'target_conflict', 'Restore storage areas overlap.');
  requireThat(hashJson(sharedNativeHomes(input)) === hashJson(manifest.sharedNativeHomes ?? []),
    'target_conflict', 'Shared Codex homes are external prerequisites and must retain their captured assignment; restore never copies or relocates their data.');
  for (const root of roots) {
    requireThat(isAbsolute(root) && !await exists(root) && await exists(dirname(root)), 'target_conflict', 'Restore target roots must be fresh absolute directories with existing parents.');
    const actual = await resolvedFuturePath(root);
    requireThat(protectedPaths.every(other => !inside(other, actual) && !inside(actual, other)) && actualRoots.every(other => other === actual || !inside(other, actual) && !inside(actual, other)),
      'target_conflict', 'Restore roots overlap each other, the backup or the source installation.');
  }
  requireThat(new Set(roots.map(root => resolve(root).toLowerCase())).size === roots.length, 'target_conflict', 'Restore roots must be distinct.');
  for (const instance of input.instances) {
    const data = servicePaths(input, instance).data;
    if (instance.componentId === 'agent-manager' && !manifest.sharedNativeHomes?.some(home => home.instanceId === instance.instanceId)) requireThat(instanceOwnedCodexHome(instance.settings, data),
      'target_conflict', 'Restored owned native home must be explicitly bound inside its new instance data.');
    if (['host-executor', 'service-manager'].includes(instance.componentId)) requireThat(instance.settings['hostConfigPath'] === hostConfigurationPath(input),
      'target_conflict', 'Restored management components must explicitly reference the new host configuration.');
    if (instance.componentId === 'hive') {
      const settings = instance.settings as unknown as Host.HiveSettings; validateHost('HiveSettings', settings);
      const prior = original.instances.find(value => value.instanceId === instance.instanceId)!;
      if (instance.engine === 'process' && prior.engine === 'process') requireThat(settings.listenPort !== (prior.settings as unknown as Host.HiveSettings).listenPort,
        'target_conflict', 'Isolated Hive process must use a different listener port from its source installation.');
      if (instance.engine === 'docker') requireThat(instance.docker && instance.docker.ports.every(port => !prior.docker?.ports.some(old => old.port === port.port)),
        'target_conflict', 'Isolated Hive container must use different host listener ports.');
      requireThat(inside(input.runtimeRoot, settings.backup.directory) && !inside(data, settings.backup.directory), 'target_conflict', 'Restored Hive backup directory must be separate owned runtime storage.');
    }
  }
  return { ...input, restoredFrom: { backupId: manifest.backupId, manifestHash, restoreId } };
}
export async function restoreHost(backupRoot: string, manifestHash: string, templatePath: string, distributionRoot: string): Promise<Host.RestoreResult> {
  const started = performance.now(), manifest = await verifyBackup(backupRoot, manifestHash);
  requireThat(manifest.platform.os === process.platform && manifest.platform.arch === process.arch && manifest.platform.node === process.version,
    'unsupported_runtime', 'Restore requires its original OS/architecture/Node toolchain.');
  const restoreId = randomUUID(), config = await targetConfiguration(templatePath, backupRoot, manifest, manifestHash, restoreId);
  const areas = restoredStorageAreas(config, manifest.storageAreas ?? []);
  const total = manifest.files.reduce((sum, file) => sum + file.bytes, 0);
  const expandedBytes = 0;
  for (const root of [...areas.map(area => area.path), config.artifactRoot, config.stagingRoot]) {
    let ancestor = dirname(root);
    while (!await exists(ancestor)) ancestor = dirname(ancestor);
    await requireStorageSpace(ancestor, total + expandedBytes + 64 * 1024 * 1024);
  }
  const createdAt = new Date().toISOString();
  await privateDirectory(config.runtimeRoot);
  await privateDirectory(config.artifactRoot); await privateDirectory(config.stagingRoot);
  const configPath = hostConfigurationPath(config);
  for (const area of areas) if (area.key !== 'state') await mkdir(area.path, { recursive: true, mode: 0o700 });
  for (const file of manifest.files) {
    if (!file.path.startsWith('state/')) {
      if (areas.some(area => area.key !== 'state' && file.path.startsWith(area.key + '/')))
        await copyVerified(relativeFile(backupRoot, file.path), storageFile(config, file.path, areas), file, true);
      continue;
    }
    let local = file.path.slice(6);
    if (local === 'restore.json' || local === 'host.json' || local === 'native-recovery.json' || local === 'project-recovery.json' || local.startsWith('restore-decisions/') || local.startsWith('recovery-original/') || local.startsWith('previous-recovery/')) local = 'previous-recovery/' + local;
    await copyVerified(relativeFile(backupRoot, file.path), relativeFile(config.runtimeRoot, local), file, true);
  }
  await mkdir(join(config.runtimeRoot, 'recovery-original'), { recursive: true, mode: 0o700 });
  await copyVerified(join(config.runtimeRoot, 'deployments.sqlite'), join(config.runtimeRoot, 'recovery-original/deployments.sqlite'));
  await copyVerified(join(backupRoot, 'configuration.json'), join(config.runtimeRoot, 'recovery-original/configuration.json'));
  const maintenanceOriginalBytes = await holdRestoredBootstrapMaintenance(config, createdAt);
  await relocateProjects(await jsonFile<Host.HostConfig>(join(backupRoot, 'configuration.json')), config, manifest.storageAreas ?? []);
  const nativeRecovery = await relocateNativeStorage(await jsonFile<Host.HostConfig>(join(backupRoot, 'configuration.json')), config, manifest, distributionRoot);
  // The exact original authority is above; only the new working journal is reconciled.
  const db = new DatabaseSync(join(config.runtimeRoot, 'deployments.sqlite'));
  try {
    db.exec('PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL; BEGIN IMMEDIATE');
    requireThat(Number(db.prepare('PRAGMA user_version').get()!['user_version']) === 1 && db.prepare('SELECT value FROM meta WHERE key=?').get('hostId')?.['value'] === config.hostId,
      'unsupported_storage', 'Restored host journal format/identity is unsupported.');
    db.exec('DELETE FROM candidates; DELETE FROM installed;');
    // Local releases are excluded from backups. Their deletion intents and retention
    // state belong only to the original installation, preserved in recovery-original.
    if (db.prepare("SELECT 1 FROM sqlite_schema WHERE type='table' AND name='candidate_retirements'").get())
      db.exec('DELETE FROM candidate_retirements;');
    db.exec("DELETE FROM meta WHERE key='storage-retention' OR key LIKE 'bootstrap-rollback:%';");
    for (const row of db.prepare('SELECT entry_json FROM operations ORDER BY deployment_id').all()) {
      const entry = JSON.parse(String(row['entry_json'])) as Host.JournalEntry; validateHost('JournalEntry', entry);
      entry.configurationPath = join(config.runtimeRoot, 'accepted-configurations', basename(entry.configurationPath));
      if (entry.activation.previousConfigurationPath) entry.activation.previousConfigurationPath = join(config.runtimeRoot, 'accepted-configurations', basename(entry.activation.previousConfigurationPath));
      if (terminalPhases.has(entry.record.phase)) {
        db.prepare('UPDATE operations SET entry_json=? WHERE deployment_id=?').run(canonical(entry), entry.record.deploymentId);
        continue;
      }
      entry.record = { ...entry.record, phase: 'needs_attention', updatedAt: createdAt, observedBuild: null, errorCode: 'restore_reconciliation_required',
        readiness: { state: 'unknown', message: 'Restored deployment may have effects after its snapshot; original evidence is retained and no action is replayed.' } };
      validateHost('JournalEntry', entry);
      db.prepare('UPDATE operations SET phase=?,entry_json=? WHERE deployment_id=?').run('needs_attention', canonical(entry), entry.record.deploymentId);
    }
    db.exec('DELETE FROM observations; DELETE FROM runtime_targets;');
    db.prepare('INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value').run('installation', installationRecord(config));
    db.exec('COMMIT');
  } finally { if (db.isTransaction) db.exec('ROLLBACK'); db.close(); }
  // Old launch-specific controls cannot act on a newly selected runtime target.
  for (const instance of config.instances) for (const name of ['health.json', 'control.json']) {
    const path = join(servicePaths(config, instance).data, name); if (await exists(path)) await unlink(path);
  }
  for (const root of [...hostStorageAreas(config).map(area => area.path), config.artifactRoot, config.stagingRoot]) await syncDirectories(root);
  await atomicJson(configPath, config);
  const result: Host.RestoreResult = { schemaVersion: 1, restoreId, backupId: manifest.backupId, manifestHash,
    configPath, state: 'disabled', restoredBytes: total + expandedBytes + maintenanceOriginalBytes + nativeRecovery.instances.flatMap(value => value.originalFiles).reduce((sum, file) => sum + file.bytes, 0), elapsedMs: Math.round(performance.now() - started) };
  validateHost('RestoreResult', result); return result;
}
