import { coordinationPending, peerComment, validateBlocker, workspaceIdentity } from './task-coordination.js';
import { publishBrowserNotice } from '../../../../packages/sdk/src/node.js';
import { canonical, digest, hashJson, jsonObjectContentBytes } from "../../../../packages/sdk/src/node.js";
import { connectionOwner } from "../../../../packages/sdk/src/connection-owner.js";
import { IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import { validateTaskBoard } from "../../../../packages/sdk/src/node.js";
import type { TaskBoard, Wire } from "../../../../packages/sdk/src/node.js";
import type { InvocationContext } from "../../../../packages/sdk/src/service.js";
import { TaskBoardChecks, date, planSemantics, reservesWorkspace, resultCommentFits, same, sharedProjectKey, sharedProjectOwner } from "./checks.js";
import { TaskBoardCoordination } from "./coordination.js";
import { TaskBoardOperations } from "./operations.js";
import { publishNative } from "./native-publication.js";
import { verifyNativeTranscript } from "./full-turn-evidence.js";
import type { ContractValues } from "./schema.js";
import { mutation, taskAttachmentPins, TaskBoardStore } from "./store.js";
import type { Document } from "./store.js";
import { initialDelivery } from "./deliveries.js";
import { checkedNativeContract } from "../../../../packages/sdk/src/node.js";
import {
  saveNativePlan,
  validateNativePlanDraft,
} from "../../../../packages/sdk/src/native-plan.js";
import { readPlan, resolvedPlan } from "./native-plan.js";
import { BoundToolClient } from "../../../../packages/sdk/src/client.js";
import type { Agent } from "../../../../packages/sdk/src/node.js";
import { executionCondition } from "./condition.js";
import { TaskBoardCategoryProjection } from "./category-projection.js";
import { TaskBoardNativeInspector } from "./native-inspector.js";
import { checkConfiguration, configurationName, configurationView } from './configuration.js';
import { TaskBoardNativeLifecycle } from './native-lifecycle.js';
import { unstartedArchivedContext } from './native-recovery.js';

/** A Task has run once it has any attempt, claim, Run or native context. */
export const started = (task: TaskBoard.Task) =>
  task.attemptCount > 0 || !!task.claim || !!task.lastRun || !!task.primaryResourceRef;

type Op = Document<"task-board/operation">;
type WriteKey = TaskBoard.PreparedWrite["contractKey"];
const terminal = (task: TaskBoard.Task) =>
  ["done", "cancelled"].includes(task.workflowState);
const emptyOutcome = (id: string): TaskBoard.ActionOutcome => ({
  operationId: id,
  task: null,
  plan: null,
  run: null,
  history: null,
  result: null,
  review: null,
  attachment: null,
});

/** A deterministic action program; each step reuses the bytes already retained by its Operation. */
export class Publication {
  index = 0;
  constructor(
    readonly operations: TaskBoardOperations,
    readonly operation: Op,
    readonly taskWritten?: (
      pin: TaskBoard.ObjectPin,
      value: TaskBoard.Task,
    ) => Promise<void>,
  ) {}
  get pin(): TaskBoard.ObjectPin {
    return { objectId: this.operation.pin.objectId, revision: 1 };
  }
  async write<K extends WriteKey>(
    key: K,
    value: ContractValues[K],
    destination: TaskBoard.PreparedWrite["destination"],
  ): Promise<TaskBoard.ObjectPin> {
    const index = this.index++;
    await this.operations.stage(
      this.operation.pin.objectId,
      index,
      key,
      destination,
      value,
    );
    const pin = await this.operations.apply(this.operation.pin.objectId, index);
    if (key === "task-board/task")
      await this.taskWritten?.(pin, value as ContractValues["task-board/task"]);
    return pin;
  }
  async create<K extends WriteKey>(
    key: K,
    value: ContractValues[K],
    label: string,
  ): Promise<TaskBoard.ObjectPin> {
    return this.write(key, value, {
      create: {
        parentId: this.operations.store.rootObjectId,
        name: label + " " + this.operation.pin.objectId,
      },
    });
  }
}

/** Authenticated transitions and durable claim/Run creation. Native execution is a separate observer. */
export class TaskBoardEngine {
  readonly operations: TaskBoardOperations;
  readonly coordination: TaskBoardCoordination;
  readonly checks: TaskBoardChecks;
  readonly categoryProjection: TaskBoardCategoryProjection;
  private readonly executions = new Map<
    string,
    Promise<TaskBoard.ActionOutcome>
  >();
  constructor(
    readonly store: TaskBoardStore,
    readonly settings: TaskBoard.Settings,
    readonly owner: { serviceNodeId: string; generation: number },
  ) {
    validateTaskBoard("Settings", settings);
    requireThat(
      settings.rootObjectId === store.rootObjectId,
      "task_board_scope_mismatch",
      "The engine and settings must select the same workflow root.",
    );
    this.operations = new TaskBoardOperations(store);
    this.coordination = new TaskBoardCoordination(store, settings.principalId);
    this.checks = new TaskBoardChecks(store);
    this.categoryProjection = new TaskBoardCategoryProjection(
      store,
      settings.principalId,
    );
  }
  async verifyOwner(): Promise<void> {
    const node = await connectionOwner(
      this.store.client,
      this.owner.serviceNodeId,
    );
    requireThat(
      node.principalId === this.settings.principalId &&
        node.serviceName === "task-board" &&
        node.connected &&
        node.synced,
      "task_board_principal_mismatch",
      "Workflow recovery requires the configured authenticated logical principal and a live synchronized service connection.",
    );
  }
  async invoke(
    input: TaskBoard.ActionInput,
    context: InvocationContext,
  ): Promise<TaskBoard.ActionOutcome> {
    const request = structuredClone(input);
    context.signal.throwIfAborted();
    await this.verifyOwner();
    requireThat(
      context.generation === this.owner.generation &&
        context.operationId === request.operationId,
      "task_board_identity_conflict",
      "Outer operation and live connection identities must match the TaskBoard request.",
    );
    if (request.expectedWorkspace)
      requireThat(
        request.expectedWorkspace.principalId === this.settings.principalId &&
          request.expectedWorkspace.rootObjectId === this.store.rootObjectId &&
          request.expectedWorkspace.callerPrincipalId ===
            context.callerPrincipalId,
        "task_board_workspace_changed",
        "The original caller or workflow owner changed. This request was not admitted.",
      );
    const actor: TaskBoard.Actor = {
      principalId: context.callerPrincipalId,
      ...this.owner,
      source: "user",
    };
    if (request.action === 'comment' && request.authorKind === 'agent') actor.source = 'worker';
    if (request.action === "savePlan") {
      const contract = checkedNativeContract(request.plan.nativeVersion, {
        sourceHash: request.plan.catalogSourceHash,
      }).contract;
      validateNativePlanDraft(contract, request.plan, "task-board");
      planSemantics(request.plan);
      if (request.plan.location) {
        const location = request.plan.location,
          node = await this.store.client.request("serviceNodes.get", {
            serviceNodeId: location.serviceNodeId,
          });
        requireThat(
          node.hostId === location.hostId &&
            node.serviceName === "agent-manager",
          "task_board_target_mismatch",
          "Plan location must identify its actual target AgentManager.",
        );
        const observed = (await new BoundToolClient(
          this.store.client,
          location.serviceNodeId,
          [{ namespace: "agent", interfaceVersion: "1.0.0" }],
        ).call("agent.resolveProject", {
          selection: { kind: "existing", cwd: location.cwd },
          expectedProjectId: location.projectId,
        })) as Agent.ProjectLocation;
        requireThat(
          observed.cwd === location.cwd,
          "task_board_project_mismatch",
          "The plan must retain the target-validated canonical working directory.",
        );
      }
      const plan = await saveNativePlan(this.store.client, contract, {
        draft: request.plan,
        kind: "task-board",
        parentId: this.store.rootObjectId,
        mutationId: hashJson({
          callerPrincipalId: context.callerPrincipalId,
          operationId: request.operationId,
        }),
      });
      return this.execute(
        await this.operations.begin(
          { ...request, plan, draftHash: hashJson(request) },
          actor,
        ),
      );
    }
    return this.execute(await this.operations.begin(request, actor));
  }
  async configuration(): Promise<TaskBoard.ConfigurationView> {
    await this.verifyOwner();
    return configurationView(this.store);
  }
  async schedule(
    request: TaskBoard.SchedulerRequest,
  ): Promise<TaskBoard.ActionOutcome> {
    await this.verifyOwner();
    requireThat(
      this.settings.scheduler.enabled,
      "task_board_scheduler_disabled",
      "Autonomous scheduling is disabled.",
    );
    return this.execute(
      await this.operations.begin(request, {
        principalId: this.settings.principalId,
        ...this.owner,
        source: "scheduler",
      }),
    );
  }
  async nativeUpdate(
    request: TaskBoard.NativeUpdateRequest,
  ): Promise<TaskBoard.ActionOutcome> {
    await this.verifyOwner();
    return this.execute(
      await this.operations.begin(request, {
        principalId: this.settings.principalId,
        ...this.owner,
        source: "native",
      }),
    );
  }
  async nativeComment(request: TaskBoard.CommentActionRequest): Promise<TaskBoard.ActionOutcome> {
    await this.verifyOwner();
    return this.execute(await this.operations.begin(request, {
      principalId: this.settings.principalId, ...this.owner, source: 'native',
    }));
  }
  async recoverPublication(): Promise<TaskBoard.ActionOutcome | null> {
    await this.verifyOwner();
    const pending = await this.coordination.pending();
    if (!pending) return null;
    const operation = await this.store.read(
      "task-board/operation",
      pending.objectId,
    );
    if (operation.value.phase === "failed") {
      await this.coordination.release(operation.pin.objectId);
      return null;
    }
    return this.execute(operation);
  }
  async recoverOperation(objectId: string): Promise<TaskBoard.ActionOutcome> {
    await this.verifyOwner();
    return this.execute(await this.store.read("task-board/operation", objectId));
  }
  async operation(
    operationId: string,
    context: InvocationContext,
  ): Promise<TaskBoard.Operation> {
    await this.verifyOwner();
    context.signal.throwIfAborted();
    validateTaskBoard("OperationQuery", { operationId });
    requireThat(
      context.generation === this.owner.generation,
      "task_board_identity_conflict",
      "The Operation query must use this live connection generation.",
    );
    const operation = await this.operations.find(
      context.callerPrincipalId,
      operationId,
    );
    requireThat(
      operation,
      "not_found",
      "This caller has no retained TaskBoard operation with that identity.",
    );
    return operation.value;
  }
  condition(task: TaskBoard.Task): TaskBoard.ExecutionCondition {
    return executionCondition({
      workflowState: task.workflowState,
      control: task.fields.control,
      publication: !!task.publication,
      claim: task.claim
        ? { phase: task.claim.phase, hostId: task.claim.target.hostId }
        : null,
      waiting: task.waiting,
      executionRequirement: task.fields.executionRequirement,
      requiredCapabilities: task.fields.requiredCapabilities,
    });
  }
  private async taskDocument(
    query: TaskBoard.TaskQuery,
  ): Promise<Document<"task-board/task">> {
    let document: Document<"task-board/task">;
    if (typeof query.task === "string")
      document = await this.store.read("task-board/task", query.task);
    else {
      const page = await this.store.page("task-board/task", {
        where: { op: "eq", field: "data:/taskKey", value: query.task.taskKey },
        limit: 2,
      });
      requireThat(
        page.items.length === 1 && !page.nextCursor,
        "not_found",
        "Task key was not found in this workspace.",
      );
      document = await this.store.read(
        "task-board/task",
        page.items[0]!.objectId,
      );
    }
    return document;
  }
  private readStateName(principalId: string, taskId: string): string {
    return (
      "Comment read state " +
      hashJson({ principalId, taskId }).slice("sha256:".length)
    );
  }
  private async unreadAgentComments(
    document: Document<"task-board/task">,
    principalId: string,
  ): Promise<number> {
    const state = await this.store.named(
      "task-board/comment-read-state",
      this.readStateName(principalId, document.pin.objectId),
    );
    if (state)
      requireThat(
        state.value.principalId === principalId &&
          state.value.taskId === document.pin.objectId,
        "task_board_identity_conflict",
        "Comment read state belongs to another user or Task.",
      );
    return Math.max(
      0,
      document.value.agentCommentCount -
        (state?.value.readAgentCommentCount ?? 0),
    );
  }
  async task(
    query: TaskBoard.TaskQuery,
    principalId: string,
  ): Promise<TaskBoard.TaskView> {
    const document = await this.taskDocument(query);
    return {
      object: document.pin,
      task: document.value,
      executionCondition: this.condition(document.value),
      unreadAgentComments: await this.unreadAgentComments(
        document,
        principalId,
      ),
    };
  }
  async list(query: TaskBoard.TaskListQuery): Promise<TaskBoard.TaskListResult> {
    validateTaskBoard("TaskListQuery", query);
    const filters: Wire.QueryPredicate[] = [];
    const relatedWorkspace = query.relatedTo ? await workspaceIdentity(this.store, (await this.store.read('task-board/task', query.relatedTo)).value) : null;
    if (query.status) filters.push({ op: "eq", field: "data:/workflowState", value: query.status });
    if (query.category) filters.push({ op: "eq", field: "data:/fields/category", value: query.category });
    const page = await this.store.page("task-board/task", {
      limit: query.limit ?? 50,
      ...(query.cursor ? { cursor: query.cursor } : {}),
      ...(filters.length ? { where: filters.length === 1 ? filters[0] : { op: "and", args: filters } } : {}),
      select: ["data:/taskKey", "data:/fields/title", "data:/workflowState", "data:/fields/category"],
    });
    const peers = new Set<string>();
    if (relatedWorkspace) for (const item of page.items) {
      if (item.objectId !== query.relatedTo && await workspaceIdentity(this.store, (await this.store.read('task-board/task', { objectId: item.objectId, revision: item.revision })).value) === relatedWorkspace) peers.add(item.objectId);
    }
    return {
      items: page.items.filter(item => !query.relatedTo || peers.has(item.objectId)).map(item => {
        const values = item.values as Record<string, unknown>;
        const taskKey = values["data:/taskKey"], title = values["data:/fields/title"],
          workflowState = values["data:/workflowState"], category = values["data:/fields/category"];
        requireThat(typeof taskKey === "string" && typeof title === "string" && typeof workflowState === "string" &&
          (category === null || typeof category === "string"), "task_board_contract_mismatch", "Task summary has invalid fields.");
        return { taskId: item.objectId, revision: item.revision, taskKey, title,
          workflowState: workflowState as TaskBoard.Task["workflowState"], category };
      }),
      nextCursor: page.nextCursor,
    };
  }
  async markCommentsRead(
    request: TaskBoard.MarkCommentsReadRequest,
    context: InvocationContext,
  ): Promise<TaskBoard.MarkCommentsReadResult> {
    validateTaskBoard("MarkCommentsReadRequest", request);
    await this.verifyOwner();
    context.signal.throwIfAborted();
    requireThat(
      context.generation === this.owner.generation &&
        context.operationId === request.operationId,
      "task_board_identity_conflict",
      "The read marker must retain the live caller operation identity.",
    );
    const task = await this.store.read("task-board/task", {
      objectId: request.taskId,
      revision: request.expectedTaskRevision,
    });
    requireThat(
      task.metadata.currentRevision === request.expectedTaskRevision,
      "revision_conflict",
      "The Task changed before its comments were marked read.",
    );
    const name = this.readStateName(context.callerPrincipalId, request.taskId),
      now = new Date().toISOString();
    for (let attempt = 0; attempt < 12; attempt++) {
      context.signal.throwIfAborted();
      const current = await this.store.named(
        "task-board/comment-read-state",
        name,
      );
      if (current)
        requireThat(
          current.value.principalId === context.callerPrincipalId &&
            current.value.taskId === request.taskId,
          "task_board_identity_conflict",
          "Comment read state belongs to another user or Task.",
        );
      if (current && current.value.readAgentCommentCount >= task.value.agentCommentCount)
        return { task: task.pin, readState: current.pin, unreadAgentComments: 0 };
      const value: TaskBoard.CommentReadState = {
        schemaVersion: 1,
        principalId: context.callerPrincipalId,
        taskId: request.taskId,
        readAgentCommentCount: task.value.agentCommentCount,
        updatedAt: now,
      };
      try {
        const readState = await this.store.write(
          "task-board/comment-read-state",
          value,
          mutation(name, "mark-comments-read:" + hashJson({ request, previous: current?.pin ?? null, value })),
          current
            ? {
                objectId: current.pin.objectId,
                expectedRevision: current.pin.revision,
              }
            : { create: { parentId: this.store.rootObjectId, name } },
        );
        return { task: task.pin, readState, unreadAgentComments: 0 };
      } catch (error) {
        if (
          error instanceof IvyError &&
          ["revision_conflict", "mutation_conflict"].includes(error.code)
        )
          continue;
        throw error;
      }
    }
    throw new IvyError("task_board_contention", "The comment read marker changed repeatedly. Retry from the current Task.");
  }
  async archive(
    request: TaskBoard.ArchiveRequest,
    context: InvocationContext,
  ): Promise<TaskBoard.ArchiveResult> {
    validateTaskBoard("ArchiveRequest", request);
    await this.verifyOwner();
    context.signal.throwIfAborted();
    requireThat(
      context.generation === this.owner.generation && context.operationId === request.operationId,
      "task_board_identity_conflict",
      "The archive request must retain the live caller operation identity.",
    );
    if (request.expectedWorkspace)
      requireThat(
        request.expectedWorkspace.principalId === this.settings.principalId &&
          request.expectedWorkspace.rootObjectId === this.store.rootObjectId &&
          request.expectedWorkspace.callerPrincipalId === context.callerPrincipalId,
        "task_board_workspace_changed",
        "The original caller or workflow owner changed. This request was not admitted.",
      );
    const outcome = await this.execute(await this.operations.begin(request, {
      principalId: context.callerPrincipalId, ...this.owner, source: 'user',
    }));
    requireThat(outcome.archive, 'task_board_contract_mismatch', 'The archive action must retain its exact metadata outcome.');
    return outcome.archive;
  }
  async categories(): Promise<TaskBoard.CategoriesResult> {
    return this.categoryProjection.categories();
  }
  private async normalizedCategory(
    category: string | null,
  ): Promise<string | null> {
    return this.categoryProjection.normalize(category);
  }
  private async execute(operation: Op): Promise<TaskBoard.ActionOutcome> {
    const existing = this.executions.get(operation.pin.objectId);
    if (existing) return existing;
    const active = this.store.withReadCache(() =>
      this.executeCached(operation),
    );
    this.executions.set(operation.pin.objectId, active);
    const clear = () => {
      if (this.executions.get(operation.pin.objectId) === active)
        this.executions.delete(operation.pin.objectId);
    };
    void active.then(clear, clear);
    return active;
  }
  private async executeCached(operation: Op): Promise<TaskBoard.ActionOutcome> {
    if (operation.value.phase === "succeeded") {
      await this.coordination.release(operation.pin.objectId);
      return operation.value.outcome!;
    }
    if (operation.value.phase === "failed") {
      await this.coordination.release(operation.pin.objectId);
      const error = operation.value.error!;
      throw new IvyError(error.code, error.message, error.execution);
    }
    // Complete transcripts can be large. Validate their immutable graph before acquiring the
    // publication gate; only the short domain writes belong in that critical section.
    let transcript:
      Awaited<ReturnType<typeof verifyNativeTranscript>> | undefined;
    const request = operation.value.request;
    let normalizedCategory: string | undefined;
    if (operation.value.writes.length === 0 && request.action === "nativeUpdate" &&
        request.change.kind === "unavailable" && request.change.code === "native_process_lost") {
      try {
        const run = await this.store.read("task-board/run", request.run);
        requireThat(await new TaskBoardNativeInspector(this).ownerStopped(run.value),
          "task_board_outcome_unresolved", "The original native process loss is not confirmed by its AgentManager.");
      } catch (error) {
        return this.failedPublication(operation.pin.objectId, error);
      }
    }
    if (
      request.action === "nativeUpdate" &&
      request.change.kind === "transcript"
    ) {
      try {
        transcript = await verifyNativeTranscript(
          this.store,
          this.settings.principalId,
          request.run,
          request.change.evidence,
        );
      } catch (error) {
        return this.failedPublication(operation.pin.objectId, error);
      }
    }
    if (
      operation.value.writes.length === 0 &&
      (request.action === "create" || request.action === "edit") &&
      request.fields.category !== null
    ) {
      try {
        normalizedCategory = (await this.normalizedCategory(
          request.fields.category,
        ))!;
      } catch (error) {
        // Admission is already durable. A projection that cannot reach its captured boundary in
        // this request leaves the same accepted action recoverable, so callers must not treat the
        // workflow as safe to repeat under a new operation identity.
        const cause = IvyError.from(error);
        throw new IvyError(cause.code, cause.message, "unknown", cause.details);
      }
    }
    // Busy or ambiguous gate acquisition is not a failed domain action. Its accepted intent remains.
    let releasedCategoryRetry = false;
    try {
      if (!(await this.coordination.reserved(operation)))
        await this.coordination.acquire(operation);
    } catch (error) {
      // Admission is durable already. A busy gate can be reconciled later, so clients must retain
      // this action rather than interpret a refused individual step as an unexecuted workflow.
      const cause = IvyError.from(error);
      throw new IvyError(cause.code, cause.message, "unknown", cause.details);
    }
    try {
      operation = await this.store.read(
        "task-board/operation",
        operation.pin.objectId,
      );
      if (await this.coordination.reserved(operation))
        await this.coordination.release(operation.pin.objectId, true);
      if (operation.value.phase === "succeeded") {
        await this.coordination.release(operation.pin.objectId);
        return operation.value.outcome!;
      }
      requireThat(
        operation.value.phase !== "failed",
        "task_board_operation_failed",
        "The accepted action has already failed conclusively.",
      );
      if (
        operation.value.writes.length === 0 &&
        (request.action === "create" || request.action === "edit") &&
        request.fields.category !== null
      ) {
        try {
          // The full alignment ran before admission. Under the gate, consume at most one new
          // journal page so a concurrently published spelling wins without moving expensive
          // snapshot or backlog work back into the critical section.
          normalizedCategory = await this.categoryProjection.revalidate(
            request.fields.category,
          );
        } catch (error) {
          releasedCategoryRetry = await this.coordination.releaseUnstarted(
            operation.pin.objectId,
          );
          const cause = IvyError.from(error);
          throw new IvyError(
            cause.code,
            cause.message,
            "unknown",
            cause.details,
          );
        }
      }
      let outcome: TaskBoard.ActionOutcome;
      try {
        outcome = await this.publish(operation, transcript, normalizedCategory);
      } catch (error) {
        const concurrent = await this.store.read(
          "task-board/operation",
          operation.pin.objectId,
        );
        // Another generation of this identical operation may reserve the Task between our first
        // stage snapshot and latest-revision check. Continue only its already retained program.
        if (!(
          error instanceof IvyError &&
          error.code === "revision_conflict" &&
          concurrent.value.writes.length > operation.value.writes.length
        ))
          throw error;
        if (concurrent.value.phase === "succeeded") {
          await this.coordination.release(operation.pin.objectId);
          return concurrent.value.outcome!;
        }
        outcome = await this.publish(
          concurrent,
          transcript,
          normalizedCategory,
        );
      }
      const result = await this.operations.finish(
        operation.pin.objectId,
        outcome,
      );
      await this.coordination.release(operation.pin.objectId);
      return result;
    } catch (error) {
      if (releasedCategoryRetry) throw error;
      return this.failedPublication(operation.pin.objectId, error);
    }
  }
  private async failedPublication(
    operationId: string,
    error: unknown,
  ): Promise<TaskBoard.ActionOutcome> {
    const cause = IvyError.from(error),
      saved = await this.store.read("task-board/operation", operationId);
    if (saved.value.phase === "succeeded") {
      await this.coordination.release(operationId);
      return saved.value.outcome!;
    }
    // A conclusive refusal before the first applied domain write can release the gate. Once any
    // effect exists (or its response is unknown), only recovery may finish the same publication.
    const stagedTaskReservation =
      saved.value.writes[0]?.contractKey === "task-board/task";
    const uncertain =
      cause.outcome === "unknown" ||
      saved.value.nativeCalls?.some(call => call.method === 'thread/start' && call.observation && call.observation.phase !== 'failed') ||
      saved.value.writes.some((write) => write.outcome !== null) ||
      (stagedTaskReservation && cause.outcome !== "not_executed");
    const reported = uncertain
      ? new IvyError(cause.code, cause.message, "unknown", cause.details)
      : cause;
    await this.operations.failure(operationId, reported, uncertain);
    if (!uncertain) {
      this.store.releaseWorkspaceOperation(operationId);
      await this.coordination.release(operationId);
    }
    throw reported;
  }
  private async publish(
    operation: Op,
    transcript?: Awaited<ReturnType<typeof verifyNativeTranscript>>,
    normalizedCategory?: string,
  ): Promise<TaskBoard.ActionOutcome> {
    const request = operation.value.request,
      actor = operation.value.actor,
      at = operation.value.createdAt;
    const steps = new Publication(
        this.operations,
        operation,
        async (pin, task) => {
          this.categoryProjection.observe(pin.objectId, pin.revision, task);
          const comment = task.comments.findLast(value => value.operationId === request.operationId && value.authorKind === 'agent' && ['question', 'handoff'].includes(value.purpose ?? ''));
          if (comment) await publishBrowserNotice(this.store.client, 'task-board', {
            title: 'Ivy · ' + task.fields.title, body: comment.body, tag: hashJson([pin.objectId, comment.commentId]),
            target: { uiId: 'task-board-ui', fragment: '#/tasks?' + new URLSearchParams({ node: this.owner.serviceNodeId, id: pin.objectId }) },
          });
          if (task.publication?.objectId === operation.pin.objectId)
            await this.coordination.release(operation.pin.objectId, true);
        },
      ),
      outcome = emptyOutcome(request.operationId);
    if (request.action === "nativeUpdate")
      return publishNative(this, operation, steps, transcript);
    if (request.action === 'archive' || request.action === 'newThread' || request.action === 'recoverThread')
      return new TaskBoardNativeLifecycle(this).publish(operation, steps, outcome);
    // Saved stages have already passed checks while holding this gate. Recovery reads the original
    // immutable Task below, not an updated aggregate changed by this action's own earlier steps.
    const first = operation.value.writes.length === 0;
    if (request.action === 'configure') {
      if (first) {
        const current = await configurationView(this.store);
        requireThat(same(current.object, request.configuration), 'revision_conflict', 'Reload the current TaskBoard settings before saving.');
        await checkConfiguration(this.store, request.value);
      }
      outcome.configuration = await steps.write('task-board/configuration', request.value, request.configuration
        ? { objectId: request.configuration.objectId, expectedRevision: request.configuration.revision }
        : { create: { parentId: this.store.rootObjectId, name: configurationName } });
      return outcome;
    }
    if (request.action === "savePlan") {
      const { draftHash, ...retained } = request,
        plan = await resolvedPlan(this.store, request.plan);
      requireThat(
        draftHash === hashJson({ ...retained, plan }),
        "task_board_native_intent_mismatch",
        "Saved parameter revisions must reconstruct the exact original plan draft.",
      );
      planSemantics(plan);
      outcome.plan = await steps.create(
        "task-board/native-plan",
        request.plan,
        "Native plan",
      );
      return outcome;
    }
    if (request.action === "create") {
      const submitted: TaskBoard.TaskFields = {
        ...request.fields,
        userContact: request.fields.userContact ?? (await configurationView(this.store)).configuration.defaults.userContact ?? "ticket",
        allowParallel: request.fields.allowParallel ?? (await configurationView(this.store)).configuration.defaults.allowParallel ?? false,
      };
      if (first && submitted.category !== null)
        requireThat(
          normalizedCategory !== undefined,
          "task_board_category_projection_pending",
          "A categorized Task needs a current canonical category before publication.",
        );
      if (first) {
        await this.checks.fields(submitted, null);
        await this.checks.executionOptions(submitted);
      }
      const initial: TaskBoard.Task = first
        ? {
            schemaVersion: 1,
            taskKey: await this.store.allocateTaskKey(operation.pin.objectId),
            fields: {
              ...submitted,
              category:
                submitted.category === null ? null : normalizedCategory!,
            },
            workflowState: "backlog",
            waiting: null,
            publication: steps.pin,
            primaryResourceRef: null,
            claim: null,
            attemptCount: 0,
            lastRun: null,
            lastHistory: null,
            latestResult: null,
            acceptedReview: null,
            comments: [],
            commentDeliveries: [],
            agentCommentCount: 0,
            workRevision: 0,
            attachments: [],
            createdAt: at,
            updatedAt: at,
          }
        : (
            await this.store.read(
              "task-board/task",
              operation.value.writes[0]!.payload.object,
              false,
            )
          ).value;
      const pin = await steps.write("task-board/task", initial, {
        create: { parentId: this.store.rootObjectId, name: initial.taskKey },
      });
      outcome.history = await steps.create(
        "task-board/history",
        this.history(
          operation,
          pin.objectId,
          null,
          "created",
          request.fields.title,
        ),
        "History",
      );
      outcome.task = await steps.write(
        "task-board/task",
        { ...initial, lastHistory: outcome.history, publication: null },
        { objectId: pin.objectId, expectedRevision: pin.revision },
      );
      return outcome;
    }
    const base = await this.store.read("task-board/task", {
        objectId: request.taskId,
        revision: request.expectedRevision,
      }),
      previous = base.value;
    if (first) {
      requireThat(
        base.metadata.currentRevision === request.expectedRevision &&
          !previous.publication,
        "revision_conflict",
        "The task changed or has an unfinished publication. Reload its current revision.",
      );
      await this.checks.aggregate(previous, request.taskId);
      await this.validateAction(request, previous, actor, at, operation.pin.objectId);
      if (request.action === 'continue') await new TaskBoardNativeLifecycle(this).restoreForExecution(operation, base);
    }
    let next: TaskBoard.Task = first
      ? { ...previous, updatedAt: at, publication: steps.pin }
      : (
          await this.store.read(
            "task-board/task",
            operation.value.writes[0]!.payload.object,
            false,
          )
        ).value;
    let kind: TaskBoard.History["kind"] = "edited",
      detail = "";
    // Start claims precede Run creation, even though publication is still pending. The gate and
    // marker prevent a native observer or another action from using a half-published claim.
    if (request.action === "start" || request.action === "continue") {
      if (!(request.action === 'continue' && request.coordinationOnly)) delete next.blocker;
      const claim: TaskBoard.Claim = {
        operationId: request.operationId,
        callerPrincipalId: operation.value.callerPrincipalId,
        attempt: previous.attemptCount + 1,
        owner: actor,
        request,
        requestHash: operation.value.requestHash,
        target: request.target,
        workspace: request.workspace,
        primaryAtStart: previous.primaryResourceRef,
        claimedAt: at,
        phase: "claimed",
        run: null,
      };
      next = {
        ...next,
        workflowState: "in_progress",
        waiting: null,
        claim,
        attemptCount: claim.attempt,
      };
    }
    if (request.action === "edit" && first) {
      const submitted: TaskBoard.TaskFields = {
        ...request.fields,
        userContact: request.fields.userContact ?? previous.fields.userContact,
        allowParallel: request.fields.allowParallel ?? previous.fields.allowParallel ?? false,
      };
      if (submitted.category !== null)
        requireThat(
          normalizedCategory !== undefined,
          "task_board_category_projection_pending",
          "A categorized Task needs a current canonical category before publication.",
        );
      next.fields = {
        ...submitted,
        category: submitted.category === null ? null : normalizedCategory!,
      };
      if (!previous.claim) next.workRevision++;
    }
    if (request.action === "reassign" && first) {
      next = {
        ...next,
        fields: {
          ...next.fields,
          executionRequirement: request.executionRequirement,
          workspaceRequirement: request.workspaceRequirement,
        },
        workflowState: "todo",
        waiting: null,
        claim: null,
        primaryResourceRef: null,
      };
    }
    if (request.action === "comment" && first) {
      const authorKind = actor.source === "user" ? "user" : actor.source === "worker" || request.nativeInput && !request.replyTo ? "agent" : "system";
      const comment: TaskBoard.Comment = {
        commentId: request.commentId, sequence: previous.comments.length + 1, author: actor, authorKind,
        body: request.body, requests: request.requests, responses: request.responses,
        ...(request.purpose ? { purpose: request.purpose } : {}),
        ...(request.nativeInput ? { nativeInput: request.nativeInput } : {}),
        ...(request.sourceTaskId ? { sourceTaskId: request.sourceTaskId } : {}),
        delivery: request.contactUser && request.purpose === "question" && previous.fields.userContact !== "ticket" ? steps.pin : null,
        attachments: request.attachments, run: previous.lastRun, turnId: null, replyTo: request.replyTo,
        createdAt: at, operationId: request.operationId,
      };
      const commentDeliveries = (authorKind === "user" || authorKind === 'agent' && !!request.sourceTaskId && request.agentDelivery === 'queue') && previous.fields.control === "agent" && request.agentDelivery !== "none" && (previous.workflowState !== 'done' || request.moveToTodo) && previous.workflowState !== 'cancelled'
        ? [...previous.commentDeliveries, { commentId: comment.commentId, state: "queued" as const,
            operationId: request.operationId, updatedAt: at, code: null }]
        : previous.commentDeliveries;
      requireThat(previous.comments.length < 512 && commentDeliveries.length <= 512,
        "content_too_large", "The Task has reached its comment capacity.");
      canonical({ ...next, comments: [...previous.comments, comment], commentDeliveries,
        lastHistory: steps.pin }, jsonObjectContentBytes - 1024);
    }
    if (request.action === "uploadAttachment" && first) {
      const bytes = Buffer.from(request.bytesBase64, "base64");
      const attachment: TaskBoard.TaskAttachment = { attachmentId: request.attachmentId, object: steps.pin,
        filename: request.filename, mediaType: request.mediaType, byteLength: bytes.length,
        contentHash: digest(bytes), uploader: actor, createdAt: at };
      canonical({ ...next, attachments: [...previous.attachments, attachment], lastHistory: steps.pin }, jsonObjectContentBytes - 1024);
    }
    if (request.action === 'defer' && request.blocker) next.blocker = request.blocker;
    const reserved = await steps.write("task-board/task", next, {
      objectId: request.taskId,
      expectedRevision: request.expectedRevision,
    });
    switch (request.action) {
      case "edit": {
        const submitted: TaskBoard.TaskFields = {
          ...request.fields,
          userContact:
            request.fields.userContact ?? previous.fields.userContact,
          allowParallel: request.fields.allowParallel ?? previous.fields.allowParallel ?? false,
        };
        if (first)
          next.fields = { ...submitted, category: next.fields.category };
        // An active claim permits only metadata edits, which must not queue another native turn.
        if (
          next.fields.control === "agent" &&
          previous.workflowState !== "cancelled" &&
          !previous.claim
        ) {
          const comment: TaskBoard.Comment = {
            commentId: request.operationId,
            sequence: previous.comments.length + 1,
            author: actor,
            authorKind: "system",
            body: "Ticket edited",
            requests: [],
            responses: [],
            delivery: null,
            attachments: [],
            run: previous.lastRun,
            turnId: null,
            replyTo: null,
            createdAt: at,
            operationId: request.operationId,
          };
          next.comments = [...previous.comments, comment];
          next.commentDeliveries = [
            ...previous.commentDeliveries,
            {
              commentId: comment.commentId,
              state: "queued",
              operationId: request.operationId,
              updatedAt: at,
              code: null,
            },
          ];
        }
        if (
          next.fields.control === "agent" &&
          ["review", "done"].includes(previous.workflowState)
        ) {
          next.workflowState = "todo";
          next.acceptedReview = null;
        }
        detail = "Updated editable task fields.";
        break;
      }
      case "transition":
        next.workflowState = request.workflowState;
        next.waiting =
          request.workflowState === "waiting"
            ? {
                reason: "user",
                detail: request.detail ?? "User-controlled work is waiting.",
                since: at,
              }
            : null;
        kind =
          request.workflowState === "todo"
            ? "ready"
            : request.workflowState === "done"
              ? "reviewed"
              : request.workflowState === "cancelled"
                ? "cancelled"
                : "edited";
        detail = request.detail ?? `Moved to ${request.workflowState}.`;
        break;
      case "defer":
        next.workflowState = "waiting";
        if (request.blocker) next.blocker = request.blocker;
        next.waiting = {
          reason: request.reason,
          detail: request.detail,
          since: at,
        };
        next.fields = { ...next.fields, nextReviewAt: request.nextReviewAt };
        kind = "deferred";
        detail = request.detail;
        break;
      case "reassign":
        next = {
          ...next,
          fields: {
            ...next.fields,
            executionRequirement: request.executionRequirement,
            workspaceRequirement: request.workspaceRequirement,
          },
          workflowState: "todo",
          waiting: null,
          claim: null,
          primaryResourceRef: null,
        };
        kind = "reassigned";
        detail = request.reason;
        break;
      case "start":
      case "continue": {
        const claim = next.claim!;
        const plan = await readPlan(this.store, request.intent);
        const run: TaskBoard.Run = {
          schemaVersion: 1,
          taskId: request.taskId,
          attempt: claim.attempt,
          operationId: request.operationId,
          callerPrincipalId: operation.value.callerPrincipalId,
          originalRequest: request,
          requestHash: hashJson(request),
          target: claim.target,
          workspace: claim.workspace,
          plan,
          owner: actor,
          phase: "starting",
          primaryResourceRef: previous.primaryResourceRef,
          turnId: null,
          nativeEpoch: null,
          calls: { thread: null, turn: null, interrupt: null },
          notificationCursor: 0,
          result: null,
          cancellation: null,
          externalOutcome: {
            state: "not_started",
            observedAt: null,
            evidence: null,
            code: null,
          },
          createdAt: at,
          updatedAt: at,
          finishedAt: null,
        };
        outcome.run = await steps.create("task-board/run", run, "Run");
        next.lastRun = outcome.run;
        next.claim = { ...claim, phase: "run_created", run: outcome.run };
        kind = request.action === "start" ? "started" : "continued";
        detail =
          request.action === "continue"
            ? request.feedback
            : "Claimed one native execution attempt.";
        break;
      }
      case "cancel": {
        detail = request.reason;
        if (!previous.claim) {
          next.workflowState = "cancelled";
          next.waiting = null;
          kind = "cancelled";
        } else {
          const run = await this.store.read(
            "task-board/run",
            previous.claim.run!,
          );
          const value: TaskBoard.Run = {
            ...run.value,
            phase: "cancel_requested",
            cancellation: {
              operationId: request.operationId,
              reason: request.reason,
              requestedAt: at,
              actor,
            },
            updatedAt: at,
          };
          outcome.run = await steps.write("task-board/run", value, {
            objectId: run.pin.objectId,
            expectedRevision: run.pin.revision,
          });
          next.claim = {
            ...previous.claim,
            phase: "cancel_requested",
            run: outcome.run,
          };
          next.lastRun = outcome.run;
          kind = "cancel_requested";
        }
        break;
      }
      case "review":
        outcome.review = await steps.create(
          "task-board/review",
          {
            schemaVersion: 1,
            taskId: request.taskId,
            result: request.result,
            acceptance: request.acceptance,
            actor,
            createdAt: at,
            operationId: request.operationId,
          },
          "Review",
        );
        next.workflowState = "done";
        next.acceptedReview = outcome.review;
        kind = "reviewed";
        detail = request.acceptance;
        break;
      case "saveResult":
        outcome.result = await steps.create(
          "task-board/result",
          {
            schemaVersion: 1,
            taskId: request.taskId,
            run: request.run,
            kind: request.kind,
            content: request.content,
            actor,
            createdAt: at,
            operationId: request.operationId,
          },
          "Result",
        );
        if (request.kind === "final") {
          next.workflowState = "review";
          next.waiting = null;
          next.latestResult = outcome.result;
          next.acceptedReview = null;
          let comment: TaskBoard.Comment = {
            commentId: request.operationId,
            sequence: previous.comments.length + 1,
            author: actor,
            authorKind: "system",
            body: (
              "Final result ready for review.\n\n" + request.content.summary
            ).slice(0, 65536),
            requests: [],
            responses: [],
            delivery: null,
            attachments: [],
            run: request.run,
            turnId: null,
            replyTo: null,
            createdAt: at,
            operationId: request.operationId,
          };
          const fits = resultCommentFits(next, comment, steps.pin);
          if (fits && previous.fields.userContact !== "ticket") {
            const delivery = await steps.create(
              "task-board/delivery",
              initialDelivery(
                this.settings,
                steps.pin,
                request.taskId,
                comment.commentId,
                previous.fields.userContact,
                at,
              ),
              "Comment delivery",
            );
            comment = { ...comment, delivery };
          }
          if (fits) {
            next.comments = [...previous.comments, comment];
            next.agentCommentCount = previous.agentCommentCount + 1;
          }
        } else if (previous.workflowState !== "review")
          next.latestResult = outcome.result;
        kind = "result_saved";
        detail = request.kind + " result saved.";
        break;
      case "comment": {
        let comment: TaskBoard.Comment = {
          commentId: request.commentId,
          sequence: previous.comments.length + 1,
          author: actor,
          authorKind:
            actor.source === "user"
              ? "user"
              : actor.source === "worker" || request.nativeInput && !request.replyTo
                ? "agent"
                : "system",
          body: request.body,
          ...(request.purpose ? { purpose: request.purpose } : {}),
          ...(request.nativeInput ? { nativeInput: request.nativeInput } : {}),
          ...(request.sourceTaskId ? { sourceTaskId: request.sourceTaskId } : {}),
          requests: request.requests,
          responses: request.responses,
          delivery: null,
          attachments: request.attachments,
          run: previous.lastRun,
          turnId: null,
          replyTo: request.replyTo,
          createdAt: at,
          operationId: request.operationId,
        };
        if (
          request.contactUser && request.purpose === "question" &&
          previous.fields.userContact !== "ticket"
        ) {
          const delivery = await steps.create(
            "task-board/delivery",
            initialDelivery(
              this.settings,
              steps.pin,
              request.taskId,
              comment.commentId,
              previous.fields.userContact,
              at,
            ),
            "Comment delivery",
          );
          comment = { ...comment, delivery };
        }
        next.comments = [...previous.comments, comment];
        kind = "commented";
        detail = "Added Task comment.";
        if (comment.authorKind === "agent")
          next.agentCommentCount = previous.agentCommentCount + 1;
        if ((comment.authorKind === "user" || peerComment(comment) && request.agentDelivery === 'queue') && (previous.workflowState !== "done" || request.moveToTodo) && previous.workflowState !== 'cancelled') {
          if (request.agentDelivery !== "none") next.workRevision++;
          if (previous.fields.control === "agent" && request.agentDelivery !== "none")
            next.commentDeliveries = [
              ...previous.commentDeliveries,
              {
                commentId: comment.commentId,
                state: "queued",
                operationId: request.operationId,
                updatedAt: at,
                code: null,
              },
            ];
          if (
            previous.fields.control === "agent" &&
            request.agentDelivery !== "none" &&
            comment.authorKind === 'user' &&
            !previous.claim &&
            ["waiting", "review", "done"].includes(previous.workflowState)
          ) {
            next.workflowState = "todo";
            next.waiting = null;
            next.acceptedReview = null;
          }
        }
        if (previous.workflowState === 'done' && request.moveToTodo) {
          next.workflowState = 'todo'; next.waiting = null; next.acceptedReview = null;
        }
        break;
      }
      case "uploadAttachment": {
        const bytes = Buffer.from(request.bytesBase64, "base64");
        const object = await this.store.writeAttachment(
          request.taskId,
          request.operationId,
          request.attachmentId,
          bytes,
        );
        const attachment: TaskBoard.TaskAttachment = {
          attachmentId: request.attachmentId,
          object,
          filename: request.filename,
          mediaType: request.mediaType,
          byteLength: bytes.length,
          contentHash: digest(bytes),
          uploader: actor,
          createdAt: at,
        };
        next.attachments = [...previous.attachments, attachment];
        outcome.attachment = attachment;
        detail = "Uploaded Task attachment.";
        break;
      }
    }
    const history = this.history(operation, request.taskId, base, kind, detail);
    history.result = outcome.result;
    history.review = outcome.review;
    if (request.action === "reassign") {
      history.fromTarget = previous.claim?.target ?? null;
      history.toTarget = null;
      history.priorRun = request.previousRun;
    }
    outcome.history = await steps.create(
      "task-board/history",
      history,
      "History",
    );
    outcome.task = await steps.write(
      "task-board/task",
      { ...next, lastHistory: outcome.history, publication: null },
      { objectId: request.taskId, expectedRevision: reserved.revision },
    );
    if (request.action === "reassign" || request.action === "review" ||
      request.action === "edit" && (!same(previous.fields.workspaceRequirement, next.fields.workspaceRequirement) || previous.fields.allowParallel !== next.fields.allowParallel) ||
      request.action === "cancel" && !previous.claim ||
      request.action === "transition" && ["done", "cancelled", "backlog"].includes(request.workflowState))
      this.store.releaseWorkspace(request.taskId);
    return outcome;
  }
  private history(
    operation: Op,
    taskId: string,
    previous: Document<"task-board/task"> | null,
    kind: TaskBoard.History["kind"],
    detail: string,
  ): TaskBoard.History {
    return {
      schemaVersion: 1,
      taskId,
      operationId: operation.value.operationId,
      actor: operation.value.actor,
      createdAt: operation.value.createdAt,
      kind,
      detail,
      previousTask: previous?.pin ?? null,
      previousHistory: previous?.value.lastHistory ?? null,
      fromTarget: null,
      toTarget: null,
      priorRun: previous?.value.lastRun ?? null,
      result: null,
      review: null,
    };
  }
  private async validateAction(
    request: Exclude<
      TaskBoard.DurableRequest,
      | TaskBoard.CreateRequest
      | TaskBoard.SavePlanRequest
      | TaskBoard.NativeUpdateRequest
      | TaskBoard.ConfigureRequest
      | TaskBoard.ArchiveRequest
      | TaskBoard.NewThreadRequest
      | TaskBoard.RecoverThreadRequest
    >,
    task: TaskBoard.Task,
    actor: TaskBoard.Actor,
    at: string,
    operationId: string,
  ): Promise<void> {
    requireThat(
      !terminal(task) ||
        (["edit", "comment", "uploadAttachment"].includes(request.action) || request.action === "transition" &&
          (request.workflowState === "backlog" || task.workflowState === "done" && request.workflowState === "todo")),
      "task_board_invalid_transition",
      "This task is terminal for the requested action.",
    );
    if (actor.source === "scheduler")
      requireThat(
        task.fields.control === "agent" &&
          ["start", "continue", "defer", "transition"].includes(
            request.action,
          ) &&
          (["todo", "waiting"].includes(task.workflowState) || request.action === 'continue' && request.coordinationOnly === true && coordinationPending(task)) &&
          (!task.waiting || request.action === 'continue' && request.coordinationOnly === true && coordinationPending(task) ||
            request.action === 'continue' && task.waiting.reason === 'user' && await unstartedArchivedContext(this,
              await this.store.read('task-board/task', { objectId: request.taskId, revision: request.expectedRevision })) ||
            ["time", "dependency", "host", "capability", "workspace"].includes(
              task.waiting.reason,
            )),
        "task_board_user_controlled",
        "Automatic actions require an agent-controlled Todo Task or a resumable wait.",
      );
    const noClaim = () =>
      requireThat(
        !task.claim,
        "task_board_claim_active",
        "Resolve the existing execution claim before this action.",
      );
    switch (request.action) {
      case "edit": {
        const fields: TaskBoard.TaskFields = {
          ...request.fields,
          userContact: request.fields.userContact ?? task.fields.userContact,
          allowParallel: request.fields.allowParallel ?? task.fields.allowParallel ?? false,
        };
        // Before the first attempt the host and workspace are plain ticket fields; afterwards the
        // native context lives there, so moving it is the explicit reassign workflow.
        requireThat(
          !started(task) ||
            (same(task.fields.executionRequirement, fields.executionRequirement) &&
              same(task.fields.workspaceRequirement, fields.workspaceRequirement)),
          "task_board_reassign_required",
          "Host and workspace are fixed once the task has run; use the reassign workflow to move it.",
        );
        if (task.claim)
          for (const key of [
            "title",
            "description",
            "acceptanceCriteria",
            "control",
            "dependencies",
            "requiredCapabilities",
            "executionRequirement",
            "nativeOptions",
            "workspaceRequirement",
            "allowParallel",
          ] as const)
            requireThat(
              same(task.fields[key], fields[key]),
              "task_board_claim_active",
              "An active claim prevents changes to the running assignment.",
            );
        await this.checks.fields(fields, request.taskId);
        if (fields.control !== task.fields.control ||
          !same(fields.executionRequirement, task.fields.executionRequirement) ||
          !same(fields.nativeOptions ?? null, task.fields.nativeOptions ?? null))
          await this.checks.executionOptions(fields, task);
        break;
      }
      case "transition": {
        noClaim();
        await this.checks.fields(task.fields, request.taskId);
        const legal: Record<
          TaskBoard.Task["workflowState"],
          TaskBoard.Task["workflowState"][]
        > = {
          // Any unclaimed, unfinished or finished Task can return to the unscheduled Backlog.
          backlog: ["todo", "cancelled"],
          todo: ["backlog", "in_progress", "waiting", "cancelled"],
          in_progress: ["backlog", "waiting", "done", "cancelled"],
          waiting: ["backlog", "todo", "in_progress", "done", "cancelled"],
          review: ["backlog", "done", "todo", "cancelled"],
          done: ["backlog", "todo"],
          cancelled: ["backlog"],
        };
        requireThat(
          legal[task.workflowState].includes(request.workflowState),
          "task_board_invalid_transition",
          "Requested workflow destination is not legal from the current state.",
        );
        requireThat(
          task.fields.control === "user" ||
            request.workflowState !== "in_progress",
          "task_board_user_controlled",
          "Starting agent execution is owned by TaskBoard.",
        );
        break;
      }
      case "start":
      case "continue":
        noClaim();
        requireThat(
          ["todo", "waiting"].includes(task.workflowState) || request.action === 'continue' && request.coordinationOnly === true && coordinationPending(task),
          "task_board_invalid_transition",
          "Automatic execution starts from Todo or a resumable wait.",
        );
        if (request.action === "continue")
          requireThat(
            task.primaryResourceRef,
            "task_board_primary_required",
            "Continuation requires the saved primary native context.",
          );
        requireThat(
          task.fields.control === "agent" && actor.source === "scheduler",
          "task_board_user_controlled",
          "Native Task allocation is automatic for agent-controlled work.",
        );
        {
          const coordinationOnly = request.action === 'continue' && request.coordinationOnly === true;
          if (coordinationOnly) requireThat(coordinationPending(task) && request.commentIds.length > 0 && request.commentIds.every(id => task.commentDeliveries.some(delivery => delivery.commentId === id && delivery.state === 'queued') && task.comments.some(comment => comment.commentId === id && peerComment(comment))),
            'task_board_invalid_transition', 'Coordination requires queued directed task messages and a saved primary.');
          const plan = await this.checks.execution(
            task,
            request.intent,
            request.taskId,
            at,
            request.target,
            request.workspace,
            coordinationOnly,
          );
          const input = plan.turnStart.input;
          requireThat(
            Array.isArray(input) && input.length > 0,
            "task_board_input_required",
            "A native execution plan must contain its explicit first-turn input.",
          );
          if (request.action === "continue" && actor.source !== "scheduler")
            requireThat(
              input.filter(
                (item) =>
                  item &&
                  typeof item === "object" &&
                  !Array.isArray(item) &&
                  item.type === "text" &&
                  item.text === request.feedback,
              ).length === 1,
              "task_board_feedback_missing",
              "The continuation plan must include the exact user feedback exactly once as native text input.",
            );
        }
        if (reservesWorkspace(task.fields.workspaceRequirement)) {
          const owner = await this.store.reserveAvailableWorkspace(
            request.target.hostId,
            request.workspace.intendedPath,
            request.taskId,
            operationId,
          );
          requireThat(
            !owner,
            "task_board_workspace_reserved",
            `Workspace is already reserved by Task ${owner}.`,
          );
        } else {
          const owner = await sharedProjectOwner(this.store, task, request.taskId, request.target, request.workspace);
          requireThat(!owner, "task_board_workspace_reserved", `Project is already in use by Task ${owner}.`);
          const key = sharedProjectKey(task.fields.workspaceRequirement, request.workspace);
          if (key && !task.fields.allowParallel) {
            const reserved = await this.store.reserveAvailableWorkspace(request.target.hostId, key, request.taskId, operationId);
            requireThat(!reserved, "task_board_workspace_reserved", `Project is already reserved by Task ${reserved}.`);
          }
        }
        break;
      case "defer":
        if (!request.blocker) noClaim();
        else {
          requireThat(request.reason === 'dependency' || request.reason === 'workspace', 'invalid_arguments', 'Task blockers require a dependency or workspace reason.');
          await validateBlocker(this.store, request.blocker, request.taskId);
        }
        requireThat(
          (["todo", "waiting"].includes(task.workflowState) || !!request.blocker && task.workflowState === 'in_progress') &&
            task.waiting?.reason !== "external_outcome",
          "task_board_invalid_transition",
          "This task cannot be deferred while its outcome is unresolved.",
        );
        date(request.nextReviewAt);
        requireThat(
          request.reason !== "time" ||
            (request.nextReviewAt && request.nextReviewAt > at),
          "task_board_invalid_date",
          "A time deferral needs a future next-review date.",
        );
        break;
      case "reassign":
        await this.checks.fields(
          {
            ...task.fields,
            executionRequirement: request.executionRequirement,
            workspaceRequirement: request.workspaceRequirement,
          },
          request.taskId,
        );
        await this.checks.executionOptions({
          ...task.fields,
          executionRequirement: request.executionRequirement,
          workspaceRequirement: request.workspaceRequirement,
        });
        requireThat(
          same(request.previousRun, task.lastRun),
          "task_board_prior_run_mismatch",
          "Reassignment must acknowledge the exact saved prior attempt.",
        );
        if (task.claim || task.waiting?.reason === "external_outcome")
          requireThat(
            request.previousRun && request.acknowledgeUnresolved,
            "task_board_unresolved_ack_required",
            "The unresolved prior attempt requires its exact Run pin and explicit acknowledgement.",
          );
        break;
      case "cancel":
        if (task.claim) {
          requireThat(
            task.claim.run,
            "task_board_publication_pending",
            "Recover the existing claim's Run before cancelling.",
          );
          const run = await this.store.read("task-board/run", task.claim.run);
          requireThat(
            run.value.taskId === request.taskId &&
              !run.value.cancellation &&
              !["completed", "failed", "cancelled"].includes(run.value.phase),
            "task_board_run_mismatch",
            "Reconcile the existing Run or cancellation before another cancellation intent.",
          );
        } else
          requireThat(
            task.waiting?.reason !== "external_outcome",
            "task_board_outcome_unresolved",
            "An unresolved external outcome cannot be declared cancelled.",
          );
        break;
      case "review": {
        noClaim();
        requireThat(
          task.workflowState === "review" &&
            same(task.latestResult, request.result),
          "task_board_result_mismatch",
          "Accept only the task's exact saved final Result.",
        );
        const result = await this.store.read("task-board/result", request.result);
        requireThat(
          result.value.taskId === request.taskId &&
            result.value.kind === "final",
          "task_board_result_mismatch",
          "The acceptance Result must be final and belong to this task.",
        );
        break;
      }
      case "saveResult":
        if (request.run) {
          const run = await this.store.read("task-board/run", request.run);
          requireThat(
            run.value.taskId === request.taskId &&
              (!task.claim || same(task.claim.run, request.run)),
            "task_board_run_mismatch",
            "Material must identify this task's exact current claimed Run.",
          );
        } else
          requireThat(
            !task.claim,
            "task_board_run_required",
            "Material for an active attempt must include its exact Run pin.",
          );
        await this.checks.content(request.content);
        if (request.kind === "final") {
          noClaim();
          requireThat(
            task.fields.control === "user" && actor.source === "user",
            "task_board_native_evidence_required",
            "Only user-controlled manual work can finish without owner-observed native evidence.",
          );
        }
        break;
      case "comment":
        if (request.sourceTaskId) {
          requireThat(actor.source === 'worker' && request.sourceTaskId !== request.taskId && !request.moveToTodo, 'task_board_forbidden', 'Directed agent comments must identify another source task and cannot reopen Done.');
          const source = await this.store.read('task-board/task', request.sourceTaskId);
          requireThat(source.value.fields.control === 'agent' && !!source.value.claim, 'task_board_forbidden', 'Directed messages require an active source task.');
        }
        requireThat(!request.nativeInput || actor.source === 'native', 'task_board_forbidden', 'Native input observations are recorded only by TaskBoard.');
        requireThat(!request.contactUser || request.purpose === 'question', 'invalid_arguments', 'Only important questions can request user contact.');
        requireThat(taskAttachmentPins({ ...task, attachments: [...task.attachments, ...request.attachments] }).length <= 254,
          "limit_exceeded", "A Task can retain at most 254 attachments alongside its Run and final Result.");
        for (const attachment of request.attachments)
          await this.checks.artifact({
            object: attachment.object,
            label: attachment.filename,
            mediaType: "application/octet-stream",
            contentHash: attachment.contentHash,
            byteLength: attachment.byteLength,
          });
        requireThat(
          !task.comments.some(
            (value) => value.commentId === request.commentId,
          ) &&
            (!request.replyTo ||
              task.comments.some(
                (value) => value.commentId === request.replyTo,
              )),
          "task_board_comment_mismatch",
          "Comment identity or reply target is invalid.",
        );
        {
          const requestIds = new Set(
            task.comments.flatMap((value) =>
              value.requests.map((item) => item.requestId),
            ),
          );
          const responseIds = new Set(
            task.comments.flatMap((value) =>
              value.responses.map((item) => item.requestId),
            ),
          );
          for (const item of request.requests) {
            requireThat(
              !requestIds.has(item.requestId),
              "mutation_conflict",
              "Request identity is already used.",
            );
            requestIds.add(item.requestId);
          }
          for (const item of request.responses) {
            requireThat(
              requestIds.has(item.requestId),
              "task_board_response_request_missing",
              "A typed response must reference an existing typed request.",
            );
            requireThat(
              !responseIds.has(item.requestId),
              "task_board_request_already_answered",
              "This typed request already has a response.",
            );
            responseIds.add(item.requestId);
          }
        }
        break;
      case "uploadAttachment":
        requireThat(task.attachments.length < 256 && taskAttachmentPins(task).length < 254,
          "limit_exceeded", "A Task can retain at most 254 attachments alongside its Run and final Result.");
        {
          const bytes = Buffer.from(request.bytesBase64, "base64");
          requireThat(bytes.length <= 8 * 1024 * 1024 && bytes.toString("base64") === request.bytesBase64,
            "invalid_arguments", "Attachment must be canonical base64 up to 8 MiB.");
        }
        requireThat(
          !task.attachments.some(
            (value) => value.attachmentId === request.attachmentId,
          ),
          "mutation_conflict",
          "Attachment identity is already used.",
        );
        break;
    }
  }
}
