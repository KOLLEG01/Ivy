import { DatabaseSync } from 'node:sqlite';
import { chmod, lstat, mkdir } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { canonical, hashJson, IvyError, validateAgent, validateNativeProgress } from '../../../packages/sdk/src/node.js';
import type { Agent } from '../../../packages/sdk/src/node.js';
import { safeDirectory } from './local-files.js';
import { need, same } from './schema.js';
import { validateMedia } from './media-schema.js';
import type { MediaDownloadEvidence, MediaIntent, MediaMaterialization, MediaReadCall, MediaReadPlan } from './media-schema.js';
import type { Pin } from './schema.js';
import { downloadOutlookMedia } from './outlook-media.js';

export interface MediaAttempt { intent: MediaIntent; phase: 'prepared' | 'downloading' | 'acquired' | 'failed'; evidence: MediaDownloadEvidence | null; errorCode: string | null }
type Row = { intent: string; materialization: string; phase: MediaAttempt['phase']; evidence: string | null; payload: Uint8Array | null; error_code: string | null };
export interface MediaNativeCall { call: MediaReadCall; seen: Agent.Operation | null; observation: Agent.Operation | null }
const maximumBytes = 32 * 1024 * 1024, quotaBytes = 512 * 1024 * 1024;
const nativeOutcomeReserve = 8 * 1024 * 1024;
const check = (value: unknown, code = 'media_spool_invalid') => need(value, code, 'Media spool must retain its exact original owner, intent, evidence and complete bytes.');

/** Protected, local acquisition journal. Neither a signed URL nor a partial body is public evidence. */
export class MediaSpool {
  private active = new Set<string>();
  private constructor(readonly root: string, private readonly db: DatabaseSync, private readonly lock: DatabaseSync) {}
  static async open(root: string, owner: unknown): Promise<MediaSpool> {
    check(isAbsolute(root), 'media_spool_path_invalid'); await mkdir(root, { recursive: true, mode: 0o700 }); await safeDirectory(root);
    for (const name of ['owner.sqlite', 'media.sqlite', 'media.sqlite-wal', 'media.sqlite-shm']) {
      try { const stat = await lstat(join(root, name)); check(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1, 'media_spool_path_invalid'); }
      catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
    }
    const lockPath = join(root, 'owner.sqlite'), lock = new DatabaseSync(lockPath);
    try { lock.exec('PRAGMA busy_timeout=0; BEGIN EXCLUSIVE; CREATE TABLE IF NOT EXISTS owner(singleton INTEGER PRIMARY KEY);'); await chmod(lockPath, 0o600); }
    catch { lock.close(); throw new IvyError('media_spool_owned', 'Another process owns this media spool.'); }
    let db: DatabaseSync | undefined;
    try {
      const path = join(root, 'media.sqlite'); db = new DatabaseSync(path); await chmod(path, 0o600);
      const format = (db.prepare('PRAGMA user_version').get() as { user_version: number }).user_version;
      check(format === 1 || (format === 0 && !db.prepare("SELECT 1 FROM sqlite_master WHERE type='table'").get()), 'media_spool_format_unsupported');
      db.exec('PRAGMA user_version=1');
      db.exec("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;" +
        "CREATE TABLE IF NOT EXISTS identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1), owner_hash TEXT NOT NULL);" +
        "CREATE TABLE IF NOT EXISTS attempts(operation_id TEXT PRIMARY KEY, intent TEXT NOT NULL, materialization TEXT NOT NULL," +
          "phase TEXT NOT NULL CHECK(phase IN ('prepared','downloading','acquired','failed')), evidence TEXT, payload BLOB, error_code TEXT," +
          "CHECK((phase='acquired' AND evidence IS NOT NULL AND payload IS NOT NULL AND error_code IS NULL) OR " +
            "(phase!='acquired' AND evidence IS NULL AND payload IS NULL)));" +
        "CREATE TABLE IF NOT EXISTS read_plans(item_hash TEXT PRIMARY KEY, plan_hash TEXT NOT NULL UNIQUE, value TEXT NOT NULL);" +
        "CREATE TABLE IF NOT EXISTS read_calls(plan_hash TEXT NOT NULL REFERENCES read_plans(plan_hash), slot INTEGER NOT NULL, value TEXT NOT NULL, seen TEXT, observation TEXT, PRIMARY KEY(plan_hash,slot));");
      db.prepare('INSERT OR IGNORE INTO identity(singleton, owner_hash) VALUES(1, ?)').run(hashJson(owner));
      check((db.prepare('SELECT owner_hash FROM identity WHERE singleton=1').get() as { owner_hash: string }).owner_hash === hashJson(owner), 'media_spool_owner_changed');
      db.exec("UPDATE attempts SET phase='failed', error_code='media_download_interrupted' WHERE phase='downloading'");
      return new MediaSpool(root, db, lock);
    } catch (error) { db?.close(); lock.close(); throw error; }
  }
  private row(operationId: string): Row | null {
    const hot = this.db.prepare('SELECT intent, materialization, phase, evidence, payload, error_code FROM attempts WHERE operation_id=?').get(operationId) as Row | undefined;
    if (hot) return hot;
    return null;
  }
  archiveStatus(): { records: number; bytes: number; activeBytes: number; maximumBytes: number } {
    return { records: 0, bytes: 0, activeBytes: this.usedBytes(), maximumBytes: quotaBytes };
  }
  /** Final Hive coverage is self-contained, so completed acquisition state is disposable. */
  archivePublished(plan: MediaReadPlan): void {
    const planHash = hashJson(plan), itemHash = hashJson(plan.item);
    check(same(this.readPlan(plan.item), plan));
    this.db.exec('BEGIN IMMEDIATE');
    try {
      for (const row of this.db.prepare("SELECT operation_id FROM attempts WHERE json_extract(intent,'$.item.objectId')=? AND json_extract(intent,'$.item.revision')=?").all(plan.item.objectId, plan.item.revision)) {
        const id = String(row['operation_id']), value = this.row(id)!;
        check(['acquired', 'failed'].includes(value.phase) && !this.active.has(id), 'media_spool_busy');
        this.db.prepare('DELETE FROM attempts WHERE operation_id=?').run(id);
      }
      this.db.prepare('DELETE FROM read_calls WHERE plan_hash=?').run(planHash);
      this.db.prepare('DELETE FROM read_plans WHERE item_hash=?').run(itemHash);
      this.db.exec('COMMIT');
    } catch (error) { this.db.exec('ROLLBACK'); throw error; }
  }
  private decode(row: Row): MediaAttempt {
    return { intent: JSON.parse(row.intent) as MediaIntent, phase: row.phase,
      evidence: row.evidence === null ? null : JSON.parse(row.evidence) as MediaDownloadEvidence, errorCode: row.error_code };
  }
  get(operationId: string): MediaAttempt | null { const row = this.row(operationId); return row ? this.decode(row) : null; }
  private usedBytes(): number {
    const row = this.db.prepare("SELECT " +
      "(SELECT COALESCE(SUM(CASE WHEN phase IN ('prepared','downloading') THEN ? ELSE COALESCE(LENGTH(payload),0) END + LENGTH(CAST(intent AS BLOB)) + LENGTH(CAST(materialization AS BLOB)) + COALESCE(LENGTH(CAST(evidence AS BLOB)),0)),0) FROM attempts) + " +
      "(SELECT COALESCE(SUM(LENGTH(CAST(value AS BLOB))),0) FROM read_plans) + " +
      "(SELECT COALESCE(SUM(LENGTH(CAST(value AS BLOB))+CASE WHEN observation IS NULL THEN 8388608 ELSE COALESCE(LENGTH(CAST(seen AS BLOB)),0)+LENGTH(CAST(observation AS BLOB)) END),0) FROM read_calls) AS bytes").get(maximumBytes) as { bytes: number };
    return row.bytes;
  }
  prepare(intent: MediaIntent, materialization: MediaMaterialization): MediaAttempt {
    validateMedia('MediaIntent', intent); validateMedia('MediaMaterialization', materialization);
    check(intent.item.objectId === intent.admission.itemObjectId && intent.materializationHash === hashJson(materialization) && intent.admission.providerMessageId === materialization.attachment.messageId, 'media_spool_intent_mismatch');
    const old = this.row(intent.operationId);
    if (old) { const attempt = this.decode(old); check(same(attempt.intent, intent) && same(JSON.parse(old.materialization), materialization), 'media_spool_intent_mismatch'); return attempt; }
    const used = this.db.prepare('SELECT COUNT(*) AS count FROM attempts').get() as { count: number };
    check(used.count < 4096 && this.usedBytes() + maximumBytes + 2 * nativeOutcomeReserve + Buffer.byteLength(canonical([intent, materialization])) <= quotaBytes, 'media_spool_full');
    this.db.prepare("INSERT INTO attempts(operation_id,intent,materialization,phase) VALUES(?,?,?,'prepared')").run(intent.operationId, canonical(intent), canonical(materialization));
    return this.get(intent.operationId)!;
  }
  async acquire(operationId: string, signal: AbortSignal, guard: () => Promise<void>, request: typeof fetch = fetch): Promise<MediaAttempt> {
    const saved = this.get(operationId); check(saved, 'media_spool_intent_missing'); if (!saved) throw Error('Unreachable');
    if (saved.phase !== 'prepared') return saved;
    await guard(); signal.throwIfAborted();
    const changed = this.db.prepare("UPDATE attempts SET phase='downloading' WHERE operation_id=? AND phase='prepared'").run(operationId);
    if (!changed.changes) return this.get(operationId)!;
    this.active.add(operationId);
    try {
      const original = JSON.parse(this.row(operationId)!.materialization) as MediaMaterialization;
      const { bytes, evidence } = await downloadOutlookMedia(original, signal, request);
      this.db.prepare("UPDATE attempts SET phase='acquired', evidence=?, payload=? WHERE operation_id=? AND phase='downloading'").run(canonical(evidence), bytes, operationId);
    } catch (error) {
      this.db.prepare("UPDATE attempts SET phase='failed', error_code=? WHERE operation_id=? AND phase='downloading'").run(error instanceof IvyError ? error.code : 'media_download_incomplete', operationId);
    } finally { this.active.delete(operationId); }
    return this.get(operationId)!;
  }
  payload(operationId: string): { attempt: MediaAttempt; bytes: Buffer } {
    const row = this.row(operationId); check(row, 'media_spool_intent_missing'); if (!row) throw Error('Unreachable');
    const attempt = this.decode(row); check(attempt.phase === 'acquired' && row.payload, 'media_spool_payload_unavailable');
    return { attempt, bytes: Buffer.from(row.payload!) };
  }
  readPlan(item: Pin): MediaReadPlan | null {
    const hot = this.db.prepare('SELECT plan_hash,value FROM read_plans WHERE item_hash=?').get(hashJson(item)) as { plan_hash: string; value: string } | undefined;
    const row = hot;
    return row ? JSON.parse(row.value) as MediaReadPlan : null;
  }
  prepareRead(plan: MediaReadPlan): MediaReadPlan {
    validateMedia('MediaReadPlan', plan); const old = this.readPlan(plan.item); if (old) return old;
    const encoded = canonical(plan), count = this.db.prepare('SELECT COUNT(*) AS count FROM read_plans').get() as { count: number };
    check(count.count < 4096 && this.usedBytes() + Buffer.byteLength(encoded) + nativeOutcomeReserve <= quotaBytes, 'media_spool_full');
    this.db.prepare('INSERT INTO read_plans(item_hash,plan_hash,value) VALUES(?,?,?)').run(hashJson(plan.item), hashJson(plan), encoded);
    return this.readPlan(plan.item)!;
  }
  nativeCall(planHash: string, slot: number): MediaNativeCall | null {
    const hot = this.db.prepare('SELECT value,seen,observation FROM read_calls WHERE plan_hash=? AND slot=?').get(planHash, slot) as { value: string; seen: string | null; observation: string | null } | undefined;
    const row = hot;
    if (!row) return null; const call = JSON.parse(row.value) as MediaReadCall;
    const seen = row.seen === null ? null : JSON.parse(row.seen) as Agent.Operation, observation = row.observation === null ? null : JSON.parse(row.observation) as Agent.Operation;
    return { call, seen, observation };
  }
  async resolvedNativeCall(_client: unknown, planHash:string,slot:number):Promise<MediaNativeCall|null>{
    return this.nativeCall(planHash,slot);
  }
  prepareNativeCall(call: MediaReadCall): MediaNativeCall {
    validateMedia('MediaReadCall', call); const old = this.nativeCall(call.planHash, call.slot);
    if (old) { check(same(old.call, call), 'media_native_intent_mismatch'); return old; }
    check(this.db.prepare('SELECT 1 FROM read_plans WHERE plan_hash=?').get(call.planHash), 'media_native_plan_missing');
    const encoded = canonical(call), count = this.db.prepare('SELECT COUNT(*) AS count FROM read_calls').get() as { count: number };
    check(count.count < 65536 && this.usedBytes() + Buffer.byteLength(encoded) + nativeOutcomeReserve * (call.slot === 38 ? 1 : 2) <= quotaBytes, 'media_spool_full');
    this.db.prepare('INSERT INTO read_calls(plan_hash,slot,value) VALUES(?,?,?)').run(call.planHash, call.slot, encoded); return this.nativeCall(call.planHash, call.slot)!;
  }
  observeNativeCall(call: MediaReadCall, operation: Agent.Operation): MediaNativeCall {
    const original = this.nativeCall(call.planHash, call.slot); check(original && same(original.call, call), 'media_native_intent_mismatch');
    validateAgent('Operation', operation); const terminal = ['succeeded', 'failed'].includes(operation.phase);
    const encoded = canonical(operation, 4 * 1024 * 1024);
    check(operation.operationId === call.operationId && operation.method === call.method && same(operation.params, call.params) &&
      operation.requestHash === hashJson({ method: call.method, params: call.params }), 'media_native_intent_mismatch');
    const previous = original?.observation ?? original?.seen;
    if (previous) { validateNativeProgress(previous, operation); if (same(previous, operation)) return original!; }
    check(!original?.observation, 'media_native_outcome_changed');
    const retainedSeen = original?.seen ? Buffer.byteLength(canonical(original.seen, 4 * 1024 * 1024)) : 0;
    const nextCharge = terminal ? retainedSeen + Buffer.byteLength(encoded) : nativeOutcomeReserve;
    check(nextCharge <= nativeOutcomeReserve && this.usedBytes() - nativeOutcomeReserve + nextCharge <= quotaBytes, 'media_spool_full');
    const column = terminal ? 'observation' : 'seen';
    this.db.prepare('UPDATE read_calls SET ' + column + '=? WHERE plan_hash=? AND slot=?').run(encoded, call.planHash, call.slot);
    return this.nativeCall(call.planHash, call.slot)!;
  }
  close(): void { check(this.active.size === 0, 'media_spool_busy'); this.db.close(); this.lock.close(); }
}
