import { DatabaseSync, type StatementSync } from 'node:sqlite';
import { join } from 'node:path';
import { canonical } from '../../../../packages/sdk/src/node.js';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import type { PhoneCall } from './admission.js';
import type { PhoneStatus } from './native.js';

/** Bounded diagnostic history, separate from non-evictable effect receipts. No raw SIP bodies,
 * authentication headers, PCM, announcements, access codes or individual DTMF digits are stored. */
export class PhoneCallLogs {
  private readonly db: DatabaseSync;
  private readonly latest: StatementSync;
  private readonly insert: StatementSync;
  private readonly trimCall: StatementSync;
  private readonly sampled = new Map<string, { signature: string; at: number }>();
  constructor(root: string) {
    this.db = new DatabaseSync(join(root, 'phone-diagnostics.sqlite'));
    this.db.exec(`PRAGMA busy_timeout=100; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA max_page_count=65536;
      CREATE TABLE IF NOT EXISTS snapshots(id INTEGER PRIMARY KEY,call_id TEXT NOT NULL,at TEXT NOT NULL,value TEXT NOT NULL) STRICT;
      CREATE INDEX IF NOT EXISTS snapshots_call ON snapshots(call_id,id);`);
    this.latest = this.db.prepare('SELECT value FROM snapshots WHERE call_id=? ORDER BY id DESC LIMIT 1');
    this.insert = this.db.prepare('INSERT INTO snapshots(call_id,at,value) VALUES (?,?,?)');
    this.trimCall = this.db.prepare('DELETE FROM snapshots WHERE call_id=? AND id NOT IN (SELECT id FROM snapshots WHERE call_id=? ORDER BY id DESC LIMIT 64)');
  }
  record(call: PhoneCall, status: PhoneStatus): void {
    if (status.call?.id !== call.callId) return;
    const signature = canonical({ call: status.call, closed: status.media?.closed ?? null, failed: status.media?.failed ?? null,
      audio: status.media?.audio?.state ?? null }, 32768);
    const now = performance.now(), sampled = this.sampled.get(call.callId);
    if (sampled?.signature === signature && now - sampled.at < 5000) return;
    const value = canonical({ call: status.call, media: status.media }, 32768);
    const previous = this.latest.get(call.callId);
    if (previous?.['value'] === value) return;
    this.db.exec('BEGIN IMMEDIATE');
    try {
      this.insert.run(call.callId, new Date().toISOString(), value);
      this.trimCall.run(call.callId, call.callId);
      this.db.exec('DELETE FROM snapshots WHERE id NOT IN (SELECT id FROM snapshots ORDER BY id DESC LIMIT 4096); COMMIT;');
      this.sampled.set(call.callId, { signature, at: now });
      if (this.sampled.size > 64) this.sampled.delete(this.sampled.keys().next().value!);
    } catch (error) { this.db.exec('ROLLBACK'); throw error; }
  }
  read(callId: string, limit = 20): unknown[] {
    requireThat(Number.isInteger(limit) && limit >= 1 && limit <= 64, 'invalid_arguments', 'Call log limit must be between 1 and 64.');
    const rows = this.db.prepare('SELECT at,value FROM snapshots WHERE call_id=? ORDER BY id DESC LIMIT ?').all(callId, limit);
    const entries: unknown[] = []; let bytes = 0;
    for (const row of rows) {
      bytes += Buffer.byteLength(String(row['value'])) + 128; if (bytes > 128 * 1024) break;
      entries.push({ at: row['at'], ...JSON.parse(String(row['value'])) });
    }
    return entries;
  }
  close(): void { this.db.close(); }
}
