import { createHash } from 'node:crypto';
import { createReadStream, realpathSync } from 'node:fs';
import { chmod } from 'node:fs/promises';
import { DatabaseSync } from 'node:sqlite';
import { fileURLToPath } from 'node:url';
import { IvyError, requireThat } from '../../../packages/contracts/src/errors.js';
import { snapshotStoppedHive } from './offline-backup.js';
import { STORAGE_FORMAT } from './storage-schema.js';

/** Host-only recovery. The source snapshot can contain secrets; never print Object content. */
export const settingsContracts = [
  'ivy/host-configuration',
  'agent/instructions',
  'agent/mcp-configuration',
  'agent/skills',
  'secretary/configuration',
  'secretary/assignment',
] as const;

type Row = Record<string, string | number | null | Uint8Array>;
const cell = (item: Row, key: string): string | number | null | Uint8Array => {
  const value = item[key];
  requireThat(value !== undefined, 'storage_invalid', 'Settings recovery found an incomplete database row.');
  return value;
};
export interface SettingsPlan {
  format: number;
  sourceHash: string;
  total: number;
  restore: number;
  preserve: number;
  preserveDifferent: number;
  conflicts: number;
  byContract: Record<string, { restore: number; preserve: number; conflicts: number }>;
}

const rows = (db: DatabaseSync, query: string, ...values: Array<string | number | null>): Row[] =>
  db.prepare(query).all(...values) as Row[];
const row = (db: DatabaseSync, query: string, ...values: Array<string | number | null>): Row | undefined =>
  db.prepare(query).get(...values) as Row | undefined;
const selected = (db: DatabaseSync): Row[] => rows(db,
  `SELECT * FROM objects WHERE contract_key IN (${settingsContracts.map(() => '?').join(',')}) ORDER BY depth,id`,
  ...settingsContracts);
const revisions = (db: DatabaseSync, id: string): Row[] => rows(db,
  'SELECT * FROM revisions WHERE object_id=? ORDER BY revision', id);

async function sha256(path: string): Promise<string> {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return 'sha256:' + hash.digest('hex');
}
function verified(db: DatabaseSync): void {
  requireThat(Number(row(db, 'PRAGMA user_version')?.['user_version']) === STORAGE_FORMAT,
    'unsupported_storage_format', 'Settings recovery requires the current Hive storage format.');
  requireThat(row(db, 'PRAGMA quick_check')?.['quick_check'] === 'ok',
    'storage_invalid', 'Settings recovery refused an invalid Hive database.');
}
function plan(source: DatabaseSync, target: DatabaseSync, sourceHash: string): SettingsPlan {
  verified(source); verified(target);
  const byContract = Object.fromEntries(settingsContracts.map(key => [key, { restore: 0, preserve: 0, conflicts: 0 }]));
  const result: SettingsPlan = { format: STORAGE_FORMAT, sourceHash, total: 0, restore: 0,
    preserve: 0, preserveDifferent: 0, conflicts: 0, byContract };
  for (const object of selected(source)) {
    const id = String(object['id']), key = String(object['contract_key']), count = byContract[key]!;
    result.total++;
    const existing = row(target, 'SELECT id,contract_key,parent_id,name FROM objects WHERE id=?', id);
    if (existing) {
      if (existing['contract_key'] !== key || existing['parent_id'] !== object['parent_id'] || existing['name'] !== object['name']) {
        result.conflicts++; count.conflicts++;
      } else {
        result.preserve++; count.preserve++;
        const sourceCurrent = row(source, 'SELECT content_hash FROM revisions WHERE object_id=? AND revision=?', id, Number(object['current_revision']));
        const targetCurrent = row(target, 'SELECT content_hash FROM revisions WHERE object_id=? AND revision=(SELECT current_revision FROM objects WHERE id=?)', id, id);
        if (sourceCurrent?.['content_hash'] !== targetCurrent?.['content_hash']) result.preserveDifferent++;
      }
      continue;
    }
    let conflict = Boolean(row(target, 'SELECT id FROM objects WHERE path=?', String(object['path']))) ||
      Boolean(object['parent_id'] && !row(target, 'SELECT id FROM objects WHERE id=?', String(object['parent_id']))) ||
      Boolean(object['owner_object_id'] && !row(target, 'SELECT id FROM objects WHERE id=?', String(object['owner_object_id'])));
    const family = row(source, 'SELECT * FROM contract_families WHERE key=?', key),
      currentFamily = row(target, 'SELECT * FROM contract_families WHERE key=?', key);
    if (!family || (currentFamily && ['owner','media_type','object_mode'].some(field => currentFamily[field] !== family[field]))) conflict = true;
    const history = revisions(source, id);
    if (!history.length || !history.some(revision => revision['revision'] === object['current_revision'])) conflict = true;
    for (const revision of history) {
      if (revision['references_json'] !== '{}') conflict = true;
      const contract = row(source, 'SELECT definition FROM contracts WHERE key=? AND version=?', key, String(revision['contract_version'])),
        current = row(target, 'SELECT definition FROM contracts WHERE key=? AND version=?', key, String(revision['contract_version']));
      if (!contract || (current && current['definition'] !== contract['definition'])) conflict = true;
    }
    if (conflict) { result.conflicts++; count.conflicts++; }
    else { result.restore++; count.restore++; }
  }
  return result;
}

export async function backupSettings(sourcePath: string, destination: string): Promise<SettingsPlan & { pages: number }> {
  const priorMask = process.umask(0o077);
  let snapshot: Awaited<ReturnType<typeof snapshotStoppedHive>>;
  try { snapshot = await snapshotStoppedHive(sourcePath, destination); }
  finally { process.umask(priorMask); }
  await chmod(destination, 0o600);
  const db = new DatabaseSync(destination, { readOnly: true, timeout: 1000 });
  try {
    verified(db);
    const hash = await sha256(destination);
    const byContract = Object.fromEntries(settingsContracts.map(key => [key, { restore: 0, preserve: 0, conflicts: 0 }]));
    const objects = selected(db);
    for (const object of objects) byContract[String(object['contract_key'])]!.restore++;
    return { format: STORAGE_FORMAT, sourceHash: hash, total: objects.length, restore: objects.length,
      preserve: 0, preserveDifferent: 0, conflicts: 0, byContract, pages: snapshot.pages };
  } finally { db.close(); }
}

export async function planSettingsRestore(sourcePath: string, targetPath: string, expectedHash: string): Promise<SettingsPlan> {
  requireThat(sourcePath !== targetPath && /^sha256:[0-9a-f]{64}$/.test(expectedHash),
    'invalid_arguments', 'Select distinct databases and the exact backup hash.');
  const actual = await sha256(sourcePath);
  requireThat(actual === expectedHash, 'backup_hash_mismatch', 'Settings backup hash does not match.');
  const source = new DatabaseSync(sourcePath, { readOnly: true, timeout: 1000 }),
    target = new DatabaseSync(targetPath, { readOnly: true, timeout: 1000 });
  try { return plan(source, target, actual); }
  finally { source.close(); target.close(); }
}

export async function restoreSettings(sourcePath: string, targetPath: string, expectedHash: string, safetyPath: string): Promise<SettingsPlan & { safetyHash: string }> {
  requireThat(safetyPath !== sourcePath && safetyPath !== targetPath, 'invalid_arguments', 'Safety backup path must be distinct.');
  const preview = await planSettingsRestore(sourcePath, targetPath, expectedHash);
  requireThat(preview.conflicts === 0, 'target_conflict', 'Settings restore has unresolved contract, identity or reference conflicts.');
  const priorMask = process.umask(0o077);
  try { await snapshotStoppedHive(targetPath, safetyPath); }
  finally { process.umask(priorMask); }
  await chmod(safetyPath, 0o600);
  const safetyHash = await sha256(safetyPath);
  const source = new DatabaseSync(sourcePath, { readOnly: true, timeout: 1000 }),
    target = new DatabaseSync(targetPath, { timeout: 1000, enableForeignKeyConstraints: true });
  try {
    target.exec('PRAGMA busy_timeout=1000; PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE');
    try {
      const current = plan(source, target, expectedHash);
      requireThat(current.conflicts === 0 && current.total === preview.total && current.restore === preview.restore,
        'target_conflict', 'Settings changed after the restore preview.');
      for (const object of selected(source)) {
        const id = String(object['id']), key = String(object['contract_key']);
        if (row(target, 'SELECT id FROM objects WHERE id=?', id)) continue;
        const family = row(source, 'SELECT * FROM contract_families WHERE key=?', key)!;
        if (!row(target, 'SELECT key FROM contract_families WHERE key=?', key)) target.prepare(
          'INSERT INTO contract_families(key,owner,media_type,object_mode) VALUES (?,?,?,?)').run(
          cell(family, 'key'), cell(family, 'owner'), cell(family, 'media_type'), cell(family, 'object_mode'));
        const history = revisions(source, id);
        for (const revision of history) {
          const version = String(revision['contract_version']);
          if (!row(target, 'SELECT key FROM contracts WHERE key=? AND version=?', key, version)) {
            const definition = row(source, 'SELECT definition FROM contracts WHERE key=? AND version=?', key, version)!;
            target.prepare('INSERT INTO contracts(key,version,definition) VALUES (?,?,?)').run(key, version, cell(definition, 'definition'));
          }
        }
        target.prepare(`INSERT INTO objects(id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at)
          VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)`).run(cell(object, 'id'), cell(object, 'parent_id'), cell(object, 'owner_object_id'), cell(object, 'name'), cell(object, 'path'),
          cell(object, 'position'), cell(object, 'icon'), cell(object, 'depth'), key, cell(object, 'current_revision'), cell(object, 'archived_at'), cell(object, 'effective_archive'), cell(object, 'created_at'), cell(object, 'updated_at'));
        for (const revision of history) target.prepare(`INSERT INTO revisions(object_id,revision,contract_key,contract_version,encoding,content,content_hash,byte_length,created_at,references_json)
          VALUES (?,?,?,?,?,?,?,?,?,?)`).run(cell(revision, 'object_id'), cell(revision, 'revision'), key, cell(revision, 'contract_version'), cell(revision, 'encoding'),
          cell(revision, 'content'), cell(revision, 'content_hash'), cell(revision, 'byte_length'), cell(revision, 'created_at'), cell(revision, 'references_json'));
        const fts = row(source, 'SELECT name,text FROM object_fts WHERE object_id=?', id);
        target.prepare('INSERT INTO object_fts(object_id,name,text) VALUES (?,?,?)').run(id, fts?.['name'] ?? cell(object, 'name'), fts?.['text'] ?? '');
        target.prepare(`UPDATE retention_families SET summary_json=json_set(summary_json,
          '$.objectCount',COALESCE(json_extract(summary_json,'$.objectCount'),0)+1,
          '$.revisionCount',COALESCE(json_extract(summary_json,'$.revisionCount'),0)+?,
          '$.byteLength',COALESCE(json_extract(summary_json,'$.byteLength'),0)+?) WHERE contract_key=?`).run(
          history.length, history.reduce((sum, revision) => sum + Number(revision['byte_length']), 0), key);
      }
      target.exec('COMMIT');
    } catch (error) { if (target.isTransaction) target.exec('ROLLBACK'); throw error; }
    verified(target);
    return { ...preview, safetyHash };
  } finally { source.close(); target.close(); }
}

if (process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))) {
  try {
    const [command, source, target, hash, safety] = process.argv.slice(2);
    requireThat(source && target, 'invalid_arguments', 'Use backup SOURCE DEST, plan BACKUP STOPPED_TARGET HASH, or restore BACKUP STOPPED_TARGET HASH SAFETY_DEST.');
    const result = command === 'backup' && !hash && !safety ? await backupSettings(source, target)
      : command === 'plan' && hash && !safety ? await planSettingsRestore(source, target, hash)
      : command === 'restore' && hash && safety ? await restoreSettings(source, target, hash, safety)
      : (() => { throw new IvyError('invalid_arguments', 'Unknown settings recovery command.'); })();
    process.stdout.write(JSON.stringify({ ok: true, result }) + '\n');
  } catch (error) {
    process.stderr.write(JSON.stringify({ ok: false, code: IvyError.from(error).code, message: 'Settings recovery failed without changing any Object content outside its allowlist.' }) + '\n');
    process.exitCode = 1;
  }
}
