import { DatabaseSync, backup } from 'node:sqlite';
import { isAbsolute } from 'node:path';
import { fileURLToPath } from 'node:url';
import { open } from 'node:fs/promises';
import { STORAGE_FORMAT } from './storage-schema.js';
import { exists } from '../../../packages/host-runtime/src/backup-files.js';
import { requireThat, IvyError } from '../../../packages/contracts/src/errors.js';

/** Only Hive's storage helper opens Hive SQLite; host ownership is established by the caller. */
export async function snapshotStoppedHive(source: string, destination: string): Promise<{ format: number; pages: number }> {
  requireThat(isAbsolute(source) && isAbsolute(destination) && await exists(source) && !await exists(destination), 'target_conflict', 'Offline Hive snapshot needs an existing source and a fresh destination.');
  const db = new DatabaseSync(source, { readOnly: true, timeout: 1000 });
  try {
    db.exec('PRAGMA query_only=ON; BEGIN');
    const format = Number(db.prepare('PRAGMA user_version').get()!['user_version']);
    requireThat(format === STORAGE_FORMAT, 'unsupported_storage_format', 'This Hive backup helper cannot read the stored format.');
    requireThat(db.prepare('PRAGMA quick_check').all().every(row => Object.values(row)[0] === 'ok'), 'storage_invalid', 'Hive snapshot refused an invalid database.');
    const pages = await backup(db, destination);
    const file = await open(destination, 'r+'); try { await file.sync(); } finally { await file.close(); }
    return { format, pages };
  } finally { if (db.isTransaction) db.exec('ROLLBACK'); db.close(); }
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const [source, destination] = process.argv.slice(2); requireThat(source && destination, 'invalid_arguments', 'Specify the offline source and destination.');
    process.stdout.write(JSON.stringify(await snapshotStoppedHive(source, destination)) + '\n');
  } catch (error) { process.stderr.write(JSON.stringify({ code: IvyError.from(error).code, message: 'Offline Hive snapshot failed.' }) + '\n'); process.exitCode = 1; }
}
