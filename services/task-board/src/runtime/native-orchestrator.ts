import { hashJson } from "../../../../packages/sdk/src/node.js";
import { IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import type { TaskBoard, Transport } from "../../../../packages/sdk/src/node.js";
import { nativeTurnItems } from "../../../../packages/sdk/src/native-observations.js";
import type { TaskBoardEngine } from "./engine.js";
import { same } from "./checks.js";
import { TaskBoardNativeDriver } from "./native-driver.js";
import { TaskBoardNativeInspector } from "./native-inspector.js";
import { catalogFor } from "./evidence.js";
import { runHandoff } from "./conversation.js";
import { finalOutputText, outputParams, pageCursor, TaskBoardTurnEvidence } from "./turn-evidence.js";
import { nativeArray, nativeRecord, nativeString } from "./native-intent.js";
import type { NativeSlot } from "./native-intent.js";
import { finishedRun, projectNativeState } from "./native-publication.js";
import { claimedWorkflowState } from "./condition.js";
import { mutation } from "./store.js";
import type { Document } from "./store.js";
import { syncNativeComments } from './native-comments.js';
import { peerState } from './task-coordination.js';

/** One bounded reconciliation pass. Persisted calls and evidence authorize every next action. */
export class TaskBoardNativeOrchestrator {
  readonly driver: TaskBoardNativeDriver;
  readonly inspector: TaskBoardNativeInspector;
  readonly evidence: TaskBoardTurnEvidence;
  private readonly threads = new Map<
    string,
    { serviceNodeId: string; threadId: string }
  >();
  private readonly retries = new Map<string, { count: number; after: number }>();
  notification(value: Transport.ProviderNotification): boolean {
    const params = value.params,
      event = nativeRecord(params.payload),
      body = nativeRecord(event["params"]);
    const relevant = [
        "turn/completed",
        "thread/status/changed",
        "serverRequest/resolved",
      ].includes(nativeString(event["method"]));
    if (!relevant) return false;
    let matched = false;
    for (const [runId, thread] of this.threads)
      if (
          thread.serviceNodeId === params.serviceNodeId &&
          thread.threadId === body["threadId"]
      ) { this.retries.delete(runId); matched = true; }
    return matched;
  }
  constructor(readonly engine: TaskBoardEngine) {
    this.driver = new TaskBoardNativeDriver(engine);
    this.inspector = new TaskBoardNativeInspector(engine);
    this.evidence = new TaskBoardTurnEvidence(engine.store, engine.settings.principalId);
  }
  private async finalOutput(run: Document<"task-board/run">, snapshot: TaskBoard.Artifact) {
    const context = await this.inspector.context(run.pin), visited = new Set<string | null>();
    let cursor: string | null = null;
    // A handoff needs at most the last few items, never a complete output archive.
    for (let count = 0; count < 3; count++) {
      requireThat(!visited.has(cursor), 'task_board_native_cursor_loop', 'The native output search repeated a cursor.');
      visited.add(cursor);
      const observed = await this.inspector.read(context, 'thread/items/list', outputParams(context.threadId, context.turnId, cursor), true);
      if ('error' in observed.reply && observed.reply.error.code === -32601) return null;
      requireThat('result' in observed.reply, 'task_board_native_read_failed', 'The final output read returned its original error.');
      const items = nativeTurnItems(catalogFor(context.nativeVersion, { catalogHash: observed.catalogHash }).contract, observed.reply.result, context.turnId);
      requireThat(items.length <= 1, 'task_board_native_page_limit', 'The final output read exceeds its one-item bound.');
      if (items.length && finalOutputText(items[0]!)) return this.evidence.saveOutput(run.pin, snapshot, cursor, observed);
      cursor = pageCursor(nativeRecord(observed.reply.result)['nextCursor']);
      if (cursor === null) return null;
    }
    return null;
  }
  private update(
    run: Document<"task-board/run">,
    task: Document<"task-board/task">,
    change: TaskBoard.NativeUpdateRequest["change"],
  ) {
    return this.engine.nativeUpdate({
      action: "nativeUpdate",
      operationId: mutation(
        run.pin.objectId,
        "reconcile:" + hashJson({ run: run.pin, task: task.pin, change }),
      ),
      taskId: task.pin.objectId,
      expectedRevision: task.pin.revision,
      run: run.pin,
      change,
    });
  }
  private async unresolved(run: Document<"task-board/run">, slot: NativeSlot) {
    const pin = run.value.calls[slot];
    if (!pin) return false;
    const latest = await this.engine.store.read(
      "task-board/native-call",
      pin.objectId,
    );
    return (
      latest.pin.revision > pin.revision ||
      !["succeeded", "failed"].includes(latest.value.ownerPhase ?? "")
    );
  }
  private postpone(runId: string): void {
    const count = Math.min((this.retries.get(runId)?.count ?? 0) + 1, 5);
    if (this.retries.size >= 1024 && !this.retries.has(runId))
      this.retries.delete(this.retries.keys().next().value!);
    this.retries.set(runId, { count, after: Date.now() + Math.min(300_000, 30_000 * 2 ** (count - 1)) });
  }
  async drain(runId: string): Promise<boolean> {
    if ((this.retries.get(runId)?.after ?? 0) > Date.now()) return false;
    await this.engine.store.withReadCache(async () => {
      // Drain immediately progressing starts, cancellation and result publication. Pending work yields.
      for (let count = 0; count < 8; count++) {
        const before = await this.engine.store.read("task-board/run", runId);
        try { await this.step(runId); }
        catch (error) { this.postpone(runId); throw error; }
        const after = await this.engine.store.read("task-board/run", runId);
        if (finishedRun(after.value)) { this.threads.delete(runId); this.retries.delete(runId); }
        else if (after.value.primaryResourceRef) {
          if (this.threads.size >= 1024)
            this.threads.delete(this.threads.keys().next().value!);
          this.threads.set(runId, {
            serviceNodeId: after.value.target.serviceNodeId,
            threadId: after.value.primaryResourceRef.nativeId,
          });
        }
        const waiting = after.value.phase === "starting"
          ? (await this.engine.store.read("task-board/task", after.value.taskId)).value.waiting : null;
        if (after.value.phase === "outcome_unknown" || waiting?.reason === "host")
          this.postpone(runId);
        else this.retries.delete(runId);
        if (
          !["starting", "publishing_result", "cancel_requested"].includes(
            after.value.phase,
          ) ||
          same(before.pin, after.pin)
        )
          return;
      }
    });
    return true;
  }
  async step(runId: string): Promise<void> {
    await this.engine.verifyOwner();
    const store = this.engine.store,
      run = await store.read("task-board/run", runId);
    if (finishedRun(run.value)) return;
    const task = await store.read("task-board/task", run.value.taskId),
      active = task.value.claim?.run?.objectId === runId;
    requireThat(
      !task.value.publication &&
        (!active || same(task.value.claim!.run, run.pin)),
      "revision_conflict",
      "Reconciliation requires current Task/Run publication state.",
    );
    try {
      if (!run.value.turnId) {
        for (const slot of ["thread", "turn"] as const)
          if (await this.unresolved(run, slot)) {
            try {
              if (slot === "thread" && await this.driver.recoverUncertainResume(runId)) return;
              await this.driver.advance(runId, slot);
            } catch (error) {
              if (!(
                error instanceof IvyError &&
                error.code === "task_board_reattach_required" &&
                slot === "turn" &&
                active &&
                !run.value.cancellation &&
                run.value.calls.thread
              ))
                throw error;
              const predecessor = await store.read(
                  "task-board/native-call",
                  run.value.calls.thread,
                ),
                status = await this.driver.status(run.value, predecessor.value);
              await this.driver.prepare(runId, "thread", {
                kind: "reattach",
                predecessor: predecessor.pin,
                preparedEpoch: status.epoch!,
              });
            }
            return;
          }
        if (run.value.cancellation || !active) {
          if (!run.value.calls.turn)
            await this.update(run, task, {
              kind: run.value.cancellation
                ? "cancelUnstarted"
                : "retireUnstarted",
            });
          return;
        }
        // A continuation inherits a primary identity, but needs its own retained thread/resume
        // receipt and epoch before preparing a new turn. The primary alone proves no attachment.
        await this.driver.prepare(
          runId,
          run.value.calls.thread ? "turn" : "thread",
        );
        return;
      }
      // Resolve the retained operation even if the target turn ended meanwhile. An unknown
      // receipt must never cause the same comment to be sent again in a successor turn.
      if (await this.unresolved(run, 'steer')) {
        await this.driver.advance(runId, 'steer');
        return;
      }
      const inspection = await this.inspector.find(run.pin);
      if (inspection.kind === "pending") return;
      requireThat(
        inspection.kind === "found",
        "task_board_turn_unavailable",
        "The original native turn is currently unavailable; its identity and outcome remain unresolved.",
      );
      requireThat(
        "result" in inspection.turns.reply &&
          "result" in inspection.metadata.reply,
        "task_board_native_read_failed",
        "Successful native read bodies are required.",
      );
      const turn = nativeArray(
        nativeRecord(inspection.turns.reply.result)["data"],
      )
        .map(nativeRecord)
        .find((value) => value["id"] === run.value.turnId)!;
      const status = nativeString(turn["status"]),
        thread = nativeRecord(
          nativeRecord(inspection.metadata.reply.result)["thread"],
        );
      if (status === "inProgress" && inspection.turns.epoch !== run.value.nativeEpoch &&
          await this.inspector.ownerStopped(run.value)) {
        await this.update(run, task, {
          kind: "unavailable", reason: "external_outcome", code: "native_process_lost",
          detail: "The original native process tree stopped; the saved unfinished turn cannot continue in the replacement owner.",
        });
        return;
      }
      if (
        status === "inProgress" &&
        run.value.cancellation &&
        inspection.turns.epoch === run.value.nativeEpoch &&
        nativeRecord(thread["status"])["type"] === "active"
      ) {
        if (!run.value.calls.interrupt) {
          await this.driver.prepare(runId, "interrupt");
          return;
        }
        if (await this.unresolved(run, "interrupt")) {
          if (await this.driver.advance(runId, "interrupt")) return;
        }
      }
      let snapshot: TaskBoard.Artifact | null = null;
      const prior = run.value.externalOutcome.evidence;
      if (status !== "inProgress" && prior) {
        if (
          store.localKey(prior.object.objectId) ===
          "task-board/native-turn-snapshot"
        ) {
          const saved = await this.evidence.snapshot(run.pin, prior);
          if (
            saved.document.value.status === status &&
            saved.document.value.epoch === inspection.turns.epoch
          )
            snapshot = prior;
        }
      }
      if (status === "inProgress") {
        if (active && !run.value.cancellation && inspection.turns.epoch === run.value.nativeEpoch) {
          const previous = run.value.calls.steer ? (await store.read('task-board/native-call', run.value.calls.steer)).value.origin : null;
          const initial = (await store.read('task-board/task', { objectId: run.value.taskId, revision: run.value.originalRequest.expectedRevision })).value;
          const peersHash = await peerState(store, task.value, task.pin.objectId);
          const commentIds = task.value.commentDeliveries.filter(value => value.state === 'queued' && !run.value.originalRequest.commentIds.includes(value.commentId)).map(value => value.commentId);
          const baseline = previous?.kind === 'steer' ? previous : { workRevision: initial.workRevision, peersHash: hashJson([]), commentIds: [] as string[] };
          if (task.value.workRevision !== baseline.workRevision || peersHash !== baseline.peersHash || commentIds.some(id => !baseline.commentIds.includes(id))) {
            await this.driver.prepare(runId, 'steer', { kind: 'steer', commentIds, workRevision: task.value.workRevision, peersHash, predecessor: run.value.calls.steer ?? null });
            return;
          }
        }
        const projection = projectNativeState(
          run.value,
          status,
          inspection.turns.epoch,
          thread,
          inspection.turns.observedAt,
          prior ?? {
            object: run.pin,
            label: "Current Run",
            contentHash: run.revision.contentHash,
            mediaType: "application/json",
          },
        );
        const waiting = active ? task.value.waiting : null;
        const projectedWaiting = claimedWorkflowState(projection.waiting) === 'waiting' ? projection.waiting : null;
        if (
          projection.phase === run.value.phase &&
          projection.externalOutcome.state ===
            run.value.externalOutcome.state &&
          projection.externalOutcome.code === run.value.externalOutcome.code &&
          (!active ||
            (projectedWaiting?.reason === waiting?.reason &&
              projectedWaiting?.detail === waiting?.detail &&
              task.value.workflowState === claimedWorkflowState(projection.waiting)))
        )
          return;
      }
      if (!snapshot) {
        snapshot = await this.evidence.saveSnapshot(
          run.pin,
          inspection.metadata,
          inspection.turns,
        );
        await this.update(run, task, {
          kind: "readSnapshot",
          snapshot,
          notificationCursor: run.value.notificationCursor,
        });
        return;
      }
      // Refresh late answers before deciding whether the handoff still needs input.
      if (active && status === 'completed') await syncNativeComments(this.engine, runId);
      const currentTask = await store.read('task-board/task', task.pin.objectId);
      const output = runHandoff(currentTask.value, runId) ? null : await this.finalOutput(run, snapshot);
      await this.update(run, await store.read('task-board/task', task.pin.objectId), {
        kind: "completion", snapshot, output, notificationCursor: run.value.notificationCursor,
      });
    } catch (error) {
      if (!(
        error instanceof IvyError &&
        [
          "not_found",
          "service_not_ready",
          "service_unavailable",
          "native_unavailable",
          "task_board_native_unavailable",
          "task_board_native_outcome_unavailable",
          "task_board_turn_unavailable",
          "task_board_native_read_failed",
          "task_board_native_epoch_changed",
          "task_board_native_search_limit",
          "task_board_native_collection_limit",
        ].includes(error.code)
      ))
        throw error;
      const detail =
        "Native reconciliation is waiting: " +
        error.code +
        ". " +
        error.message.slice(0, 2048);
      if (
        !active &&
        ["succeeded", "failed", "cancelled"].includes(
          run.value.externalOutcome.state,
        )
      )
        throw error;
      if (
        run.value.externalOutcome.state === "unknown" &&
        run.value.externalOutcome.code === error.code &&
        (!active || (task.value.workflowState === 'in_progress' && task.value.claim?.phase === 'outcome_unknown' && !task.value.waiting))
      )
        return;
      if (task.value.waiting?.detail === detail && (!active || task.value.workflowState === claimedWorkflowState(task.value.waiting))) return;
      await this.update(run, task, {
        kind: "unavailable",
        reason: ["service_not_ready", "service_unavailable", "native_unavailable", "task_board_native_unavailable"].includes(error.code)
          ? "host" : "external_outcome",
        code: error.code,
        detail,
      });
    }
  }
}
