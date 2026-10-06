import { DatabaseSync, backup } from 'node:sqlite';
import { lstat, mkdir, open, realpath } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { HostJournal, ExecutorLock } from './journal.js';
import { hostConfig } from './host-config.js';
import { atomicJson, inside, jsonFile } from './config.js';
import { fileHash } from './artifact.js';
import { copyVerified, exists, inventoryFiles, privateDirectory, relativeFile, syncDirectories } from './backup-files.js';
import { inspectContainer } from './docker.js';
import { requireClearProcessStopFence } from './process-fence.js';
import { requireStorageSpace } from './storage-space.js';
import { runCommand } from './process.js';
import { nativeBackupExclusions, sharedNativeHomes } from './native-backup-paths.js';
import { servicePaths, hostStorageAreas, storagePath, storageFile, resolveHostConfiguration } from './layout.js';
import { projectStorageAreas } from './project-storage.js';
import { hashJson } from '../../contracts/src/canonical.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import { requireThat, IvyError } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';

/** Held OS locks prevent a new runtime-owner/executor/native owner throughout offline maintenance. */
export async function offlineOwners(config: Host.HostConfig): Promise<() => void> {
  const locks: ExecutorLock[] = [];
  const close = () => { for (const lock of locks.reverse()) lock.close(); };
  try {
    locks.push(new ExecutorLock(join(config.runtimeRoot, 'executor')));
    for (const instance of [...config.instances].sort((a, b) => a.instanceId.localeCompare(b.instanceId))) {
      locks.push(new ExecutorLock(join(config.runtimeRoot, 'owners', instance.instanceId)));
      const data = servicePaths(config, instance).data; await requireClearProcessStopFence(data);
      if (instance.engine === 'docker') {
        const container = await inspectContainer(config, instance.instanceId);
        requireThat(!container || ['created', 'exited', 'dead'].includes(container.state), 'backup_busy', 'Owned container must be stopped before complete backup.');
      }
    }
    return close;
  } catch (error) {
    close();
    if (error instanceof IvyError && error.code === 'executor_already_running') throw new IvyError('backup_busy', 'Stop the explicitly owned OS bootstrap processes before complete backup.');
    throw error;
  }
}

/** Bootstrap replacement stops only its OS owners; ordinary services keep running. */
export async function offlineBootstrapOwners(config: Host.HostConfig, instanceIds: readonly string[]): Promise<() => void> {
  const locks: ExecutorLock[] = [];
  const close = () => { for (const lock of locks.reverse()) lock.close(); };
  const acquireStoppedOwner = async (path: string): Promise<ExecutorLock> => {
    const deadline = Date.now() + 5_000;
    for (;;) {
      try { return new ExecutorLock(path); }
      catch (error) {
        if (!(error instanceof IvyError) || error.code !== 'executor_already_running' || Date.now() >= deadline) throw error;
        await delay(100);
      }
    }
  };
  try {
    locks.push(await acquireStoppedOwner(join(config.runtimeRoot, 'executor')));
    for (const instanceId of [...new Set(instanceIds)].sort()) {
      const instance = config.instances.find(value => value.instanceId === instanceId);
      requireThat(instance && instance.engine === 'process' && ['host-executor', 'service-manager'].includes(instance.componentId),
        'target_conflict', 'Bootstrap maintenance may lock only configured bootstrap process owners.');
      locks.push(await acquireStoppedOwner(join(config.runtimeRoot, 'owners', instanceId)));
      await requireClearProcessStopFence(servicePaths(config, instance).data);
    }
    return close;
  } catch (error) {
    close();
    if (error instanceof IvyError && error.code === 'executor_already_running') throw new IvyError('backup_busy', 'Stop the explicitly owned OS bootstrap processes before bootstrap maintenance.');
    throw error;
  }
}
async function snapshotJournal(source: string, destination: string): Promise<void> {
  const db = new DatabaseSync(source, { readOnly: true, timeout: 1000 });
  try {
    db.exec('PRAGMA query_only=ON; BEGIN');
    requireThat(db.prepare('PRAGMA quick_check').all().every(row => Object.values(row)[0] === 'ok'), 'storage_invalid', 'Host journal failed its integrity check.');
    await backup(db, destination);
    const file = await open(destination, 'r+'); try { await file.sync(); } finally { await file.close(); }
  } finally { if (db.isTransaction) db.exec('ROLLBACK'); db.close(); }
}
export async function backupHost(configPath: string, destination: string, distributionRoot: string): Promise<Host.BackupResult> {
  const started = performance.now(), config = await hostConfig(configPath);
  requireThat(isAbsolute(destination), 'invalid_arguments', 'Backup destination must be absolute.');
  let areas = hostStorageAreas(config);
  const sourceRoots = await Promise.all([...areas.map(area => area.path), config.artifactRoot, config.stagingRoot].map(async path => await exists(path) ? realpath(path) : resolve(path)));
  const parent = await realpath(dirname(destination));
  requireThat(sourceRoots.every(root => !inside(root, join(parent, basename(destination))) && !inside(destination, root)), 'target_conflict', 'Private backup storage must be outside live runtime, artifact and staging roots.');
  const closeOwners = await offlineOwners(config); let journal: HostJournal | null = null, created = false;
  const backupId = randomUUID(), createdAt = new Date().toISOString();
  try {
    areas = await projectStorageAreas(config);
    const actualDestination = join(parent, basename(destination));
    for (const area of areas) if (await exists(area.path)) {
      const actual = await realpath(area.path);
      requireThat(!inside(actual, actualDestination) && !inside(actualDestination, actual), 'target_conflict', 'Backup destination overlaps retained internal work.');
    }
    journal = new HostJournal(config); journal.db.exec('BEGIN IMMEDIATE');
    const configurationHash = hashJson(config);
    const currentConfiguration = async () => resolveHostConfiguration(await jsonFile<Host.HostConfigInput>(configPath), resolve(configPath));
    requireThat(hashJson(await currentConfiguration()) === configurationHash, 'configuration_changed', 'Backup master configuration changed.');
    const hive = config.instances.filter(instance => instance.componentId === 'hive');
    for (const instance of hive) {
      const settings = instance.settings as unknown as Host.HiveSettings; validateHost('HiveSettings', settings);
      requireThat(areas.every(area => !inside(settings.backup.directory, area.path)),
        'target_conflict', 'Rotating Hive backup storage cannot cover authoritative runtime data.');
    }
  const ownerLocks = ['executor', 'restore-decision-lock', 'bootstrap-maintenance-lock', ...config.instances.map(instance => 'owners/' + instance.instanceId)].map(path => path + '/executor-lock.sqlite');
    const ephemeralNativePaths = nativeBackupExclusions(config);
    const sharedHomes = sharedNativeHomes(config);
    for (const entry of ephemeralNativePaths) {
      const path = storageFile(config, entry.path.startsWith('services/') ? entry.path : 'state/' + entry.path);
      if (await exists(path)) requireThat((await lstat(path)).isDirectory() && !(await lstat(path)).isSymbolicLink(), 'target_conflict', 'Native helper cache root must be an owned directory.');
    }
    const excluded = (key: string) => ownerLocks.some(path => [path, path + '-journal', path + '-wal', path + '-shm'].some(local => key === 'state/' + local)) || /^state\/deployments\.sqlite(?:-(?:wal|shm))?$/.test(key) ||
      ephemeralNativePaths.some(entry => (entry.path.startsWith('services/') ? entry.path : 'state/' + entry.path) === key) ||
      hive.some(instance => ['', '-wal', '-shm'].some(suffix => key === storagePath(config, join(servicePaths(config, instance).data, 'hive.sqlite' + suffix)))) ||
      hive.some(instance => { const settings = instance.settings as unknown as Host.HiveSettings; return inside(settings.backup.directory, storageFile(config, key, areas)); });
    const inventoryAreas = async () => {
      const result: (Host.BackupFile & { source: string })[] = [];
      for (const area of areas) if (await exists(area.path)) for (const file of await inventoryFiles(area.path, local => excluded(area.key + '/' + local)))
        result.push({ ...file, path: area.key + '/' + file.path, source: relativeFile(area.path, file.path) });
      return result;
    };
    const runtimeFiles = await inventoryAreas();
    const externalPaths = new Set(Object.values(config.executables));
    for (const instance of config.instances) if (instance.componentId === 'agent-manager') {
      const executable = instance.settings['nativeExecutable']; if (typeof executable === 'string') externalPaths.add(executable);
      const shell = instance.settings['windowsShell'] as { executable?: string } | undefined; if (shell?.executable) externalPaths.add(shell.executable);
    }
    const externalFiles: Host.BackupFile[] = [];
    for (const path of [...externalPaths].sort()) {
      requireThat(isAbsolute(path), 'invalid_arguments', 'Backup executable prerequisites must be absolute.');
      const info = await lstat(await realpath(path));
      requireThat(info.isFile(), 'backup_invalid', 'Runtime prerequisite is not a file.');
      externalFiles.push({ path, hash: await fileHash(path), bytes: info.size, mode: info.mode & 0o777 });
    }
    const archiveBytes = 0;
    let sqliteBytes = (await lstat(join(config.runtimeRoot, 'deployments.sqlite'))).size;
    for (const instance of hive) for (const suffix of ['', '-wal']) {
      const path = join(servicePaths(config, instance).data, 'hive.sqlite' + suffix);
      if (await exists(path)) sqliteBytes += (await lstat(path)).size;
    }
    const reserve = runtimeFiles.reduce((sum, value) => sum + value.bytes, 0) + archiveBytes + sqliteBytes * 2 + 64 * 1024 * 1024;
    requireThat(reserve <= 64 * 1024 ** 3, 'limit_exceeded', 'Private backup exceeds its bounded allocation workload.');
    await requireStorageSpace(parent, reserve); await privateDirectory(destination); created = true;
    const files: Host.BackupFile[] = [];
    const copy = async (source: string, local: string, expected?: Host.BackupFile) => {
      const file = await copyVerified(source, relativeFile(destination, local), expected); files.push({ ...file, path: local });
    };
    await atomicJson(join(destination, 'configuration.json'), config);
    const configurationInfo = await lstat(join(destination, 'configuration.json'));
    files.push({ path: 'configuration.json', hash: await fileHash(join(destination, 'configuration.json')), bytes: configurationInfo.size, mode: configurationInfo.mode & 0o777 });
    await mkdir(join(destination, 'state'), { mode: 0o700 });
    await snapshotJournal(join(config.runtimeRoot, 'deployments.sqlite'), join(destination, 'state/deployments.sqlite'));
    const addSnapshot = async (local: string) => { const path = relativeFile(destination, local), info = await lstat(path); files.push({ path: local, hash: await fileHash(path), bytes: info.size, mode: 0o600 }); };
    await addSnapshot('state/deployments.sqlite');
    for (const file of runtimeFiles) await copy(file.source, file.path, file);
    const hiveSnapshots: Host.BackupManifest['hiveSnapshots'] = [];
    for (const instance of hive) {
      const source = join(servicePaths(config, instance).data, 'hive.sqlite'); if (!await exists(source)) continue;
      const local = storagePath(config, source), path = relativeFile(destination, local);
      await mkdir(dirname(path), { recursive: true, mode: 0o700 });
      const result = await runCommand({ executable: 'node', args: ['dist/services/hive/src/offline-backup.js', source, path], timeoutMs: 600_000 }, distributionRoot, config.executables,
        { jobLauncher: join(distributionRoot, 'dist/native/ivy-job.exe') });
      const snapshot = JSON.parse(result.stdout) as { format: number; pages: number };
      hiveSnapshots.push({ instanceId: instance.instanceId, path: local, ...snapshot }); await addSnapshot(local);
    }
    requireThat(hashJson(await inventoryAreas()) === hashJson(runtimeFiles) && hashJson(await currentConfiguration()) === configurationHash,
      'backup_changed', 'Runtime state or configuration changed during offline snapshot.');
    files.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
    const manifest: Host.BackupManifest = { schemaVersion: 1, backupId, hostId: config.hostId, createdAt, completedAt: new Date().toISOString(),
      consistency: 'offline-owned-state', platform: { os: process.platform as 'win32' | 'linux', arch: process.arch, node: process.version },
      configurationHash, files, externalFiles, hiveSnapshots, ephemeralNativePaths, sharedNativeHomes: sharedHomes, storageAreas: areas,
      excluded: ['OS owner lock files', 'Source checkouts, staging payloads and local releases', 'OS bootstrap definitions',
        'Runtime executables/native bundles outside runtime data (hashed prerequisites)', 'Docker daemon images (retained immutable candidate prerequisites)', 'Rotating Hive snapshot directories',
        'Shared Codex user homes and externally managed App Servers (references only; separate user-state backup required)'] };
    validateHost('BackupManifest', manifest); await syncDirectories(destination); await atomicJson(join(destination, 'backup.json'), manifest);
    const result: Host.BackupResult = { schemaVersion: 1, backupId, hostId: config.hostId, directory: destination, manifestHash: await fileHash(join(destination, 'backup.json')),
      completedAt: manifest.completedAt, files: files.length, bytes: files.reduce((sum, value) => sum + value.bytes, 0), elapsedMs: Math.round(performance.now() - started) };
    validateHost('BackupResult', result); return result;
  } catch (error) {
    if (created) await atomicJson(join(destination, 'backup-failure.json'), { schemaVersion: 1, backupId, at: new Date().toISOString(), code: IvyError.from(error).code }).catch(() => undefined);
    throw error;
  } finally {
    if (journal?.db.isTransaction) journal.db.exec('ROLLBACK'); journal?.close(); closeOwners();
  }
}
