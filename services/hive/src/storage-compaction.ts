import { DatabaseSync } from 'node:sqlite';
import type { DatabaseSyncOptions } from 'node:sqlite';
import { chmodSync, closeSync, fsyncSync, lstatSync, openSync, renameSync, statfsSync, unlinkSync } from 'node:fs';
import { dirname } from 'node:path';
import { requireThat } from '../../../packages/contracts/src/errors.js';
import { STORAGE_FORMAT } from './storage-schema.js';

const reserve = 64 * 1024 * 1024;
const scalar = (db: DatabaseSync, pragma: string) => Number(Object.values(db.prepare('PRAGMA ' + pragma).get()!)[0]);

function removeTemporary(filename: string): void {
  for (const path of [filename + '-wal', filename + '-shm', filename + '-journal', filename]) {
    const info = lstatSync(path, { throwIfNoEntry: false });
    if (!info) continue;
    requireThat(info.isFile() && !info.isSymbolicLink(), 'storage_invalid', 'Database compaction path must be an owned regular file.');
    unlinkSync(path);
  }
}

/** Convert before admitting work. VACUUM INTO uses the data volume, including in containers with small /tmp mounts. */
export function openHiveStorage(filename: string, options: DatabaseSyncOptions = {},
  available = () => { const fs = statfsSync(dirname(filename)); return fs.bavail * fs.bsize; }):
  { db: DatabaseSync; pending: string | null } {
  let db = new DatabaseSync(filename, options), cleanupAllowed = false;
  if (filename === ':memory:') return { db, pending: null };
  const temporary = filename + '.compacting';
  try {
    const format = scalar(db, 'user_version');
    requireThat(format === 0 || format === STORAGE_FORMAT, 'unsupported_storage_format', 'This binary cannot read the database format.');
    cleanupAllowed = true;
    removeTemporary(temporary);
    const pages = scalar(db, 'page_count');
    if (!pages || scalar(db, 'auto_vacuum') === 2) return { db, pending: null };
    const source = lstatSync(filename);
    requireThat(source.isFile() && !source.isSymbolicLink(), 'storage_invalid', 'Database compaction requires an owned regular database file.');
    if (available() < pages * scalar(db, 'page_size') * 2 + reserve)
      return { db, pending: 'Automatic database compaction is waiting for temporary disk space; deleted pages remain reusable internally.' };
    db.exec('PRAGMA locking_mode=EXCLUSIVE; PRAGMA synchronous=FULL; BEGIN EXCLUSIVE; COMMIT; PRAGMA wal_checkpoint(TRUNCATE); PRAGMA auto_vacuum=INCREMENTAL;');
    db.prepare('VACUUM INTO ?').run(temporary);
    const candidate = new DatabaseSync(temporary, options);
    try {
      candidate.exec('PRAGMA journal_mode=DELETE;');
      requireThat(scalar(candidate, 'auto_vacuum') === 2 &&
        candidate.prepare('PRAGMA quick_check').get()!['quick_check'] === 'ok' &&
        candidate.prepare('PRAGMA foreign_key_check').get() === undefined,
      'storage_invalid', 'Compacted database did not pass its integrity checks.');
    } finally { candidate.close(); }
    chmodSync(temporary, source.mode & 0o777);
    const file = openSync(temporary, 'r+');
    try { fsyncSync(file); } finally { closeSync(file); }
    db.close();
    renameSync(temporary, filename);
    if (process.platform !== 'win32') {
      const directory = openSync(dirname(filename), 'r');
      try { fsyncSync(directory); } finally { closeSync(directory); }
    }
    db = new DatabaseSync(filename, options);
    return { db, pending: null };
  } catch (error) {
    if (!db.isOpen) db = new DatabaseSync(filename, options);
    if ((error as { code?: string }).code === 'unsupported_storage_format') { db.close(); throw error; }
    return { db, pending: 'Automatic database compaction is pending; the database remains usable and conversion retries on restart. ' +
      String((error as Error).message).slice(0, 200) };
  } finally {
    if (cleanupAllowed) try { removeTemporary(temporary); } catch { /* Preserve an unknown or locked path for the next startup. */ }
  }
}

/** Bound the work of each live retention pass; no full database rewrite while serving requests. */
export function reclaimStoragePages(db: DatabaseSync): void {
  if (scalar(db, 'auto_vacuum') !== 2 || scalar(db, 'freelist_count') < 64) return;
  db.exec('PRAGMA incremental_vacuum(16384); PRAGMA wal_checkpoint(TRUNCATE);');
}
