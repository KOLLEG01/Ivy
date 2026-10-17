import { DatabaseSync } from "node:sqlite";
import { mkdirSync } from "node:fs";
import { dirname } from "node:path";
import { digest, requireThat } from "../../../packages/sdk/src/node.js";
import type { Output, Task } from "./schema.js";

export interface RecordTask {
  task: Task;
  revision: number;
  resultObjectId: string | null;
  nextAt: number;
  state: unknown;
  deleted: boolean;
  lastRunId: string | null;
  retentionWarning: string | null;
  resultRootId?: string;
  definition?: {
    jobId: string;
    revision: number;
    assignmentId: string;
    assignmentRevision: number;
  };
  adoptionError?: string | null;
}
export interface Run {
  id: string;
  taskId: string;
  task: Task;
  status: string;
  input: unknown;
  output: Output | null;
  startedAt: string | null;
  finishedAt: string | null;
  eventIndex: number;
  error: string | null;
  workerJobName?: string;
  executionCommitted?: boolean;
  definition?: RecordTask["definition"];
}
export class CollectorStore {
  readonly db: DatabaseSync;
  constructor(path: string, readonly limits = { operations: 10_000, operationBytes: 256 * 1024 * 1024, runs: 1_000, runBytes: 64 * 1024 * 1024 }) {
    mkdirSync(dirname(path), { recursive: true });
    this.db = new DatabaseSync(path);
    this.db
      .exec(`PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, document TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS runs(id TEXT PRIMARY KEY,task_id TEXT NOT NULL,status TEXT NOT NULL,document TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS operations(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL,created_at INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS retired_operations(id TEXT PRIMARY KEY,request_hash TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS retired_runs(id TEXT PRIMARY KEY);
      CREATE TRIGGER IF NOT EXISTS retired_operation_insert BEFORE INSERT ON operations
        WHEN EXISTS(SELECT 1 FROM retired_operations WHERE id=NEW.id)
        BEGIN SELECT RAISE(ABORT,'operation_expired'); END;
      CREATE INDEX IF NOT EXISTS runs_status ON runs(status);
      CREATE INDEX IF NOT EXISTS runs_task_status ON runs(task_id,status);`);
    for (const run of this.runs("running")) {
      run.status = "interrupted";
      run.finishedAt = new Date().toISOString();
      // Preserve the schedule when a named process tree can be reconciled by the engine.
      run.error = "outcome_unknown";
      this.transaction(() => {
        this.saveRun(run);
        const row = this.get(run.taskId);
        if (row && !run.workerJobName) {
          row.task.enabled = false;
          row.revision++;
          this.save(row);
        }
      });
    }
  }
  transaction<T>(work: () => T): T {
    this.db.exec("BEGIN IMMEDIATE");
    try {
      const value = work();
      this.db.exec("COMMIT");
      return value;
    } catch (error) {
      this.db.exec("ROLLBACK");
      throw error;
    }
  }
  get(id: string, includeDeleted = false): RecordTask | null {
    const row = this.db
      .prepare("SELECT document FROM tasks WHERE id=?")
      .get(id);
    const task: RecordTask | null = row
      ? JSON.parse(String(row["document"]))
      : null;
    return task && (includeDeleted || !task.deleted) ? task : null;
  }
  list(includeDeleted = false): RecordTask[] {
    return this.db
      .prepare("SELECT document FROM tasks ORDER BY id")
      .all()
      .map((r) => JSON.parse(String(r["document"])) as RecordTask)
      .filter((task) => includeDeleted || !task.deleted);
  }
  save(value: RecordTask) {
    this.db
      .prepare(
        "INSERT INTO tasks VALUES (?,?) ON CONFLICT(id) DO UPDATE SET document=excluded.document",
      )
      .run(value.task.id, JSON.stringify(value));
  }
  run(id: string): Run | null {
    const row = this.db.prepare("SELECT document FROM runs WHERE id=?").get(id);
    return row ? JSON.parse(String(row["document"])) : null;
  }
  runs(status: string): Run[] {
    return this.db
      .prepare("SELECT document FROM runs WHERE status=? ORDER BY rowid")
      .all(status)
      .map((r) => JSON.parse(String(r["document"])));
  }
  saveRun(run: Run) {
    const document = ["queued", "running", "publishing"].includes(run.status) || /unknown/i.test(run.error ?? "")
      ? run
      : {
          ...run,
          task: { ...run.task, script: "", config: {}, dependencies: {} },
          input: null,
          output: null,
        };
    const work = () => {
      this.db.prepare("INSERT INTO runs VALUES (?,?,?,?) ON CONFLICT(id) DO UPDATE SET status=excluded.status,document=excluded.document")
        .run(run.id, run.taskId, run.status, JSON.stringify(document));
      if (["succeeded", "failed", "cancelled"].includes(run.status) && !/unknown/i.test(run.error ?? "")) this.collectRuns(run.id);
    };
    if (this.db.isTransaction || !["succeeded", "failed", "cancelled"].includes(run.status)) work();
    else this.transaction(work);
  }
  busy(id: string) {
    return !!this.db
      .prepare(
        "SELECT 1 FROM runs WHERE task_id=? AND status IN ('queued','running','publishing') LIMIT 1",
      )
      .get(id);
  }
  executing(id: string) {
    return !!this.db
      .prepare(
        "SELECT 1 FROM runs WHERE task_id=? AND status IN ('queued','running') LIMIT 1",
      )
      .get(id);
  }
  metadata(key: string): string | null {
    const r = this.db
      .prepare("SELECT value FROM metadata WHERE key=?")
      .get(key);
    return r ? String(r["value"]) : null;
  }
  setMetadata(key: string, value: string) {
    this.db
      .prepare(
        "INSERT INTO metadata VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
      )
      .run(key, value);
  }
  operation(id: string) {
    const row = this.db
      .prepare("SELECT result FROM operations WHERE id=?")
      .get(id);
    if (row) return JSON.parse(String(row["result"]));
    requireThat(!this.db.prepare("SELECT 1 FROM retired_operations WHERE id=?").get(id),
      "operation_expired", "The completed Collector operation was evicted; its identity cannot be reused.");
    return null;
  }
  usedRunId(id: string): boolean {
    return !!this.db.prepare("SELECT 1 FROM runs WHERE id=? UNION ALL SELECT 1 FROM retired_runs WHERE id=? LIMIT 1").get(id, id);
  }
  private completedRun(id: string): boolean {
    const run = this.run(id);
    return run ? ["succeeded", "failed", "cancelled"].includes(run.status) && !/unknown/i.test(run.error ?? "")
      : !!this.db.prepare("SELECT 1 FROM retired_runs WHERE id=?").get(id);
  }
  private forgetDefinition(id: string): number {
    let bytes = 0;
    for (const kind of ["definition-write:", "definition-operation:", "definition-request:", "definition-expiry:"]) {
      const prefix = kind + id;
      for (const row of this.db.prepare("DELETE FROM metadata WHERE key=? OR substr(key,1,?)=? RETURNING value").all(prefix, prefix.length + 1, prefix + ":"))
        bytes += Buffer.byteLength(String(row["value"]));
    }
    return bytes;
  }
  private collectOperations(protectedId?: string): void {
    const usage = () => this.db.prepare(`SELECT COUNT(*) AS count,COALESCE(SUM(length(CAST(request AS BLOB))+length(CAST(result AS BLOB))),0)+
      (SELECT COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM metadata m WHERE key GLOB 'definition-*'
        AND NOT EXISTS(SELECT 1 FROM retired_operations r WHERE m.key='definition-request:'||r.id)) AS bytes FROM operations`).get()!;
    const totals = usage();
    let count = Number(totals["count"]), bytes = Number(totals["bytes"]);
    const full = () => count > this.limits.operations || bytes > this.limits.operationBytes;
    if (!full()) return;
    for (const row of this.db.prepare("SELECT id,request,result,length(CAST(request AS BLOB))+length(CAST(result AS BLOB)) AS bytes FROM operations ORDER BY created_at,rowid").all()) {
      const id = String(row["id"]), result = JSON.parse(String(row["result"])) as { runId?: string } | null;
      if (id === protectedId || result?.runId && !this.completedRun(result.runId)) continue;
      // Opaque caller IDs need compact replay guards after their result leaves the ring.
      this.db.prepare("INSERT OR IGNORE INTO retired_operations VALUES(?,?)").run(id, digest(String(row["request"])));
      const definition = this.metadata("definition-request:" + id) !== null;
      const definitionBytes = this.forgetDefinition(id);
      if (definition) this.setMetadata("definition-request:" + id, JSON.stringify({ retired: true, requestHash: digest(String(row["request"])) }));
      this.db.prepare("DELETE FROM operations WHERE id=?").run(id);
      count--; bytes -= Number(row["bytes"]) + definitionBytes;
      if (!full()) break;
    }
    requireThat(!full(), "capacity_exceeded", "Collector journal capacity is occupied by live or unresolved work.");
  }
  mutate(id: string, request: string, work: () => unknown): unknown {
    return this.transaction(() => {
      const prior = this.db
        .prepare("SELECT request,result FROM operations WHERE id=?")
        .get(id);
      if (prior) {
        requireThat(
          prior["request"] === request,
          "operation_conflict",
          "Operation ID was already used for another request.",
        );
        return JSON.parse(String(prior["result"]));
      }
      const retired = this.db.prepare("SELECT request_hash FROM retired_operations WHERE id=?").get(id);
      requireThat(!retired || retired["request_hash"] === digest(request), "operation_conflict", "The evicted operation had different arguments.");
      requireThat(!retired, "operation_expired", "The completed Collector operation was evicted; its identity cannot be reused.");
      const result = work();
      this.db
        .prepare("INSERT INTO operations VALUES (?,?,?,?)")
        .run(id, request, JSON.stringify(result), Date.now());
      this.collectOperations(id);
      return result;
    });
  }
  cleanup() {
    this.transaction(() => {
    for (const row of this.db
      .prepare(
        "SELECT key FROM metadata WHERE key LIKE 'definition-expiry:%' AND CAST(value AS REAL)<?",
      )
      .all(Date.now())) {
      const operationId = String(row["key"]).slice("definition-expiry:".length);
      // A deadline cannot release a prepared write whose effect remains unresolved.
      if (this.db.prepare("SELECT 1 FROM operations WHERE id=? UNION ALL SELECT 1 FROM retired_operations WHERE id=? LIMIT 1").get(operationId, operationId))
        this.forgetDefinition(operationId);
    }
    this.collectOperations();
    this.collectRuns();
    });
  }
  private collectRuns(protectedId?: string): void {
    const totals = this.db.prepare("SELECT COUNT(*) AS count,COALESCE(SUM(length(CAST(document AS BLOB))),0) AS bytes FROM runs").get()!;
    let count = Number(totals["count"]), bytes = Number(totals["bytes"]);
    if (count <= this.limits.runs && bytes <= this.limits.runBytes) return;
    const retire = this.db.prepare("INSERT OR IGNORE INTO retired_runs VALUES(?)"), remove = this.db.prepare("DELETE FROM runs WHERE id=?");
    for (const row of this.db.prepare(`SELECT id,length(CAST(document AS BLOB)) AS bytes FROM runs WHERE status IN ('succeeded','failed','cancelled')
      AND lower(COALESCE(json_extract(document,'$.error'),'')) NOT GLOB '*unknown*'
      AND (? IS NULL OR id<>?)
      AND id NOT IN (SELECT json_extract(document,'$.lastRunId') FROM tasks WHERE json_extract(document,'$.lastRunId') IS NOT NULL)
      ORDER BY json_extract(document,'$.finishedAt'),rowid`).all(protectedId ?? null, protectedId ?? null)) {
      if (count <= this.limits.runs && bytes <= this.limits.runBytes) break;
      const id = String(row["id"]);
      retire.run(id); remove.run(id);
      count--; bytes -= Number(row["bytes"]);
    }
  }
  close() {
    this.db.close();
  }
}
