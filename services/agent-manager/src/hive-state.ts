import { DatabaseSync } from 'node:sqlite';
import { mkdirSync } from 'node:fs';
import { dirname } from 'node:path';
import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { requireThat } from '../../../packages/sdk/src/node.js';
import { parseOperationId } from '../../../packages/sdk/src/node.js';
import type { Agent } from '../../../packages/sdk/src/node.js';

type Pin = { objectId: string; revision: number };
export type SavedState<T> = { value: T; pin: Pin };

/** Local current workflow rows and bounded native-operation receipts. */
export class LocalAgentState {
  private readonly db: DatabaseSync;
  private runtimeEpoch: string | null = null;
  private static readonly maximumRows = 10_000;
  private static readonly maximumBytes = 256 * 1024 * 1024;
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
      INSERT OR IGNORE INTO state_metadata VALUES ('expired_before','0');`);
  }
  setRuntimeEpoch(value: string): void { requireThat(value.length > 0 && !value.includes(':'), 'invalid_arguments', 'Invalid runtime epoch.'); this.runtimeEpoch = value; }
  close(): void { this.db.close(); }
  private id(kind: string, key: string): string { return hashJson([this.serviceNodeId, kind, key]).slice(7); }
  private cleanup(now = Date.now()): void {
    this.db.prepare('DELETE FROM state WHERE kind=? AND terminal_at_ms IS NOT NULL AND terminal_at_ms<=?').run('operation', now - 24 * 60 * 60 * 1000);
    const prior = Number((this.db.prepare("SELECT value FROM state_metadata WHERE key='expired_before'").get() as { value: string }).value);
    this.db.prepare("UPDATE state_metadata SET value=? WHERE key='expired_before'").run(String(Math.max(prior, now - 24 * 60 * 60 * 1000)));
  }
  async read<T>(kind: string, key: string): Promise<SavedState<T> | null> {
    this.cleanup();
    const row = this.db.prepare('SELECT revision,value_json FROM state WHERE kind=? AND key=?').get(kind, key) as { revision: number; value_json: string } | undefined;
    return row ? { value: JSON.parse(row.value_json) as T, pin: { objectId: this.id(kind, key), revision: row.revision } } : null;
  }
  async write<T>(kind: string, key: string, value: T, previous?: Pin): Promise<SavedState<T>> {
    const encoded = canonical(value), bytes = Buffer.byteLength(encoded), now = Date.now();
    requireThat(bytes <= 32 * 1024 * 1024, 'limit_exceeded', 'Agent state exceeds its byte limit.');
    this.db.exec('BEGIN IMMEDIATE');
    try {
      this.cleanup(now);
      const row = this.db.prepare('SELECT revision FROM state WHERE kind=? AND key=?').get(kind, key) as { revision: number } | undefined;
      requireThat(previous ? previous.objectId === this.id(kind, key) && row?.revision === previous.revision : !row,
        'revision_conflict', 'Agent workflow row changed.');
      const usage = this.db.prepare('SELECT COUNT(*) AS count,COALESCE(SUM(byte_length),0) AS bytes FROM state').get() as { count: number; bytes: number };
      if (!row && kind === 'operation') {
        const operation = value as { operationId?: string };
        requireThat(typeof operation.operationId === 'string' && this.runtimeEpoch, 'operation_expired', 'AgentManager has no current Hive runtime epoch.');
        const identity = parseOperationId(operation.operationId);
        const expiredBefore = Number((this.db.prepare("SELECT value FROM state_metadata WHERE key='expired_before'").get() as { value: string }).value);
        requireThat(identity.runtimeEpoch === this.runtimeEpoch && identity.issuedAtUnixMs <= now + 60_000 && identity.issuedAtUnixMs >= expiredBefore && now < identity.issuedAtUnixMs + 24 * 60 * 60 * 1000,
          'operation_expired', 'Native operation identity belongs to another runtime or is outside its replay window.');
        requireThat(usage.count < Math.min(this.limits.maxOperations, LocalAgentState.maximumRows) &&
          usage.bytes + bytes + LocalAgentState.resultReservation <= Math.min(this.limits.maxJournalBytes, LocalAgentState.maximumBytes),
          'native_journal_capacity', 'Agent operation journal is full; no native request sent.');
      }
      if (row) {
        const old = this.db.prepare('SELECT byte_length FROM state WHERE kind=? AND key=?').get(kind, key) as { byte_length: number };
        requireThat(usage.bytes - old.byte_length + bytes <= Math.min(this.limits.maxJournalBytes, LocalAgentState.maximumBytes),
          'native_journal_capacity', 'Agent operation journal is full; no native result was discarded.');
      }
      const phase = kind === 'operation' && value && typeof value === 'object' ? (value as { phase?: string }).phase : undefined;
      const terminalAt = phase && ['succeeded', 'failed', 'outcome_unknown'].includes(phase) ? now : null;
      const revision = (row?.revision ?? 0) + 1;
      this.db.prepare(`INSERT INTO state(kind,key,revision,value_json,byte_length,terminal_at_ms,updated_at_ms) VALUES(?,?,?,?,?,?,?)
        ON CONFLICT(kind,key) DO UPDATE SET revision=excluded.revision,value_json=excluded.value_json,byte_length=excluded.byte_length,
        terminal_at_ms=COALESCE(state.terminal_at_ms,excluded.terminal_at_ms),updated_at_ms=excluded.updated_at_ms`).run(kind, key, revision, encoded, bytes, terminalAt, now);
      this.db.exec('COMMIT');
      return { value: structuredClone(value), pin: { objectId: this.id(kind, key), revision } };
    } catch (error) { if (this.db.isTransaction) this.db.exec('ROLLBACK'); throw error; }
  }
}
