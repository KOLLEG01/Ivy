import type { DatabaseSync, StatementSync } from 'node:sqlite';
import { requireThat } from '../../contracts/src/errors.js';
import { digest } from '../../contracts/src/canonical.js';

/** Uncertainty in a nested owner observation is still live recovery evidence. */
export function hasUnresolvedOutcome(value: unknown): boolean {
  if (!value || typeof value !== 'object') return false;
  if (Array.isArray(value)) return value.some(hasUnresolvedOutcome);
  return Object.entries(value).some(([key, part]) =>
    (['phase', 'state', 'ownerPhase', 'outcome', 'execution'].includes(key) &&
      ['unknown', 'outcome_unknown', 'needs_attention'].includes(String(part))) ||
    (['error', 'errorCode', 'code'].includes(key) && (String(part).includes('outcome_unknown') ||
      ['native_connection_lost', 'native_epoch_lost'].includes(String(part)))) || hasUnresolvedOutcome(part));
}

/** Receipts share the owner's database and transaction. SQLite owns atomicity and integrity. */
export class ReceiptArchive {
  private readonly lookup: StatementSync;
  private readonly insert: StatementSync;
  private readonly usage: StatementSync;
  constructor(private readonly db: DatabaseSync, readonly limits = { count: 4_096, bytes: 64 * 1024 * 1024 }) {
    db.exec(`CREATE TABLE IF NOT EXISTS receipt_archive(
      kind TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL,
      PRIMARY KEY(kind,key)
    ) STRICT;
      CREATE TABLE IF NOT EXISTS receipt_archive_usage(id INTEGER PRIMARY KEY CHECK(id=1),records INTEGER NOT NULL,bytes INTEGER NOT NULL) STRICT;
      INSERT OR IGNORE INTO receipt_archive_usage VALUES(1,0,0);
      CREATE TRIGGER IF NOT EXISTS receipt_archive_insert AFTER INSERT ON receipt_archive BEGIN
        UPDATE receipt_archive_usage SET records=records+1,bytes=bytes+length(CAST(NEW.value AS BLOB)) WHERE id=1;
      END;
      CREATE TRIGGER IF NOT EXISTS receipt_archive_delete AFTER DELETE ON receipt_archive BEGIN
        UPDATE receipt_archive_usage SET records=records-1,bytes=bytes-length(CAST(OLD.value AS BLOB)) WHERE id=1;
      END;
      CREATE TABLE IF NOT EXISTS receipt_archive_history(sequence INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL,key TEXT NOT NULL,UNIQUE(kind,key));
      CREATE TABLE IF NOT EXISTS receipt_archive_retired(kind TEXT NOT NULL,key TEXT NOT NULL,value_hash TEXT NOT NULL,PRIMARY KEY(kind,key)) STRICT;`);
    this.lookup = db.prepare('SELECT value FROM receipt_archive WHERE kind=? AND key=?');
    this.insert = db.prepare('INSERT INTO receipt_archive VALUES (?,?,?)');
    this.usage = db.prepare('SELECT records,bytes FROM receipt_archive_usage WHERE id=1');
  }
  read(kind: string, key: string): string | null {
    const row = this.lookup.get(kind, key);
    requireThat(row || !this.db.prepare('SELECT 1 FROM receipt_archive_retired WHERE kind=? AND key=?').get(kind, key),
      'operation_expired', 'The released receipt was evicted; its identity cannot be reused.');
    return row ? String(row['value']) : null;
  }
  retain(kind: string, key: string, value: string): void {
    const retired = this.db.prepare('SELECT value_hash FROM receipt_archive_retired WHERE kind=? AND key=?').get(kind, key);
    requireThat(!retired || retired['value_hash'] === digest(value), 'receipt_archive_conflict', 'An original receipt cannot be replaced.');
    const prior = this.read(kind, key);
    requireThat(prior === null || prior === value, 'receipt_archive_conflict', 'An original receipt cannot be replaced.');
    if (prior === null) this.insert.run(kind, key, value);
  }
  /** Only the owner can release a receipt after its dependencies have finished. */
  release(kind: string, key: string): void {
    const work = () => {
      const row = this.lookup.get(kind, key);
      if (!row) return;
      let decoded: unknown;
      try { decoded = JSON.parse(String(row['value'])); } catch { /* Non-JSON receipts use the same explicit owner release. */ }
      requireThat(!hasUnresolvedOutcome(decoded), 'receipt_archive_unresolved', 'An unresolved outcome cannot enter the history ring.');
      this.db.prepare('INSERT OR IGNORE INTO receipt_archive_history(kind,key) VALUES(?,?)').run(kind, key);
      const usage = this.db.prepare(`SELECT COUNT(*) AS count,COALESCE(SUM(length(CAST(a.value AS BLOB))),0) AS bytes
        FROM receipt_archive_history h JOIN receipt_archive a ON a.kind=h.kind AND a.key=h.key`).get()!;
      let count = Number(usage['count']), bytes = Number(usage['bytes']);
      if (count <= this.limits.count && bytes <= this.limits.bytes) return;
      for (const row of this.db.prepare(`SELECT a.kind,a.key,a.value FROM receipt_archive_history h
        JOIN receipt_archive a ON a.kind=h.kind AND a.key=h.key ORDER BY h.sequence`).all()) {
        const value = String(row['value']);
        this.db.prepare('INSERT INTO receipt_archive_retired VALUES(?,?,?)').run(row['kind']!, row['key']!, digest(value));
        this.db.prepare('DELETE FROM receipt_archive WHERE kind=? AND key=?').run(row['kind']!, row['key']!);
        this.db.prepare('DELETE FROM receipt_archive_history WHERE kind=? AND key=?').run(row['kind']!, row['key']!);
        count--; bytes -= Buffer.byteLength(value);
        if (count <= this.limits.count && bytes <= this.limits.bytes) break;
      }
    };
    if (this.db.isTransaction) work();
    else { this.db.exec('BEGIN IMMEDIATE'); try { work(); this.db.exec('COMMIT'); } catch (error) { this.db.exec('ROLLBACK'); throw error; } }
  }
  status(): { records: number; bytes: number } {
    const row = this.usage.get()!;
    return { records: Number(row['records']), bytes: Number(row['bytes']) };
  }
}
