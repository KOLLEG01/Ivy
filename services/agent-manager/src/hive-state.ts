import { DatabaseSync } from 'node:sqlite';
import { mkdirSync } from 'node:fs';
import { dirname } from 'node:path';
import { canonical, hashJson, hasUnresolvedOutcome } from '../../../packages/sdk/src/node.js';
import { requireThat } from '../../../packages/sdk/src/node.js';
import { parseOperationId } from '../../../packages/sdk/src/node.js';
import type { Agent } from '../../../packages/sdk/src/node.js';

type Pin = { objectId: string; revision: number };
export type SavedState<T> = { value: T; pin: Pin };

/** Local current workflow rows and bounded native-operation receipts. */
export class LocalAgentState {
  private readonly db: DatabaseSync;
  private runtimeEpoch: string | null = null;
  private static readonly resultReservation = 25 * 1024 * 1024;
  constructor(filename: string, readonly serviceNodeId: string, private readonly limits: Pick<Agent.Settings['limits'], 'maxOperations' | 'maxJournalBytes'>) {
    mkdirSync(dirname(filename), { recursive: true });
    this.db = new DatabaseSync(filename, { timeout: 1000, enableDoubleQuotedStringLiterals: false });
    this.db.exec(`PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=1000;
      CREATE TABLE IF NOT EXISTS state (
        kind TEXT NOT NULL, key TEXT NOT NULL, revision INTEGER NOT NULL, value_json TEXT NOT NULL,
        byte_length INTEGER NOT NULL, terminal_at_ms INTEGER, updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY(kind,key)
      ) STRICT;
      CREATE TABLE IF NOT EXISTS state_metadata (key TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
      CREATE INDEX IF NOT EXISTS state_usage ON state(kind,json_extract(value_json,'$.phase'),byte_length,terminal_at_ms,key);
      CREATE INDEX IF NOT EXISTS state_completion_owner ON state(json_extract(value_json,'$.params.threadId'),
        json_extract(value_json,'$.reply.result.turn.id'),json_extract(value_json,'$.callerPrincipalId'),key)
        WHERE kind='operation' AND json_extract(value_json,'$.method')='turn/start'
          AND json_extract(value_json,'$.phase')='succeeded';
      INSERT OR IGNORE INTO state_metadata VALUES ('expired_before','0');
      CREATE TABLE IF NOT EXISTS completion_receipts(operation_key TEXT PRIMARY KEY,released INTEGER NOT NULL) STRICT;
      CREATE TABLE IF NOT EXISTS retired_operations(operation_key TEXT PRIMARY KEY,issued_at INTEGER NOT NULL) STRICT;
      CREATE TRIGGER IF NOT EXISTS retired_operation_insert BEFORE INSERT ON state
        WHEN NEW.kind='operation' AND EXISTS(SELECT 1 FROM retired_operations WHERE operation_key=NEW.key)
        BEGIN SELECT RAISE(ABORT,'operation_expired'); END;
      INSERT OR IGNORE INTO completion_receipts
        SELECT key,0 FROM state WHERE kind='operation' AND json_extract(value_json,'$.method')='turn/start'
          AND json_extract(value_json,'$.phase')='succeeded';`);
  }
  setRuntimeEpoch(value: string): void { requireThat(value.length > 0 && !value.includes(':'), 'invalid_arguments', 'Invalid runtime epoch.'); this.runtimeEpoch = value; }
  close(): void { this.db.close(); }
  completionCaller(threadId: string, turnId: string): string | null {
    const rows = this.db.prepare(`SELECT DISTINCT json_extract(value_json,'$.callerPrincipalId') AS caller FROM state
      WHERE kind='operation' AND json_extract(value_json,'$.method')='turn/start'
        AND json_extract(value_json,'$.phase')='succeeded'
        AND json_extract(value_json,'$.params.threadId')=?
        AND json_extract(value_json,'$.reply.result.turn.id')=? LIMIT 2`).all(threadId, turnId);
    return rows.length === 1 && typeof rows[0]!['caller'] === 'string' ? rows[0]!['caller'] : null;
  }
  releaseCompletion(threadId: string, turnId: string): void {
    this.db.prepare(`UPDATE completion_receipts SET released=1 WHERE operation_key IN
      (SELECT key FROM state WHERE kind='operation' AND json_extract(value_json,'$.method')='turn/start'
        AND json_extract(value_json,'$.phase')='succeeded' AND json_extract(value_json,'$.params.threadId')=?
        AND json_extract(value_json,'$.reply.result.turn.id')=?)`).run(threadId, turnId);
  }
  private id(kind: string, key: string): string { return hashJson([this.serviceNodeId, kind, key]).slice(7); }
  private makeRoom(extraBytes: number, extraCount: number, protectedKey: string): void {
    const usage = this.db.prepare(`SELECT COUNT(*) AS count,COALESCE(SUM(byte_length),0)+
      COALESCE(SUM(CASE WHEN kind='operation' AND json_extract(value_json,'$.phase') IN ('accepted','dispatched') THEN ? ELSE 0 END),0) AS bytes FROM state INDEXED BY state_usage`)
      .get(LocalAgentState.resultReservation)!;
    let count = Number(usage['count']) + extraCount, bytes = Number(usage['bytes']) + extraBytes;
    let boundary = Number(this.db.prepare("SELECT value FROM state_metadata WHERE key='expired_before'").get()!['value']);
    if (count <= this.limits.maxOperations && bytes <= this.limits.maxJournalBytes) return;
    const candidates = this.db.prepare(`SELECT key,byte_length FROM state s INDEXED BY state_usage WHERE kind='operation' AND key<>?
      AND json_extract(value_json,'$.phase') IN ('succeeded','failed')
      AND NOT EXISTS(SELECT 1 FROM completion_receipts c WHERE c.operation_key=s.key AND c.released=0)
      ORDER BY terminal_at_ms,rowid`).all(protectedKey);
    const original = this.db.prepare("SELECT value_json FROM state WHERE kind='operation' AND key=?");
    for (const row of candidates) {
      const value = JSON.parse(String(original.get(row['key']!)!['value_json'])) as Agent.Operation;
      if (hasUnresolvedOutcome(value)) continue;
      // Records without a fenced identity cannot safely lose their replay evidence.
      let issuedAt: number;
      try { issuedAt = parseOperationId(value.operationId).issuedAtUnixMs; } catch { continue; }
      this.db.prepare('INSERT OR IGNORE INTO retired_operations VALUES(?,?)').run(row['key']!, issuedAt);
      this.db.prepare("DELETE FROM state WHERE kind='operation' AND key=?").run(row['key']!);
      this.db.prepare('DELETE FROM completion_receipts WHERE operation_key=?').run(row['key']!);
      boundary = Math.max(boundary, issuedAt); count--; bytes -= Number(row['byte_length']);
      if (count <= this.limits.maxOperations && bytes <= this.limits.maxJournalBytes) break;
    }
    this.db.prepare("UPDATE state_metadata SET value=? WHERE key='expired_before'").run(String(boundary));
    this.db.prepare('DELETE FROM retired_operations WHERE issued_at<?').run(boundary);
    requireThat(count <= this.limits.maxOperations && bytes <= this.limits.maxJournalBytes,
      'native_journal_capacity', 'Agent journal capacity is occupied by live or unresolved work; no native request sent.');
  }
  async read<T>(kind: string, key: string): Promise<SavedState<T> | null> {
    const row = this.db.prepare('SELECT revision,value_json FROM state WHERE kind=? AND key=?').get(kind, key) as { revision: number; value_json: string } | undefined;
    return row ? { value: JSON.parse(row.value_json) as T, pin: { objectId: this.id(kind, key), revision: row.revision } } : null;
  }
  async write<T>(kind: string, key: string, value: T, previous?: Pin): Promise<SavedState<T>> {
    const encoded = canonical(value), bytes = Buffer.byteLength(encoded), now = Date.now();
    requireThat(bytes <= 32 * 1024 * 1024, 'limit_exceeded', 'Agent state exceeds its byte limit.');
    this.db.exec('BEGIN IMMEDIATE');
    try {
      const row = this.db.prepare('SELECT revision,byte_length,value_json FROM state WHERE kind=? AND key=?').get(kind, key) as { revision: number; byte_length: number; value_json: string } | undefined;
      requireThat(previous ? previous.objectId === this.id(kind, key) && row?.revision === previous.revision : !row,
        'revision_conflict', 'Agent workflow row changed.');
      if (!row && kind === 'operation') {
        const operation = value as { operationId?: string };
        requireThat(typeof operation.operationId === 'string' && this.runtimeEpoch, 'operation_expired', 'AgentManager has no current Hive runtime epoch.');
        const identity = parseOperationId(operation.operationId);
        const expiredBefore = Number((this.db.prepare("SELECT value FROM state_metadata WHERE key='expired_before'").get() as { value: string }).value);
        requireThat(identity.runtimeEpoch === this.runtimeEpoch && identity.issuedAtUnixMs <= now + 60_000 && identity.issuedAtUnixMs > expiredBefore,
          'operation_expired', 'Native operation identity belongs to another runtime or predates the receipt ring.');
      }
      const phase = kind === 'operation' && value && typeof value === 'object' ? (value as { phase?: string }).phase : undefined;
      const priorPhase = kind === 'operation' && row ? (JSON.parse(row.value_json) as Agent.Operation).phase : undefined;
      const reservation = (phase: string | undefined) => phase && ['accepted', 'dispatched'].includes(phase) ? LocalAgentState.resultReservation : 0;
      this.makeRoom(bytes - (row?.byte_length ?? 0) + reservation(phase) - reservation(priorPhase), row ? 0 : 1, key);
      const terminalAt = phase && ['succeeded', 'failed'].includes(phase) ? now : null;
      const revision = (row?.revision ?? 0) + 1;
      this.db.prepare(`INSERT INTO state(kind,key,revision,value_json,byte_length,terminal_at_ms,updated_at_ms) VALUES(?,?,?,?,?,?,?)
        ON CONFLICT(kind,key) DO UPDATE SET revision=excluded.revision,value_json=excluded.value_json,byte_length=excluded.byte_length,
        terminal_at_ms=COALESCE(state.terminal_at_ms,excluded.terminal_at_ms),updated_at_ms=excluded.updated_at_ms`).run(kind, key, revision, encoded, bytes, terminalAt, now);
      if (kind === 'operation' && phase === 'succeeded' && (value as Agent.Operation).method === 'turn/start')
        this.db.prepare('INSERT OR IGNORE INTO completion_receipts VALUES(?,0)').run(key);
      this.db.exec('COMMIT');
      return { value: structuredClone(value), pin: { objectId: this.id(kind, key), revision } };
    } catch (error) { if (this.db.isTransaction) this.db.exec('ROLLBACK'); throw error; }
  }
}
