import type { DatabaseSync, StatementSync } from 'node:sqlite';
import { requireThat } from '../../contracts/src/errors.js';

/** Receipts share the owner's database and transaction. SQLite owns atomicity and integrity. */
export class ReceiptArchive {
  private readonly lookup: StatementSync;
  private readonly insert: StatementSync;
  private readonly usage: StatementSync;
  constructor(db: DatabaseSync) {
    db.exec(`CREATE TABLE IF NOT EXISTS receipt_archive(
      kind TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL,
      PRIMARY KEY(kind,key)
    ) STRICT;
      CREATE TABLE IF NOT EXISTS receipt_archive_usage(id INTEGER PRIMARY KEY CHECK(id=1),records INTEGER NOT NULL,bytes INTEGER NOT NULL) STRICT;
      INSERT OR IGNORE INTO receipt_archive_usage VALUES(1,0,0);
      CREATE TRIGGER IF NOT EXISTS receipt_archive_insert AFTER INSERT ON receipt_archive BEGIN
        UPDATE receipt_archive_usage SET records=records+1,bytes=bytes+length(CAST(NEW.value AS BLOB)) WHERE id=1;
      END;`);
    this.lookup = db.prepare('SELECT value FROM receipt_archive WHERE kind=? AND key=?');
    this.insert = db.prepare('INSERT INTO receipt_archive VALUES (?,?,?)');
    this.usage = db.prepare('SELECT records,bytes FROM receipt_archive_usage WHERE id=1');
  }
  read(kind: string, key: string): string | null {
    const row = this.lookup.get(kind, key);
    return row ? String(row['value']) : null;
  }
  retain(kind: string, key: string, value: string): void {
    const prior = this.read(kind, key);
    requireThat(prior === null || prior === value, 'receipt_archive_conflict', 'An original receipt cannot be replaced.');
    if (prior === null) this.insert.run(kind, key, value);
  }
  status(): { records: number; bytes: number } {
    const row = this.usage.get()!;
    return { records: Number(row['records']), bytes: Number(row['bytes']) };
  }
}
