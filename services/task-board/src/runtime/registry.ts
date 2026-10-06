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
  create: 'Create a Backlog task with a stable task key. Read task_configuration first and use current defaults unless the user explicitly requests different settings. Omitted userContact inherits the current board default.',
  edit: 'Update task content at its current revision. Contact, priority and date changes are allowed during an active run without interrupting it; running assignment fields are fixed. Omitted userContact preserves the ticket setting.',
  transition: 'Move a task to an allowed workflow state on the Kanban board.',
  defer: 'Block a task until a time or a linked task condition (blocker.taskId, until idle/done); TaskBoard resumes automatically when prerequisites are satisfied.',
  reassign: 'Assign a task to another agent while retaining unresolved attempts.',
  newThread: 'Explicitly start work in a new Codex task, preserving the ticket and previous runs. Normal work continues in the existing task.',
  cancel: 'Request cancellation of a task and track the original execution outcome.',
  review: 'Accept the saved final task result after user review.',
  saveResult: 'Save an intermediate or final result on a task.',
  comment: 'Discuss a task. Set authorKind=agent for worker comments. For directed task messages also set sourceTaskId and agentDelivery=queue; ordinary agent updates never wake work. contactUser=true is only for important user questions.',
  uploadAttachment: 'Attach a file to a task, up to 8 MiB; keep code changes in Git.',
  archive: 'Archive a ticket and its Codex tasks, or restore it. Restoration reuses the native task when possible and creates a new one only after confirmed failure.',
  operation: 'Check the original outcome of a TaskBoard action by operationId.',
  read: 'Read task content, comments and execution status by object ID or task key.',
  list: 'List tasks with pagination and optional workflow status or category filters. Use relatedTo=<task object ID> to find peers allocated to the same actual working directory on the same host. Separate worktrees are independent.',
  categories: 'List the task categories used in this workspace.',
  workspace: 'Find the Kanban workspace root, caller role and readiness.',
  markCommentsRead: 'Mark current agent comments on a task as read for the user, using the observed task revision.',
};
export const taskBoardTools: Wire.ToolDefinition[] = Object.entries(catalog.operations).filter(([name]) => !['review', 'saveResult'].includes(name)).map(([name, operation]) => ({
  namespace: 'task-board', name, interfaceVersion: '1.0.0', description: descriptions[name]!,
  discovery: { ...((['workspace', 'markCommentsRead'].includes(name)) ? {} : { mcp: {
    name: ['edit', 'transition', 'defer', 'reassign', 'newThread', 'cancel', 'saveResult', 'archive'].includes(name) ? 'task_update'
      : ['read', 'operation'].includes(name) ? 'task_read'
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
    guideMarkdown: 'Before creating a ticket, read task_configuration and use its current defaults unless the user explicitly requests different settings. Start with task_list to find tasks, optionally filtering by workflow status or category; pass categories: true to list categories instead. Read a selected task or original operation with task_read before changing it. '
      + 'Use task_comment for discussion and feedback, with empty requests/responses for ordinary conversation. User comments queue agent work by default; set agentDelivery=none to record without scheduling. Worker comments must set authorKind=agent to avoid scheduling themselves. Use purpose=question for a question; contactUser=true requests configured Main/voice contact only for an important actionable question. Routine updates remain in the ticket. On Done, comments do not schedule work unless moveToTodo=true. '
      + 'Use task_update for content, workflow, waiting, assignment, cancellation and archival. Archive uses archived=true for the ticket and its Codex tasks; archived=false restores the ticket and tries to restore its current Codex task, creating a new task only after confirmed failure. newThread explicitly queues work in a new Codex task; never use it as the default continuation. Use the current revision, the selected execution plan and one operationId per action. Read the original operation through task_read after an uncertain reply. Publish one final comment with purpose=handoff and non-Git files as Task attachments. TaskBoard alone moves completed execution to Review and sends completion contact; users move tickets to Done through the normal transition. Read the task again at every turn. Answer native agent inputs through the owning AgentManager using their exact identity; their questions and answers appear in comments.',
    tools: taskBoardTools, topics: [browserNoticeTopic('task-board')], inventoryKinds: [] }], contracts,
    requiredContracts: contracts.filter((contract, index) => contracts.findIndex(value => value.key === contract.key) === index).map(contract => ({
      key: contract.key,
      readVersions: readableVersions(contract.key as ContractKey),
      writeVersions: [contractVersion(contract.key as ContractKey)],
    })) };
}
