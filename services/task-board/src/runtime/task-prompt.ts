import type { TaskBoard } from '../../../../packages/sdk/src/node.js';

export function taskUpdatePrompt(task: TaskBoard.Task, serviceNodeId: string): string {
  return `TaskBoard ${task.taskKey} was updated. Read the ticket, new comments and related tasks through @Ivy MCP (service ${serviceNodeId}) before continuing.`;
}
export function taskPrompt(task: TaskBoard.Task, serviceNodeId: string, coordinationOnly = false): string {
  const reference = `TaskBoard ${task.taskKey}, @Ivy MCP service ${serviceNodeId}. Read the current ticket and comments now and after updates.`;
  if (coordinationOnly) return `${reference} This turn is for coordination only: answer directed task messages in the ticket. Preserve the review or blocker; do not implement changes or publish a handoff. Avoid acknowledgment loops.`;
  return `${reference}
Work in the assigned workspace; preserve others' changes. Use task_list relatedTo to find peers and coordinate overlapping work. Send directed comments with authorKind="agent", sourceTaskId=<your task ID>, agentDelivery="queue"; ordinary agent comments do not wake tasks. Use defer with blocker {taskId, until:"idle"|"done"} when waiting on another task; stop and let TaskBoard resume you.${task.fields.workspaceRequirement.kind === 'task_workspace' ? ` Keep task files under outputs/${task.taskKey}/.` : ''}
Keep comments concise, with authorKind="agent". Mark user questions purpose="question"; contactUser=true only when actionable. Answer native requests through their owning AgentManager. Attach non-Git outputs; link code commits or PRs. Finish with one purpose="handoff" comment containing results, verification and limitations, then stop. TaskBoard handles Review and notification.`;
}
