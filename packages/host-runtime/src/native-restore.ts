import { DatabaseSync } from 'node:sqlite';
import { createHash } from 'node:crypto';
import { lstat, open, realpath } from 'node:fs/promises';
import { isAbsolute, join, relative, sep } from 'node:path';
import { atomicJson, inside, jsonFile } from './config.js';
import { copyVerified, exists, relativeFile } from './backup-files.js';
import { fileHash } from './artifact.js';
import { nativeBackupExclusions } from './native-backup-paths.js';
import { instanceOwnedCodexHome, usesExternalAppServer } from './codex-home.js';
import { servicePaths, storagePath } from './layout.js';
import { canonical, hashJson } from '../../contracts/src/canonical.js';
import { requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import type { Host } from '../../contracts/src/generated.js';

const sqlName = (name: string): string => '"' + name.replaceAll('"', '""') + '"';
const portable = (path: string): string => path.split(sep).join('/');
type Replacements = Map<string, Map<string, Map<string, string>>>;

function verifyLayout(db: DatabaseSync, layout: Host.NativeStorageLayout): void {
  requireThat(db.prepare('PRAGMA user_version').get()?.['user_version'] === 0,
    'unsupported_storage', 'Native storage user version is unsupported.');
  const schema = db.prepare("SELECT type,name,tbl_name AS tableName,sql FROM sqlite_master WHERE sql IS NOT NULL AND name <> '_sqlx_migrations' ORDER BY type,name")
    .all().map(row => ({ ...row, sql: String(row['sql']).replaceAll('\r\n', '\n') }));
  requireThat(hashJson(schema) === hashJson(layout.schema), 'unsupported_storage', 'Native storage schema differs from its pinned public migrations.');
  const migrations = db.prepare('SELECT version,description,lower(hex(checksum)) AS checksum,success FROM _sqlx_migrations ORDER BY version').all();
  requireThat(migrations.every(row => row['success'] === 1) && hashJson(migrations.map(({ success: _success, ...row }) => row)) === hashJson(layout.migrations),
    'unsupported_storage', 'Native migration checksums or completion differ from the pinned build.');
}

function verifyIntegrity(db: DatabaseSync): void {
  requireThat(db.prepare('PRAGMA quick_check').all().every(row => row['quick_check'] === 'ok') && db.prepare('PRAGMA foreign_key_check').all().length === 0,
    'backup_invalid', 'Native recovery requires intact storage and foreign keys.');
}

/** Hash each SQLite value without JSON number rounding, preserving row identity and order. */
function logicalDigest(db: DatabaseSync, inverse: Replacements = new Map()): string {
  const hash = createHash('sha256');
  const tables = db.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").all();
  for (const table of tables) {
    const name = String(table['name']), replacement = inverse.get(name);
    hash.update(canonical(name) + '\n');
    const statement = db.prepare('SELECT * FROM ' + sqlName(name) + ' ORDER BY rowid'); statement.setReadBigInts(true);
    let rows = 0;
    for (const row of statement.iterate()) {
      requireThat(++rows <= 1000000, 'limit_exceeded', 'Native recovery table exceeds its bounded row workload.');
      hash.update(canonical(Object.entries(row).map(([column, value]) => {
        if (typeof value === 'string') value = replacement?.get(column)?.get(value) ?? value;
        return [column, value === null ? 'null' : value instanceof Uint8Array ? 'blob' : typeof value,
          typeof value === 'bigint' ? value.toString() : value instanceof Uint8Array ? Buffer.from(value).toString('hex') : value];
      })) + '\n');
    }
  }
  return 'sha256:' + hash.digest('hex');
}

async function mappedFile(raw: string, scope: Host.NativeStorageLayout['pathColumns'][number]['scope'], oldHome: string,
  newHome: string, nativePrefix: string, manifestFiles: Map<string, Host.BackupFile>): Promise<string | null> {
  if (scope === 'optional-owned-file' && (!isAbsolute(raw) || !inside(oldHome, raw))) return null;
  requireThat(isAbsolute(raw) && inside(oldHome, raw) && !raw.split(/[\\/]/).includes('..'),
    'backup_invalid', 'Native rollout pointer must be absolute and inside its captured native home.');
  const local = portable(relative(oldHome, raw));
  requireThat(scope === 'optional-owned-file' || local.startsWith('sessions/') || local.startsWith('archived_sessions/'),
    'backup_invalid', 'Native rollout pointer leaves its owned history namespace.');
  const target = relativeFile(newHome, local), expected = manifestFiles.get(nativePrefix + '/' + local);
  if (expected) {
    const metadata = await lstat(target);
    requireThat(metadata.isFile() && !metadata.isSymbolicLink() && inside(await realpath(newHome), await realpath(target)) &&
      metadata.size === expected.bytes && await fileHash(target) === expected.hash,
    'backup_changed', 'Native pointer target differs from its captured regular file.');
  } else requireThat(scope !== 'required-rollout' && !await exists(target), 'backup_invalid', 'A native pointer has no captured target file.');
  return target;
}

/** Called only on the fresh, activation-held working copy, never the source or backup SQLite. */
export async function relocateNativeStorage(original: Host.HostConfig, config: Host.HostConfig, manifest: Host.BackupManifest,
  distributionRoot: string): Promise<Host.NativeRecoveryReport> {
  requireThat(config.restoredFrom && config.restoredFrom.backupId === manifest.backupId, 'target_conflict', 'Native recovery requires its selected restored host.');
  nativeBackupExclusions(original); nativeBackupExclusions(config);
  const report: Host.NativeRecoveryReport = { schemaVersion: 1, ...config.restoredFrom, completedAt: '', instances: [] };
  const files = new Map(manifest.files.map(file => [file.path, file]));
  for (const instance of config.instances.filter(value => value.componentId === 'agent-manager')) {
    const prior = original.instances.find(value => value.instanceId === instance.instanceId);
    requireThat(prior?.componentId === 'agent-manager', 'target_conflict', 'Native recovery instance differs from its captured identity.');
    if (usesExternalAppServer(prior.settings)) continue;
    const oldHome = instanceOwnedCodexHome(prior.settings, servicePaths(original, prior).data);
    if (!oldHome) continue;
    const newHome = instanceOwnedCodexHome(instance.settings, servicePaths(config, instance).data);
    requireThat(newHome, 'target_conflict', 'Owned native restore must retain a separate instance-owned home.');
    const prefix = storagePath(config, newHome);
    requireThat(prefix === storagePath(original, oldHome) && instance.settings['nativeVersion'] === prior.settings['nativeVersion'] &&
      instance.settings['nativeExecutableHash'] === prior.settings['nativeExecutableHash'],
    'target_conflict', 'Native restore requires the captured relative home and exact native build.');
    const databaseFiles = manifest.files.filter(file => file.path.startsWith(prefix + '/') &&
      /^state_\d+\.sqlite(?:-wal|-shm)?$/.test(file.path.slice((prefix + '/').length)));
    requireThat(databaseFiles.every(file => file.bytes === 0 || /\/state_5\.sqlite(?:-wal|-shm)?$/.test(file.path)),
      'unsupported_storage', 'Native home contains an unsupported state database.');
    const base = files.get(prefix + '/state_5.sqlite');
    if (!base) { requireThat(databaseFiles.every(file => file.bytes === 0), 'backup_invalid', 'Native state companions have no captured database.'); continue; }
    const version = prior.settings['nativeVersion'];
    requireThat(typeof version === 'string' && version.length <= 64 && /^\d+\.\d+\.\d+$/.test(version), 'unsupported_storage', 'Native recovery needs an exact supported storage layout.');
    const platformLayout = join(distributionRoot, 'dist/specs/native', 'codex-' + version, 'storage.' + process.platform + '-' + process.arch + '.json');
    const layoutPath = await exists(platformLayout) ? platformLayout : join(distributionRoot, 'dist/specs/native', 'codex-' + version, 'storage.json');
    requireThat(await exists(layoutPath), 'unsupported_storage', 'The selected native build has no retained storage layout.');
    const layout = await jsonFile<Host.NativeStorageLayout>(layoutPath);
    validateHost('NativeStorageLayout', layout);
    requireThat(layout.nativeVersion === version, 'unsupported_storage', 'Native storage layout does not match its configured version.');
    const originalFiles: Host.BackupFile[] = [];
    for (const suffix of ['', '-wal', '-shm']) {
      const captured = files.get(prefix + '/state_5.sqlite' + suffix); if (!captured) continue;
      const path = 'recovery-original/native/' + instance.instanceId + '/state_5.sqlite' + suffix;
      await copyVerified(join(newHome, 'state_5.sqlite' + suffix), relativeFile(config.runtimeRoot, path), captured, true);
      originalFiles.push({ ...captured, path });
    }
    const database = join(newHome, 'state_5.sqlite'), changes: Host.NativeRecoveryReport['instances'][number]['changes'] = [];
    const db = new DatabaseSync(database); let logicalDataHash: string;
    try {
      db.exec('PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=0; PRAGMA synchronous=FULL; BEGIN IMMEDIATE');
      verifyLayout(db, layout); verifyIntegrity(db);
      logicalDataHash = logicalDigest(db);
      const inverse: Replacements = new Map();
      for (const definition of layout.pathColumns) {
        const { table, column, scope } = definition, tableName = sqlName(table), columnName = sqlName(column);
        const statement = db.prepare(`SELECT DISTINCT ${columnName} AS path FROM ${tableName} WHERE ${columnName} IS NOT NULL LIMIT 1000001`);
        const paths = statement.all(); requireThat(paths.length <= 1000000, 'limit_exceeded', 'Native recovery has too many distinct pointers.');
        const mapping = new Map<string, string>(), updates: Array<[string, string]> = [];
        for (const row of paths) {
          requireThat(typeof row['path'] === 'string', 'backup_invalid', 'Native file pointer is not text.');
          const before = row['path'], after = await mappedFile(before, scope, oldHome, newHome, prefix, files); if (after === null) continue;
          requireThat(!mapping.has(after), 'backup_invalid', 'Distinct native pointers collide in the restored home.');
          mapping.set(after, before); updates.push([after, before]);
        }
        const originalValues = new Set(paths.map(row => row['path']));
        requireThat([...mapping.keys()].every(value => !originalValues.has(value)), 'backup_invalid', 'Native relocation collides with an existing pointer.');
        let rows = 0; const update = db.prepare(`UPDATE ${tableName} SET ${columnName}=? WHERE ${columnName}=?`);
        for (const values of updates) rows += Number(update.run(...values).changes);
        if (!inverse.has(table)) inverse.set(table, new Map()); inverse.get(table)!.set(column, mapping);
        changes.push({ table, column, rows });
      }
      requireThat(logicalDigest(db, inverse) === logicalDataHash, 'backup_changed', 'Native relocation changed data outside its declared pointers.');
      verifyIntegrity(db); db.exec('COMMIT');
      requireThat(db.prepare('PRAGMA wal_checkpoint(TRUNCATE)').get()?.['busy'] === 0, 'backup_busy', 'Native working index checkpoint was not exclusive.');
    } finally { if (db.isTransaction) db.exec('ROLLBACK'); db.close(); }
    const descriptor = await open(database, 'r+'); try { await descriptor.sync(); } finally { await descriptor.close(); }
    report.instances.push({ instanceId: instance.instanceId, nativeVersion: version, layoutHash: hashJson(layout), database: 'state_5.sqlite',
      originalFiles, workingHash: await fileHash(database), logicalDataHash, changes });
  }
  report.completedAt = new Date().toISOString(); validateHost('NativeRecoveryReport', report);
  await atomicJson(join(config.runtimeRoot, 'native-recovery.json'), report); return report;
}
