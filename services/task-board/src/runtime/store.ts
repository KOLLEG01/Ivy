import {
  canonical,
  hasUnresolvedOutcome,
  digest,
  hashJson,
} from "../../../../packages/sdk/src/node.js";
import { AsyncLocalStorage } from "node:async_hooks";
import { DatabaseSync } from "node:sqlite";
import { randomUUID } from "node:crypto";
import { statfsSync, statSync } from "node:fs";
import { join, win32 } from "node:path";
import { setImmediate as yieldToRequests } from "node:timers/promises";
import { IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import type {
  Operation,
  TaskBoard,
  Wire,
} from "../../../../packages/sdk/src/node.js";
import { scopedOperationId } from "../../../../packages/sdk/src/client.js";
import type { RpcClient } from "../../../../packages/sdk/src/client.js";
import { RequestQueue } from '../../../../packages/sdk/src/request-queue.js';
import { contractVersion, localContractKeys, readableVersions, upgradeRecord, version } from "./schema.js";
import type { ContractKey, ContractValues } from "./schema.js";
import { TaskComments } from './comment-store.js';
import { operationReceipt } from './operation-receipt.js';
import { protectRunSources } from './revision-protection.js';
import { connectionOwner } from '../../../../packages/sdk/src/connection-owner.js';
import { taskReferences } from './reference-pages.js';

export interface Document<K extends ContractKey> {
  key: K;
  pin: TaskBoard.ObjectPin;
  value: ContractValues[K];
  metadata: Operation.ObjectMetadata;
  revision: Operation.RevisionMetadata;
}
export const pinOf = (
  value: Operation.ObjectRead | Operation.ObjectWriteResult,
): TaskBoard.ObjectPin => ({
  objectId: value.object.id,
  revision: value.revision.revision,
});
export const identityHash = (callerPrincipalId: string, operationId: string) =>
  hashJson({ callerPrincipalId, operationId }).slice("sha256:".length);
export const mutation = (scope: string, step: string) =>
  "task-board:" + hashJson({ scope, step }).slice("sha256:".length);

export const taskAttachmentPins = (task: Pick<TaskBoard.Task, "attachments" | "comments">) =>
  [...new Map([
    ...task.attachments.map(attachment => attachment.object),
    ...task.comments.flatMap(comment => comment.attachments.map(attachment => attachment.object)),
  ].map(pin => [hashJson(pin), pin])).values()];

// The journal may run on Linux while reserving a Windows host's workspace.
const workspaceKey = (path: string) => /^[a-z]:[\\/]|^\\\\/i.test(path)
  ? win32.normalize(path).toLowerCase() : path;

/** Generic persistence of TaskBoard's exact contracts. It does not authorize workflow transitions. */
export class TaskBoardStore {
  readonly client: RpcClient;
  private readonly reads = new AsyncLocalStorage<
    Map<string, Document<ContractKey>>
  >();
  private readonly local: DatabaseSync;
  private readonly dataRoot: string | null;
  private collection: Promise<void> | null = null;
  private commentCursor: string | null = null;
  private commentsCompacted = false;
  private compactingComments = false;
  private commentPassPending = false;
  private commentRetryAt = 0;
  private resultCursor: string | null = null;
  private resultsCompacted = false;
  private conversation: TaskComments | undefined;
  private get comments(): TaskComments {
    return this.conversation ??= new TaskComments(this.client, scope => this.preparedMutationId(scope));
  }
  commentRange(tree: import('../../../../packages/sdk/src/task-comments.js').CommentTree, taskId: string, start: number, end: number) {
    return this.comments.range(tree, taskId, start, end);
  }
  commentSummary(task: TaskBoard.Task, tree: import('../../../../packages/sdk/src/task-comments.js').CommentTree, taskId: string) {
    return this.comments.summary(task, tree, taskId);
  }
  async prepareTaskSnapshot(task: TaskBoard.Task, taskId: string, scope: string): Promise<TaskBoard.Task> {
    return this.comments.write(task, taskId, scope);
  }
  /** A refused publication is operational evidence, not a missing agent response. */
  failedHandoff(runId: string): Pick<TaskBoard.CompletionIssue, 'code' | 'detail' | 'operationId'> | null {
    const row = this.local.prepare(`SELECT r.value_json FROM documents d
      JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision
      WHERE d.key='task-board/operation' AND json_extract(r.value_json,'$.handoffRun.objectId')=?
        AND json_extract(r.value_json,'$.phase')='failed'
        AND json_extract(r.value_json,'$.error.code') IN ('content_too_large','storage_full','quota_exceeded','database_error')
      ORDER BY d.updated_at DESC LIMIT 1`).get(runId);
    if (!row) return null;
    const operation = JSON.parse(String(row['value_json'])) as TaskBoard.Operation;
    return { code: operation.error!.code, detail: 'Die Übergabe konnte nicht gespeichert werden: ' + operation.error!.message.slice(0, 1800),
      operationId: operation.operationId };
  }
  async protectRunSources(): Promise<void> {
    await protectRunSources(this.client, this.rootObjectId, scope => this.preparedMutationId(scope));
  }
  /** Bounded storage-only backfill, including archived tickets; normal workflow CAS remains authoritative. */
  async compactComments(limit = 2): Promise<boolean> {
    if (this.compactingComments) return false;
    this.compactingComments = true;
    try {
      const resultsCompacted = await this.compactResults(10);
      if (this.commentsCompacted) return resultsCompacted;
      if (Date.now() < this.commentRetryAt) return false;
      const page: Operation.ObjectsQueryResult = await this.client.request('objects.query', { contractKey: 'task-board/task', includeArchived: true,
        where: this.rootObjectId === null ? { field: 'object.parentId', op: 'isNull' } :
          { field: 'object.parentId', op: 'eq', value: this.rootObjectId },
        limit, ...(this.commentCursor ? { cursor: this.commentCursor } : {}) });
      for (const item of page.items) {
        if (item.contractVersion === contractVersion('task-board/task')) continue;
        const task = await this.read('task-board/task', item.objectId, true, true);
        if (task.value.claim || task.value.publication) { this.commentPassPending = true; continue; }
        const value = await this.comments
          .write(task.value, task.pin.objectId, 'compact:' + hashJson(task.pin));
        const recoveryRun = task.value.completionIssue ? await this.recoveryReference(task.value.completionIssue.run) : null;
        try {
          await this.client.request('objects.write', { objectId: task.pin.objectId, expectedRevision: task.pin.revision,
            expectedArchived: task.metadata.effectivelyArchived, contractVersion: contractVersion('task-board/task'),
            mutationId: await this.preparedMutationId('compact:' + hashJson(task.pin)),
            references: await taskReferences(this.client, task.pin.objectId,
              { ...task.revision.references, ...('kind' in value.commentStore! && value.commentStore.root
                ? { conversation: value.commentStore.root.object } : {}), ...(recoveryRun ? { 'recovery-run': recoveryRun } : {}) },
              taskAttachmentPins(task.value), scope => this.preparedMutationId(scope)),
            content: { encoding: 'json', value: value as unknown as Wire.Json } });
        } catch (error) {
          if (!(error instanceof IvyError && error.code === 'revision_conflict')) throw error;
          this.commentPassPending = true;
        }
      }
      this.commentCursor = page.nextCursor;
      this.commentsCompacted = page.nextCursor === null && !this.commentPassPending;
      if (page.nextCursor === null && this.commentPassPending) {
        this.commentPassPending = false;
        this.commentRetryAt = Date.now() + 60_000;
      }
      return this.commentsCompacted && resultsCompacted;
    } finally { this.compactingComments = false; }
  }
  private async compactResults(limit: number): Promise<boolean> {
    if (this.resultsCompacted) return true;
    const page = await this.client.request('objects.query', { contractKey: 'task-board/result', includeArchived: true,
      where: this.rootObjectId === null ? { field: 'object.parentId', op: 'isNull' } :
        { field: 'object.parentId', op: 'eq', value: this.rootObjectId }, limit,
      ...(this.resultCursor ? { cursor: this.resultCursor } : {}) });
    for (const item of page.items) {
      if (item.contractVersion === contractVersion('task-board/result')) continue;
      const result = await this.read('task-board/result', item.objectId, true, true);
      const references = { ...result.revision.references };
      for (const [index, artifact] of result.value.content.artifacts.entries())
        if (!this.localKey(artifact.object.objectId)) references['artifact-' + index] = artifact.object;
      await this.client.request('objects.write', { objectId: item.objectId, expectedRevision: result.pin.revision,
        expectedArchived: result.metadata.effectivelyArchived, contractVersion: contractVersion('task-board/result'),
        mutationId: await this.preparedMutationId('preserve-result-findings:' + hashJson(result.pin)),
        references, content: { encoding: 'json', value: result.value as unknown as Wire.Json } });
      await yieldToRequests();
    }
    this.resultCursor = page.nextCursor;
    this.resultsCompacted = page.nextCursor === null;
    return this.resultsCompacted;
  }
  async upgradeStored<K extends ContractKey>(key: K, value: ContractValues[K]): Promise<ContractValues[K]> {
    if (key !== 'task-board/task') return upgradeRecord(key, value);
    const stored = structuredClone(value) as unknown as { latestChecks?: { evidence?: TaskBoard.Artifact[] }[];
      deliveries?: { delivery: { artifacts: TaskBoard.Artifact[] } }[];
      comments?: { handoff?: { checks?: { evidence?: TaskBoard.Artifact[] }[] } }[];
      content?: { checks?: { evidence?: TaskBoard.Artifact[] }[] } };
    const findings = [...stored.latestChecks ?? [], ...stored.content?.checks ?? [],
      ...(stored.comments ?? []).flatMap(comment => comment.handoff?.checks ?? [])];
    for (const artifact of [...findings.flatMap(finding => finding.evidence ?? []),
      ...(stored.deliveries ?? []).flatMap(receipt => receipt.delivery.artifacts)]) {
      if (artifact.byteLength !== undefined) continue;
      const read = await this.client.request('objects.read', artifact.object);
      requireThat(read.revision.contentHash === artifact.contentHash && read.revision.mediaType === artifact.mediaType,
        'task_board_evidence_mismatch', 'Retained verification evidence differs from its immutable pin.');
      artifact.byteLength = read.revision.byteLength;
    }
    return upgradeRecord(key, stored as unknown as ContractValues[K]);
  }
  withReadCache<T>(work: () => Promise<T>): Promise<T> {
    return this.reads.getStore() ? work() : this.reads.run(new Map(), work);
  }
  connectionOwner(serviceNodeId: string) {
    // Preserve the SDK's per-connection identity cache and disconnect fence.
    return connectionOwner(this.connection, serviceNodeId);
  }
  constructor(
    private readonly connection: RpcClient,
    readonly rootObjectId: string | null,
    dataRoot?: string,
    readonly historyLimits = { count: 10_000, bytes: 256 * 1024 * 1024 },
  ) {
    // Share admission across all concurrent ticket reads/writes. Per-page batches
    // alone can multiply beyond the service connection's 64 in-flight requests.
    const requests = new RequestQueue(48, 16, 512), idleSignal = new AbortController().signal;
    this.client = { request: async (method, params, options) => {
      const release = await requests.acquire(method === 'tools.call' || method === 'discovery.call', options?.signal ?? idleSignal);
      try { return await connection.request(method, params, options); }
      finally { release(); }
    } };
    this.dataRoot = dataRoot ?? null;
    this.local = new DatabaseSync(
      dataRoot ? join(dataRoot, "task-board.sqlite") : ":memory:",
    );
    this.local
      .exec(`PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000; PRAGMA auto_vacuum=INCREMENTAL;
      CREATE TABLE IF NOT EXISTS documents(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,parent_id TEXT,current_revision INTEGER NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,UNIQUE(key,name,parent_id));
      CREATE TABLE IF NOT EXISTS revisions(id TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,created_at TEXT NOT NULL,PRIMARY KEY(id,revision));
      CREATE TABLE IF NOT EXISTS blobs(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,parent_id TEXT,bytes BLOB NOT NULL,UNIQUE(key,name,parent_id));
      CREATE TABLE IF NOT EXISTS markers(key TEXT PRIMARY KEY,value TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS cleanup_roots(root_id TEXT PRIMARY KEY,delete_after TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS cleanup_dependencies(root_id TEXT NOT NULL,operation_id TEXT NOT NULL,PRIMARY KEY(root_id,operation_id));
      CREATE TABLE IF NOT EXISTS active_gates(gate_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS operation_gates(operation_id TEXT NOT NULL,gate_name TEXT NOT NULL,PRIMARY KEY(operation_id,gate_name));
      CREATE TABLE IF NOT EXISTS workspace_reservations(host_id TEXT NOT NULL,canonical_cwd TEXT NOT NULL,task_id TEXT NOT NULL,operation_id TEXT NOT NULL,PRIMARY KEY(host_id,canonical_cwd),UNIQUE(task_id));
      CREATE TABLE IF NOT EXISTS storage_usage(kind TEXT PRIMARY KEY,rows INTEGER NOT NULL,bytes INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS retired_operations(caller_id TEXT NOT NULL,operation_id TEXT NOT NULL,request_hash TEXT NOT NULL,name TEXT NOT NULL,PRIMARY KEY(caller_id,operation_id));
      CREATE TABLE IF NOT EXISTS retired_operation_receipts(caller_id TEXT NOT NULL,operation_id TEXT NOT NULL,receipt_json TEXT NOT NULL,PRIMARY KEY(caller_id,operation_id));
      CREATE TRIGGER IF NOT EXISTS retired_operation_insert BEFORE INSERT ON documents
        WHEN NEW.key='task-board/operation' AND EXISTS(SELECT 1 FROM retired_operations WHERE name=NEW.name)
        BEGIN SELECT RAISE(ABORT,'operation_expired'); END;
      CREATE UNIQUE INDEX IF NOT EXISTS documents_identity ON documents(key,name,COALESCE(parent_id,''));
      CREATE UNIQUE INDEX IF NOT EXISTS blobs_identity ON blobs(key,name,COALESCE(parent_id,''));
      CREATE INDEX IF NOT EXISTS documents_parent ON documents(parent_id);
      CREATE INDEX IF NOT EXISTS blobs_parent ON blobs(parent_id);
      CREATE INDEX IF NOT EXISTS cleanup_due ON cleanup_roots(delete_after);
      INSERT OR REPLACE INTO active_gates SELECT d.id,json_extract(r.value_json,'$.activeOperation.objectId')
        FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision
        WHERE d.key='task-board/coordination' AND json_type(r.value_json,'$.activeOperation')='object';
      INSERT OR IGNORE INTO storage_usage SELECT 'revisions',COUNT(*),COALESCE(SUM(length(CAST(value_json AS BLOB))),0) FROM revisions;
      INSERT OR IGNORE INTO storage_usage SELECT 'blobs',COUNT(*),COALESCE(SUM(length(bytes)),0) FROM blobs;
      CREATE TRIGGER IF NOT EXISTS revision_usage_insert AFTER INSERT ON revisions BEGIN
        UPDATE storage_usage SET rows=rows+1,bytes=bytes+length(CAST(NEW.value_json AS BLOB)) WHERE kind='revisions';
      END;
      CREATE TRIGGER IF NOT EXISTS revision_usage_delete AFTER DELETE ON revisions BEGIN
        UPDATE storage_usage SET rows=rows-1,bytes=bytes-length(CAST(OLD.value_json AS BLOB)) WHERE kind='revisions';
      END;
      CREATE TRIGGER IF NOT EXISTS blob_usage_insert AFTER INSERT ON blobs BEGIN
        UPDATE storage_usage SET rows=rows+1,bytes=bytes+length(NEW.bytes) WHERE kind='blobs';
      END;
      CREATE TRIGGER IF NOT EXISTS blob_usage_delete AFTER DELETE ON blobs BEGIN
        UPDATE storage_usage SET rows=rows-1,bytes=bytes-length(OLD.bytes) WHERE kind='blobs';
      END;`);
    // Advisory signals replace their earlier checkpoint; native receipts and evidence are separate records.
    this.local.exec(`DELETE FROM revisions WHERE id IN (SELECT id FROM documents WHERE key='task-board/native-signals')
      AND revision < (SELECT current_revision FROM documents WHERE documents.id=revisions.id)`);
    // An existing journal can predate incremental auto-vacuum. Convert it only
    // when SQLite has room for a complete temporary copy.
    if (dataRoot && Number(this.local.prepare("PRAGMA auto_vacuum").get()?.["auto_vacuum"]) === 0) {
      const fileBytes = statSync(join(dataRoot, "task-board.sqlite")).size;
      const filesystem = statfsSync(dataRoot);
      const freeBytes = Number(filesystem.bavail) * Number(filesystem.bsize);
      if (freeBytes >= Math.max(64 * 1024 * 1024, fileBytes * 3))
        this.local.exec("PRAGMA auto_vacuum=INCREMENTAL; VACUUM;");
    }
  }
  localUsage(): { revisions: { rows: number; bytes: number }; blobs: { rows: number; bytes: number }; freeBytes: number | null } {
    const rows = this.local.prepare("SELECT kind,rows,bytes FROM storage_usage").all();
    const usage = Object.fromEntries(rows.map(row => [String(row["kind"]),
      { rows: Number(row["rows"]), bytes: Number(row["bytes"]) }]));
    const freeBytes = this.dataRoot ? (() => { const stat = statfsSync(this.dataRoot); return Number(stat.bavail) * Number(stat.bsize); })() : null;
    return { revisions: usage["revisions"]!, blobs: usage["blobs"]!, freeBytes };
  }
  pendingCoordination(): TaskBoard.ObjectPin | null {
    const row = this.local.prepare("SELECT operation_id FROM active_gates ORDER BY gate_id LIMIT 1").get();
    return row ? { objectId: String(row["operation_id"]), revision: 1 } : null;
  }
  pendingCoordinationPage(afterGateId: string | null, limit: number): { gateId: string; operationId: string }[] {
    const rows = this.local.prepare("SELECT gate_id,operation_id FROM active_gates WHERE gate_id>? ORDER BY gate_id LIMIT ?")
      .all(afterGateId ?? "", limit);
    return rows.map(row => ({ gateId: String(row["gate_id"]), operationId: String(row["operation_id"]) }));
  }
  operationGates(operationId: string): string[] {
    return this.local.prepare("SELECT gate_name FROM operation_gates WHERE operation_id=? ORDER BY gate_name")
      .all(operationId).map(row => String(row["gate_name"]));
  }
  retainOperationGates(operationId: string, names: string[]): void {
    this.local.exec("BEGIN IMMEDIATE");
    try {
      for (const name of names)
        this.local.prepare("INSERT OR IGNORE INTO operation_gates VALUES(?,?)").run(operationId, name);
      this.local.exec("COMMIT");
    } catch (error) {
      this.local.exec("ROLLBACK");
      throw error;
    }
  }
  private async retainedLocalRoot(rootId: string): Promise<boolean> {
    if (this.local.prepare("SELECT 1 FROM active_gates WHERE operation_id=? LIMIT 1").get(rootId)) return true;
    const pending = this.local.prepare(`SELECT 1 FROM cleanup_dependencies c
      JOIN documents d ON d.id=c.operation_id
      JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision
      WHERE c.root_id=? AND json_extract(r.value_json,'$.phase') NOT IN ('succeeded','failed') LIMIT 1`).get(rootId);
    if (pending) return true;
    const row = this.local.prepare("SELECT key FROM documents WHERE id=?").get(rootId);
    if (!row) return false;
    const key = String(row["key"]);
    const taskActive = async (taskId: string, revision: number): Promise<boolean> => {
      try {
        const read = await this.client.request("objects.read", { objectId: taskId });
        const value = read.content.encoding === "json" ? read.content.value as TaskBoard.Task : null;
        return read.object.contractKey === "task-board/task" && !read.object.effectivelyArchived &&
          read.object.currentRevision === revision && !!value &&
          ["todo", "waiting"].includes(value.workflowState);
      } catch (error) {
        if (error instanceof IvyError && error.code === "not_found") return false;
        throw error;
      }
    };
    if (key === "task-board/scheduler-attempt") {
      const attempt = this.localDocument("task-board/scheduler-attempt", rootId, false).value;
      return taskActive(attempt.task.objectId, attempt.task.revision);
    }
    if (key !== "task-board/operation") return false;
    const operation = this.localDocument("task-board/operation", rootId, false).value;
    if (!["succeeded", "failed"].includes(operation.phase)) return true;
    if (operation.error?.execution === "unknown") return true;
    if (operation.phase === 'succeeded' && operation.handoffRun && 'taskId' in operation.request) {
      const task = await this.read('task-board/task', operation.request.taskId, true, true).catch(error => {
        if (error instanceof IvyError && error.code === 'not_found') return null;
        throw error;
      });
      if (task?.value.claim?.run?.objectId === operation.handoffRun.objectId ||
          task?.value.completionIssue?.run.objectId === operation.handoffRun.objectId) return true;
    }
    if (operation.actor.source === "scheduler" && "taskId" in operation.request &&
        "expectedRevision" in operation.request &&
        await taskActive(operation.request.taskId, operation.request.expectedRevision)) return true;
    for (const write of operation.writes) {
      if (write.contractKey !== "task-board/delivery" || !write.outcome) continue;
      try {
        const delivery = await this.client.request("objects.read", { objectId: write.outcome.objectId });
        if (delivery.object.contractKey !== "task-board/delivery" || delivery.content.encoding !== "json" ||
            (delivery.content.value as TaskBoard.Delivery).state !== "confirmed") return true;
      } catch (error) {
        if (!(error instanceof IvyError && error.code === "not_found")) throw error;
      }
    }
    return false;
  }
  retiredOperation(caller: string, operationId: string): string | null {
    const row = this.local.prepare("SELECT request_hash FROM retired_operations WHERE caller_id=? AND operation_id=?").get(caller, operationId);
    return row ? String(row["request_hash"]) : null;
  }
  retiredOperationReceipt(caller: string, operationId: string): TaskBoard.OperationReceipt | null {
    const row = this.local.prepare('SELECT receipt_json FROM retired_operation_receipts WHERE caller_id=? AND operation_id=?').get(caller, operationId);
    return row ? JSON.parse(String(row['receipt_json'])) as TaskBoard.OperationReceipt : null;
  }
  async collectLocal(_now = Date.now(), reservedBytes = 0, reservedRows = 0, protectedId?: string, incoming?: unknown): Promise<void> {
    const full = (headroom = false) => {
      const usage = this.local.prepare("SELECT SUM(rows) AS rows,SUM(bytes) AS bytes FROM storage_usage").get()!;
      const factor = headroom ? 0.9 : 1;
      return Number(usage["rows"]) + reservedRows > this.historyLimits.count * factor || Number(usage["bytes"]) + reservedBytes > this.historyLimits.bytes * factor;
    };
    if (!full()) return;
    const previous = this.collection;
    const work = (async () => {
      await previous;
      if (full()) await this.collectLocalHistory(full, protectedId, incoming);
    })();
    this.collection = work.catch(() => undefined);
    const current = this.collection;
    try { await work; } finally { if (this.collection === current) this.collection = null; }
  }
  private async collectLocalHistory(full: (headroom?: boolean) => boolean, protectedId?: string, incoming?: unknown): Promise<void> {
    const incomingIds = new Set(protectedId ? [protectedId] : []);
    const references = (value: unknown, found: Set<string>): void => {
      if (typeof value === "string") found.add(value);
      else if (Array.isArray(value)) for (const part of value) references(part, found);
      else if (value && typeof value === "object") for (const part of Object.values(value)) references(part, found);
    };
    references(incoming, incomingIds);
    // A terminal native turn is not releasable while its business handoff is unresolved.
    // Query only those primary tickets, then follow their exact original evidence pins.
    let recoveryCursor: string | null = null;
    do {
      const page: Operation.ObjectsQueryResult = await this.client.request('objects.query', { contractKey: 'task-board/task', includeArchived: true,
        where: { op: 'and', args: [{ field: 'data:/completionIssue/code', op: 'in', value: ['task_board_handoff_missing',
          'task_board_recovery_context_unavailable', 'task_board_recovery_evidence_missing', 'task_board_no_progress'] },
          { field: 'object.ownerObjectId', op: 'isNull' }, this.rootObjectId === null ? { field: 'object.parentId', op: 'isNull' } :
            { field: 'object.parentId', op: 'eq', value: this.rootObjectId }] }, limit: 100, ...(recoveryCursor ? { cursor: recoveryCursor } : {}) });
      for (const item of page.items) {
        const task = await this.read('task-board/task', item.objectId);
        const pin = task.value.completionIssue?.run; if (!pin) continue;
        let run: Document<'task-board/run'>;
        try { run = await this.retainedRecoveryRun(pin); }
        catch (error) { if (error instanceof IvyError && error.code === 'not_found') continue; throw error; }
        const pending = new Set<string>(); references(run.value, pending);
        for (const id of pending) {
          if (incomingIds.has(id)) continue;
          incomingIds.add(id);
          const row = this.local.prepare('SELECT value_json FROM revisions WHERE id=? ORDER BY revision DESC LIMIT 1').get(id);
          if (row) references(JSON.parse(String(row['value_json'])), pending);
          requireThat(pending.size <= 10000, 'task_board_evidence_mismatch', 'Recovery evidence exceeds its bounded lineage.');
        }
      }
      recoveryCursor = page.nextCursor;
    } while (recoveryCursor);
    // Decode each current document once, rather than the entire journal for every
    // cleanup root. Refresh changed revisions after awaits before deciding deletion.
    const documents = new Map<string, { revision: number; references: Set<string>; unresolved: boolean }>();
    const referrers = new Map<string, Set<string>>();
    let indexedVersion = "";
    const databaseVersion = () => String(this.local.prepare("PRAGMA data_version").get()!["data_version"]) + ":" +
      String(this.local.prepare("SELECT total_changes() AS changes").get()!["changes"]);
    const forget = (id: string) => {
      for (const target of documents.get(id)?.references ?? []) {
        const sources = referrers.get(target)!; sources.delete(id);
        if (!sources.size) referrers.delete(target);
      }
      documents.delete(id);
    };
    const refresh = function* (this: TaskBoardStore): Generator<void> {
      if (indexedVersion !== databaseVersion()) {
        const version = databaseVersion();
        const heads = this.local.prepare("SELECT id,key,current_revision FROM documents").all();
        const live = new Set<string>();
        for (const [index, head] of heads.entries()) {
          const id = String(head["id"]), revision = Number(head["current_revision"]);
          live.add(id);
          if (documents.get(id)?.revision !== revision) {
            const row = this.local.prepare("SELECT value_json FROM revisions WHERE id=? AND revision=?").get(id, revision);
            if (row) {
              const value = JSON.parse(String(row["value_json"]));
              const found = new Set<string>(); references(value, found); forget(id);
              // Prepared domain payloads are immutable snapshots, not owners of
              // the native uncertainty they happened to show when copied.
              documents.set(id, { revision, references: found, unresolved: localContractKeys.has(String(head["key"]) as ContractKey) && hasUnresolvedOutcome(value) ||
                head["key"] === "task-board/native-call" && (value.state !== "observed" || !["succeeded", "failed"].includes(value.ownerPhase)) });
              for (const target of found) {
                let sources = referrers.get(target);
                if (!sources) referrers.set(target, sources = new Set());
                sources.add(id);
              }
            }
          }
          if (index % 32 === 31) yield;
        }
        for (const id of documents.keys()) if (!live.has(id)) forget(id);
        indexedVersion = version;
      }
    }.bind(this);
    // Build the initial index cooperatively. A busy journal need not become idle
    // to finish this pass; refresh changed heads inside the deletion transaction.
    for (const _ of refresh()) await yieldToRequests();
    const due = this.local
      .prepare("SELECT root_id FROM cleanup_roots ORDER BY delete_after,rowid")
      .all();
    for (const [index, row] of due.entries()) {
      if (!full(true)) break;
      if (index % 32 === 31) await yieldToRequests();
      const rootId = String(row["root_id"]);
      const before = this.local.prepare("SELECT current_revision,key,name FROM documents WHERE id=?").get(rootId);
      const subtree = () => this.local.prepare(`WITH RECURSIVE subtree(id) AS (
        SELECT ? UNION ALL SELECT documents.id FROM documents JOIN subtree ON documents.parent_id=subtree.id
      ) SELECT id FROM subtree`).all(rootId).map(value => String(value["id"]));
      let ids = subtree(), selected = new Set(ids);
      const protectedReference = () => ids.some(id => documents.get(id)?.unresolved ||
        [...referrers.get(id) ?? []].some(source => !selected.has(source)));
      // Reject protected history before doing owner reads for its receipts.
      for (const _ of refresh()) { /* No await between this snapshot and its use. */ }
      if (protectedReference() || await this.retainedLocalRoot(rootId)) continue;
      this.local.exec("BEGIN IMMEDIATE");
      try {
        for (const _ of refresh()) { /* Include writes made during the owner read. */ }
        ids = subtree(); selected = new Set(ids);
        const after = this.local.prepare("SELECT current_revision FROM documents WHERE id=?").get(rootId);
        if (before?.["current_revision"] !== after?.["current_revision"] || ids.some(id => incomingIds.has(id)) ||
            this.local.prepare("SELECT 1 FROM active_gates WHERE operation_id=? LIMIT 1").get(rootId)) {
          this.local.exec("ROLLBACK"); continue;
        }
        if (protectedReference()) { this.local.exec("ROLLBACK"); continue; }
        if (before?.["key"] === "task-board/operation") {
          const operation = this.localDocument("task-board/operation", rootId, false).value;
          this.local.prepare("INSERT OR IGNORE INTO retired_operations VALUES(?,?,?,?)")
            .run(operation.callerPrincipalId, operation.operationId, operation.requestHash, before["name"]!);
          this.local.prepare('INSERT OR IGNORE INTO retired_operation_receipts VALUES(?,?,?)')
            .run(operation.callerPrincipalId, operation.operationId, canonical(operationReceipt(operation)));
        }
        for (const id of ids) {
          this.local.prepare("DELETE FROM active_gates WHERE gate_id=?").run(id);
          this.local.prepare("DELETE FROM operation_gates WHERE operation_id=?").run(id);
          this.local.prepare("DELETE FROM cleanup_dependencies WHERE root_id=? OR operation_id=?").run(id, id);
          this.local
            .prepare("DELETE FROM blobs WHERE id=? OR parent_id=?")
            .run(id, id);
          this.local.prepare("DELETE FROM revisions WHERE id=?").run(id);
          this.local.prepare("DELETE FROM documents WHERE id=?").run(id);
        }
        this.local
          .prepare("DELETE FROM cleanup_roots WHERE root_id=?")
          .run(rootId);
        const collectedVersion = databaseVersion();
        this.local.exec("COMMIT");
        for (const id of ids) forget(id);
        indexedVersion = collectedVersion;
      } catch (error) {
        this.local.exec("ROLLBACK");
        throw error;
      }
    }
    if (due.length) this.local.exec("PRAGMA incremental_vacuum(256); PRAGMA wal_checkpoint(PASSIVE)");
  }
  completeLocalWorkflow(
    rootId: string,
    completedAt = new Date().toISOString(),
    operationId?: string,
  ): void {
    const deleteAfter = new Date(completedAt).toISOString();
    if (operationId) this.local.prepare("INSERT OR IGNORE INTO cleanup_dependencies VALUES(?,?)").run(rootId, operationId);
    this.local
      .prepare(
        "INSERT INTO cleanup_roots VALUES(?,?) ON CONFLICT(root_id) DO UPDATE SET delete_after=excluded.delete_after",
      )
      .run(rootId, deleteAfter);
  }
  async allocateTaskKey(operationId: string): Promise<string> {
    const allocationName =
      "Task key allocation " + identityHash("task-board", operationId);
    const retained = await this.named(
      "task-board/task-key-allocation",
      allocationName,
    );
    if (retained) {
      requireThat(
        retained.value.operationId === operationId,
        "task_board_identity_conflict",
        "The retained Task key allocation belongs to another operation.",
      );
      return retained.value.taskKey;
    }
    for (;;) {
      let sequence = await this.named(
        "task-board/task-key-sequence",
        "Task key sequence",
      );
      if (!sequence) {
        try {
          const pin = await this.write(
            "task-board/task-key-sequence",
            {
              schemaVersion: 1,
              nextValue: 0,
              updatedAt: new Date().toISOString(),
            },
            mutation("task-key-sequence", "initialize"),
            {
              create: {
                parentId: this.rootObjectId,
                name: "Task key sequence",
              },
            },
          );
          sequence = await this.read("task-board/task-key-sequence", pin);
        } catch (error) {
          if (
            !(error instanceof IvyError) ||
            !["revision_conflict", "mutation_conflict"].includes(error.code)
          )
            throw error;
          continue;
        }
      }
      const nextValue = sequence.value.nextValue + 1;
      try {
        await this.write(
          "task-board/task-key-sequence",
          { schemaVersion: 1, nextValue, updatedAt: new Date().toISOString() },
          mutation(operationId, "allocate-task-key:" + nextValue),
          {
            objectId: sequence.pin.objectId,
            expectedRevision: sequence.pin.revision,
          },
        );
      } catch (error) {
        if (
          error instanceof IvyError &&
          ["revision_conflict", "mutation_conflict"].includes(error.code)
        )
          continue;
        throw error;
      }
      const value: TaskBoard.TaskKeyAllocation = {
        schemaVersion: 1,
        operationId,
        taskKey: "TASK-" + String(nextValue).padStart(4, "0"),
        createdAt: new Date().toISOString(),
      };
      try {
        await this.write(
          "task-board/task-key-allocation",
          value,
          mutation(operationId, "retain-task-key"),
          { create: { parentId: this.rootObjectId, name: allocationName } },
        );
        return value.taskKey;
      } catch (error) {
        if (
          !(error instanceof IvyError) ||
          !["revision_conflict", "mutation_conflict"].includes(error.code)
        )
          throw error;
        const winner = await this.named(
          "task-board/task-key-allocation",
          allocationName,
        );
        requireThat(
          winner && winner.value.operationId === operationId,
          "task_board_identity_conflict",
          "The competing Task key allocation is unavailable or belongs to another operation.",
        );
        return winner.value.taskKey;
      }
    }
  }
  private workspaceReservation(hostId: string, canonicalCwd: string) {
    const key = workspaceKey(canonicalCwd);
    return this.local.prepare("SELECT canonical_cwd,task_id,operation_id FROM workspace_reservations WHERE host_id=?")
      .all(hostId).find(row => workspaceKey(String(row["canonical_cwd"])) === key);
  }
  reserveWorkspace(
    hostId: string,
    canonicalCwd: string,
    taskId: string,
    operationId: string,
  ): string | null {
    const key = workspaceKey(canonicalCwd);
    this.local.exec("BEGIN IMMEDIATE");
    try {
      const existing = this.workspaceReservation(hostId, key);
      if (existing && String(existing["task_id"]) !== taskId) {
        this.local.exec("ROLLBACK");
        return String(existing["task_id"]);
      }
      this.local
        .prepare(
          "DELETE FROM workspace_reservations WHERE task_id=? AND (host_id<>? OR canonical_cwd<>?)",
        )
        .run(taskId, hostId, key);
      this.local
        .prepare(
          "INSERT INTO workspace_reservations VALUES(?,?,?,?) ON CONFLICT(host_id,canonical_cwd) DO NOTHING",
        )
        .run(hostId, key, taskId, existing?.["operation_id"] ?? operationId);
      this.local.exec("COMMIT");
      return null;
    } catch (error) {
      this.local.exec("ROLLBACK");
      throw error;
    }
  }
  async workspaceOwner(
    hostId: string,
    canonicalCwd: string,
    taskId: string,
  ): Promise<string | null> {
    const key = workspaceKey(canonicalCwd);
    const row = this.workspaceReservation(hostId, key);
    if (!row || row["task_id"] === taskId) return null;
    const owner = String(row["task_id"]);
    let task: Document<"task-board/task"> | null = null;
    try {
      task = await this.read("task-board/task", owner, true, true);
    } catch (error) {
      if (!(error instanceof IvyError && error.code === "not_found")) throw error;
    }
    if (task && !task.metadata.effectivelyArchived) {
      const shared = key.startsWith("project:") || key.startsWith("directory:");
      if (task.value.claim || task.value.publication ||
        !shared && !["done", "cancelled", "backlog"].includes(task.value.workflowState)) return owner;
      const run = task.value.lastRun ? await this.read("task-board/run", task.value.lastRun) : null;
      if (run && !["completed", "failed", "cancelled"].includes(run.value.phase)) return owner;
      // An admission can hold the reservation before publishing its Task marker. Do not
      // reclaim across a concurrent revision or while that Task's publication gate is held.
      const current = await this.client.request("objects.stat", { objectId: owner });
      if (current.currentRevision !== task.pin.revision || this.local.prepare(
        "SELECT 1 FROM active_gates g JOIN documents d ON d.id=g.gate_id WHERE d.key='task-board/coordination' AND d.name=?",
      ).get("TaskBoard publication gate " + owner)) return owner;
    }
    this.local.prepare(
      "DELETE FROM workspace_reservations WHERE host_id=? AND canonical_cwd=? AND task_id=? AND operation_id=?",
    ).run(hostId, String(row["canonical_cwd"]), owner, String(row["operation_id"]));
    return null;
  }
  async reserveAvailableWorkspace(
    hostId: string,
    canonicalCwd: string,
    taskId: string,
    operationId: string,
  ): Promise<string | null> {
    const owner = await this.workspaceOwner(hostId, canonicalCwd, taskId);
    if (owner) return owner;
    return this.reserveWorkspace(hostId, canonicalCwd, taskId, operationId);
  }
  releaseWorkspace(taskId: string): void {
    this.local
      .prepare("DELETE FROM workspace_reservations WHERE task_id=?")
      .run(taskId);
  }
  releaseWorkspaceOperation(operationId: string): void {
    this.local
      .prepare("DELETE FROM workspace_reservations WHERE operation_id=?")
      .run(operationId);
  }
  private localDocument<K extends ContractKey>(
    key: K,
    ref: string | TaskBoard.ObjectPin,
    primary: boolean,
  ): Document<K> {
    const id = typeof ref === "string" ? ref : ref.objectId,
      head = this.local
        .prepare("SELECT * FROM documents WHERE id=? AND key=?")
        .get(id, key);
    requireThat(
      head && (!primary || (head["parent_id"] ?? null) === this.rootObjectId),
      "task_board_scope_mismatch",
      "The local workflow record has another scope.",
    );
    const revision =
        typeof ref === "string"
          ? Number(head!["current_revision"])
          : ref.revision,
      row = this.local
        .prepare("SELECT * FROM revisions WHERE id=? AND revision=?")
        .get(id, revision);
    requireThat(
      row,
      "task_board_evidence_mismatch",
      "The local workflow revision is unavailable.",
    );
    const value = JSON.parse(String(row!["value_json"])) as ContractValues[K],
      createdAt = String(head!["created_at"]),
      updatedAt = String(head!["updated_at"]);
    return {
      key,
      pin: { objectId: id, revision },
      value,
      metadata: {
        id,
        parentId:
          head!["parent_id"] === null ? null : String(head!["parent_id"]),
        ownerObjectId: null,
        name: String(head!["name"]),
        path: "",
        position: 0,
        icon: null,
        contractKey: key,
        currentRevision: Number(head!["current_revision"]),
        contractVersion: contractVersion(key),
        archivedAt: null,
        effectivelyArchived: false,
        createdAt,
        updatedAt,
      },
      revision: {
        objectId: id,
        revision,
        contractVersion: contractVersion(key),
        contentHash: hashJson(value),
        mediaType: "application/json",
        byteLength: Buffer.byteLength(String(row!["value_json"])),
        createdAt: String(row!["created_at"]),
        references: {},
      },
    };
  }
  async read<K extends ContractKey>(
    key: K,
    ref: string | TaskBoard.ObjectPin,
    primary = true,
    includeArchived = false,
    retained = false,
  ): Promise<Document<K>> {
    const objectId = typeof ref === "string" ? ref : ref.objectId;
    if (localContractKeys.has(key) || this.localKey(objectId) === key) {
      const document = this.localDocument(key, ref, primary);
      if (!retained && key === 'task-board/task' && 'commentStore' in document.value) {
        const operation = this.localDocument('task-board/operation', document.metadata.parentId!, false);
        requireThat('taskId' in operation.value.request, 'task_board_scope_mismatch',
          'A prepared conversation must belong to its original task operation.');
        document.value = await this.comments.read(document.value as unknown as Wire.Json,
          operation.value.request.taskId) as ContractValues[K];
      }
      return document;
    }
    const cache = this.reads.getStore(),
      cacheKey =
        typeof ref === "string"
          ? null
          : key + ":" + ref.objectId + ":" + ref.revision + ":" + primary + ":" + includeArchived + ":" + retained;
    const cached = cacheKey ? cache?.get(cacheKey) : undefined;
    if (cached) return structuredClone(cached) as Document<K>;
    const result = await this.client.request(
      "objects.read",
      typeof ref === "string"
        ? { objectId: ref }
        : { objectId: ref.objectId, revision: ref.revision },
    );
    requireThat(
      result.object.contractKey === key &&
        readableVersions(key).includes(result.revision.contractVersion as '1.0.0' | '1.1.0' | '1.2.0') &&
        result.revision.mediaType === "application/json" &&
        result.content.encoding === "json",
      "task_board_contract_mismatch",
      "The selected Object is not an exact supported TaskBoard record.",
    );
    requireThat(
      !primary || result.object.parentId === this.rootObjectId,
      "task_board_scope_mismatch",
      "Prepared payloads and records from another workflow root are not primary domain Objects.",
    );
    requireThat(
      !result.object.effectivelyArchived || includeArchived,
      "task_board_record_archived",
      "An archived workflow record is not writable workflow state.",
    );
    requireThat(
      typeof ref === "string" || result.revision.revision === ref.revision,
      "task_board_evidence_mismatch",
      "The selected immutable revision differs from its pin.",
    );
    const document = {
      key,
      pin: pinOf(result),
      value: retained ? result.content.value as unknown as ContractValues[K] : await this.upgradeStored(key, (key === 'task-board/task'
        ? await this.comments.read(result.content.value, result.object.id)
        : result.content.value) as ContractValues[K]),
      metadata: result.object,
      revision: result.revision,
    };
    // Latest reads always reach Hive; cached pins are scoped to one action, not service lifetime.
    if (
      cacheKey &&
      cache &&
      Buffer.byteLength(canonical(document.value)) <= 65536 &&
      cache.size < 256
    )
      cache.set(cacheKey, structuredClone(document));
    return document;
  }
  /** Legacy completion issues can pin a pre-closure revision already pruned before recovery existed. */
  async retainedRecoveryRun(pin: TaskBoard.ObjectPin): Promise<Document<'task-board/run'>> {
    try { return await this.read('task-board/run', pin); }
    catch (error) {
      if (!(error instanceof IvyError) || !['not_found', 'revision_pruned'].includes(error.code)) throw error;
      const current = await this.read('task-board/run', pin.objectId);
      requireThat(current.pin.revision >= pin.revision && ['completed', 'failed'].includes(current.value.phase) &&
        ['succeeded', 'failed'].includes(current.value.externalOutcome.state), 'task_board_recovery_evidence_missing',
        'A pruned recovery revision needs the same original Run with a confirmed terminal owner outcome.');
      return current;
    }
  }
  private async recoveryReference(pin: TaskBoard.ObjectPin): Promise<TaskBoard.ObjectPin | null> {
    try { return (await this.retainedRecoveryRun(pin)).pin; }
    catch (error) {
      if (error instanceof IvyError && ['not_found', 'revision_pruned', 'task_board_recovery_evidence_missing'].includes(error.code)) return null;
      throw error;
    }
  }
  async named<K extends ContractKey>(
    key: K,
    name: string,
    parentId = this.rootObjectId,
  ): Promise<Document<K> | null> {
    if (
      localContractKeys.has(key) ||
      (parentId !== null && this.localKey(parentId) === "task-board/operation")
    ) {
      const row =
        parentId === null
          ? this.local
              .prepare(
                "SELECT id FROM documents WHERE key=? AND name=? AND parent_id IS NULL",
              )
              .get(key, name)
          : this.local
              .prepare(
                "SELECT id FROM documents WHERE key=? AND name=? AND parent_id=?",
              )
              .get(key, name, parentId);
      return row
        ? this.localDocument(
            key,
            String(row["id"]),
            parentId === this.rootObjectId,
          )
        : null;
    }
    // Resolve identity across versions, then refuse an unsupported current record. Filtering first
    // could hide a migrated operation and incorrectly attempt to create its identity again.
    const page = await this.client.request("objects.query", {
      contractKey: key,
      where: {
        op: "and",
        args: [
          parentId === null
            ? { op: "isNull", field: "object.parentId" }
            : { op: "eq", field: "object.parentId", value: parentId },
          { op: "eq", field: "object.name", value: name },
        ],
      },
      limit: 2,
    });
    requireThat(
      page.items.length <= 1 && !page.nextCursor,
      "task_board_identity_conflict",
      "A durable workflow identity must select exactly one record.",
    );
    return page.items[0]
      ? this.read(key, page.items[0].objectId, parentId === this.rootObjectId)
      : null;
  }
  preparedMutationId(scope: string): Promise<string> {
    return scopedOperationId(this.client, ["task-board", scope]);
  }
  async write<K extends ContractKey>(
    key: K,
    value: ContractValues[K],
    mutationId: string,
    destination: TaskBoard.PreparedWrite["destination"],
  ): Promise<TaskBoard.ObjectPin> {
    return this.writeWithIdentity(key, value, mutationId, destination, false);
  }
  async writePrepared<K extends ContractKey>(
    key: K,
    value: ContractValues[K],
    mutationId: string,
    destination: TaskBoard.PreparedWrite["destination"],
    retainedVersion: TaskBoard.PreparedWrite['contractVersion'] = contractVersion(key),
  ): Promise<TaskBoard.ObjectPin> {
    return this.writeWithIdentity(key, value, mutationId, destination, true, retainedVersion);
  }
  private async writeWithIdentity<K extends ContractKey>(
    key: K,
    value: ContractValues[K],
    mutationId: string,
    destination: TaskBoard.PreparedWrite["destination"],
    retainedIdentity: boolean,
    selectedVersion: TaskBoard.PreparedWrite['contractVersion'] = contractVersion(key),
  ): Promise<TaskBoard.ObjectPin> {
    if (key === 'task-board/task' && !('create' in destination) &&
        'commentStore' in (value as TaskBoard.Task))
      value = await this.comments.read(value as unknown as Wire.Json, destination.objectId) as ContractValues[K];
    if (!retainedIdentity) value = await this.upgradeStored(key, value);
    if (
      localContractKeys.has(key) ||
      ("create" in destination &&
        destination.create.parentId !== null &&
        this.localKey(destination.create.parentId) === "task-board/operation")
    ) {
      const encoded = canonical(value),
        now = new Date().toISOString();
      // The reconciler collects history periodically. Workflow writes must not
      // queue behind cleanup's remote receipt checks, especially gate releases.
      if ("create" in destination) {
        const existing = await this.named(
          key,
          destination.create.name,
          destination.create.parentId,
        );
        if (existing) {
          requireThat(
            hashJson(existing.value) === hashJson(value),
            "revision_conflict",
            "The local TaskBoard identity differs.",
          );
          return existing.pin;
        }
        const id = randomUUID();
        this.local.exec("BEGIN IMMEDIATE");
        try {
          this.local
            .prepare("INSERT INTO documents VALUES(?,?,?,?,?,?,?)")
            .run(
              id,
              key,
              destination.create.name,
              destination.create.parentId,
              1,
              now,
              now,
            );
          this.local
            .prepare("INSERT INTO revisions VALUES(?,?,?,?)")
            .run(id, 1, encoded, now);
          if (key === "task-board/coordination" && (value as TaskBoard.Coordination).activeOperation)
            this.local.prepare("INSERT INTO active_gates VALUES(?,?)")
              .run(id, (value as TaskBoard.Coordination).activeOperation!.objectId);
          this.local.exec("COMMIT");
        } catch (error) {
          this.local.exec("ROLLBACK");
          const winner = await this.named(
            key,
            destination.create.name,
            destination.create.parentId,
          );
          if (winner) {
            if (hashJson(winner.value) === hashJson(value)) return winner.pin;
            throw new IvyError(
              "revision_conflict",
              "The local TaskBoard identity already exists.",
            );
          }
          throw error;
        }
        return { objectId: id, revision: 1 };
      }
      const next = destination.expectedRevision + 1;
      this.local.exec("BEGIN IMMEDIATE");
      try {
        const changed = this.local
          .prepare(
            "UPDATE documents SET current_revision=?,updated_at=? WHERE id=? AND key=? AND current_revision=?",
          )
          .run(
            next,
            now,
            destination.objectId,
            key,
            destination.expectedRevision,
          );
        requireThat(
          changed.changes === 1,
          "revision_conflict",
          "The local TaskBoard row changed.",
        );
        this.local
          .prepare("INSERT INTO revisions VALUES(?,?,?,?)")
          .run(destination.objectId, next, encoded, now);
        if (key === "task-board/coordination") {
          const active = (value as TaskBoard.Coordination).activeOperation;
          if (active) this.local.prepare("INSERT INTO active_gates VALUES(?,?) ON CONFLICT(gate_id) DO UPDATE SET operation_id=excluded.operation_id")
            .run(destination.objectId, active.objectId);
          else this.local.prepare("DELETE FROM active_gates WHERE gate_id=?").run(destination.objectId);
        }
        if (key === "task-board/scan-cursor" || key === "task-board/coordination" || key === "task-board/native-signals")
          this.local
            .prepare("DELETE FROM revisions WHERE id=? AND revision<?")
            .run(destination.objectId, next);
        this.local.exec("COMMIT");
      } catch (error) {
        this.local.exec("ROLLBACK");
        throw error;
      }
      return { objectId: destination.objectId, revision: next };
    }
    const content: Wire.Content = {
      encoding: "json",
      value: (key === 'task-board/task' && !('create' in destination)
        ? await this.comments.write(value as TaskBoard.Task, destination.objectId, mutationId, selectedVersion)
        : value) as Wire.Json,
    };
    let references: Wire.RevisionReferences = {};
    if (key === "task-board/task") {
      const stored = content.value as unknown as { commentStore?: { kind?: string; root?: { object: TaskBoard.ObjectPin } | null } };
      if (stored.commentStore?.kind === 'tree' && stored.commentStore.root)
        references.conversation = stored.commentStore.root.object;
      const task = value as TaskBoard.Task,
        result = task.latestResult;
      if (result) references["result"] = result;
      if (task.lastRun) references["run"] = task.lastRun;
      if (task.completionIssue) {
        const recoveryRun = await this.recoveryReference(task.completionIssue.run);
        if (recoveryRun) references['recovery-run'] = recoveryRun;
      }
      const attachments = taskAttachmentPins(task);
      if ('create' in destination) {
        requireThat(attachments.length + Object.keys(references).length <= 256,
          'limit_exceeded', 'An initial Task exceeds its exact reference capacity.');
        for (const [index, pin] of attachments.entries()) references['attachment-' + index] = pin;
      } else references = await taskReferences(this.client, destination.objectId, references, attachments,
        scope => this.preparedMutationId(scope));
    } else if (key === 'task-board/run') {
      const run = value as TaskBoard.Run;
      if (!['completed', 'failed', 'cancelled'].includes(run.phase))
        references['source-task'] = { objectId: run.taskId, revision: run.originalRequest.expectedRevision };
    } else if (key === "task-board/delivery") {
      const request = (value as TaskBoard.Delivery).chatRequest;
      if (request) references["source"] = request.source;
    } else if (key === "task-board/result") {
      const artifacts = [
        ...(value as TaskBoard.Result).content.artifacts,
        ...((value as unknown as { content: { checks?: { evidence: TaskBoard.Artifact[] }[] } }).content.checks ?? [])
          .flatMap(finding => finding.evidence),
      ]
        .filter((artifact) => !this.localKey(artifact.object.objectId))
        .slice(0, 256);
      for (const [index, artifact] of artifacts.entries())
        references["artifact-" + index] = artifact.object;
    }
    if (!retainedIdentity)
      mutationId = await this.preparedMutationId(mutationId);
    let result: Operation.ObjectWriteResult;
    try {
      result = await this.client.request(
        "objects.write",
        "create" in destination
          ? {
              mutationId,
              contractVersion: selectedVersion,
              content,
              references,
              create: {
                ...destination.create,
                ownerObjectId: null,
                contractKey: key,
              },
            }
          : {
              mutationId,
              contractVersion: selectedVersion,
              content,
              references,
              objectId: destination.objectId,
              expectedRevision: destination.expectedRevision,
              ...(key === "task-board/task" ? { expectedArchived: false } : {}),
            },
      );
    } catch (error) {
      if (
        !("create" in destination) ||
        !(error instanceof IvyError) ||
        error.code !== "revision_conflict"
      )
        throw error;
      const existing = await this.named(
        key,
        destination.create.name,
        destination.create.parentId,
      );
      requireThat(
        existing &&
          existing.pin.revision === 1 &&
          hashJson(existing.value) === hashJson(value),
        "revision_conflict",
        "The durable TaskBoard identity already contains different content.",
      );
      return existing.pin;
    }
    const cache = this.reads.getStore();
    if (cache)
      for (const [id, document] of cache)
        if (document.pin.objectId === result.object.id) cache.delete(id);
    if (cache && Buffer.byteLength(canonical(value)) <= 65536 && cache.size < 256) {
      const primary = result.object.parentId === this.rootObjectId;
      cache.set(
        key +
          ":" +
          result.object.id +
          ":" +
          result.revision.revision +
          ":" +
          primary,
        {
          key,
          pin: pinOf(result),
          value: structuredClone(value),
          metadata: result.object,
          revision: result.revision,
        },
      );
    }
    return pinOf(result);
  }
  async writeAttachment(
    taskId: string,
    operationId: string,
    attachmentId: string,
    bytes: Buffer,
  ): Promise<TaskBoard.ObjectPin> {
    requireThat(
      bytes.length <= 8 * 1024 * 1024,
      "limit_exceeded",
      "Task attachments are limited to 8 MiB.",
    );
    const mutationId = await scopedOperationId(this.client, [
      "task-board-attachment",
      operationId,
    ]);
    const name = "Task attachment " + attachmentId;
    let result: Operation.ObjectWriteResult;
    try {
      result = await this.client.request("objects.write", {
        mutationId,
        contractVersion: version,
        references: {},
        create: {
          parentId: taskId,
          ownerObjectId: taskId,
          contractKey: "task-board/attachment",
          name,
        },
        content: { encoding: "base64", value: bytes.toString("base64") },
      });
    } catch (error) {
      if (!(error instanceof IvyError) || !["revision_conflict", "mutation_expired"].includes(error.code)) throw error;
      const found = await this.client.request("objects.query", {
        contractKey: "task-board/attachment", limit: 2,
        where: { op: "and", args: [
          { op: "eq", field: "object.parentId", value: taskId },
          { op: "eq", field: "object.name", value: name },
        ] },
      });
      requireThat(found.items.length === 1 && !found.nextCursor, "task_board_evidence_mismatch", "The original attachment identity is unavailable.");
      const existing = await this.client.request("objects.read", { objectId: found.items[0]!.objectId });
      requireThat(existing.object.parentId === taskId && existing.object.ownerObjectId === taskId &&
        !existing.object.effectivelyArchived && existing.object.currentRevision === 1 && existing.revision.revision === 1 &&
        existing.revision.contractVersion === version && existing.revision.byteLength === bytes.length &&
        existing.revision.contentHash === digest(bytes) && existing.content.encoding === "base64" &&
        Buffer.from(existing.content.value, "base64").equals(bytes),
      "task_board_evidence_mismatch", "The retained attachment must match its exact original bytes and owner.");
      return pinOf(existing);
    }
    return pinOf(result);
  }
  async page<K extends ContractKey>(
    key: K,
    options: {
      cursor?: string;
      limit?: number;
      where?: Wire.QueryPredicate;
      orderBy?: Wire.ObjectQuery["orderBy"];
      select?: string[];
      contractVersions?: string[];
    } = {},
  ) {
    if (localContractKeys.has(key)) {
      type PageDocument = Pick<Document<K>, "pin" | "value"> & { metadata: { parentId: string | null } };
      const field = (document: PageDocument, path: string): unknown => {
        if (path === "object.id") return document.pin.objectId;
        if (path === "object.parentId") return document.metadata.parentId;
        if (!path.startsWith("data:/")) return undefined;
        let value: unknown = document.value;
        for (const part of path.slice(6).split("/"))
          value =
            value && typeof value === "object"
              ? (value as Record<string, unknown>)[part]
              : undefined;
        return value;
      };
      const matches = (
        document: PageDocument,
        predicate: Wire.QueryPredicate,
      ): boolean => {
        if (predicate.op === "and" || predicate.op === "or")
          return predicate.op === "and"
            ? predicate.args.every((value) => matches(document, value))
            : predicate.args.some((value) => matches(document, value));
        if (predicate.op === "not") return !matches(document, predicate.arg);
        const leaf = predicate as {
          op: string;
          field: string;
          value?: unknown;
        };
        const value = field(document, leaf.field);
        if (leaf.op === "eq") return value !== undefined && canonical(value) === canonical(leaf.value);
        if (leaf.op === "ne") return value !== undefined && canonical(value) !== canonical(leaf.value);
        if (leaf.op === "in")
          return (
            value !== undefined && Array.isArray(leaf.value) &&
            leaf.value.some((item) => canonical(item) === canonical(value))
          );
        if (leaf.op === "contains")
          return (
            typeof value === "string" &&
            typeof leaf.value === "string" &&
            value.includes(leaf.value)
          );
        if (leaf.op === "isNull") return value == null;
        return false;
      };
      const rows = this.local
        .prepare(`SELECT d.id,d.parent_id,d.current_revision,r.value_json FROM documents d
          JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key=? ORDER BY d.id`)
        .all(key);
      const documents = rows
        .map((row): PageDocument => ({
          pin: { objectId: String(row["id"]), revision: Number(row["current_revision"]) },
          metadata: { parentId: row["parent_id"] === null ? null : String(row["parent_id"]) },
          value: JSON.parse(String(row["value_json"])) as ContractValues[K],
        }))
        .filter((value) => !options.where || matches(value, options.where));
      const ordered = [...documents].sort((a, b) => {
        for (const order of options.orderBy ?? [
          { field: "object.id", direction: "asc" as const },
        ]) {
          const left = field(a, order.field),
            right = field(b, order.field),
            result =
              typeof left === "number" && typeof right === "number"
                ? left - right
                : String(left ?? "").localeCompare(String(right ?? ""));
          if (result) return order.direction === "desc" ? -result : result;
        }
        return a.pin.objectId.localeCompare(b.pin.objectId);
      });
      const offset = options.cursor
        ? Number(Buffer.from(options.cursor, "base64url").toString("utf8"))
        : 0;
      requireThat(
        Number.isInteger(offset) && offset >= 0 && offset <= ordered.length,
        "invalid_cursor",
        "The local workflow cursor is invalid.",
      );
      const limit = options.limit ?? 50,
        selected = ordered.slice(offset, offset + limit);
      return {
        items: selected.map((value) => ({
          objectId: value.pin.objectId,
          revision: value.pin.revision,
          values: {},
        })),
        nextCursor:
          offset + selected.length < ordered.length
            ? Buffer.from(String(offset + selected.length)).toString(
                "base64url",
              )
            : null,
      };
    }
    const root: Wire.QueryPredicate =
      this.rootObjectId === null
        ? { op: "isNull", field: "object.parentId" }
        : { op: "eq", field: "object.parentId", value: this.rootObjectId };
    return this.client.request("objects.query", {
      contractKey: key,
      contractVersions: options.contractVersions ?? readableVersions(key),
      where: options.where ? { op: "and", args: [root, options.where] } : root,
      orderBy: options.orderBy ?? [{ field: "object.id", direction: "asc" }],
      limit: options.limit ?? 50,
      ...(options.select ? { select: options.select } : {}),
      ...(options.cursor ? { cursor: options.cursor } : {}),
    });
  }
  saveLocalBlob(
    key: string,
    name: string,
    parentId: string,
    bytes: Buffer,
  ): TaskBoard.ObjectPin {
    const row = this.local
      .prepare(
        "SELECT id,bytes FROM blobs WHERE key=? AND name=? AND parent_id=?",
      )
      .get(key, name, parentId);
    if (row) {
      requireThat(
        Buffer.from(row["bytes"] as Uint8Array).equals(bytes),
        "revision_conflict",
        "The local evidence identity differs.",
      );
      return { objectId: String(row["id"]), revision: 1 };
    }
    const id = randomUUID();
    this.local
      .prepare("INSERT INTO blobs VALUES(?,?,?,?,?)")
      .run(id, key, name, parentId, bytes);
    return { objectId: id, revision: 1 };
  }
  localBlob(key: string, pin: TaskBoard.ObjectPin, parentId: string): Buffer {
    requireThat(
      pin.revision === 1,
      "task_board_evidence_mismatch",
      "Local evidence blobs have one revision.",
    );
    const row = this.local
      .prepare("SELECT bytes FROM blobs WHERE id=? AND key=? AND parent_id=?")
      .get(pin.objectId, key, parentId);
    requireThat(
      row,
      "task_board_evidence_mismatch",
      "The local evidence blob is unavailable.",
    );
    return Buffer.from(row!["bytes"] as Uint8Array);
  }
  localKey(objectId: string): string | null {
    const row =
      this.local
        .prepare("SELECT key FROM documents WHERE id=?")
        .get(objectId) ??
      this.local.prepare("SELECT key FROM blobs WHERE id=?").get(objectId);
    return row ? String(row["key"]) : null;
  }
  localArtifact(pin: TaskBoard.ObjectPin): {
    contractKey: string;
    parentId: string | null;
    contentHash: string;
    mediaType: string;
    bytes: Buffer;
  } | null {
    const blob = this.local
      .prepare("SELECT key,parent_id,bytes FROM blobs WHERE id=?")
      .get(pin.objectId);
    if (blob) {
      requireThat(
        pin.revision === 1,
        "task_board_evidence_mismatch",
        "Local evidence blobs have one revision.",
      );
      const bytes = Buffer.from(blob["bytes"] as Uint8Array);
      return {
        contractKey: String(blob["key"]),
        parentId: blob["parent_id"] === null ? null : String(blob["parent_id"]),
        contentHash: digest(bytes),
        mediaType: "application/octet-stream",
        bytes,
      };
    }
    const head = this.local
      .prepare("SELECT key,parent_id FROM documents WHERE id=?")
      .get(pin.objectId);
    if (!head) return null;
    const row = this.local
      .prepare("SELECT value_json FROM revisions WHERE id=? AND revision=?")
      .get(pin.objectId, pin.revision);
    requireThat(
      row,
      "task_board_evidence_mismatch",
      "The local artifact revision is unavailable.",
    );
    const encoded = String(row!["value_json"]),
      value = JSON.parse(encoded),
      bytes = Buffer.from(encoded);
    return {
      contractKey: String(head["key"]),
      parentId: head["parent_id"] === null ? null : String(head["parent_id"]),
      contentHash: hashJson(value),
      mediaType: "application/json",
      bytes,
    };
  }
  close(): void {
    this.local.close();
  }
}
