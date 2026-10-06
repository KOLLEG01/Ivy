import {
  canonical,
  digest,
  hashJson,
} from "../../../../packages/sdk/src/node.js";
import { AsyncLocalStorage } from "node:async_hooks";
import { DatabaseSync } from "node:sqlite";
import { randomUUID } from "node:crypto";
import { statfsSync, statSync } from "node:fs";
import { join, win32 } from "node:path";
import { IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import type {
  Operation,
  TaskBoard,
  Wire,
} from "../../../../packages/sdk/src/node.js";
import { scopedOperationId } from "../../../../packages/sdk/src/client.js";
import type { RpcClient } from "../../../../packages/sdk/src/client.js";
import { contractVersion, localContractKeys, readableVersions, upgradeRecord, version } from "./schema.js";
import type { ContractKey, ContractValues } from "./schema.js";

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
  [...new Map([...task.attachments, ...task.comments.flatMap(comment => comment.attachments)]
    .map(attachment => [hashJson(attachment.object), attachment.object])).values()];

// The journal may run on Linux while reserving a Windows host's workspace.
const workspaceKey = (path: string) => /^[a-z]:[\\/]|^\\\\/i.test(path)
  ? win32.normalize(path).toLowerCase() : path;

/** Generic persistence of TaskBoard's exact contracts. It does not authorize workflow transitions. */
export class TaskBoardStore {
  private static readonly replayWindowMs = 24 * 60 * 60 * 1000;
  static readonly nativeEvidenceWindowMs = 7 * 24 * 60 * 60 * 1000;
  private readonly reads = new AsyncLocalStorage<
    Map<string, Document<ContractKey>>
  >();
  private readonly local: DatabaseSync;
  private readonly dataRoot: string | null;
  withReadCache<T>(work: () => Promise<T>): Promise<T> {
    return this.reads.getStore() ? work() : this.reads.run(new Map(), work);
  }
  constructor(
    readonly client: RpcClient,
    readonly rootObjectId: string | null,
    dataRoot?: string,
  ) {
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
  async collectLocal(now = Date.now()): Promise<void> {
    const due = this.local
      .prepare("SELECT root_id FROM cleanup_roots WHERE delete_after<=? ORDER BY delete_after LIMIT 32")
      .all(new Date(now).toISOString());
    for (const row of due) {
      const rootId = String(row["root_id"]);
      if (await this.retainedLocalRoot(rootId)) {
        this.local.prepare("UPDATE cleanup_roots SET delete_after=? WHERE root_id=?")
          .run(new Date(now + 60 * 60 * 1000).toISOString(), rootId);
        continue;
      }
      const ids = this.local.prepare(`WITH RECURSIVE subtree(id) AS (
        SELECT ? UNION ALL SELECT documents.id FROM documents JOIN subtree ON documents.parent_id=subtree.id
      ) SELECT id FROM subtree`).all(rootId).map(value => String(value["id"]));
      this.local.exec("BEGIN IMMEDIATE");
      try {
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
        this.local.exec("COMMIT");
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
    retentionMs = TaskBoardStore.replayWindowMs,
    operationId?: string,
  ): void {
    const deleteAfter = new Date(
      Date.parse(completedAt) + retentionMs,
    ).toISOString();
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
  ): Promise<Document<K>> {
    const objectId = typeof ref === "string" ? ref : ref.objectId;
    if (localContractKeys.has(key) || this.localKey(objectId) === key)
      return this.localDocument(key, ref, primary);
    const cache = this.reads.getStore(),
      cacheKey =
        typeof ref === "string"
          ? null
          : key + ":" + ref.objectId + ":" + ref.revision + ":" + primary + ":" + includeArchived;
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
      value: upgradeRecord(key, result.content.value as ContractValues[K]),
      metadata: result.object,
      revision: result.revision,
    };
    // Latest reads always reach Hive; cached pins are scoped to one action, not service lifetime.
    if (
      cacheKey &&
      cache &&
      result.revision.byteLength <= 65536 &&
      cache.size < 256
    )
      cache.set(cacheKey, structuredClone(document));
    return document;
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
  ): Promise<TaskBoard.ObjectPin> {
    return this.writeWithIdentity(key, value, mutationId, destination, true);
  }
  private async writeWithIdentity<K extends ContractKey>(
    key: K,
    value: ContractValues[K],
    mutationId: string,
    destination: TaskBoard.PreparedWrite["destination"],
    retainedIdentity: boolean,
  ): Promise<TaskBoard.ObjectPin> {
    if (
      localContractKeys.has(key) ||
      ("create" in destination &&
        destination.create.parentId !== null &&
        this.localKey(destination.create.parentId) === "task-board/operation")
    ) {
      const encoded = canonical(value),
        now = new Date().toISOString();
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
        if (key === "task-board/scan-cursor" || key === "task-board/coordination")
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
      value: value as Wire.Json,
    };
    const references: Wire.RevisionReferences = {};
    if (key === "task-board/task") {
      const task = value as TaskBoard.Task,
        result = task.latestResult;
      if (result) references["result"] = result;
      if (task.lastRun) references["run"] = task.lastRun;
      const attachments = taskAttachmentPins(task);
      requireThat(
        attachments.length + Object.keys(references).length <= 256,
        "limit_exceeded",
        "A Task can retain at most 256 exact Run, Result and attachment references.",
      );
      for (const [index, pin] of attachments.entries())
        references["attachment-" + index] = pin;
    } else if (key === "task-board/delivery") {
      const request = (value as TaskBoard.Delivery).chatRequest;
      if (request) references["source"] = request.source;
    } else if (key === "task-board/result") {
      const artifacts = [
        ...(value as TaskBoard.Result).content.artifacts,
        ...(value as TaskBoard.Result).content.checks.flatMap(
          (check) => check.evidence,
        ),
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
              contractVersion: contractVersion(key),
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
              contractVersion: contractVersion(key),
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
    if (cache && result.revision.byteLength <= 65536 && cache.size < 256) {
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
    const result = await this.client.request("objects.write", {
      mutationId,
      contractVersion: version,
      references: {},
      create: {
        parentId: taskId,
        ownerObjectId: taskId,
        contractKey: "task-board/attachment",
        name: "Task attachment " + attachmentId,
      },
      content: { encoding: "base64", value: bytes.toString("base64") },
    });
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
    } = {},
  ) {
    if (localContractKeys.has(key)) {
      const field = (document: Document<K>, path: string): unknown => {
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
        document: Document<K>,
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
        if (leaf.op === "eq") return canonical(value) === canonical(leaf.value);
        if (leaf.op === "ne") return canonical(value) !== canonical(leaf.value);
        if (leaf.op === "in")
          return (
            Array.isArray(leaf.value) &&
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
        .prepare("SELECT id FROM documents WHERE key=? ORDER BY id")
        .all(key);
      const documents = rows
        .map((row) => this.localDocument(key, String(row["id"]), false))
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
      contractVersions: readableVersions(key),
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
