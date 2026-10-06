import { randomUUID } from "node:crypto";
import {
  canonical,
  IvyError,
  requireThat,
  scopedOperationId,
} from "../../../packages/sdk/src/node.js";
import type {
  RpcClient,
  Wire,
  InvocationContext,
} from "../../../packages/sdk/src/node.js";
import { CollectorStore } from "./store.js";
import type { Run, RecordTask } from "./store.js";
import { effectiveRetention, readSchema, updateSchema } from "./schema.js";
import type { Settings } from "./schema.js";
import { TaskRunner } from "./runner.js";

const json = (value: unknown) => value as Wire.Json;
const resultContent = (run: Run) =>
  json({
    schemaVersion: 1,
    taskId: run.taskId,
    runId: run.id,
    collectedAt: run.finishedAt,
    data: run.output!.data,
    events: run.output!.events ?? [],
  });
const eventContent = (run: Run, eventIndex: number) =>
  json({
    kind: "collected",
    taskId: run.taskId,
    runId: run.id,
    eventIndex,
    ...run.output!.events![eventIndex]!,
  });
function validateEvents(run: Run) {
  // Hive's 4 KiB limit includes the collector envelope, not only the script payload.
  for (let index = 0; index < (run.output!.events?.length ?? 0); index++)
    canonical(eventContent(run, index), 4096);
}
export class CollectorEngine {
  client: RpcClient | null = null;
  rootId = "";
  readonly active = new Map<
    string,
    { controller: AbortController; work: Promise<void> }
  >();
  private ticking = false;
  private closing = false;
  private lastMaintenance = 0;
  private nextRecoveryAt = 0;
  constructor(
    readonly store: CollectorStore,
    readonly runner: TaskRunner,
    readonly settings: Settings,
  ) {}
  private hive() {
    requireThat(
      this.client,
      "service_not_ready",
      "Hive connection is not ready.",
    );
    return this.client;
  }
  private async mutation(label: string) {
    return scopedOperationId(this.hive(), "collector:" + label);
  }
  async attach(client: RpcClient) {
    this.client = client;
    const saved = this.settings.rootObjectId ?? this.store.metadata("rootId");
    if (saved) {
      const root = await client.request("objects.read", { objectId: saved });
      requireThat(
        root.object.contractKey === "data-collector/root" &&
          !root.object.effectivelyArchived,
        "target_conflict",
        "Configured collector root must be an active data-collector/root object.",
      );
      this.rootId = saved;
    } else {
      const found = await client.request("objects.query", {
        contractKey: "data-collector/root",
        where: {
          op: "eq",
          field: "object.name",
          value: this.settings.rootName,
        },
        limit: 2,
      });
      requireThat(
        found.items.length <= 1,
        "target_conflict",
        "Collector root name is ambiguous. Configure rootObjectId.",
      );
      if (found.items[0]) this.rootId = found.items[0].objectId;
      else {
        const result = await client.request("objects.write", {
          mutationId: await this.mutation("root"),
          create: {
            parentId: null,
            ownerObjectId: null,
            name: this.settings.rootName,
            contractKey: "data-collector/root",
          },
          contractVersion: "1.0.0",
          references: {},
          content: { encoding: "json", value: { schemaVersion: 1 } },
        });
        this.rootId = result.object.id;
      }
      this.store.setMetadata("rootId", this.rootId);
    }
  }
  detach() {
    this.client = null;
  }
  private task(id: string) {
    const row = this.store.get(id);
    requireThat(row, "not_found", "Collector task not found.");
    return row;
  }
  private unconfirmed(row: RecordTask | null) {
    return Boolean(
      row?.lastRunId &&
      this.store.run(row.lastRunId)?.error === "outcome_unknown",
    );
  }
  private requireStopped(row: RecordTask | null) {
    requireThat(
      !this.unconfirmed(row),
      "outcome_unknown",
      "Task termination is unconfirmed. Stop and verify its workers on the host before disabling with confirmedStoppedRunId.",
    );
  }
  private visible(row: RecordTask) {
    const { state, deleted, lastRunId, ...visible } = row;
    const run = lastRunId ? this.store.run(lastRunId) : null;
    return {
      ...visible,
      lastRunId,
      status: run?.status ?? "idle",
      error: run?.error ?? null,
      lastRunAt: run?.startedAt ?? null,
      effectiveRetention: effectiveRetention(
        row.task.retention,
        this.settings.retention,
      ),
    };
  }
  async read(args: unknown) {
    const input = readSchema.parse(args);
    if (input.view === "tasks")
      return {
        rootObjectId: this.rootId,
        limits: {
          retention: this.settings.retention,
          concurrency: this.settings.concurrency,
          maximumTasks: this.settings.maximumTasks,
          maximumMemoryMb: this.settings.maximumMemoryMb,
          maximumTimeoutSeconds: this.settings.maximumTimeoutSeconds,
        },
        items: this.store.list().map((row) => ({
          ...this.visible(row),
          task: {
            id: row.task.id,
            name: row.task.name,
            enabled: row.task.enabled,
          },
        })),
      };
    if (input.view === "operation") {
      const run = this.store.run(input.operationId);
      return {
        operation: this.store.operation(input.operationId),
        run: run
          ? {
              id: run.id,
              taskId: run.taskId,
              status: run.status,
              startedAt: run.startedAt,
              finishedAt: run.finishedAt,
              error: run.error,
            }
          : null,
      };
    }
    const row = this.task(input.id);
    if (input.view === "task") return this.visible(row);
    if (!row.resultObjectId) return null;
    if (input.view === "history")
      return this.hive().request("objects.history", {
        objectId: row.resultObjectId,
        limit: 50,
        ...(input.cursor ? { cursor: input.cursor } : {}),
      });
    return this.hive().request("objects.read", {
      objectId: row.resultObjectId,
      ...(input.revision ? { revision: input.revision } : {}),
    });
  }
  update(args: unknown, context?: InvocationContext) {
    const input = updateSchema.parse(args);
    let newlyDisabled = false;
    requireThat(
      !context?.operationId || context.operationId === input.operationId,
      "operation_conflict",
      "Routed and inner operation IDs must match.",
    );
    const result = this.store.mutate(
      input.operationId,
      canonical(json({ caller: context?.callerPrincipalId ?? "local", input })),
      () => {
        if (input.action === "save") {
          const saved = this.store.get(input.task.id, true);
          this.requireStopped(saved);
          const previous = saved?.deleted ? null : saved;
          requireThat(
            (previous?.revision ?? 0) === input.expectedRevision,
            "revision_conflict",
            "Task changed; reload before saving.",
          );
          requireThat(
            !this.store.busy(input.task.id),
            "task_busy",
            "Wait for the current run before changing a task.",
          );
          requireThat(
            previous || this.store.list().length < this.settings.maximumTasks,
            "limit_exceeded",
            "Maximum task count reached.",
          );
          const row: RecordTask = {
            task: input.task,
            revision: (saved?.revision ?? 0) + 1,
            resultObjectId: saved?.resultObjectId ?? null,
            nextAt: Date.now() + input.task.intervalSeconds * 1000,
            state: previous?.state ?? null,
            deleted: false,
            lastRunId: previous?.lastRunId ?? null,
            retentionWarning: null,
          };
          this.store.save(row);
          return this.visible(row);
        }
        const row = this.task(input.id);
        if (input.action !== "disable") this.requireStopped(row);
        if (input.action === "run") {
          requireThat(
            !this.store.busy(input.id),
            "task_busy",
            "Task already has an active or queued run.",
          );
          this.enqueue(row, input.operationId, input.input ?? null);
          return { runId: input.operationId, status: "queued" };
        }
        requireThat(
          row.revision === input.expectedRevision,
          "revision_conflict",
          "Task changed; reload before updating.",
        );
        if (input.action === "delete") {
          requireThat(
            !this.store.busy(input.id),
            "task_busy",
            "Disable and wait for the task before deleting.",
          );
          this.runner.clearAuthentication(input.id);
          row.deleted = true;
          row.revision++;
          row.task = {
            ...row.task,
            enabled: false,
            script: "",
            config: {},
            dependencies: {},
            secretNames: [],
            inputSecretName: null,
          };
          row.state = null;
          row.lastRunId = null;
          this.store.save(row);
          return { deleted: true, resultObjectId: row.resultObjectId };
        }
        if (input.action === "disable" && input.confirmedStoppedRunId) {
          const run = this.store.run(input.confirmedStoppedRunId);
          requireThat(
            run &&
              row.lastRunId === run.id &&
              run.taskId === input.id &&
              run.error === "outcome_unknown",
            "operation_conflict",
            "Confirm the current run whose worker termination is unconfirmed.",
          );
          requireThat(
            !this.store.busy(input.id) && !this.active.has(input.id),
            "task_busy",
            "Wait for the active run before confirming worker termination.",
          );
          run.error =
            "Operator confirmed worker termination; execution outcome remains unknown.";
          this.store.saveRun(run);
        }
        row.task.enabled = input.action === "enable";
        row.revision++;
        row.nextAt = Date.now() + row.task.intervalSeconds * 1000;
        this.store.save(row);
        if (input.action === "disable") {
          newlyDisabled = true;
          for (const run of this.store.runs("queued"))
            if (run.taskId === input.id) {
              run.status = "cancelled";
              run.finishedAt = new Date().toISOString();
              run.error = "Task disabled before execution.";
              this.store.saveRun(run);
            }
        }
        return this.visible(row);
      },
    );
    if (newlyDisabled && input.action === "disable")
      this.active.get(input.id)?.controller.abort();
    return result;
  }
  private enqueue(row: RecordTask, id: string, input: unknown) {
    this.requireStopped(row);
    requireThat(
      !this.store.run(id),
      "operation_conflict",
      "Run ID was already used for another collection.",
    );
    row.lastRunId = id;
    row.nextAt = Date.now() + row.task.intervalSeconds * 1000;
    this.store.save(row);
    this.store.saveRun({
      id,
      taskId: row.task.id,
      task: structuredClone(row.task),
      status: "queued",
      input,
      output: null,
      startedAt: null,
      finishedAt: null,
      eventIndex: 0,
      error: null,
    });
  }
  async tick() {
    if (this.ticking || !this.client || this.closing) return;
    this.ticking = true;
    try {
      await this.recoverWorkers();
      for (const run of this.store.runs("publishing")) {
        try {
          await this.publish(run);
        } catch (error) {
          const failure = IvyError.from(error);
          if (
            failure.outcome === "not_executed" &&
            [
              "content_too_large",
              "invalid_arguments",
              "target_conflict",
            ].includes(failure.code)
          ) {
            run.status = "failed";
            run.error = failure.code;
          } else run.error = "Result publication pending: " + failure.code;
          this.store.saveRun(run);
        }
      }
      for (const row of this.store.list())
        if (
          row.task.enabled &&
          row.task.intervalSeconds > 0 &&
          row.nextAt <= Date.now() &&
          !this.unconfirmed(row) &&
          !this.store.busy(row.task.id)
        ) {
          this.store.transaction(() => this.enqueue(row, randomUUID(), null));
        }
      for (const run of this.store.runs("queued")) {
        if (this.closing || !this.client) break;
        if (this.active.size >= this.settings.concurrency) break;
        if (this.active.has(run.taskId)) continue;
        const controller = new AbortController();
        const work = this.execute(run, controller.signal).finally(() =>
          this.active.delete(run.taskId),
        );
        this.active.set(run.taskId, { controller, work });
      }
      if (Date.now() - this.lastMaintenance > 60000) {
        this.lastMaintenance = Date.now();
        this.store.cleanup();
        for (const row of this.store.list(true))
          if (row.resultObjectId && !this.store.busy(row.task.id))
            await this.retain(row);
      }
    } finally {
      this.ticking = false;
    }
  }
  private async execute(run: Run, signal: AbortSignal) {
    const row = this.task(run.taskId);
    run.status = "running";
    run.startedAt = new Date().toISOString();
    this.store.saveRun(run);
    try {
      run.output = await this.runner.run(
        run.task,
        row.state,
        run.input,
        signal,
        (name) => {
          run.workerJobName = name;
          this.store.saveRun(run);
        },
      );
      signal.throwIfAborted();
      run.finishedAt = new Date().toISOString();
      const size = Buffer.byteLength(canonical(resultContent(run)));
      requireThat(
        size <=
          Math.min(
            1048576,
            effectiveRetention(run.task.retention, this.settings.retention)
              .maximumBytes,
          ),
        "limit_exceeded",
        "Result exceeds the task retained byte budget.",
      );
      validateEvents(run);
      run.status = "publishing";
    } catch (error) {
      run.status = signal.aborted ? "cancelled" : "failed";
      run.finishedAt = new Date().toISOString();
      run.error =
        IvyError.from(error).code === "outcome_unknown"
          ? "outcome_unknown"
          : signal.aborted
            ? "Task was cancelled."
            : IvyError.from(error).code;
    }
    this.store.saveRun(run);
    const current = this.store.get(run.taskId);
    if (current && run.error === "outcome_unknown" && !run.workerJobName) {
      current.task.enabled = false;
      current.revision++;
      this.store.save(current);
    }
  }
  private async recoverWorkers() {
    if (Date.now() < this.nextRecoveryAt) return;
    for (const row of this.store.list()) {
      if (!this.unconfirmed(row) || this.active.has(row.task.id)) continue;
      const run = this.store.run(row.lastRunId!)!;
      if (!run.workerJobName) continue;
      this.nextRecoveryAt = Date.now() + 2000;
      try {
        if (!(await this.runner.stopped(row.task.id, run.workerJobName)))
          continue;
        const current = this.store.get(row.task.id);
        if (
          current?.lastRunId !== run.id ||
          this.active.has(row.task.id) ||
          !this.unconfirmed(current)
        )
          continue;
        this.store.transaction(() => {
          run.error =
            "Worker termination verified; execution outcome remains unknown.";
          this.store.saveRun(run);
          // Never replay the interrupted input; only the next scheduled observation may run.
          current.nextAt = Date.now() + current.task.intervalSeconds * 1000;
          this.store.save(current);
        });
      } catch {
        // A failed observation is retried without assuming that the old tree is stopped.
      }
    }
  }
  private async publish(run: Run) {
    // Pending output survives restarts; validate it before any further Hive effects.
    validateEvents(run);
    const client = this.hive(),
      row = this.task(run.taskId);
    if (!row.resultObjectId) {
      const page = await client.request("objects.query", {
        contractKey: "data-collector/result",
        where: {
          op: "and",
          args: [
            { op: "eq", field: "object.parentId", value: this.rootId },
            { op: "eq", field: "object.name", value: run.taskId },
          ],
        },
        limit: 1,
      });
      row.resultObjectId = page.items[0]?.objectId ?? null;
    }
    const current = row.resultObjectId
      ? await client.request("objects.read", { objectId: row.resultObjectId })
      : null;
    requireThat(
      !current ||
        (current.object.parentId === this.rootId &&
          current.object.contractKey === "data-collector/result" &&
          !current.object.effectivelyArchived),
      "target_conflict",
      "Result object no longer belongs to this collector root.",
    );
    const previous =
      current?.content.encoding === "json"
        ? (current.content.value as Record<string, Wire.Json>)
        : null;
    if (previous?.["runId"] !== run.id) {
      const written = await client.request("objects.write", {
        mutationId: await this.mutation("result:" + run.id),
        contractVersion: "1.0.0",
        references: {},
        ...(current
          ? {
              objectId: current.object.id,
              expectedRevision: current.revision.revision,
            }
          : {
              create: {
                parentId: this.rootId,
                ownerObjectId: null,
                name: run.taskId,
                contractKey: "data-collector/result",
              },
            }),
        content: { encoding: "json", value: resultContent(run) },
      });
      row.resultObjectId = written.object.id;
    }
    const latest = this.task(run.taskId);
    latest.resultObjectId = row.resultObjectId;
    this.store.save(latest);
    const events = run.output!.events ?? [];
    while (run.eventIndex < events.length) {
      await client.request("events.publish", {
        topic: "data-collector.event",
        topicVersion: "1.0.0",
        mutationId: await this.mutation(
          "event:" + run.id + ":" + run.eventIndex,
        ),
        payload: eventContent(run, run.eventIndex),
      });
      // Event consumers deduplicate by runId/eventIndex across a Hive epoch change.
      run.eventIndex++;
      this.store.saveRun(run);
    }
    const final = this.task(run.taskId);
    if (Object.hasOwn(run.output!, "state")) final.state = run.output!.state;
    final.nextAt = Date.now() + final.task.intervalSeconds * 1000;
    this.store.transaction(() => {
      this.store.save(final);
      this.store.saveRun({ ...run, status: "succeeded", error: null });
    });
    await this.retain(final);
  }
  private async retain(row: RecordTask) {
    if (!row.resultObjectId) return;
    const retention = effectiveRetention(
      row.task.retention,
      this.settings.retention,
    );
    let warning: string | null = null;
    try {
      const object = await this.hive().request("objects.stat", {
        objectId: row.resultObjectId,
      });
      const result = await this.hive().request("objects.pruneRevisions", {
        objectId: row.resultObjectId,
        expectedRevision: object.currentRevision,
        mutationId: await this.mutation("prune:" + randomUUID()),
        ...retention,
      });
      warning = result.protected
        ? "Referenced revisions are protected from retention."
        : result.remainingBytes > retention.maximumBytes
          ? "Retained history exceeds the byte budget; cleanup may still be pending."
          : null;
    } catch (error) {
      warning = "Retention pending: " + IvyError.from(error).code;
    }
    const latest = this.store.get(row.task.id, true)!;
    latest.retentionWarning = warning;
    this.store.save(latest);
  }
  async close() {
    this.closing = true;
    for (const run of this.active.values()) run.controller.abort();
    await Promise.allSettled([...this.active.values()].map((run) => run.work));
  }
}
