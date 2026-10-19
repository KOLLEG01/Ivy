import type { TaskBoard } from "../../../../packages/sdk/src/node.js";

export interface ExecutionFacts {
  workflowState: TaskBoard.Task["workflowState"];
  control: TaskBoard.TaskFields["control"];
  publication: boolean;
  claim: { phase: TaskBoard.Claim["phase"]; hostId: string; mode?: string } | null;
  waiting: Pick<TaskBoard.Waiting, "reason" | "detail"> | null;
  executionRequirement: TaskBoard.ExecutionRequirement | null;
  requiredCapabilities?: string[] | undefined;
  openQuestionCount?: number | undefined;
  scheduledCheckFailure?: string | undefined;
  completionIssue?: string | undefined;
  questionWait?: boolean | undefined;
  recovery?: { attempts: number; retryAt: string | null } | undefined;
}

/** Automatic recovery and result publication retain the current execution's column. */
export function claimedWorkflowState(waiting: Pick<TaskBoard.Waiting, "reason"> | null): "waiting" | "in_progress" {
  return waiting && !["host", "external_outcome", "publication"].includes(waiting.reason) ? "waiting" : "in_progress";
}

/** One derived explanation for service reads and lightweight board projections. */
export function executionCondition(
  task: ExecutionFacts,
): TaskBoard.ExecutionCondition {
  if (task.publication)
    return {
      state: "publishing",
      detail: "Publishing the current Task update.",
    };
  if (task.claim) {
    if (task.claim.mode === 'recovery') return { state: 'recovering', detail: `Restoring the original missing handoff on ${task.claim.hostId}.` };
    if (task.claim.phase === "publishing_result")
      return { state: "publishing", detail: "Native execution ended; publishing the ticket handoff." };
    if (task.waiting?.reason === "host")
      return { state: "recovering", detail: `Reconnecting to ${task.claim.hostId}; TaskBoard will retry automatically.` };
    if (task.claim.phase === "outcome_unknown" || task.waiting?.reason === "external_outcome")
      return {
        state: "recovering",
        detail: `Reconciling the original native execution on ${task.claim.hostId}; TaskBoard will retry automatically.`,
      };
    if (["claimed", "run_created", "starting"].includes(task.claim.phase))
      return { state: "starting", detail: `Starting on ${task.claim.hostId}.` };
    if (task.claim.phase === "waiting_input")
      return {
        state: "needs_user",
        detail: "The native Task needs user input.",
      };
    return { state: "active", detail: `Working on ${task.claim.hostId}.` };
  }
  if (
    task.workflowState === "backlog" ||
    ["done", "cancelled"].includes(task.workflowState)
  )
    return {
      state: "idle",
      detail:
        task.workflowState === "backlog"
          ? "Backlog Tasks are not scheduled."
          : "No work is scheduled.",
    };
  if (task.recovery) return { state: task.recovery.attempts < 2 ? 'recovering' : 'workflow_attention', detail:
    task.recovery.attempts >= 2 ? task.completionIssue ?? 'The original handoff needs inspection after two recovery attempts.' :
      task.recovery.retryAt ? `Handoff recovery will retry after ${task.recovery.retryAt}.` : 'The original missing handoff is queued for automatic recovery.' };
  if (task.workflowState === "review")
    return { state: "needs_user", detail: task.scheduledCheckFailure ?? "Final result is ready for review." };
  if (task.openQuestionCount || task.questionWait)
    return { state: 'needs_user', detail: task.questionWait ? 'Waiting for the linked user question to be answered.' : 'Waiting for an answer in the ticket.' };
  if (task.completionIssue || task.waiting?.detail === 'Execution ended without an explicit completion intent; review the ticket before continuing.')
    return { state: 'workflow_attention', detail: task.completionIssue ?? 'The execution ended without a structured handoff. Its original outcome needs reconciliation.' };
  if (task.scheduledCheckFailure)
    return { state: "blocked_environment", detail: task.scheduledCheckFailure };
  if (task.waiting?.reason === "time")
    return { state: "deferred", detail: task.waiting.detail };
  if (task.waiting?.reason === "dependency")
    return { state: "blocked_dependency", detail: task.waiting.detail };
  if (task.waiting?.reason === "external_outcome")
    return { state: "outcome_unknown", detail: task.waiting.detail };
  if (task.waiting?.reason === "user")
    return { state: "needs_user", detail: task.waiting.detail };
  if (task.waiting)
    return { state: "blocked_environment", detail: task.waiting.detail };
  if (task.control === "user")
    return task.workflowState === "in_progress"
      ? { state: "active", detail: "Manual work is in progress." }
      : { state: "needs_user", detail: "Ready for you to start." };
  const needs = task.requiredCapabilities?.length
    ? ` with ${task.requiredCapabilities.join(", ")}`
    : "";
  if (!task.executionRequirement || task.executionRequirement.kind === 'automatic')
    return {
      state: "queued",
      detail: needs
        ? `Waiting for a ready host${needs}.`
        : "Waiting for the next available execution PC.",
    };
  return {
    state: "queued",
    detail: `Waiting for ${task.executionRequirement.hostId}${needs}.`,
  };
}
