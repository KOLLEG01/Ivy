import type { Wire } from '../../../../packages/sdk/src/node.js';
import { taskBoardAttachmentGuidance } from '../../../../instructions/task-board.js';
import { browserNoticeTopic } from '../../../../packages/sdk/src/node.js';
import catalog from '../../../../specs/schemas/task-board.operations.json' with { type: 'json' };
import { bundledSchema } from '../../../../packages/sdk/src/node.js';
import { contractVersion, readableVersions, taskBoardContracts } from './schema.js';
import type { ContractKey } from './schema.js';

const descriptions: Record<string, string> = {
  configuration: 'Read current TaskBoard defaults and their revision before creating a ticket; use them unless the user explicitly requests different settings.',
  configure: 'Save TaskBoard execution defaults at their current revision without restarting the service.',
  savePlan: 'Save an agent execution plan for a specific provider and version.',
  create: 'Create a task in Todo by default, or choose workflowState explicitly. Todo can queue agent work; manual tasks never start an agent. Waiting resumes on comments, watched ticket changes or wakeAt; manual Review needs initialResult. When delegating, set parentTaskId and choose the intended start state explicitly; the parent owns review, Done and archival. Read task_configuration first. Omitted userContact inherits the board default for root tasks and ticket for delegated tasks.',
  edit: 'Update task content at its current revision. Model, reasoning and speed can change during a run: the agent yields and continues in the same chat with the latest settings. Idle settings changes apply on the next start. Contact, priority and dates remain editable; other running assignment fields are fixed. Omitted userContact preserves the ticket setting.',
  retargetQuestion: 'Correct the recipient of an unresolved ordinary question without replacing its text or identity. Native input questions stay bound to their owner.',
  transition: 'Move a task to an allowed workflow state on the Kanban board.',
  defer: 'Wait for new input. Optionally set wakeAt to the next UTC appointment and dependencies to ticket IDs to watch. Omit a field to preserve it; null clears the appointment and [] clears watched tickets. Comments always wake the agent',
  reassign: 'Assign a task to another agent while retaining unresolved attempts.',
  newThread: 'Explicitly start work in a new Codex task, preserving the ticket and previous runs. Normal work continues in the existing task.',
  cancel: 'Request cancellation of a task and track the original execution outcome.',
  review: 'Accept the saved final task result after user review.',
  saveResult: 'Save an intermediate or final result on a task.',
  comment: 'Discuss a task. Set authorKind=agent for worker comments. Use sourceTaskId only for a directed message to another task; omit it on your own handoff. Include relevant code references in the result. Record verification findings and technical issues in comment or result text. Ordinary agent updates never wake work. contactUser=true is only for important user questions.',
  uploadAttachment: 'Attach a file to a task, up to 8 MiB; keep code changes in Git. ' + taskBoardAttachmentGuidance,
  archive: 'Archive a ticket and its Codex tasks, or restore it. Restoration reuses the native task when possible and creates a new one only after confirmed failure.',
  operation: 'Read the compact caller-bound original action outcome and submitted request hash by operationId. Native payloads remain at their owning service; a receipt never authorizes replay.',
  operationReceipt: 'Use operationId with receiptOnly=true to read a compact identity-bound outcome even after terminal payload retention. A receipt never authorizes replay.',
  read: 'Read task content, execution status and the latest 50 comments, newest first. To read older messages, pass comments={before:commentPage.nextBefore,limit:50}; null nextBefore ends the history. New appends do not shift this sequence cursor. questionsOnly=true pages unresolved questions. view=card reads compact board facts without conversation bodies or per-user read tracking.',
  list: 'List tasks with pagination and optional workflow status, category or parentTaskId filters. parentTaskId finds delegated children across hosts and worktrees. Use relatedTo=<task object ID> to find peers allocated to the same actual working directory on the same host. Separate worktrees are independent.',
  categories: 'List the task categories used in this workspace.',
  workspace: 'Find the Kanban workspace root, caller role and readiness.',
  markCommentsRead: 'Mark observed agent comments as read for the user at the task revision. includeCrossAgent=false marks only direct ticket-agent replies and preserves hidden directed communication.',
};
export const taskBoardTools: Wire.ToolDefinition[] = Object.entries(catalog.operations).filter(([name]) => !['review', 'saveResult'].includes(name)).map(([name, operation]) => ({
  namespace: 'task-board', name, interfaceVersion: '1.0.0', description: descriptions[name]!,
  discovery: { ...((['workspace', 'markCommentsRead'].includes(name)) ? {} : { mcp: {
    name: ['edit', 'retargetQuestion', 'transition', 'defer', 'reassign', 'newThread', 'cancel', 'saveResult', 'archive'].includes(name) ? 'task_update'
      : ['read', 'operation', 'operationReceipt'].includes(name) ? 'task_read'
      : ['list', 'categories'].includes(name) ? 'task_list'
      : 'task_' + name.replace(/[A-Z]/g, letter => '_' + letter.toLowerCase()),
    surface: name === 'savePlan' ? 'ivy_dev' : 'ivy',
  } }), keywords: ['kanban', 'collaboration'] },
  inputSchema: bundledSchema(operation.input, catalog.schema), outputSchema: bundledSchema(operation.output, catalog.schema),
  annotations: { readOnlyHint: !operation.mutation, idempotentHint: true },
}));
// Tool guidance may evolve without changing immutable retained request contracts.
const commentTool = taskBoardTools.find(tool => tool.name === 'comment')!;
const commentInput = structuredClone(commentTool.inputSchema) as { properties: Record<string, { description?: string }> };
commentInput.properties.agentDelivery!.description = 'User comments and directed agent messages queue a ticket-update turn by default. none records without waking. A task’s own agent updates never wake themselves.';
commentInput.properties.sourceTaskId!.description = 'For a directed message, identify the different sending task and set authorKind=agent. Delivery is queued by default.';
commentTool.inputSchema = commentInput;
export function taskBoardRegistry(): Wire.RegistrySync {
  const contracts = taskBoardContracts();
  return { mcpPrefix: 'task', discoveryHint: 'TaskBoard is a collaborative Kanban system for tracking work as tasks that the user and agents carry out together, with progress updates, discussion and result review.', namespaces: [{ namespace: 'task-board', description: 'Collaborative Kanban tasks, agent work and user review',
    guideMarkdown: 'Before creating a ticket, read task_configuration and use its current defaults unless the user explicitly requests different settings. Creation defaults to Todo; use workflowState=backlog for ideas or preparation that must not start. Waiting can optionally set wakeAt and watched dependencies; manual Review needs initialResult. Agent In Progress and Review require the actual native execution workflow. Start with task_list to find tasks, optionally filtering by workflow status or category; pass categories: true to list categories instead. Read a selected task or original operation with task_read before changing it. '
      + 'Use task_comment for discussion and feedback, with empty requests/responses for ordinary conversation. User comments and directed agent messages queue a ticket-update turn by default; set agentDelivery=none to record without scheduling. Worker comments must set authorKind=agent to avoid scheduling themselves. Use purpose=question with recipient={kind:user} or recipient={kind:task,taskId} for a question. TaskBoard delivers task questions automatically; never send a second manual copy. A receiving task answers in the sender ticket using sourceTaskId and replyTo; addressed user replies use replyTo, explicit resolutions use resolvesQuestions with an explanation, and unrelated replies or handoffs never close questions; contactUser=true requests configured Main/voice contact only for an important actionable question. Routine updates remain in the ticket. User comments and directed task messages deliver the same ticket-update prompt to the existing context, including on blocked, manual and terminal tickets. Read the ticket and decide whether to answer, continue work or wait in that turn. A reply needs no handoff; when work changes, publish its actual outcome. Every active turn is In progress. A reply without a handoff restores the prior idle state and result after completion. '
      + 'When delegating work, set parentTaskId to the original task object ID. Find children through task_list parentTaskId, including separate worktrees and hosts. The original task owns their review: verify each saved final result and integration, move accepted children to Done with task_update transition, then archive them. The parent receives a comment when a child is ready for review; dependencies can also watch its other material changes. Only root tasks need final user review. '
      + 'Use task_update for content, workflow, waiting, assignment, cancellation and archival. Archive uses archived=true for the ticket and its Codex tasks; archived=false restores the ticket and tries to restore its current Codex task, creating a new task only after confirmed failure. newThread explicitly queues work in a new Codex task; never use it as the default continuation. Use the current revision, the selected execution plan and one operationId per action. Read the original operation through task_read after an uncertain reply. The agent owns planning and coordination from the current ticket. Use an optional purpose=handoff comment with handoff.workflowState to select review, waiting or todo after this turn. Waiting needs an open question, watched ticket or wakeAt appointment; without one the agent is retried automatically. Review means the overall result is ready for acceptance; todo continues active work. An ordinary reply needs no handoff and preserves the previous waiting or review state. Comments from the user or another task, changes to watched dependency tickets and due wakeAt appointments wake the agent. No step plan, named delivery or prerequisite expression is required. An own question highlights the ticket but does not prevent processing comments. Record verification findings, technical errors and unavailable evidence in ordinary comments or result text; do not add structured check lists. A negative finding is not an execution failure. Set wakeAt to the next UTC appointment for repeated checks. Omit wakeAt and dependencies to preserve them when processing unrelated comments. Multiple future appointments belong in the ticket text; set the next one and renew it after its attempt. ' + taskBoardAttachmentGuidance + ' TaskBoard applies the intent only after confirmed native completion and delivers genuinely ready delegated results to the parent; users move root tickets to Done through the normal transition. Read the task again at every turn. Answer native agent inputs through the owning AgentManager using their exact identity; their questions and answers appear in comments.',
    tools: taskBoardTools, topics: [browserNoticeTopic('task-board')], inventoryKinds: [] }], contracts,
    requiredContracts: contracts.filter((contract, index) => contracts.findIndex(value => value.key === contract.key) === index).map(contract => ({
      key: contract.key,
      readVersions: readableVersions(contract.key as ContractKey),
      writeVersions: [contractVersion(contract.key as ContractKey)],
    })) };
}
