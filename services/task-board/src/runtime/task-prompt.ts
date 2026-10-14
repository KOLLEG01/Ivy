import type { TaskBoard } from '../../../../packages/sdk/src/node.js';

export function taskUpdatePrompt(task: TaskBoard.Task, serviceNodeId: string, coordinationOnly = false): string {
  const reference = `TaskBoard ${task.taskKey} was updated. Read the ticket and new comments via @Ivy MCP (service ${serviceNodeId}).`;
  return coordinationOnly
    ? `${reference} Coordination only: review delegated children, mark accepted work Done and archive. Preserve this ticket's result and blocker; do not implement or publish a handoff.`
    : `${reference} Continue the requested work, including the latest user instructions and questions.`;
}
export function taskPrompt(task: TaskBoard.Task, serviceNodeId: string, coordinationOnly = false): string {
  const reference = `TaskBoard ${task.taskKey}, @Ivy MCP service ${serviceNodeId}. Read the current ticket and comments now and after updates.`;
  const delegation = 'When delegating work, create the child with parentTaskId=<your ticket object ID>. Find your children with task_list parentTaskId=<your ticket object ID>, across hosts and worktrees. You own their review: read their saved final result, verify the delivered scope and integration, then use task_update transition workflowState="done" and archive archived=true, each at its current revision. Request corrections when needed. Wait for a child with blocker until="idle", so you can review it; do not wait for your own child to be Done before reviewing it. Close accepted children before your final handoff. Only the root task needs the user\'s final review.';
  if (coordinationOnly) return `${reference} This turn is for coordination only: answer directed task messages and review completed delegated children. You may mark accepted children Done and archive them after verification. Preserve this ticket's review result or blocker; do not implement changes or publish a handoff. Avoid acknowledgment loops.`;
  return `${reference}
Work in the assigned workspace; preserve others' changes. Use task_list relatedTo to find peers and coordinate overlapping work. Send directed comments with authorKind="agent", sourceTaskId=<your task ID>, agentDelivery="queue"; ordinary agent comments do not wake tasks. Use defer with blocker {taskId, until:"idle"|"done"} when waiting on another task; stop and let TaskBoard resume you.${task.fields.workspaceRequirement.kind === 'task_workspace' ? ` Keep task files under outputs/${task.taskKey}/.` : ''}
${delegation}${task.parentTaskId ? ` This task is delegated by ${task.parentTaskId}. The parent owns acceptance and archival. TaskBoard delivers your saved final result to it automatically; direct questions or coordination to the parent.` : ''}
Keep comments concise, with authorKind="agent". Mark user questions purpose="question"; contactUser=true only when actionable. Answer native requests through their owning AgentManager. Attach non-Git outputs; link code commits or PRs. Finish with one purpose="handoff" comment containing results, verification and limitations, then stop. TaskBoard handles Review and notification.`;
}
