import type { Wire } from '../../../../packages/sdk/src/node.js';
import { browserNoticeTopic } from '../../../../packages/sdk/src/node.js';
import catalog from '../../../../specs/schemas/task-board.operations.json' with { type: 'json' };
import { bundledSchema } from '../../../../packages/sdk/src/node.js';
import { contractVersion, readableVersions, taskBoardContracts } from './schema.js';
import type { ContractKey } from './schema.js';

const descriptions: Record<string, string> = {
  configuration: 'Read current TaskBoard defaults and their revision before creating a ticket; use them unless the user explicitly requests different settings.',
  configure: 'Save TaskBoard execution defaults at their current revision without restarting the service.',
  savePlan: 'Save an agent execution plan for a specific provider and version.',
  create: 'Create a task in Todo by default, or choose workflowState explicitly. Todo can queue agent work; manual tasks never start an agent. Waiting needs waiting details; manual Review needs initialResult. When delegating, set parentTaskId and choose the intended start state explicitly; the parent owns review, Done and archival. Read task_configuration first. Omitted userContact inherits the board default for root tasks and ticket for delegated tasks.',
  edit: 'Update task content at its current revision. Contact, priority and date changes are allowed during an active run without interrupting it; running assignment fields are fixed. Omitted userContact preserves the ticket setting.',
  transition: 'Move a task to an allowed workflow state on the Kanban board.',
  defer: 'Wait until a time, a new idle/done result or a named delivery (blocker.taskId, until delivery, key). A consumed condition cannot wake the same work repeatedly. questionWait links the exact unanswered user question in another ticket. Capacity belongs to the execution system, not ticket blockers.',
  reassign: 'Assign a task to another agent while retaining unresolved attempts.',
  newThread: 'Explicitly start work in a new Codex task, preserving the ticket and previous runs. Normal work continues in the existing task.',
  cancel: 'Request cancellation of a task and track the original execution outcome.',
  review: 'Accept the saved final task result after user review.',
  saveResult: 'Save an intermediate or final result on a task.',
  comment: 'Discuss a task. Set authorKind=agent for worker comments. Use sourceTaskId only for a directed message to another task; omit it on your own handoff. A handoff may publish named deliveries with exact artifacts/code references and link questionWait. Ordinary agent updates never wake work. contactUser=true is only for important user questions.',
  uploadAttachment: 'Attach a file to a task, up to 8 MiB; keep code changes in Git.',
  archive: 'Archive a ticket and its Codex tasks, or restore it. Restoration reuses the native task when possible and creates a new one only after confirmed failure.',
  operation: 'Check the original outcome of a TaskBoard action by operationId.',
  operationReceipt: 'Use operationId with receiptOnly=true to read a compact identity-bound outcome even after terminal payload retention. A receipt never authorizes replay.',
  read: 'Read task content, comments and execution status by object ID or task key.',
  list: 'List tasks with pagination and optional workflow status, category or parentTaskId filters. parentTaskId finds delegated children across hosts and worktrees. Use relatedTo=<task object ID> to find peers allocated to the same actual working directory on the same host. Separate worktrees are independent.',
  categories: 'List the task categories used in this workspace.',
  workspace: 'Find the Kanban workspace root, caller role and readiness.',
  markCommentsRead: 'Mark current agent comments on a task as read for the user, using the observed task revision.',
};
export const taskBoardTools: Wire.ToolDefinition[] = Object.entries(catalog.operations).filter(([name]) => !['review', 'saveResult'].includes(name)).map(([name, operation]) => ({
  namespace: 'task-board', name, interfaceVersion: '1.0.0', description: descriptions[name]!,
  discovery: { ...((['workspace', 'markCommentsRead'].includes(name)) ? {} : { mcp: {
    name: ['edit', 'transition', 'defer', 'reassign', 'newThread', 'cancel', 'saveResult', 'archive'].includes(name) ? 'task_update'
      : ['read', 'operation', 'operationReceipt'].includes(name) ? 'task_read'
      : ['list', 'categories'].includes(name) ? 'task_list'
      : 'task_' + name.replace(/[A-Z]/g, letter => '_' + letter.toLowerCase()),
    surface: name === 'savePlan' ? 'ivy_dev' : 'ivy',
  } }), keywords: ['kanban', 'collaboration'] },
  inputSchema: bundledSchema(operation.input, catalog.schema), outputSchema: bundledSchema(operation.output, catalog.schema),
  annotations: { readOnlyHint: !operation.mutation, idempotentHint: true },
}));
export function taskBoardRegistry(): Wire.RegistrySync {
  const contracts = taskBoardContracts();
  return { mcpPrefix: 'task', discoveryHint: 'TaskBoard is a collaborative Kanban system for tracking work as tasks that the user and agents carry out together, with progress updates, discussion and result review.', namespaces: [{ namespace: 'task-board', description: 'Collaborative Kanban tasks, agent work and user review',
    guideMarkdown: 'Before creating a ticket, read task_configuration and use its current defaults unless the user explicitly requests different settings. Creation defaults to Todo; use workflowState=backlog for ideas or preparation that must not start. Waiting needs a reason and details; manual Review needs initialResult. Agent In Progress and Review require the actual native execution workflow. Start with task_list to find tasks, optionally filtering by workflow status or category; pass categories: true to list categories instead. Read a selected task or original operation with task_read before changing it. '
      + 'Use task_comment for discussion and feedback, with empty requests/responses for ordinary conversation. User comments queue agent work by default; set agentDelivery=none to record without scheduling. Worker comments must set authorKind=agent to avoid scheduling themselves. Use purpose=question for a question; addressed user replies use replyTo, explicit resolutions use resolvesQuestions with an explanation, and unrelated replies or handoffs never close questions; contactUser=true requests configured Main/voice contact only for an important actionable question. Routine updates remain in the ticket. On Done, comments do not schedule work unless moveToTodo=true. '
      + 'When delegating work, set parentTaskId to the original task object ID. Find children through task_list parentTaskId, including separate worktrees and hosts. The original task owns their review: verify each saved final result and integration, move accepted children to Done with task_update transition, then archive them. Review-only coordination may close accepted children while preserving the parent result or blocker. Wait for your child with blocker until=idle to review it; do not wait for its Done state before you can accept it. Only root tasks need final user review. '
      + 'Use task_update for content, workflow, waiting, assignment, cancellation and archival. Archive uses archived=true for the ticket and its Codex tasks; archived=false restores the ticket and tries to restore its current Codex task, creating a new task only after confirmed failure. newThread explicitly queues work in a new Codex task; never use it as the default continuation. Use the current revision, the selected execution plan and one operationId per action. Read the original operation through task_read after an uncertain reply. Publish one purpose=handoff comment with a structured handoff declaring workflowState=review only for an overall result ready for acceptance, todo with nextStep and nextReviewAt=null for already authorized work to continue, or waiting with waiting.reason/detail for a real remaining condition. Missing handoffs are recovered automatically in the same context; never ask for artificial user continuation comments. Include nextReviewAt (or null); checkScope explicitly authorizes independent scheduled checks despite open questions or Review. Report unavailable checks with checkFailure and a future retry date. Attach non-Git outputs. TaskBoard applies the intent only after confirmed native completion and delivers genuinely ready delegated results to the parent; users move root tickets to Done through the normal transition. Read the task again at every turn. Answer native agent inputs through the owning AgentManager using their exact identity; their questions and answers appear in comments.',
    tools: taskBoardTools, topics: [browserNoticeTopic('task-board')], inventoryKinds: [] }], contracts,
    requiredContracts: contracts.filter((contract, index) => contracts.findIndex(value => value.key === contract.key) === index).map(contract => ({
      key: contract.key,
      readVersions: readableVersions(contract.key as ContractKey),
      writeVersions: [contractVersion(contract.key as ContractKey)],
    })) };
}
