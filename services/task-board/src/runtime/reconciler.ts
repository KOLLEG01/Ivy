import { IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import type { TaskBoard, Wire } from "../../../../packages/sdk/src/node.js";
import type { TaskBoardEngine } from "./engine.js";
import { TaskBoardNativeOrchestrator } from "./native-orchestrator.js";
import { TaskBoardScheduler } from "./scheduler.js";
import { TaskBoardNativeSignals } from "./native-signals.js";
import { TaskBoardDeliveries } from "./deliveries.js";
import { syncNativeComments } from './native-comments.js';

type Lane = TaskBoard.ScanCursor["lane"];
const lanes: Lane[] = ["operations", "runs", "tasks", "deliveries"];
const queries = {
  operations: {
    key: "task-board/operation",
    where: {
      op: "in",
      field: "data:/phase",
      value: ["accepted", "preparing", "publishing", "needs_attention"],
    },
  },
  runs: {
    key: "task-board/run",
    where: {
      op: "in",
      field: "data:/phase",
      value: [
        "starting",
        "running",
        "waiting_input",
        "publishing_result",
        "cancel_requested",
        "outcome_unknown",
      ],
    },
  },
  tasks: {
    key: "task-board/task",
    where: {
      op: "and",
      args: [
        { op: "eq", field: "data:/fields/control", value: "agent" },
        { op: "in", field: "data:/workflowState", value: ["todo", "waiting", "review"] },
      ],
    },
  },
  deliveries: {
    key: "task-board/delivery",
    where: {
      op: "in",
      field: "data:/state",
      value: ["pending", "dispatching", "outcome_unknown"],
    },
  },
} satisfies Record<
  Lane,
  {
    key:
      | "task-board/operation"
      | "task-board/run"
      | "task-board/task"
      | "task-board/delivery";
    where: Wire.QueryPredicate;
  }
>;

/** Fair bounded scans. Durable Operations and calls make repeated crash-interrupted pages safe. */
export class TaskBoardReconciler {
  readonly native: TaskBoardNativeOrchestrator;
  readonly scheduler: TaskBoardScheduler;
  readonly signals: TaskBoardNativeSignals;
  private recovering: "operations" | "runs" | null = "operations";
  private recoveryScanned = false;
  private lane = 0;
  private taskChanges = 0;
  private busy = false;
  private lastCleanupAt = 0;
  private gateCursor: string | null = null;
  private stopped = false;
  private readonly activeTasks = new Map<string, Promise<void>>();
  private readonly pages = new Map<Lane, { ids: string[]; next: string | null; taskChanges: number }>();
  private readonly cursors = new Map<Lane, string | null>();
  private readonly failures = new Map<string, Wire.Diagnostic>();
  constructor(readonly engine: TaskBoardEngine) {
    this.native = new TaskBoardNativeOrchestrator(engine);
    this.scheduler = new TaskBoardScheduler(engine);
    this.signals = new TaskBoardNativeSignals(engine);
  }
  get recovered(): boolean {
    return this.recovering === null;
  }
  taskChanged(): void {
    this.taskChanges++;
    this.cursors.delete("tasks");
    this.pages.delete("tasks");
  }
  /** Drain already admitted work before its store is closed or its owner replaced. */
  async settled(): Promise<void> { await Promise.all(this.activeTasks.values()); }
  async stop(): Promise<void> { this.stopped = true; await this.settled(); }
  get diagnostics(): Wire.Diagnostic[] {
    return [...this.failures.values()];
  }
  private record(key: string, error?: unknown) {
    if (!error) {
      this.failures.delete(key);
      return;
    }
    const failure = IvyError.from(error),
      now = new Date().toISOString(),
      old = this.failures.get(key);
    if (!old && this.failures.size >= 32)
      this.failures.delete(this.failures.keys().next().value!);
    this.failures.set(key, {
      code: failure.code,
      resource: { objectId: key },
      source: this.engine.owner.serviceNodeId,
      severity: "error",
      status: "current",
      firstObservedAt: old?.firstObservedAt ?? now,
      lastObservedAt: now,
      message: failure.message.slice(0, 2048),
    });
  }
  private async process(lane: Lane, id: string) {
    try {
      if (lane === "operations") await this.engine.recoverOperation(id);
      else if (lane === "deliveries")
        await new TaskBoardDeliveries(this.engine).step(id);
      else if (lane === "runs") {
        // Input mirroring must not prevent native recovery when a host is offline.
        try { await syncNativeComments(this.engine, id); this.record('inputs:' + id); }
        catch (error) { this.record('inputs:' + id, error); }
        // Backoff is not a successful recovery and must not clear its diagnostic.
        if (!await this.native.drain(id)) return;
        try {
          await this.signals.step(id);
          this.record("signals:" + id);
        } catch (error) {
          this.record("signals:" + id, error);
        }
      } else {
        requireThat(
          this.recovered,
          "task_board_recovery_pending",
          "Complete startup recovery before new automatic claims.",
        );
        await this.scheduler.task(id);
      }
      this.record(id);
    } catch (error) {
      // Conflicting current revisions are retried from fresh state on a later pass; they do not
      // authorize another mutation or prevent progress past unrelated blocked rows.
      if (
        error instanceof IvyError &&
        ["revision_conflict", "task_board_publication_busy"].includes(error.code)
      )
        return;
      this.record(id, error);
    }
  }
  private async taskKey(lane: Lane, id: string): Promise<string> {
    if (lane === "tasks") return id;
    if (lane === "runs") return (await this.engine.store.read("task-board/run", id)).value.taskId;
    if (lane === "deliveries") return (await this.engine.store.read("task-board/delivery", id)).value.taskId;
    const operation = await this.engine.store.read("task-board/operation", id);
    return "taskId" in operation.value.request ? operation.value.request.taskId : "shared";
  }
  private startTask(key: string, work: () => Promise<void>): void {
    const current = Promise.resolve().then(work).catch(error => this.record(key, error))
      .finally(() => { this.activeTasks.delete(key); });
    this.activeTasks.set(key, current);
  }
  private async recoverGates(): Promise<void> {
    let gates = this.engine.store.pendingCoordinationPage(this.gateCursor, 10);
    if (!gates.length && this.gateCursor) {
      this.gateCursor = null;
      gates = this.engine.store.pendingCoordinationPage(null, 10);
    }
    for (const gate of gates) {
      if (this.stopped) return;
      const id = gate.operationId;
      try {
        const key = await this.taskKey("operations", id);
        if (this.stopped) return;
        if (!this.activeTasks.has(key)) {
          if (this.activeTasks.size >= 10) return;
          this.startTask(key, async () => {
            try { await this.engine.recoverOperation(id); this.record("publication:" + id); }
            catch (error) { this.record("publication:" + id, error); }
          });
        }
        this.gateCursor = gate.gateId;
      } catch (error) {
        this.record("publication:" + id, error);
      }
    }
  }
  private async scan(lane: Lane): Promise<boolean> {
    const taskChanges = this.taskChanges;
    const store = this.engine.store,
      query = queries[lane],
      limit = this.engine.settings.scheduler.pageSize;
    const orderBy: Wire.ObjectQuery["orderBy"] =
      lane === "tasks"
        ? [
            { field: "data:/fields/priority", direction: "desc" },
            { field: "object.id", direction: "asc" },
          ]
        : [{ field: "object.id", direction: "asc" }];
    const cursor = this.cursors.get(lane) ?? null;
    const readPage = (cursor: string | null) =>
      store.page(query.key, {
        where: query.where,
        limit,
        orderBy,
        ...(cursor ? { cursor } : {}),
      });
    let pending = this.pages.get(lane);
    if (!pending) {
      let page;
      try {
        page = await readPage(cursor);
      } catch (error) {
        if (!(error instanceof IvyError && error.code === "invalid_cursor" && cursor)) throw error;
        page = await readPage(null);
      }
      requireThat(!page.nextCursor || page.nextCursor !== cursor,
        "task_board_cursor_regression", "A nonempty scan must advance its query cursor.");
      pending = { ids: page.items.map(item => item.objectId), next: page.nextCursor, taskChanges };
      this.pages.set(lane, pending);
    }
    const count = pending.ids.length;
    for (let index = 0; index < count && !this.stopped; index++) {
      const id = pending.ids[0]!;
      const key = await this.taskKey(lane, id).catch(() => id);
      if (this.stopped) return false;
      // A busy Task is revisited on the next pass; other Tasks keep their slots.
      const active = this.activeTasks.has(key);
      if (!active) {
        if (this.activeTasks.size >= 10) return false;
        this.startTask(key, () => this.process(lane, id));
      }
      pending.ids.shift();
      if (active && this.recovering === lane) pending.ids.push(id);
    }
    if (this.stopped || pending.ids.length) return false;
    if (this.pages.get(lane) === pending) this.pages.delete(lane);
    if (lane !== "tasks" || pending.taskChanges === this.taskChanges)
      this.cursors.set(lane, pending.next);
    return pending.next === null;
  }
  async tick(): Promise<void> {
    if (this.busy || this.stopped) return;
    this.busy = true;
    try {
      await this.engine.verifyOwner();
      if (Date.now() - this.lastCleanupAt >= 60_000) {
        this.lastCleanupAt = Date.now();
        try {
          await this.engine.store.collectLocal();
          this.record("cleanup");
        } catch (error) {
          this.record("cleanup", error);
        }
        try {
          const usage = this.engine.store.localUsage();
          if (usage.freeBytes !== null && usage.freeBytes < 1024 * 1024 * 1024)
            this.record("storage", new IvyError("task_board_storage_low", "Less than 1 GiB remains on the TaskBoard journal filesystem."));
          else this.record("storage");
        } catch (error) {
          this.record("storage", error);
        }
      }
      this.record("publication");
      if (this.recovering) {
        if (!this.recoveryScanned) this.recoveryScanned = await this.scan(this.recovering);
        await this.recoverGates();
        if (this.recoveryScanned && this.activeTasks.size === 0) {
          this.recovering = this.recovering === "operations" ? "runs" : null;
          this.recoveryScanned = false;
        }
      } else {
        const lane = lanes[this.lane]!;
        const results = await Promise.allSettled([this.scan(lane), this.recoverGates()]);
        for (const result of results) if (result.status === "rejected") throw result.reason;
        this.lane = (this.lane + 1) % lanes.length;
      }
    } catch (error) {
      this.record("publication", error);
    } finally {
      this.busy = false;
    }
  }
}
