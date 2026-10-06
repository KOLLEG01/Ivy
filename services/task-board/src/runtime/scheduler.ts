import { taskPrompt } from './task-prompt.js';
import { checkBlocker, coordinationPending, peerComment } from './task-coordination.js';
import { hashJson } from "../../../../packages/sdk/src/node.js";
import { IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import type { TaskBoard } from "../../../../packages/sdk/src/node.js";
import type { Agent } from "../../../../packages/sdk/src/node.js";
import { checkedNativeContract } from "../../../../packages/sdk/src/node.js";
import { nativeThreadProject } from "../../../../packages/sdk/src/node.js";
import {
  saveNativePlan,
  validateNativePlanDraft,
} from "../../../../packages/sdk/src/native-plan.js";
import {
  scopedOperationId,
  BoundToolClient,
} from "../../../../packages/sdk/src/client.js";
import { planSemantics, reservesWorkspace, sharedProjectOwner } from "./checks.js";
import type { TaskBoardEngine } from "./engine.js";
import { same } from "./checks.js";
import { mutation } from "./store.js";
import type { Document } from "./store.js";
import { effectiveExecution, supportsExecutionOptions } from './configuration.js';
import { freshContextRecovery, unstartedArchivedContext, unstartedMissingContext } from './native-recovery.js';

/** Retains an automatic action before invocation and never replaces an uncertain Operation. */
export class TaskBoardScheduler {
  constructor(
    readonly engine: TaskBoardEngine,
    readonly now = () => new Date(),
  ) {}
  async allocate(
    task: Document<"task-board/task">,
    operationId: string,
    feedback?: string,
  ): Promise<{
    target: TaskBoard.Target;
    workspace: TaskBoard.WorkspaceResolution;
    intent: TaskBoard.ObjectPin;
  }> {
    const recovery = await freshContextRecovery(this.engine.store, task.value);
    const effective = await effectiveExecution(this.engine.store, task.value);
    const requirement = effective.executionRequirement?.kind === 'automatic' ? null : effective.executionRequirement;
    // Every node that could have run the Task but did not records why, so the waiting ticket
    // names the actual obstacle instead of a generic unavailability.
    const skipped: string[] = [];
    const nodes = (
      await this.engine.store.client.request("serviceNodes.list", {
        serviceName: "agent-manager",
        limit: 200,
      })
    ).items.filter((value) => {
      if (requirement?.kind === "host" && value.hostId !== requirement.hostId) return false;
      if (recovery && (value.hostId !== recovery.target.hostId || value.serviceNodeId !== recovery.target.serviceNodeId)) {
        skipped.push(`${value.hostId}: recovery must use AgentManager ${recovery.target.serviceNodeId}`);
        return false;
      }
      if (task.value.primaryResourceRef && value.serviceNodeId !== task.value.primaryResourceRef.serviceNodeId) {
        skipped.push(`${value.hostId}: the task conversation belongs to AgentManager ${task.value.primaryResourceRef.serviceNodeId}`);
        return false;
      }
      if (!value.connected || !value.synced || !value.ready) {
        skipped.push(`${value.hostId}: AgentManager is ${!value.connected ? "disconnected" : !value.synced ? "not synchronized" : "not ready"}`);
        return false;
      }
      return true;
    });
    const candidates: {
      node: (typeof nodes)[number];
      status: Agent.Status;
      catalog: Agent.Catalog;
      workspace: Agent.WorkspaceResolution;
    }[] = [];
    let workspaceFailure: IvyError | null = null;
    let unsupportedOptions = false;
    // Every listed dependency must be advertised by the same host that runs the Task.
    const requiredCapabilities = task.value.fields.requiredCapabilities ?? [];
    let capabilityGap: { hostId: string; missing: string[] } | null = null;
    for (const node of nodes)
      try {
        // New plans discover current contracts; retained Runs keep their original definitions.
        const tools = new BoundToolClient(
          this.engine.store.client,
          node.serviceNodeId,
          [{ namespace: "agent", interfaceVersion: "1.0.0" }],
        );
        const status = (await tools.read("agent.status", {})) as Agent.Status;
        if (
          status.state !== "ready" ||
          status.hostId !== node.hostId ||
          status.serviceNodeId !== node.serviceNodeId
        ) {
          skipped.push(`${node.hostId}: native agent is ${status.state}`);
          continue;
        }
        const profile = (await tools.read(
          "agent.capabilities",
          {},
        )) as Agent.CapabilityProfile;
        if (requirement?.kind === "host" && node.hostId !== requirement.hostId)
          continue;
        const missing = requiredCapabilities.filter(
          (key) => !profile.capabilities.some((value) => value.key === key),
        );
        if (missing.length) {
          capabilityGap ??= { hostId: node.hostId, missing };
          continue;
        }
        const workspace = (await tools.call("agent.resolveWorkspace", {
          taskKey: task.value.taskKey,
          requirement: task.value.fields.workspaceRequirement,
          prepare: false,
        })) as Agent.WorkspaceResolution;
        if (recovery) requireThat(workspace.canonicalCwd === recovery.workspace.canonicalCwd && workspace.intendedPath === recovery.workspace.intendedPath,
          'task_board_workspace_mismatch', 'Fresh recovery must retain the original workspace.');
        const catalog = (await tools.read(
          "agent.catalog",
          {},
        )) as Agent.Catalog;
        if (!await supportsExecutionOptions(this.engine.store.client, node, effective.nativeOptions)) { unsupportedOptions = true; continue; }
        candidates.push({ node, status, catalog, workspace });
      } catch (error) {
        if (
          error instanceof IvyError &&
          [
            "not_found",
            "target_conflict",
            "task_board_project_unavailable",
            "task_board_project_mismatch",
            "task_board_workspace_mismatch",
          ].includes(error.code)
        )
          workspaceFailure ??= error;
        else
          skipped.push(`${node.hostId}: ${error instanceof Error ? error.message : String(error)}`);
      }
    if (candidates.length === 0 && workspaceFailure) throw workspaceFailure;
    requireThat(
      candidates.length > 0,
      unsupportedOptions ? "task_board_model_unavailable" : capabilityGap ? "task_board_capability_unavailable" : "task_board_target_unavailable",
      unsupportedOptions ? "No eligible ready host supports the selected model, reasoning and speed."
        : capabilityGap
        ? `${capabilityGap.hostId} does not advertise ${capabilityGap.missing.join(", ")}.`
        : (requirement?.kind === "host"
          ? `No ready AgentManager is available on ${requirement.hostId}.`
          : requiredCapabilities.length
            ? `No ready AgentManager advertises ${requiredCapabilities.join(", ")}.`
            : "No ready AgentManager is currently available.") +
          (skipped.length ? " " + skipped.slice(0, 3).join("; ") + "." : ""),
    );
    const runs = new Map<string, number>();
    let cursor: string | undefined;
    do {
      const page = await this.engine.store.page("task-board/run", {
        limit: 50,
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
        ...(cursor ? { cursor } : {}),
      });
      for (const item of page.items) {
        const run = await this.engine.store.read("task-board/run", {
          objectId: item.objectId,
          revision: item.revision,
        });
        runs.set(
          run.value.target.serviceNodeId,
          (runs.get(run.value.target.serviceNodeId) ?? 0) + 1,
        );
      }
      cursor = page.nextCursor ?? undefined;
    } while (cursor);
    candidates.sort((a, b) => {
      const aProject =
        task.value.fields.workspaceRequirement.kind === "existing_project" &&
        !!a.workspace.project
          ? 1
          : 0;
      const bProject =
        task.value.fields.workspaceRequirement.kind === "existing_project" &&
        !!b.workspace.project
          ? 1
          : 0;
      return (
        bProject - aProject ||
        (runs.get(a.node.serviceNodeId) ?? 0) -
          (runs.get(b.node.serviceNodeId) ?? 0) ||
        a.node.serviceNodeId.localeCompare(b.node.serviceNodeId)
      );
    });
    let selected: (typeof candidates)[number] | undefined;
    for (const candidate of candidates) {
      const owner = reservesWorkspace(task.value.fields.workspaceRequirement)
        ? await this.engine.store.workspaceOwner(candidate.node.hostId, candidate.workspace.intendedPath, task.pin.objectId)
        : await sharedProjectOwner(this.engine.store, task.value, task.pin.objectId,
            { hostId: candidate.node.hostId, serviceNodeId: candidate.node.serviceNodeId }, candidate.workspace);
      if (!owner) { selected = candidate; break; }
      workspaceFailure = new IvyError("task_board_workspace_reserved", `Workspace is already reserved by Task ${owner}.`, "not_executed", { blockerTaskId: owner });
    }
    if (!selected) throw workspaceFailure!;
    const tools = new BoundToolClient(
        this.engine.store.client,
        selected.node.serviceNodeId,
        [
          { namespace: "agent", interfaceVersion: "1.0.0" },
          { namespace: "codex", interfaceVersion: selected.catalog.version },
        ],
      );
    const workspace = (await tools.call(
      "agent.resolveWorkspace",
      {
        taskKey: task.value.taskKey,
        requirement: task.value.fields.workspaceRequirement,
        prepare: true,
      },
      operationId,
    )) as Agent.WorkspaceResolution;
    const owner = reservesWorkspace(task.value.fields.workspaceRequirement) ? await this.engine.store.workspaceOwner(
      selected.node.hostId, workspace.intendedPath, task.pin.objectId,
    ) : await sharedProjectOwner(this.engine.store, task.value, task.pin.objectId,
      { hostId: selected.node.hostId, serviceNodeId: selected.node.serviceNodeId }, workspace);
    if (owner) throw new IvyError("task_board_workspace_reserved", `Workspace is already reserved by Task ${owner}.`, "not_executed", { blockerTaskId: owner });
    if (recovery) requireThat(workspace.canonicalCwd === recovery.workspace.canonicalCwd && workspace.intendedPath === recovery.workspace.intendedPath,
      'task_board_workspace_mismatch', 'Fresh recovery must retain the original workspace after preparation.');
    const names = [
      "thread/start",
      "thread/resume",
      "turn/start",
      "turn/interrupt",
    ] as const;
    const bindings = await Promise.all(
      names.map((name) => tools.binding("codex." + name)),
    );
    const [threadStart, threadResume, turnStart, turnInterrupt] = bindings;
    requireThat(
      threadStart && threadResume && turnStart && turnInterrupt,
      "tool_not_found",
      "The selected AgentManager is missing required native tools.",
    );
    const text = taskPrompt(task.value, this.engine.owner.serviceNodeId),
      nativeOptions = effective.nativeOptions,
      draft: Agent.PlanDraft = {
        nativeVersion: selected.catalog.version,
        catalogSourceHash: selected.catalog.sourceHash,
        location: {
          serviceNodeId: selected.node.serviceNodeId,
          hostId: selected.node.hostId,
          kind:
            (task.value.fields.workspaceRequirement.kind === "existing_project" && !workspace.useWorktree) || task.value.fields.workspaceRequirement.kind === "directory_path"
              ? "existing"
              : ["repository_path", "new_project_path"].includes(task.value.fields.workspaceRequirement.kind)
                ? "normal"
                : "internal",
          cwd: workspace.canonicalCwd,
          projectId:
            workspace.nativeProjectId ?? workspace.project?.nativeId ?? "task-board-" + task.value.taskKey,
        },
        definitions: {
          threadStart: threadStart.definitionHash,
          threadResume: threadResume.definitionHash,
          turnStart: turnStart.definitionHash,
          turnInterrupt: turnInterrupt.definitionHash,
        },
        threadStart: {
          cwd: workspace.canonicalCwd,
          ...(workspace.nativeProjectId ? { projectId: workspace.nativeProjectId } : {}),
          ...(nativeOptions?.model ? { model: nativeOptions.model } : {}),
          serviceTier: nativeOptions.serviceTier === 'fast' || nativeOptions.serviceTier === 'flex' ? nativeOptions.serviceTier : null,
        },
        threadResume: { cwd: workspace.canonicalCwd },
        turnStart: {
          input: [
            { type: "text", text, text_elements: [] },
          ],
          ...(nativeOptions?.model ? { model: nativeOptions.model } : {}),
          ...(nativeOptions?.reasoningEffort
            ? { effort: nativeOptions.reasoningEffort }
            : {}),
          serviceTier: nativeOptions.serviceTier === 'fast' || nativeOptions.serviceTier === 'flex' ? nativeOptions.serviceTier : null,
        },
      };
    draft.threadStart = await nativeThreadProject(this.engine.store.client, selected.node.serviceNodeId, draft.threadStart);
    if (typeof draft.threadStart['projectId'] === 'string' && draft.threadStart['projectId'])
      draft.location!.projectId = draft.threadStart['projectId'];
    const contract = checkedNativeContract(draft.nativeVersion, {
      sourceHash: draft.catalogSourceHash,
    }).contract;
    validateNativePlanDraft(contract, draft, "task-board");
    planSemantics(draft);
    const plan = await saveNativePlan(this.engine.store.client, contract, {
      draft,
      kind: "task-board",
      parentId: this.engine.store.rootObjectId,
      mutationId: operationId + ":parameters",
    });
    const name =
      "Allocated plan " + task.value.taskKey + " revision " + task.pin.revision + " operation " + operationId;
    const existing = await this.engine.store.named(
      "task-board/native-plan",
      name,
    );
    const intent =
      existing?.pin ??
      (await this.engine.store.write(
        "task-board/native-plan",
        plan,
        mutation(operationId, "plan"),
        { create: { parentId: this.engine.store.rootObjectId, name } },
      ));
    const { nativeProjectId: _nativeProjectId, ...storedWorkspace } = workspace;
    return {
      target: {
        hostId: selected.node.hostId,
        serviceNodeId: selected.node.serviceNodeId,
      },
      workspace: storedWorkspace as TaskBoard.WorkspaceResolution,
      intent,
    };
  }
  private async proposed(
    task: Document<"task-board/task">,
    sequence: number,
    recoverThread = false,
  ): Promise<TaskBoard.SchedulerRequest | null> {
    const value = task.value,
      base = {
        operationId: "scheduler-pending",
        taskId: task.pin.objectId,
        expectedRevision: task.pin.revision,
      };
    let waiting: Pick<
      TaskBoard.DeferRequest,
      "reason" | "detail" | "nextReviewAt"
    > & { blocker?: TaskBoard.Blocker } | null = null;
    const operationId = await scopedOperationId(this.engine.store.client, [
      "task-board-scheduler",
      task.pin.objectId,
      task.pin.revision,
      sequence,
    ]);
    if (recoverThread) return { action: 'recoverThread', ...base, operationId, run: value.lastRun! };
    const coordinationOnly = coordinationPending(value);
    const comments = value.commentDeliveries
      .filter((item) => item.state === "queued")
      .map((item) => item.commentId)
      .filter(id => !coordinationOnly || value.comments.some(comment => comment.commentId === id && peerComment(comment)));
    const feedback = taskPrompt(value, this.engine.owner.serviceNodeId, coordinationOnly);
    let allocation: Awaited<ReturnType<TaskBoardScheduler["allocate"]>> | null =
      null;
    try {
      if (!coordinationOnly) {
      requireThat(
        !value.fields.nextReviewAt ||
          value.fields.nextReviewAt <= this.now().toISOString(),
        "task_board_time_waiting",
        "The task is deferred until its next review time.",
      );
      await this.engine.checks.dependencies(
        value.fields,
        task.pin.objectId,
        true,
      );
      await checkBlocker(this.engine.store, value.blocker);
      }
      allocation = await this.allocate(
        task,
        operationId,
        value.primaryResourceRef ? feedback : undefined,
      );
    } catch (error) {
      if (!(error instanceof IvyError)) throw error;
      let reason: TaskBoard.DeferRequest["reason"];
      if (error.code === "task_board_time_waiting") reason = "time";
      else if (error.code === "task_board_dependency_waiting")
        reason = "dependency";
      else if (error.code === "task_board_capability_unavailable") reason = "capability";
      else if (["task_board_target_unavailable", "task_board_model_unavailable"].includes(error.code))
        reason = "host";
      else if (
        ["service_not_ready", "service_unavailable"].includes(error.code)
      )
        reason = "host";
      else if (
        [
          "not_found",
          "target_conflict",
          "task_board_project_unavailable",
          "task_board_project_mismatch",
          "task_board_workspace_reserved",
          "task_board_workspace_mismatch",
        ].includes(error.code)
      )
        reason = "workspace";
      else if (error.code === "task_board_input_required") reason = "user";
      else throw error;
      const detail =
        ["task_board_model_unavailable", "task_board_capability_unavailable"].includes(error.code) ? error.message.slice(0, 2048)
          : reason === "time" && value.fields.nextReviewAt
          ? `Deferred until ${value.fields.nextReviewAt}.`
          : error.message.slice(0, 2048);
      const blockerTaskId = error.details && typeof error.details === 'object' && 'blockerTaskId' in error.details
        ? error.details.blockerTaskId : null;
      waiting = { reason, detail, nextReviewAt: value.fields.nextReviewAt,
        ...(reason === 'workspace' && typeof blockerTaskId === 'string' ? { blocker: { taskId: blockerTaskId, until: 'idle' as const } } : {}) };
    }
    if (waiting) {
      if (coordinationOnly) return null;
      if (
        value.workflowState === 'waiting' &&
        value.waiting?.reason === waiting.reason &&
        value.waiting.detail === waiting.detail &&
        same(value.blocker ?? null, waiting.blocker ?? value.blocker ?? null)
      )
        return null;
      return { action: "defer", ...base, operationId, ...waiting };
    }
    requireThat(
      allocation,
      "task_board_target_unavailable",
      "No resolved execution allocation is available.",
    );
    return value.primaryResourceRef
      ? {
          action: "continue",
          ...base,
          operationId,
          feedback,
          ...allocation,
          commentIds: comments,
          ...(coordinationOnly ? { coordinationOnly: true } : {}),
        }
      : {
          action: "start",
          ...base,
          operationId,
          ...allocation,
          commentIds: comments,
        };
  }
  private async verifyAttempt(
    document: Document<"task-board/scheduler-attempt">,
    task: Document<"task-board/task">,
  ) {
    const value = document.value;
    requireThat(
      document.metadata.parentId === task.pin.objectId &&
        same(value.task, task.pin) &&
        value.principalId === this.engine.settings.principalId &&
        value.request.taskId === task.pin.objectId &&
        value.request.expectedRevision === task.pin.revision &&
        value.requestHash === hashJson(value.request),
      "task_board_identity_conflict",
      "Automatic attempt must retain its exact pinned task, principal and original request.",
    );
    if (value.sequence > 1) {
      requireThat(
        value.previous && value.previousOperation,
        "task_board_identity_conflict",
        "A successor needs both original predecessor pins.",
      );
      const previous = await this.engine.store.read(
          "task-board/scheduler-attempt",
          value.previous,
          false,
        ),
        operation = await this.engine.store.read(
          "task-board/operation",
          value.previousOperation,
        );
      requireThat(
        previous.pin.objectId === document.pin.objectId &&
          previous.pin.revision < document.pin.revision &&
          previous.value.sequence + 1 === value.sequence &&
          same(previous.value.task, value.task) &&
          previous.value.requestHash === hashJson(previous.value.request) &&
          previous.value.principalId === value.principalId &&
          operation.value.callerPrincipalId === value.principalId &&
          operation.value.requestHash === previous.value.requestHash &&
          operation.value.operationId === previous.value.request.operationId,
        "task_board_identity_conflict",
        "Scheduler succession must retain the exact previous attempt and its original Operation.",
      );
      this.retryable(operation);
      requireThat(
        Date.parse(value.preparedAt) >=
          Date.parse(operation.value.updatedAt) +
            Math.max(30000, this.engine.settings.scheduler.intervalMs),
        "task_board_retry_delay",
        "A successor must retain the minimum delay after its conclusive predecessor failure.",
      );
    }
  }
  private retryable(operation: Document<"task-board/operation">) {
    requireThat(
      operation.value.phase === "failed" &&
        operation.value.error?.execution === "not_executed" &&
        operation.value.writes.every((write) => write.outcome === null),
      "task_board_outcome_unresolved",
      "Only a conclusively failed, unapplied original operation permits another automatic attempt.",
    );
  }
  async task(taskId: string): Promise<TaskBoard.ActionOutcome | null> {
    await this.engine.verifyOwner();
    if (!this.engine.settings.scheduler.enabled) return null;
    const store = this.engine.store,
      task = await store.read("task-board/task", taskId),
      value = task.value;
    if (
      value.fields.control !== "agent" ||
      value.claim ||
      value.publication ||
      (!["todo", "waiting"].includes(value.workflowState) && !coordinationPending(value)) ||
      (value.waiting && !coordinationPending(value) &&
        !["user", "time", "dependency", "host", "capability", "workspace"].includes(value.waiting.reason))
    )
      return null;
    const recoveryAllowed = !value.blocker && (!value.fields.nextReviewAt || value.fields.nextReviewAt <= this.now().toISOString()) &&
      (!value.waiting || value.waiting.reason === 'user');
    const recoverThread = recoveryAllowed && await unstartedMissingContext(this.engine, task);
    const restoreArchived = recoveryAllowed && !recoverThread && value.waiting?.reason === 'user' && await unstartedArchivedContext(this.engine, task);
    if (recoverThread || restoreArchived) {
      const run = await store.read('task-board/run', value.lastRun!);
      if (this.now().getTime() < Date.parse(run.value.finishedAt!) + Math.max(30000, this.engine.settings.scheduler.intervalMs)) return null;
    } else if (value.waiting?.reason === 'user' && !coordinationPending(value)) return null;
    const freeBytes = store.localUsage().freeBytes;
    requireThat(freeBytes === null || freeBytes >= 512 * 1024 * 1024,
      "task_board_storage_low", "New agent work is paused until the TaskBoard journal has at least 512 MiB free.");
    const name =
      "Scheduled task " + task.pin.objectId + " revision " + task.pin.revision;
    let attempt = await store.named("task-board/scheduler-attempt", name, taskId),
      previousOperation: Document<"task-board/operation"> | null = null;
    if (attempt) {
      await this.verifyAttempt(attempt, task);
      const operation = await this.engine.operations.find(
        this.engine.settings.principalId,
        attempt.value.request.operationId,
      );
      if (!operation) return this.engine.schedule(attempt.value.request);
      requireThat(
        operation.value.requestHash === attempt.value.requestHash,
        "task_board_identity_conflict",
        "The original scheduler Operation must match its prepared intent.",
      );
      if (operation.value.phase === "succeeded") return operation.value.outcome;
      if (operation.value.phase !== "failed")
        return this.engine.recoverOperation(operation.pin.objectId);
      this.retryable(operation);
      if (
        this.now().getTime() <
        Date.parse(operation.value.updatedAt) +
          Math.max(30000, this.engine.settings.scheduler.intervalMs)
      )
        return null;
      previousOperation = operation;
    }
    const sequence = (attempt?.value.sequence ?? 0) + 1;
    const proposed = await this.proposed(task, sequence, recoverThread);
    if (!proposed) return null;
    const request = proposed;
    const next: TaskBoard.SchedulerAttempt = {
      schemaVersion: 1,
      principalId: this.engine.settings.principalId,
      task: task.pin,
      sequence,
      request,
      requestHash: hashJson(request),
      previous: attempt?.pin ?? null,
      previousOperation: previousOperation?.pin ?? null,
      preparedAt: this.now().toISOString(),
    };
    await this.engine.verifyOwner();
    try {
      const pin = await store.write(
        "task-board/scheduler-attempt",
        next,
        attempt
          ? mutation(name, "successor:" + hashJson({ pin: attempt.pin, next }))
          : mutation(name, "prepare"),
        attempt
          ? {
              objectId: attempt.pin.objectId,
              expectedRevision: attempt.pin.revision,
            }
          : { create: { parentId: taskId, name } },
      );
      attempt = await store.read("task-board/scheduler-attempt", pin, false);
    } catch (error) {
      if (!(
        error instanceof IvyError &&
        ["mutation_conflict", "revision_conflict"].includes(error.code)
      ))
        throw error;
      attempt = await store.named("task-board/scheduler-attempt", name, taskId);
      requireThat(
        attempt,
        "task_board_identity_conflict",
        "The competing prepared scheduler action must remain available.",
      );
    }
    await this.verifyAttempt(attempt, task);
    store.completeLocalWorkflow(attempt.pin.objectId, attempt.value.preparedAt);
    return this.engine.schedule(attempt.value.request);
  }
}
