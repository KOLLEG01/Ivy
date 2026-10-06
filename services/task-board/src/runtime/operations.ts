import { canonical, hashJson } from "../../../../packages/sdk/src/node.js";
import { IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import type { Agent, TaskBoard } from "../../../../packages/sdk/src/node.js";
import type { ContractValues } from "./schema.js";
import { contractVersion } from "./schema.js";
import { identityHash, mutation, TaskBoardStore } from "./store.js";
import type { Document } from "./store.js";

type OperationDocument = Document<"task-board/operation">;
type WriteKey = TaskBoard.PreparedWrite["contractKey"];
const recoverableCreate = (error: unknown) =>
  error instanceof IvyError &&
  ["revision_conflict", "mutation_conflict"].includes(error.code);
const operationName = (caller: string, id: string) =>
  "TaskBoard operation " + identityHash(caller, id);

/** Durable prepare/apply/receipt protocol. Each destination write is replayed with its saved bytes. */
export class TaskBoardOperations {
  constructor(readonly store: TaskBoardStore) {}
  async find(
    callerPrincipalId: string,
    operationId: string,
  ): Promise<OperationDocument | null> {
    const found = await this.store.named(
      "task-board/operation",
      operationName(callerPrincipalId, operationId),
    );
    requireThat(
      !found ||
        (found.value.callerPrincipalId === callerPrincipalId &&
          found.value.operationId === operationId),
      "task_board_identity_conflict",
      "Operation identity does not match its original caller.",
    );
    return found;
  }
  async begin(
    request: TaskBoard.DurableRequest,
    actor: TaskBoard.Actor,
  ): Promise<OperationDocument> {
    const existing = await this.find(actor.principalId, request.operationId),
      requestHash = hashJson(request);
    if (existing) {
      requireThat(
        existing.value.requestHash === requestHash,
        "mutation_conflict",
        "The operation identity already names different TaskBoard arguments.",
      );
      return existing;
    }
    const now = new Date().toISOString(),
      value: TaskBoard.Operation = {
        schemaVersion: 1,
        operationId: request.operationId,
        callerPrincipalId: actor.principalId,
        request,
        requestHash,
        actor,
        createdAt: now,
        updatedAt: now,
        phase: "accepted",
        writes: [],
        outcome: null,
        error: null,
      };
    try {
      const pin = await this.store.write(
        "task-board/operation",
        value,
        mutation(
          identityHash(actor.principalId, request.operationId),
          "accept",
        ),
        {
          create: {
            parentId: this.store.rootObjectId,
            name: operationName(actor.principalId, request.operationId),
          },
        },
      );
      return this.store.read("task-board/operation", pin);
    } catch (error) {
      if (!recoverableCreate(error)) throw error;
      const winner = await this.find(actor.principalId, request.operationId);
      requireThat(
        winner && winner.value.requestHash === requestHash,
        "mutation_conflict",
        "The winning Operation has different original arguments.",
      );
      return winner;
    }
  }
  private async amend(
    id: string,
    transition: (current: TaskBoard.Operation) => TaskBoard.Operation | null,
  ): Promise<OperationDocument> {
    for (let attempt = 0; attempt < 12; attempt++) {
      const current = await this.store.read("task-board/operation", id),
        next = transition(current.value);
      if (!next) return current;
      const updated = { ...next, updatedAt: new Date().toISOString() };
      try {
        const pin = await this.store.write(
          "task-board/operation",
          updated,
          mutation(
            id,
            "amend:" + current.pin.revision + ":" + hashJson(updated),
          ),
          { objectId: id, expectedRevision: current.pin.revision },
        );
        return this.store.read("task-board/operation", pin);
      } catch (error) {
        if (!(error instanceof IvyError && error.code === "revision_conflict"))
          throw error;
      }
    }
    throw new IvyError(
      "task_board_contention",
      "The operation changed repeatedly. Reconcile its saved stages before retrying.",
      "unknown",
    );
  }
  async prepareNative(id: string, proposed: TaskBoard.NativeLifecycleCall): Promise<TaskBoard.NativeLifecycleCall> {
    const saved = await this.amend(id, operation => {
      const existing = operation.nativeCalls?.find(call => call.key === proposed.key);
      if (existing) {
        requireThat(canonical({ ...existing, observation: null }) === canonical(proposed),
          'task_board_native_intent_mismatch', 'The native lifecycle stage already has another retained request.');
        return null;
      }
      requireThat(!['failed', 'succeeded'].includes(operation.phase), 'task_board_operation_failed', 'A finished action cannot prepare native work.');
      return { ...operation, nativeCalls: [...(operation.nativeCalls ?? []), proposed] };
    });
    return saved.value.nativeCalls!.find(call => call.key === proposed.key)!;
  }
  async observeNative(id: string, key: string, observation: Agent.Operation): Promise<TaskBoard.NativeLifecycleCall> {
    const saved = await this.amend(id, operation => {
      const calls = [...(operation.nativeCalls ?? [])], index = calls.findIndex(call => call.key === key);
      requireThat(index >= 0 && calls[index]!.operationId === observation.operationId,
        'task_board_native_intent_mismatch', 'The observation must identify the retained native lifecycle call.');
      if (canonical(calls[index]!.observation) === canonical(observation)) return null;
      calls[index] = { ...calls[index]!, observation };
      return { ...operation, nativeCalls: calls };
    });
    return saved.value.nativeCalls!.find(call => call.key === key)!;
  }
  async archiveIdentity(id: string): Promise<string> {
    const current = await this.store.read('task-board/operation', id);
    if (current.value.archiveMutationId) return current.value.archiveMutationId;
    const identity = await this.store.preparedMutationId(mutation(id, 'archive-membership'));
    const saved = await this.amend(id, operation => operation.archiveMutationId ? null : { ...operation, archiveMutationId: identity });
    return saved.value.archiveMutationId!;
  }
  async stage<K extends WriteKey>(
    id: string,
    index: number,
    key: K,
    destination: TaskBoard.PreparedWrite["destination"],
    proposed: ContractValues[K],
  ): Promise<TaskBoard.PreparedWrite> {
    requireThat(
      Number.isSafeInteger(index) && index >= 0 && index < 16,
      "task_board_stage_limit",
      "An operation supports at most 16 ordered prepared writes.",
    );
    const current = await this.store.read("task-board/operation", id),
      existing = current.value.writes[index];
    const agrees = (write: TaskBoard.PreparedWrite) =>
      requireThat(
        write.contractKey === key &&
          canonical(write.destination) === canonical(destination),
        "task_board_stage_conflict",
        "This operation stage already names another contract or destination.",
      );
    if (existing) {
      agrees(existing);
      return existing;
    }
    requireThat(
      current.value.phase !== "failed" &&
        current.value.phase !== "succeeded" &&
        index === current.value.writes.length,
      "task_board_stage_conflict",
      "New writes must append to the unfinished operation in order.",
    );
    const hash = hashJson(proposed),
      name = "Prepared " + index + " " + hash.slice(7),
      payloadId = mutation(id, "payload:" + index + ":" + hash);
    let payload: TaskBoard.ObjectPin;
    try {
      payload = await this.store.write(key, proposed, payloadId, {
        create: { parentId: id, name },
      });
    } catch (error) {
      if (!recoverableCreate(error)) throw error;
      const found = await this.store.named(key, name, id);
      requireThat(
        found && found.revision.contentHash === hash,
        "task_board_evidence_mismatch",
        "Prepared payload identity has different saved bytes.",
      );
      payload = found.pin;
    }
    const entry: TaskBoard.PreparedWrite = {
      mutationId: await this.store.preparedMutationId(
        mutation(id, "apply:" + index),
      ),
      contractKey: key,
      contractVersion: contractVersion(key),
      payload: {
        object: payload,
        label: name,
        mediaType: "application/json",
        contentHash: hash,
      },
      destination,
      outcome: null,
    };
    const saved = await this.amend(id, (operation) => {
      const winner = operation.writes[index];
      if (winner) {
        agrees(winner);
        return null;
      }
      requireThat(
        !["failed", "succeeded"].includes(operation.phase) &&
          operation.writes.length === index,
        "task_board_stage_conflict",
        "The operation cannot accept this new write.",
      );
      return {
        ...operation,
        phase: "preparing",
        writes: [...operation.writes, entry],
        error: null,
      };
    });
    return saved.value.writes[index]!;
  }
  async apply(id: string, index: number): Promise<TaskBoard.ObjectPin> {
    const current = await this.store.read("task-board/operation", id),
      entry = current.value.writes[index];
    requireThat(
      entry,
      "task_board_stage_conflict",
      "No write has been prepared at this stage.",
    );
    if (entry.outcome) return entry.outcome;
    requireThat(
      !["failed", "succeeded"].includes(current.value.phase),
      "task_board_stage_conflict",
      "A completed/failed operation cannot issue another prepared write.",
    );
    const payload = await this.store.read(
      entry.contractKey,
      entry.payload.object,
      false,
    );
    requireThat(
      payload.metadata.parentId === id &&
        payload.revision.contentHash === entry.payload.contentHash &&
        entry.payload.mediaType === "application/json",
      "task_board_evidence_mismatch",
      "The staged payload must be owned by this operation and match its immutable content hash.",
    );
    const result = await this.store.writePrepared(
      entry.contractKey,
      payload.value,
      entry.mutationId,
      entry.destination,
    );
    await this.amend(id, (operation) => {
      const latest = operation.writes[index];
      requireThat(
        latest &&
          canonical({ ...latest, outcome: null }) ===
            canonical({ ...entry, outcome: null }),
        "task_board_stage_conflict",
        "The applied stage no longer has its original saved intent.",
      );
      if (latest.outcome) {
        requireThat(
          canonical(latest.outcome) === canonical(result),
          "task_board_evidence_mismatch",
          "The retained write receipt differs from Hive replay.",
        );
        return null;
      }
      const writes = [...operation.writes];
      writes[index] = { ...latest, outcome: result };
      return { ...operation, phase: "publishing", writes, error: null };
    });
    return result;
  }
  async finish(
    id: string,
    outcome: TaskBoard.ActionOutcome,
  ): Promise<TaskBoard.ActionOutcome> {
    const done = await this.amend(id, (operation) => {
      requireThat(
        outcome.operationId === operation.operationId,
        "task_board_identity_conflict",
        "An outcome must retain its original operation ID.",
      );
      if (operation.phase === "succeeded") {
        requireThat(
          canonical(operation.outcome) === canonical(outcome),
          "mutation_conflict",
          "The completed action has another original outcome.",
        );
        return null;
      }
      requireThat(
        operation.phase !== "failed" &&
          operation.writes.every((write) => write.outcome !== null),
        "task_board_publication_pending",
        "All prepared writes must have durable receipts before completing the action.",
      );
      return { ...operation, phase: "succeeded", outcome, error: null };
    });
    this.store.completeLocalWorkflow(id, done.value.updatedAt);
    return done.value.outcome!;
  }
  async failure(
    id: string,
    error: IvyError,
    needsAttention: boolean,
  ): Promise<OperationDocument> {
    const failed = await this.amend(id, (operation) => {
      if (["succeeded", "failed"].includes(operation.phase)) return null;
      return {
        ...operation,
        phase: needsAttention ? "needs_attention" : "failed",
        outcome: null,
        error: {
          code: error.code,
          message: error.message,
          execution: error.outcome,
        },
      };
    });
    if (failed.value.phase === "failed")
      this.store.completeLocalWorkflow(id, failed.value.updatedAt);
    if (failed.value.phase === "failed" && failed.value.actor.source === "scheduler" &&
        ["start", "continue"].includes(failed.value.request.action) &&
        "intent" in failed.value.request &&
        this.store.localKey(failed.value.request.intent.objectId) === "task-board/native-plan")
      this.store.completeLocalWorkflow(failed.value.request.intent.objectId, failed.value.updatedAt,
        TaskBoardStore.nativeEvidenceWindowMs);
    return failed;
  }
}
