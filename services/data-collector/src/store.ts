import { DatabaseSync } from "node:sqlite";
import { mkdirSync } from "node:fs";
import { dirname } from "node:path";
import { requireThat } from "../../../packages/sdk/src/node.js";
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
}
export class CollectorStore {
  readonly db: DatabaseSync;
  constructor(path: string) {
    mkdirSync(dirname(path), { recursive: true });
    this.db = new DatabaseSync(path);
    this.db
      .exec(`PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, document TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS runs(id TEXT PRIMARY KEY,task_id TEXT NOT NULL,status TEXT NOT NULL,document TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS operations(id TEXT PRIMARY KEY,request TEXT NOT NULL,result TEXT NOT NULL,created_at INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);`);
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
    const document = ["queued", "running", "publishing"].includes(run.status)
      ? run
      : {
          ...run,
          task: { ...run.task, script: "", config: {}, dependencies: {} },
          input: null,
          output: null,
        };
    this.db
      .prepare(
        "INSERT INTO runs VALUES (?,?,?,?) ON CONFLICT(id) DO UPDATE SET status=excluded.status,document=excluded.document",
      )
      .run(run.id, run.taskId, run.status, JSON.stringify(document));
  }
  busy(id: string) {
    return !!this.db
      .prepare(
        "SELECT 1 FROM runs WHERE task_id=? AND status IN ('queued','running','publishing') LIMIT 1",
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
    return row ? JSON.parse(String(row["result"])) : null;
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
      const result = work();
      this.db
        .prepare("INSERT INTO operations VALUES (?,?,?,?)")
        .run(id, request, JSON.stringify(result), Date.now());
      return result;
    });
  }
  cleanup() {
    // Keep each task's latest status; older receipts have a seven-day replay window.
    const before = Date.now() - 7 * 86400000;
    this.db.prepare("DELETE FROM operations WHERE created_at<?").run(before);
    this.db
      .prepare(
        `DELETE FROM runs WHERE status NOT IN ('queued','running','publishing')
        AND json_extract(document,'$.finishedAt')<?
        AND id NOT IN (SELECT json_extract(document,'$.lastRunId') FROM tasks
          WHERE json_extract(document,'$.lastRunId') IS NOT NULL)`,
      )
      .run(new Date(before).toISOString());
  }
  close() {
    this.db.close();
  }
}
