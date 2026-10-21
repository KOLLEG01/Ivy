import { IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import type { TaskBoard, Transport, Wire } from "../../../../packages/sdk/src/node.js";
import type { TaskBoardEngine } from "./engine.js";
import { TaskBoardNativeOrchestrator } from "./native-orchestrator.js";
import { TaskBoardScheduler } from "./scheduler.js";
import { TaskBoardNativeSignals } from "./native-signals.js";
import { TaskBoardDeliveries } from "./deliveries.js";
import { syncNativeComments } from './native-comments.js';
import { finishedRun } from './native-publication.js';
import { routeTaskQuestions } from './question-routing.js';
import { missingWakeTrigger, wakeCondition } from './wake.js';
import { adoptDirectTurn } from './native-adoption.js';
import { nativeRecord, nativeString } from './native-intent.js';

type Lane = TaskBoard.ScanCursor["lane"];
const lanes: Lane[] = ["operations", "runs", "tasks", "deliveries"];
const maxConcurrentTasks = 20;
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

        { op: "in", field: "data:/workflowState", value: ["backlog", "todo", "in_progress", "waiting", "review", "done", "cancelled"] },
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
  private commentsCompacted = false;
  private commentRetryAt = 0;
  private revisionProtectionAt = Date.now() + 15 * 60_000;
  private readonly activeTasks = new Map<string, Promise<void>>();
  private readonly changedTasks = new Set<string>();
  private readonly primaries = new Map<string, { serviceNodeId: string; threadId: string }>();
  private readonly nativeChecks = new Map<string, { runId: string | null; after: number }>();
  private readonly pages = new Map<Lane, { ids: string[]; next: string | null; taskChanges: number }>();
  private readonly cursors = new Map<Lane, string | null>();
  private readonly failures = new Map<string, Wire.Diagnostic>();
  private readonly failedRuns = new Set<string>();
  private readonly operationRetries = new Map<string, { after: number; identity: Pick<TaskBoard.Operation, 'callerPrincipalId' | 'operationId'> | null }>();
  constructor(readonly engine: TaskBoardEngine) {
    this.native = new TaskBoardNativeOrchestrator(engine);
    this.scheduler = new TaskBoardScheduler(engine);
    this.signals = new TaskBoardNativeSignals(engine);
    engine.onTaskChanged = id => this.taskChanged(id);
    engine.runIssue = id => {
      const failure = this.failures.get(id);
      return failure ? failure.code + ': ' + failure.message : undefined;
    };
  }
  get recovered(): boolean {
    return this.recovering === null;
  }
  notification(value: Transport.ProviderNotification): boolean {
    let relevant = this.native.notification(value);
    const event = nativeRecord(value.params.payload), body = nativeRecord(event['params']);
    if (['turn/started', 'turn/completed', 'thread/status/changed'].includes(nativeString(event['method']))) {
      for (const [taskId, primary] of this.primaries) {
        if (primary.serviceNodeId === value.params.serviceNodeId && primary.threadId === body['threadId']) {
          this.taskChanged(taskId); relevant = true;
        }
      }
    }
    return relevant;
  }
  taskChanged(taskId?: string): void {
    this.taskChanges++;
    this.cursors.delete("tasks");
    this.pages.delete("tasks");
    if (taskId && !this.stopped) {
      this.nativeChecks.delete(taskId);
      // Coalesce bursts per ticket; the durable scan covers overflow and restarts.
      if (this.changedTasks.size < 1024) this.changedTasks.add(taskId);
      this.dispatchChanged();
    }
  }
  /** Drain already admitted work before its store is closed or its owner replaced. */
  async settled(): Promise<void> {
    while (this.activeTasks.size) await Promise.all(this.activeTasks.values());
  }
  async stop(): Promise<void> { this.stopped = true; await this.settled(); }
  get diagnostics(): Wire.Diagnostic[] {
    return [...this.failures.values(), ...this.native.progress.diagnostics];
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
      if (lane === "operations") {
        await this.recoverOperation(id);
        this.record('publication:' + id);
      }
      else if (lane === "deliveries")
        await new TaskBoardDeliveries(this.engine).step(id);
      else if (lane === "runs") {
        this.failedRuns.add(id);
        if ([id, 'inputs:' + id, 'signals:' + id].some(key => this.failures.has(key)) &&
            finishedRun((await this.engine.store.read('task-board/run', id, true, true)).value)) {
          this.record(id); this.record('inputs:' + id); this.record('signals:' + id);
          this.failedRuns.delete(id); return;
        }
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
        await routeTaskQuestions(this.engine, id);
        const before = await this.engine.store.read('task-board/task', id, true, true, true);
        if (before.metadata.effectivelyArchived) { this.primaries.delete(id); this.nativeChecks.delete(id); return; }
        const primary = before.value.primaryResourceRef;
        if (primary && before.value.fields.control === 'agent') {
          if (this.primaries.size >= 1024 && !this.primaries.has(id)) {
            const oldest = this.primaries.keys().next().value!;
            this.primaries.delete(oldest); this.nativeChecks.delete(oldest);
          }
          this.primaries.set(id, { serviceNodeId: primary.serviceNodeId, threadId: primary.nativeId });
          const checked = this.nativeChecks.get(id), runId = before.value.lastRun?.objectId ?? null;
          // Notifications trigger an immediate check; bounded polling repairs lost
          // notifications and reconnects without repeatedly reading idle history.
          if (!before.value.claim && (!checked || checked.runId !== runId || checked.after <= Date.now() ||
              before.value.workflowState === 'todo' || before.value.commentDeliveries.some(value => value.state === 'queued'))) {
            const adopted = await adoptDirectTurn(this.engine, id);
            this.nativeChecks.set(id, { runId: adopted?.run?.objectId ?? runId, after: Date.now() + 30_000 });
            if (adopted?.run) await this.process('runs', adopted.run.objectId);
          }
        } else { this.primaries.delete(id); this.nativeChecks.delete(id); }
        await this.scheduler.task(id);
        const task = await this.engine.store.read('task-board/task', id);
        if (task.value.completionIssue) {
          const issue = this.engine.store.failedHandoff(task.value.completionIssue.run.objectId) ?? task.value.completionIssue;
          this.record(id, new IvyError(issue.code, task.value.taskKey + ': ' + issue.detail));
          return;
        }
        if (missingWakeTrigger(task.value)) {
          this.record(id, new IvyError('task_board_missing_wake_trigger', task.value.taskKey + ': ' + wakeCondition(task.value).detail));
          return;
        }
      }
      this.record(id);
      if (lane === 'runs' && !['inputs:' + id, 'signals:' + id].some(key => this.failures.has(key))) this.failedRuns.delete(id);
    } catch (error) {
      if (lane === 'operations') { await this.publicationFailure(id, error, id); return; }
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
      .finally(() => { this.activeTasks.delete(key); this.dispatchChanged(); });
    this.activeTasks.set(key, current);
  }
  private dispatchChanged(): void {
    if (this.stopped || !this.recovered) return;
    for (const id of this.changedTasks) {
      if (this.activeTasks.size >= maxConcurrentTasks) return;
      if (this.activeTasks.has(id)) continue;
      this.changedTasks.delete(id);
      this.startTask(id, async () => {
        let task = await this.engine.store.read('task-board/task', id, true, true);
        if (task.metadata.effectivelyArchived) return;
        const previousRun = task.value.claim?.run?.objectId;
        if (previousRun) await this.process('runs', previousRun);
        await this.process('tasks', id);
        task = await this.engine.store.read('task-board/task', id, true, true);
        const nextRun = task.value.claim?.run?.objectId;
        if (nextRun && nextRun !== previousRun) await this.process('runs', nextRun);
      });
    }
  }
  private async recoverOperation(id: string): Promise<void> {
    try {
      await this.engine.recoverOperation(id);
      this.clearOperationFailure(id);
    } catch (error) {
      const original = await this.engine.store.read('task-board/operation', id).catch(() => null);
      this.operationRetries.set(id, { after: Date.now() + 30_000, identity: original
        ? { callerPrincipalId: original.value.callerPrincipalId, operationId: original.value.operationId }
        : this.operationRetries.get(id)?.identity ?? null });
      throw error;
    }
  }
  private clearOperationFailure(id: string): void {
    this.operationRetries.delete(id);
    this.record(id); this.record('publication:' + id);
  }
  private async publicationFailure(id: string, error: unknown, key = 'publication:' + id): Promise<void> {
    const failure = IvyError.from(error);
    if (failure.outcome === 'not_executed') {
      const original = await this.engine.store.read('task-board/operation', id).catch(() => null);
      // A terminal refusal remains action history, without an outstanding recovery fault.
      if (original?.value.phase === 'failed' && original.value.error?.execution === 'not_executed' &&
          original.value.error.code === failure.code && original.value.error.message === failure.message) {
        this.clearOperationFailure(id); return;
      }
    }
    this.record(key, error);
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
        if (!this.activeTasks.has(key) && (this.operationRetries.get(id)?.after ?? 0) <= Date.now()) {
          if (this.activeTasks.size >= maxConcurrentTasks) return;
          this.startTask(key, async () => {
            try { await this.recoverOperation(id); this.record(id); this.record("publication:" + id); }
            catch (error) { await this.publicationFailure(id, error); }
          });
        }
        this.gateCursor = gate.gateId;
      } catch (error) {
        await this.publicationFailure(id, error);
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
      const ids = new Set(page.items.map(item => item.objectId));
      if (lane === 'runs') for (const id of this.failedRuns) ids.add(id);
      if (lane === 'operations') for (const [id, retry] of this.operationRetries) {
        if (retry.after > Date.now()) continue;
        // An original action can finish outside this scan and disappear from its
        // nonterminal query. Confirm its cleanup, or its durable eviction receipt.
        if (!store.localKey(id) && retry.identity && store.retiredOperation(retry.identity.callerPrincipalId, retry.identity.operationId)) {
          this.clearOperationFailure(id); continue;
        }
        ids.add(id);
      }
      pending = { ids: [...ids], next: page.nextCursor, taskChanges };
      this.pages.set(lane, pending);
    }
    const count = pending.ids.length;
    for (let index = 0; index < count && !this.stopped; index++) {
      const id = pending.ids[0]!;
      const key = await this.taskKey(lane, id).catch(() => id);
      if (this.stopped) return false;
      // A busy Task is revisited on the next pass; other Tasks keep their slots.
      const active = this.activeTasks.has(key);
      if (!active && (lane !== "operations" || (this.operationRetries.get(id)?.after ?? 0) <= Date.now())) {
        if (this.activeTasks.size >= maxConcurrentTasks) return false;
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
      this.dispatchChanged();
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
          this.dispatchChanged();
        }
      } else {
        const lane = lanes[this.lane]!;
        const results = await Promise.allSettled([this.scan(lane), this.recoverGates()]);
        for (const result of results) if (result.status === "rejected") throw result.reason;
        this.lane = (this.lane + 1) % lanes.length;
        if (Date.now() >= this.revisionProtectionAt && !this.activeTasks.has('revision-protection') &&
            this.activeTasks.size < maxConcurrentTasks) {
          this.revisionProtectionAt = Date.now() + 15 * 60_000;
          this.startTask('revision-protection', async () => {
            try { await this.engine.store.protectRunSources(); this.record('revision-protection'); }
            catch (error) { this.revisionProtectionAt = Date.now() + 60_000; this.record('revision-protection', error); }
          });
        }
        if (!this.commentsCompacted && Date.now() >= this.commentRetryAt &&
            !this.activeTasks.has('comment-storage') && this.activeTasks.size < maxConcurrentTasks) {
          this.startTask('comment-storage', async () => {
            try { this.commentsCompacted = await this.engine.store.compactComments(2); this.record('comment-storage'); }
            catch (error) { this.commentRetryAt = Date.now() + 60_000; this.record('comment-storage', error); }
          });
        }
      }
    } catch (error) {
      this.record("publication", error);
    } finally {
      this.busy = false;
    }
  }
}
