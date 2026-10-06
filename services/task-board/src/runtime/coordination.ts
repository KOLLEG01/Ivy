import { canonical } from "../../../../packages/sdk/src/node.js";
import { IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import type { TaskBoard } from "../../../../packages/sdk/src/node.js";
import { mutation, TaskBoardStore } from "./store.js";
import type { Document } from "./store.js";

/** Root gate for shared indexes; Task gates allow independent publications to proceed. */
export class TaskBoardCoordination {
  constructor(
    readonly store: TaskBoardStore,
    readonly principalId: string,
  ) {}
  private async gates(operation: Document<"task-board/operation">): Promise<string[]> {
    const retained = this.store.operationGates(operation.pin.objectId);
    if (retained.length) return retained;
    const request = operation.value.request;
    if (!("taskId" in request)) {
      this.store.retainOperationGates(operation.pin.objectId, ["TaskBoard publication gate"]);
      return ["TaskBoard publication gate"];
    }
    const task = "TaskBoard publication gate " + request.taskId;
    if (!["edit", "start", "continue", "transition", "review", "cancel", "defer"].includes(request.action)) {
      this.store.retainOperationGates(operation.pin.objectId, [task]);
      return [task];
    }
    const previous = await this.store.read("task-board/task", {
      objectId: request.taskId,
      revision: request.expectedRevision,
    });
    const shared = request.action === "edit"
      ? previous.value.fields.category !== request.fields.category ||
        canonical(previous.value.fields.dependencies) !== canonical(request.fields.dependencies)
      : request.action === "start" || request.action === "continue"
        ? previous.value.fields.dependencies.length > 0 || !!previous.value.blocker
        : request.action === 'defer' ? !!request.blocker : request.action === "transition" || request.action === "review" || request.action === "cancel";
    const names = shared ? ["TaskBoard publication gate", task] : [task];
    this.store.retainOperationGates(operation.pin.objectId, names);
    return names;
  }
  async current(name = "TaskBoard publication gate"): Promise<Document<"task-board/coordination">> {
    let value = await this.store.named(
      "task-board/coordination",
      name,
    );
    if (!value) {
      try {
        const pin = await this.store.write(
          "task-board/coordination",
          {
            schemaVersion: 1,
            principalId: this.principalId,
            activeOperation: null,
            updatedAt: new Date().toISOString(),
          },
          mutation(this.store.rootObjectId ?? "root", "coordination:" + name),
          {
            create: {
              parentId: this.store.rootObjectId,
              name,
            },
          },
        );
        value = await this.store.read("task-board/coordination", pin);
      } catch (error) {
        if (!(
          error instanceof IvyError &&
          ["mutation_conflict", "revision_conflict"].includes(error.code)
        ))
          throw error;
        value = await this.store.named(
          "task-board/coordination",
          name,
        );
      }
    }
    requireThat(
      value && value.value.principalId === this.principalId,
      "task_board_principal_mismatch",
      "This workflow publication gate belongs to another logical principal.",
    );
    return value;
  }
  async acquire(operation: Document<"task-board/operation">): Promise<void> {
    const pin = { objectId: operation.pin.objectId, revision: 1 };
    for (const name of await this.gates(operation)) {
      const current = await this.current(name);
      if (current.value.activeOperation) {
        requireThat(
          canonical(current.value.activeOperation) === canonical(pin),
          "task_board_publication_busy",
          "Reconcile the recorded publication before starting another workflow action.",
        );
        continue;
      }
      await this.store.write(
        "task-board/coordination",
        {
          ...current.value,
          activeOperation: pin,
          updatedAt: operation.value.createdAt,
        },
        mutation(
          current.pin.objectId,
          "acquire:" + current.pin.revision + ":" + pin.objectId,
        ),
        {
          objectId: current.pin.objectId,
          expectedRevision: current.pin.revision,
        },
      );
    }
  }
  async reserved(operation: Document<"task-board/operation">): Promise<boolean> {
    const first = operation.value.writes[0];
    if (!first || first.contractKey !== "task-board/task" || !first.outcome)
      return false;
    const payload = await this.store.read(
      "task-board/task",
      first.payload.object,
      false,
    );
    requireThat(
      payload.value.publication?.objectId === operation.pin.objectId &&
        payload.value.publication.revision === 1,
      "task_board_publication_pending",
      "The first Task write must durably retain its owning publication marker.",
    );
    requireThat(
      "create" in first.destination ||
        first.destination.objectId === first.outcome.objectId,
      "task_board_stage_conflict",
      "The retained Task reservation receipt names another destination.",
    );
    return true;
  }
  async release(operationId: string, reserved = false): Promise<void> {
    const operation = await this.store.read("task-board/operation", operationId);
    const taskReserved = reserved && (await this.reserved(operation));
    for (const name of await this.gates(operation)) {
      const current = await this.current(name);
      if (current.value.activeOperation?.objectId !== operationId) continue;
      requireThat(
        ["succeeded", "failed"].includes(operation.value.phase) || taskReserved,
        "task_board_publication_pending",
        "An unfinished publication gate cannot be released before its first Task reservation is durable.",
      );
      await this.clear(current, operationId, operation.value.updatedAt);
    }
  }
  async releaseUnstarted(operationId: string): Promise<boolean> {
    const operation = await this.store.read("task-board/operation", operationId);
    if (
      ["succeeded", "failed"].includes(operation.value.phase) ||
      operation.value.writes.length > 0
    )
      return false;
    let released = false;
    for (const name of await this.gates(operation)) {
      const current = await this.current(name);
      if (current.value.activeOperation?.objectId !== operationId) continue;
      await this.clear(current, operationId, operation.value.updatedAt);
      released = true;
    }
    return released;
  }
  private async clear(
    current: Document<"task-board/coordination">,
    operationId: string,
    updatedAt: string,
  ): Promise<void> {
    await this.store.write(
      "task-board/coordination",
      {
        ...current.value,
        activeOperation: null,
        updatedAt,
      },
      mutation(
        current.pin.objectId,
        "release:" + current.pin.revision + ":" + operationId,
      ),
      {
        objectId: current.pin.objectId,
        expectedRevision: current.pin.revision,
      },
    );
  }
  async pending(): Promise<TaskBoard.ObjectPin | null> {
    return this.store.pendingCoordination();
  }
}
