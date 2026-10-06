import { readFileSync, writeFileSync } from 'node:fs';

// Specification generation only. Native parameters come from the checked public catalogs;
// no service, task, native process or external action is executed by this generator.
const ref = name => ({ $ref: '#/$defs/' + name });
const wire = name => ({ $ref: 'https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/' + name });
const chat = name => ({ $ref: 'https://ivy.invalid/schemas/chat.schema.json#/$defs/' + name });
const id = wire('Identifier'), hash = wire('Hash'), resource = wire('ResourceRef');
const str = (maxLength, minLength = 0) => ({ type: 'string', minLength, maxLength });
const integer = (minimum = 0, maximum = Number.MAX_SAFE_INTEGER) => ({ type: 'integer', minimum, maximum });
const obj = (properties, required = Object.keys(properties)) => ({ type: 'object', properties, required, additionalProperties: false });
const array = (items, maxItems = 256) => ({ type: 'array', items, maxItems });
const nil = { type: 'null' }, nullable = value => ({ anyOf: [value, nil] });
const bool = { type: 'boolean' };
const timestamp = { ...str(24, 24), pattern: '^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$' };
const agent = name => ({ $ref: 'https://ivy.invalid/schemas/agent.schema.json#/$defs/' + name });
const operation = name => ({ $ref: 'https://ivy.invalid/schemas/hive-operations.schema.json#/$defs/' + name });
const defs = {};
Object.assign(defs, {
  NativePlan: agent('Plan'), NativePlanDraft: agent('PlanDraft'),
  NativeRequest: obj({ nativeVersion: id, catalogSourceHash: hash,
    method: { enum: ['thread/start', 'thread/resume', 'turn/start', 'turn/steer', 'turn/interrupt'] }, params: agent('ParametersRef') }),
  ObjectPin: obj({ objectId: id, revision: integer(1) }),
  Target: obj({ hostId: id, serviceNodeId: id }),
  Capability: obj({ key: { ...str(64, 1), pattern: '^[a-z0-9]+(?:-[a-z0-9]+)*$' }, label: str(128, 1) }),
  ExecutionRequirement: { description: 'Select one exact AgentManager host, or automatic for any ready host. Use null in task fields to inherit the TaskBoard default.',
    oneOf: [obj({ kind: { const: 'host' }, hostId: id }), obj({ kind: { const: 'automatic' } })] },
  NativeOptions: obj({ model: nullable(str(256, 1)), reasoningEffort: nullable(str(64, 1)), serviceTier: nullable({ type: 'string', enum: ['standard', 'fast', 'flex'] }) }),
  ExecutionDefaults: obj({ executionRequirement: nullable(ref('ExecutionRequirement')), nativeOptions: ref('NativeOptions'),
    userContact: { enum: ['ticket', 'chat', 'phone'] }, useWorktree: bool, allowParallel: bool }, ['executionRequirement', 'nativeOptions']),
  Configuration: obj({ schemaVersion: { const: 1 }, defaults: ref('ExecutionDefaults') }),
  ConfigurationQuery: obj({}),
  ConfigurationView: obj({ object: nullable(ref('ObjectPin')), configuration: ref('Configuration') }),
  WorkspaceRequirement: { description: 'task_workspace allocates an internal workspace; directory_path uses any existing absolute host directory; existing_project selects a known project, optionally with a managed worktree; repository_path checks out a repository URL; new_project_path creates a new project folder.', oneOf: [
    obj({ kind: { const: 'task_workspace' } }),
    obj({ kind: { const: 'directory_path' }, path: str(2048, 1) }),
    obj({ kind: { const: 'existing_project' }, projectId: id, path: nullable(str(2048, 1)), useWorktree: bool }),
    obj({ kind: { const: 'repository_path' }, repositoryUrl: { ...str(4096, 1), pattern: '^https?://[^@/]+(?:/|$)' }, folderName: { ...str(128, 1), pattern: '^[^./\\\\][^/\\\\]*$' } }),
    obj({ kind: { const: 'new_project_path' }, folderName: { ...str(128, 1), pattern: '^[^./\\\\][^/\\\\]*$' } }),
  ] },
  WorkspaceResolution: obj({ hostId: id, serviceNodeId: id, canonicalCwd: str(2048, 1), taskRoot: str(2048, 1), bootstrapPath: str(2048, 1),
    intendedPath: str(2048, 1), sourcePath: nullable(str(2048, 1)), project: nullable(resource), useWorktree: bool,
    repository: nullable(obj({ name: str(512, 1), branch: nullable(str(512, 1)), commit: nullable(str(128, 1)), dirty: bool,
      originName: nullable(str(128, 1)), originUrl: nullable(str(4096, 1)), originState: { enum: ['confirmed', 'local_only', 'no_origin', 'unknown'] },
      remoteRef: nullable(str(1024, 1)), observedAt: timestamp, limitation: nullable(str(4096, 1)) })) },
    ['hostId', 'serviceNodeId', 'canonicalCwd', 'taskRoot', 'bootstrapPath', 'intendedPath', 'sourcePath', 'project', 'useWorktree', 'repository']),
  Actor: obj({ principalId: id, serviceNodeId: id, generation: integer(1), source: { enum: ['user', 'worker', 'scheduler', 'recovery', 'native'] } }),
  Artifact: obj({ object: ref('ObjectPin'), label: str(512, 1), mediaType: str(256, 1), contentHash: hash, byteLength: integer(0, 8388608) }, ['object', 'label', 'mediaType', 'contentHash']),
  CodeReference: obj({ kind: { enum: ['repository', 'commit', 'pull_request', 'issue', 'file'] },
    url: { ...str(4096, 1), pattern: '^https?://' }, label: str(512, 1), commit: nullable(str(128, 1)) }),
  Check: obj({ name: str(512, 1), status: { enum: ['passed', 'failed', 'not_run', 'blocked'] },
    detail: str(16384), evidence: array(ref('Artifact'), 32) }),
  RepositoryResult: obj({ hostId: id, serviceNodeId: id, project: resource, repositoryName: str(512, 1), branch: nullable(str(512, 1)), commit: nullable(str(128, 1)),
    originName: nullable(str(128, 1)), originUrl: nullable(str(4096, 1)), originState: { enum: ['confirmed', 'local_only', 'no_origin', 'unknown'] },
    remoteRef: nullable(str(512, 1)), observedAt: timestamp, limitation: nullable(str(16384, 1)) }),
  ResultContent: obj({ summary: str(1048576, 1), artifacts: array(ref('Artifact')), checks: array(ref('Check')),
    codeReferences: array(ref('CodeReference')), repositoryResult: nullable(ref('RepositoryResult')), limitations: array(str(16384, 1), 128) }),
  Waiting: obj({ reason: { type: 'string', enum: ['user', 'time', 'dependency', 'host', 'capability', 'workspace', 'external_outcome', 'publication'] },
    detail: str(16384, 1), since: timestamp }),
  ChatTarget: obj({ serviceNodeId: id, expectedBridge: chat('ExpectedBridge'), channel: chat('Channel') }),
  PhoneTarget: obj({ serviceNodeId: id, recipientId: id }),
  TaskFields: obj({ title: str(512, 1), description: str(1048576), acceptanceCriteria: array(str(16384, 1)), category: nullable(str(128, 1)),
    control: { type: 'string', enum: ['user', 'agent'], description: 'user means manual work; agent lets TaskBoard schedule and resume native agent work.' },
    priority: { ...integer(0, 4), description: 'Scheduling priority: 4 is highest and 0 is lowest.' },
    executionRequirement: { ...nullable(ref('ExecutionRequirement')), description: 'Required field. null inherits TaskBoard defaults; automatic selects any eligible PC; host restricts scheduling to that PC.' },
    nativeOptions: ref('NativeOptions'),
    workspaceRequirement: { ...ref('WorkspaceRequirement'), description: 'Required workspace policy for this task. Choose one kind deliberately; TaskBoard resolves it on the selected host before starting agent work.' },
    dependencies: { ...array(id), uniqueItems: true },
    requiredCapabilities: { ...array({ ...str(64, 1), pattern: '^[a-z0-9]+(?:-[a-z0-9]+)*$' }, 32), uniqueItems: true, description: 'Capability keys the selected host must advertise before agent work starts. Missing means none.' },
    userContact: { type: 'string', enum: ['ticket', 'chat', 'phone'], default: 'ticket' },
    allowParallel: bool,
    nextReviewAt: nullable(timestamp), dueAt: nullable(timestamp) }),
  TaskAttachment: obj({ attachmentId: id, object: ref('ObjectPin'), filename: str(512, 1), mediaType: str(256, 1), byteLength: integer(0, 8388608),
    contentHash: hash, uploader: ref('Actor'), createdAt: timestamp }),
  ApprovalRequest: obj({ requestId: id, type: { const: 'approval' }, title: str(512, 1), detail: str(16384) }),
  ApprovalResponse: obj({ requestId: id, type: { const: 'approval' }, decision: { enum: ['approved', 'rejected'] }, detail: str(16384) }),
  CommentRequest: { oneOf: [ref('ApprovalRequest')] },
  CommentResponse: { oneOf: [ref('ApprovalResponse')] },
  Comment: obj({ commentId: id, sequence: integer(1), author: ref('Actor'), authorKind: { enum: ['user', 'agent', 'system'] }, body: str(65536, 1),
    requests: array(ref('CommentRequest'), 32), responses: array(ref('CommentResponse'), 32), delivery: nullable(ref('ObjectPin')),
    attachments: array(ref('TaskAttachment'), 32), run: nullable(ref('ObjectPin')), turnId: nullable(id), replyTo: nullable(id), createdAt: timestamp, operationId: id }),
  CommentDelivery: obj({ commentId: id, state: { enum: ['queued', 'delivering', 'delivered', 'outcome_unknown'] }, operationId: id, updatedAt: timestamp, code: nullable(id) }),
  PhoneDeliveryRequest: obj({ operationId: id, recipientId: id, route: { const: 'voice' }, voicePrompt: str(4096, 1) }),
  Delivery: obj({ schemaVersion: { const: 1 }, taskId: id, commentId: id, publication: ref('ObjectPin'), requestedRoute: { enum: ['chat', 'phone'] }, activeRoute: { enum: ['chat', 'phone'] },
    state: { enum: ['pending', 'dispatching', 'confirmed', 'outcome_unknown'] }, chatTarget: nullable(ref('ChatTarget')), phoneTarget: nullable(ref('PhoneTarget')),
    operationId: nullable(id), chatRequest: nullable(chat('AdmittedNotifyRequest')), phoneRequest: nullable(ref('PhoneDeliveryRequest')), phoneCallId: nullable(id),
    deliveryEvidence: nullable(ref('ObjectPin')), code: nullable(id), createdAt: timestamp, updatedAt: timestamp }),
  TaskKeySequence: obj({ schemaVersion: { const: 1 }, nextValue: integer(), updatedAt: timestamp }),
  TaskKeyAllocation: obj({ schemaVersion: { const: 1 }, operationId: id, taskKey: { ...id, pattern: '^TASK-[0-9]{4,}$' }, createdAt: timestamp }),
  CommentReadState: obj({ schemaVersion: { const: 1 }, principalId: id, taskId: id, readAgentCommentCount: integer(), updatedAt: timestamp }),
});
// Conversation metadata does not introduce another ticket workflow.
Object.assign(defs.Comment.properties, {
  purpose: { enum: ['update', 'question', 'handoff'] },
  nativeInput: agent('PendingInput'),
  sourceTaskId: id,
});
defs.TaskFields.required = defs.TaskFields.required.filter(key => key !== 'nativeOptions' && key !== 'requiredCapabilities' && key !== 'allowParallel');
defs.TaskFieldsInput = { ...defs.TaskFields, properties: { ...defs.TaskFields.properties,
  userContact: { type: 'string', enum: ['ticket', 'chat', 'phone'],
    description: 'Contact route: ticket records only in the ticket; chat also contacts Main; phone also contacts by voice. Omit on creation to inherit current task_configuration defaults.userContact; omit on edit to preserve the ticket setting.' },
}, required: defs.TaskFields.required.filter(key => key !== 'userContact') };
defs.ExpectedWorkspace = obj({ principalId: id, rootObjectId: nullable(id), callerPrincipalId: id });
const actionBase = { operationId: id }, existing = { ...actionBase, taskId: id, expectedRevision: integer(1) };
const action = (name, properties, base = existing) => {
  const fields = { action: { const: name }, ...base, expectedWorkspace: ref('ExpectedWorkspace'), ...properties };
  return obj(fields, Object.keys(fields).filter(key => key !== 'expectedWorkspace'));
};
Object.assign(defs, {
  ConfigureRequest: action('configure', { configuration: nullable(ref('ObjectPin')), value: ref('Configuration') }, actionBase),
  SavePlanRequest: action('savePlan', { plan: ref('NativePlan'), draftHash: hash }, actionBase),
  SavePlanInput: action('savePlan', { plan: ref('NativePlanDraft') }, actionBase),
  CreateRequest: action('create', { fields: ref('TaskFieldsInput') }, actionBase),
  EditRequest: action('edit', { fields: ref('TaskFieldsInput') }),
  TransitionRequest: action('transition', { workflowState: { enum: ['backlog', 'todo', 'in_progress', 'waiting', 'done', 'cancelled'] }, detail: nullable(str(16384, 1)) }),
  StartRequest: action('start', { target: ref('Target'), intent: ref('ObjectPin'), workspace: ref('WorkspaceResolution'), commentIds: array(id, 256) }),
  DeferRequest: action('defer', { reason: { enum: ['user', 'time', 'dependency', 'host', 'capability', 'workspace'] }, detail: str(16384, 1), nextReviewAt: nullable(timestamp) }),
  ReassignRequest: action('reassign', { executionRequirement: nullable(ref('ExecutionRequirement')), workspaceRequirement: ref('WorkspaceRequirement'),
    reason: str(16384, 1), previousRun: nullable(ref('ObjectPin')), acknowledgeUnresolved: bool }),
  NewThreadRequest: action('newThread', { reason: str(16384, 1) }),
  RecoverThreadRequest: action('recoverThread', { run: ref('ObjectPin') }),
  CancelRequest: action('cancel', { reason: str(16384, 1) }),
  ReviewRequest: action('review', { result: ref('ObjectPin'), acceptance: str(65536, 1) }),
  ContinueRequest: action('continue', { feedback: str(65536, 1), target: ref('Target'), intent: ref('ObjectPin'), workspace: ref('WorkspaceResolution'), commentIds: array(id, 256) }),
  SaveResultRequest: action('saveResult', { run: nullable(ref('ObjectPin')), content: ref('ResultContent'),
    kind: { enum: ['intermediate', 'final'] } }),
  CommentActionRequest: action('comment', { commentId: id, body: str(65536, 1), requests: array(ref('CommentRequest'), 32),
    responses: array(ref('CommentResponse'), 32), attachments: array(ref('TaskAttachment'), 32), replyTo: nullable(id),
    agentDelivery: { enum: ['queue', 'none'], description: 'For a user comment on an agent-controlled task, queue asks TaskBoard to start or resume agent work; none records the comment without scheduling it. Omitted means queue. Worker comments use the task userContact route instead.' } }),
  UploadAttachmentRequest: action('uploadAttachment', { attachmentId: id, filename: str(512, 1), mediaType: str(256, 1), bytesBase64: str(11184812, 1) }),
  ArchiveRequest: action('archive', { archived: bool }),
  ArchiveResult: operation('ObjectMetadata'),
  OperationQuery: obj({ operationId: id }),
  CategoriesQuery: obj({ categories: { const: true } }), CategoriesResult: obj({ categories: array(str(128, 1), 256) }),
  TaskQuery: obj({ task: { anyOf: [id, obj({ taskKey: id })] } }),
  TaskListQuery: obj({
    relatedTo: { ...id, description: 'Find other allocated tasks sharing this task canonical working directory and host. Separate worktrees do not match. Read peer tickets before coordinating.' },
    status: { enum: ['backlog', 'todo', 'in_progress', 'waiting', 'review', 'done', 'cancelled'] },
    category: str(128, 1), limit: integer(1, 100), cursor: str(4096, 1),
  }, []),
  TaskSummary: obj({ taskId: id, revision: integer(1), taskKey: id, title: str(512, 1),
    workflowState: { enum: ['backlog', 'todo', 'in_progress', 'waiting', 'review', 'done', 'cancelled'] },
    category: nullable(str(128, 1)) }),
  TaskListResult: obj({ items: array(ref('TaskSummary'), 100), nextCursor: nullable(str(4096, 1)) }),
  MarkCommentsReadRequest: obj({ operationId: id, taskId: id, expectedTaskRevision: integer(1) }),
  MarkCommentsReadResult: obj({ task: ref('ObjectPin'), readState: ref('ObjectPin'), unreadAgentComments: { const: 0 } }),
  ExecutionCondition: obj({ state: { enum: ['idle', 'queued', 'allocating', 'starting', 'active', 'needs_user', 'deferred', 'blocked_dependency', 'blocked_environment', 'recovering', 'outcome_unknown', 'publishing'] }, detail: str(2048) }),
  TaskView: obj({ object: ref('ObjectPin'), task: ref('Task'), executionCondition: ref('ExecutionCondition'), unreadAgentComments: integer() }),
});
defs.CommentActionRequest.required = defs.CommentActionRequest.required.filter(key => key !== 'agentDelivery');
Object.assign(defs.CommentActionRequest.properties, {
  sourceTaskId: { ...id, description: 'For a directed agent message, identify the different sending task and explicitly set authorKind=agent and agentDelivery=queue. Ordinary worker updates never wake themselves.' },
  authorKind: { enum: ['user', 'agent'], description: 'Use agent for worker-authored comments; these never enqueue their own execution.' },
  purpose: { enum: ['update', 'question', 'handoff'], description: 'Use question when waiting for user input and handoff for the final delivery. TaskBoard owns the transition after the turn ends.' },
  contactUser: { type: 'boolean', description: 'Request contact through the configured Main or voice route for an important question. Routine updates stay in the ticket. Completion contact is automatic.' },
  moveToTodo: { type: 'boolean', description: 'Explicitly reopen a Done task with this comment. Otherwise comments on Done tasks do not schedule work.' },
  nativeInput: agent('PendingInput'),
});
defs.Blocker = obj({ taskId: id, until: { enum: ['idle', 'done'] } });
defs.DeferRequest.properties.blocker = ref('Blocker');
defs.ContinueRequest.properties.coordinationOnly = bool;
defs.ExecutionRequest = { oneOf: [ref('StartRequest'), ref('ContinueRequest')] };
defs.ActionRequest = { oneOf: ['Configure', 'SavePlan', 'Create', 'Edit', 'Transition', 'Defer', 'Reassign', 'NewThread', 'Cancel', 'Review', 'SaveResult', 'CommentAction', 'UploadAttachment'].map(name => ref(name + 'Request')) };
defs.ActionInput = { oneOf: defs.ActionRequest.oneOf.map(value => value.$ref.endsWith('/SavePlanRequest') ? ref('SavePlanInput') : value) };
Object.assign(defs, {
  Claim: obj({ operationId: id, callerPrincipalId: id, attempt: integer(1), owner: ref('Actor'),
    request: ref('ExecutionRequest'), requestHash: hash, target: ref('Target'), workspace: ref('WorkspaceResolution'), primaryAtStart: nullable(resource),
    claimedAt: timestamp, phase: { enum: ['claimed', 'run_created', 'starting', 'running', 'waiting_input', 'publishing_result', 'cancel_requested', 'outcome_unknown'] },
    run: nullable(ref('ObjectPin')) }),
  Task: obj({ schemaVersion: { const: 1 }, taskKey: { ...id, pattern: '^TASK-[0-9]{4,}$' }, fields: ref('TaskFields'),
    blocker: ref('Blocker'),
    workflowState: { type: 'string', enum: ['backlog', 'todo', 'in_progress', 'waiting', 'review', 'done', 'cancelled'] },
    waiting: nullable(ref('Waiting')), publication: nullable(ref('ObjectPin')), primaryResourceRef: nullable(resource), claim: nullable(ref('Claim')),
    attemptCount: integer(), lastRun: nullable(ref('ObjectPin')), lastHistory: nullable(ref('ObjectPin')),
    latestResult: nullable(ref('ObjectPin')), acceptedReview: nullable(ref('ObjectPin')),
    comments: array(ref('Comment'), 512), commentDeliveries: array(ref('CommentDelivery'), 512), agentCommentCount: integer(), workRevision: integer(),
    attachments: array(ref('TaskAttachment'), 256), createdAt: timestamp, updatedAt: timestamp }),
  NativeCallOrigin: { oneOf: [obj({ kind: { const: 'initial' } }), obj({ kind: { const: 'reattach' }, predecessor: ref('ObjectPin'), preparedEpoch: id }),
    obj({ kind: { const: 'steer' }, commentIds: array(id, 512), workRevision: integer(), peersHash: hash, predecessor: nullable(ref('ObjectPin')) })] },
  NativeCall: obj({ operationId: id, request: ref('NativeRequest'), expectedDefinitionHash: hash, origin: ref('NativeCallOrigin'),
    state: { enum: ['prepared', 'observed', 'outcome_unknown'] }, preparedAt: timestamp, observedAt: nullable(timestamp),
    ownerPhase: nullable({ enum: ['accepted', 'dispatched', 'succeeded', 'failed', 'outcome_unknown'] }),
    epoch: nullable(id), evidence: nullable(ref('Artifact')), code: nullable(id) }, ['operationId', 'request', 'expectedDefinitionHash', 'state', 'preparedAt', 'observedAt', 'ownerPhase', 'epoch', 'evidence', 'code']),
  Run: obj({ schemaVersion: { const: 1 }, taskId: id, attempt: integer(1), operationId: id, callerPrincipalId: id,
    originalRequest: ref('ExecutionRequest'), requestHash: hash, target: ref('Target'), workspace: ref('WorkspaceResolution'), plan: ref('NativePlanDraft'), owner: ref('Actor'),
    phase: { type: 'string', enum: ['starting', 'running', 'waiting_input', 'publishing_result', 'completed', 'failed', 'cancel_requested', 'cancelled', 'outcome_unknown'] },
    primaryResourceRef: nullable(resource), turnId: nullable(id), nativeEpoch: nullable(id),
    calls: obj({ thread: nullable(ref('ObjectPin')), turn: nullable(ref('ObjectPin')), interrupt: nullable(ref('ObjectPin')), steer: nullable(ref('ObjectPin')) }, ['thread', 'turn', 'interrupt']),
    notificationCursor: integer(), result: nullable(ref('ObjectPin')),
    cancellation: nullable(obj({ operationId: id, reason: str(16384, 1), requestedAt: timestamp, actor: ref('Actor') })),
    externalOutcome: obj({ state: { enum: ['not_started', 'active', 'succeeded', 'failed', 'cancelled', 'unknown'] },
      observedAt: nullable(timestamp), evidence: nullable(ref('Artifact')), code: nullable(id) }),
    createdAt: timestamp, updatedAt: timestamp, finishedAt: nullable(timestamp) }),
  Result: obj({ schemaVersion: { const: 1 }, taskId: id, run: nullable(ref('ObjectPin')), kind: { enum: ['intermediate', 'final'] },
    content: ref('ResultContent'), createdAt: timestamp, actor: ref('Actor'), operationId: id }),
  Review: obj({ schemaVersion: { const: 1 }, taskId: id, result: ref('ObjectPin'), acceptance: str(65536, 1),
    createdAt: timestamp, actor: ref('Actor'), operationId: id }),
  History: obj({ schemaVersion: { const: 1 }, taskId: id, operationId: id, actor: ref('Actor'), createdAt: timestamp,
    kind: { enum: ['created', 'edited', 'ready', 'started', 'deferred', 'reassigned', 'cancel_requested', 'cancelled', 'reviewed', 'continued', 'result_saved', 'commented', 'recovered', 'outcome_unknown'] },
    detail: str(65536), previousTask: nullable(ref('ObjectPin')), previousHistory: nullable(ref('ObjectPin')),
    fromTarget: nullable(ref('Target')), toTarget: nullable(ref('Target')), priorRun: nullable(ref('ObjectPin')),
    result: nullable(ref('ObjectPin')), review: nullable(ref('ObjectPin')) }),
  ActionOutcome: obj({ operationId: id, task: nullable(ref('ObjectPin')), plan: nullable(ref('ObjectPin')), run: nullable(ref('ObjectPin')),
    history: nullable(ref('ObjectPin')), result: nullable(ref('ObjectPin')), review: nullable(ref('ObjectPin')), attachment: nullable(ref('TaskAttachment')), configuration: ref('ObjectPin'), archive: ref('ArchiveResult') },
    ['operationId', 'task', 'plan', 'run', 'history', 'result', 'review', 'attachment']),
});
defs.Task.required = defs.Task.required.filter(key => key !== 'blocker');
// Domain Objects are strict, distinct contracts. Large native schemas stay behind exact revision
// pins. Publication bytes are a hash-checked JSON artifact, validated against contractKey both
// before staging and before dispatch; the destination Hive write validates them again.
const contracts = { configuration: 'Configuration', task: 'Task', run: 'Run', result: 'Result', review: 'Review', history: 'History', delivery: 'Delivery', 'native-plan': 'NativePlan', 'native-call': 'NativeCall' };
const contractVersions = { task: '1.7.0', run: '1.1.0', configuration: '1.2.0', delivery: '1.1.0' };
// Internal owner observations are durable actions, but are never public service tools or accepted
// as a user/worker ActionRequest. Their referenced bytes precede every resulting publication.
defs.NativeUpdateRequest = obj({ action: { const: 'nativeUpdate' }, ...existing, run: ref('ObjectPin'), change: { oneOf: [
  obj({ kind: { const: 'attachCall' }, slot: { enum: ['thread', 'turn', 'interrupt', 'steer'] }, call: ref('ObjectPin') }),
  obj({ kind: { const: 'reattachCall' }, call: ref('ObjectPin') }),
  obj({ kind: { const: 'observeCall' }, slot: { enum: ['thread', 'turn', 'interrupt', 'steer'] }, call: ref('ObjectPin') }),
  obj({ kind: { const: 'snapshot' }, evidence: ref('Artifact'), nativeOperationId: id, notificationCursor: integer() }),
  obj({ kind: { const: 'readSnapshot' }, snapshot: ref('Artifact'), notificationCursor: integer() }),
  obj({ kind: { const: 'completion' }, snapshot: ref('Artifact'), output: nullable(obj({ cursor: nullable(str(8192, 1)), evidence: ref('ReadEvidenceRef') })), notificationCursor: integer() }),
  obj({ kind: { const: 'transcript' }, evidence: ref('Artifact'), notificationCursor: integer() }),
  obj({ kind: { const: 'unavailable' }, reason: { enum: ['host', 'external_outcome'] }, code: id, detail: str(16384, 1) }),
  obj({ kind: { const: 'cancelUnstarted' } }), obj({ kind: { const: 'retireUnstarted' } }),
] } });
defs.DurableRequest = { oneOf: [ref('ActionRequest'), ref('SchedulerRequest'), ref('NativeUpdateRequest'), ref('ArchiveRequest')] };

defs.NativeLifecycleCall = obj({ key: id,
  owner: obj({ serviceNodeId: id, hostId: id, nativeVersion: id, nativeExecutableHash: hash, catalogHash: hash }),
  operationId: id, method: { enum: ['thread/archive', 'thread/unarchive', 'thread/start', 'thread/resume'] }, params: wire('Json'),
  definitionHash: hash, observation: nullable(agent('Operation')) });
defs.PreparedWrite = obj({
  mutationId: id, contractKey: { enum: Object.keys(contracts).map(key => 'task-board/' + key) }, contractVersion: { enum: ['1.0.0', '1.1.0', '1.2.0', '1.3.0', '1.4.0', '1.5.0', '1.6.0', '1.7.0'] }, payload: ref('Artifact'),
  destination: { oneOf: [obj({ create: obj({ parentId: nullable(id), name: str(256, 1) }) }),
    obj({ objectId: id, expectedRevision: integer(1) })] }, outcome: nullable(ref('ObjectPin')) });
defs.Operation = obj({ schemaVersion: { const: 1 }, operationId: id, callerPrincipalId: id, request: ref('DurableRequest'),
  requestHash: hash, actor: ref('Actor'), createdAt: timestamp, updatedAt: timestamp,
  phase: { type: 'string', enum: ['accepted', 'preparing', 'publishing', 'succeeded', 'failed', 'needs_attention'] },
  writes: array(ref('PreparedWrite'), 16), outcome: nullable(ref('ActionOutcome')),
  error: nullable(obj({ code: id, message: str(16384, 1), execution: { enum: ['not_executed', 'completed', 'unknown'] } })),
  nativeCalls: array(ref('NativeLifecycleCall'), 1024), archiveMutationId: id },
  ['schemaVersion', 'operationId', 'callerPrincipalId', 'request', 'requestHash', 'actor', 'createdAt', 'updatedAt', 'phase', 'writes', 'outcome', 'error']);
defs.Settings = obj({ principalId: id, rootObjectId: nullable(id),
  scheduler: obj({ enabled: bool, intervalMs: integer(1000, 3600000), pageSize: integer(1, 50) }),
  phoneTarget: nullable(ref('PhoneTarget')) });
defs.WorkspaceQuery = obj({});
defs.WorkspaceInfo = obj({ serviceNodeId: id, generation: integer(1), principalId: id, rootObjectId: nullable(id),
  callerPrincipalId: id, role: { enum: ['user', 'worker', 'observer'] }, recovered: bool, observedAt: timestamp,
  scheduler: defs.Settings.properties.scheduler, phoneTarget: nullable(ref('PhoneTarget')) });
// A durable, non-expiring gate serializes short domain publications and dependency graph edits.
// Native execution and waiting for external outcomes never hold this gate.
defs.Coordination = obj({ schemaVersion: { const: 1 }, principalId: id, activeOperation: nullable(ref('ObjectPin')), updatedAt: timestamp });
defs.SchedulerRequest = { oneOf: [ref('StartRequest'), ref('ContinueRequest'), ref('TransitionRequest'), ref('DeferRequest'), ref('RecoverThreadRequest')] };
defs.ScanCursor = obj({ schemaVersion: { const: 1 }, principalId: id, serviceNodeId: id, lane: { enum: ['operations', 'runs', 'tasks', 'deliveries'] },
  queryHash: hash, cursor: nullable(str(8192, 1)), updatedAt: timestamp });
defs.SchedulerAttempt = obj({ schemaVersion: { const: 1 }, principalId: id, task: ref('ObjectPin'), sequence: integer(1),
  request: ref('SchedulerRequest'), requestHash: hash, previous: nullable(ref('ObjectPin')), previousOperation: nullable(ref('ObjectPin')), preparedAt: timestamp });
// Current-owner observations are advisory. Reply payloads and answer authority remain in AgentManager.
defs.NativeSignals = obj({ schemaVersion: { const: 1 }, run: ref('ObjectPin'), serviceNodeId: id,
  nativeVersion: id, epoch: id, threadId: id, turnId: id, observedAt: timestamp,
  inputsTruncated: bool, inputs: array(obj({ identity: { $ref: 'https://ivy.invalid/schemas/agent.schema.json#/$defs/InputIdentity' },
    method: str(192, 1), turnId: nullable(id), state: { enum: ['pending', 'answering'] }, observedAt: timestamp }), 128),
  notificationCursor: integer(), notificationGap: bool, notificationsPending: bool,
  activity: array(obj({ epoch: id, sequence: integer(1), method: str(192, 1), observedAt: timestamp }), 100) });
defs.SchedulerAttempt.allOf = [
  { if: { properties: { sequence: { const: 1 } } }, then: { properties: { previous: nil, previousOperation: nil } },
    else: { properties: { previous: ref('ObjectPin'), previousOperation: ref('ObjectPin') } } },
];
defs.NativeEvidenceChunk = obj({ object: ref('ObjectPin'), contentHash: hash, byteLength: integer(1, 1048576) });
defs.NativeEvidence = obj({ schemaVersion: { const: 1 }, format: { const: 'agent-operation/canonical-json' },
  operationId: id, callerPrincipalId: id, serviceNodeId: id, nativeVersion: id, nativeExecutableHash: hash,
  method: str(256, 1), requestHash: hash, phase: { enum: ['accepted', 'dispatched', 'succeeded', 'failed', 'outcome_unknown'] }, epoch: nullable(id),
  contentHash: hash, byteLength: integer(1, 16777216), chunks: { ...array(ref('NativeEvidenceChunk'), 16), minItems: 1 } });
defs.NativeReadEvidence = obj({ schemaVersion: { const: 1 }, format: { const: 'agent-read-observation/canonical-json' },
  observationId: id, callerPrincipalId: id, serviceNodeId: id, nativeVersion: id, nativeExecutableHash: hash,
  catalogHash: hash, method: { enum: ['thread/read', 'thread/turns/list', 'thread/items/list'] }, requestHash: hash, epoch: id,
  contentHash: hash, byteLength: integer(1, 16777216), chunks: { ...array(ref('NativeEvidenceChunk'), 16), minItems: 1 } });
const nativeCursor = nullable(str(8192, 1));
defs.NativeTurnSearch = obj({ schemaVersion: { const: 1 }, run: ref('ObjectPin'), nativeVersion: id, serviceNodeId: id,
  epoch: id, threadId: id, turnId: id, phase: { enum: ['searching', 'finished'] }, nextCursor: nativeCursor,
  pageCount: integer(1, 1024), visitedCursorHashes: { ...array(hash, 1024), minItems: 1, uniqueItems: true }, createdAt: timestamp, updatedAt: timestamp });
defs.NativeTurnSearch.allOf = [
  { if: { properties: { phase: { const: 'searching' } } }, then: { properties: { nextCursor: str(8192, 1) } } },
  { if: { properties: { phase: { const: 'finished' } } }, then: { properties: { nextCursor: nil } } },
];
defs.ReadEvidenceRef = obj({ artifact: ref('Artifact'), observationId: id, epoch: id });
defs.NativeTurnSnapshot = obj({ schemaVersion: { const: 1 }, run: ref('ObjectPin'), nativeVersion: id, serviceNodeId: id, epoch: id,
  threadId: id, turnId: id, turnCursor: nativeCursor, metadata: ref('ReadEvidenceRef'), turn: ref('ReadEvidenceRef'),
  status: { enum: ['inProgress', 'completed', 'failed', 'interrupted'] }, createdAt: timestamp });
defs.NativeResultPage = obj({ schemaVersion: { const: 1 }, runId: id, collectionId: id, previous: nullable(ref('ObjectPin')), index: integer(1, 1024),
  cursor: nativeCursor, nextCursor: nativeCursor, evidence: ref('ReadEvidenceRef'), itemCount: integer(0, 100), nativeBytes: integer(1, 16777216) });
defs.NativeTranscript = obj({ schemaVersion: { const: 1 }, collectionId: id, run: ref('ObjectPin'), snapshot: ref('Artifact'),
  phase: { enum: ['collecting', 'complete'] }, head: nullable(ref('ObjectPin')), pageCount: integer(0, 1024), nativeBytes: integer(0, 1073741824),
  nextCursor: nativeCursor, verification: nullable(ref('ReadEvidenceRef')), verificationCursor: nativeCursor, createdAt: timestamp, updatedAt: timestamp });
defs.NativeTranscript.allOf = [{ if: { properties: { phase: { const: 'complete' } } }, then: { properties: {
  head: ref('ObjectPin'), pageCount: integer(1, 1024), nextCursor: nil, verification: ref('ReadEvidenceRef'),
} } }];
// Separate immutable complete evidence for native builds whose declared item API returns -32601.
// Existing paged transcript/page contracts retain their exact schemas and interpretation.
defs.NativeFullTurnTranscript = obj({ schemaVersion: { const: 1 }, collectionId: id, run: ref('ObjectPin'), snapshot: ref('Artifact'),
  unsupportedItems: ref('ReadEvidenceRef'), fullTurn: ref('ReadEvidenceRef'), fullTurnCursor: nativeCursor,
  fullTurnBytes: integer(1, 16777216), verification: ref('ReadEvidenceRef'), verificationCursor: nativeCursor, createdAt: timestamp });

const when = (key, value, properties) => ({ if: { properties: { [key]: { const: value } } }, then: { properties } });
defs.Task.allOf = [
  when('workflowState', 'waiting', { waiting: ref('Waiting') }),
  { if: { properties: { workflowState: { enum: ['backlog', 'in_progress', 'review', 'done', 'cancelled'] } } }, then: { properties: { waiting: nil } } },
  { if: { properties: { workflowState: { const: 'in_progress' }, fields: { properties: { control: { const: 'agent' } } } } }, then: { properties: { claim: ref('Claim') } } },
  { if: { properties: { workflowState: { enum: ['backlog', 'todo', 'review', 'done', 'cancelled'] } } }, then: { properties: { claim: nil } } },
];
defs.Run.allOf = [
  { if: { properties: { phase: { enum: ['running', 'waiting_input', 'publishing_result', 'completed'] } } },
    then: { properties: { primaryResourceRef: resource, turnId: id, nativeEpoch: id } } },
  when('phase', 'completed', { result: ref('ObjectPin'), finishedAt: timestamp }),
  when('phase', 'cancel_requested', { cancellation: defs.Run.properties.cancellation.anyOf[0] }),
  when('phase', 'outcome_unknown', { externalOutcome: { properties: { state: { const: 'unknown' } } } }),
  when('phase', 'cancelled', { cancellation: defs.Run.properties.cancellation.anyOf[0], finishedAt: timestamp,
    externalOutcome: { properties: { state: { const: 'cancelled' } } } }),
];
defs.Operation.allOf = [when('phase', 'succeeded', { outcome: ref('ActionOutcome'), error: nil }),
  when('phase', 'failed', { outcome: nil, error: defs.Operation.properties.error.anyOf[0] }),
  when('phase', 'needs_attention', { error: defs.Operation.properties.error.anyOf[0] }),
  { if: { properties: { request: { properties: { action: { const: 'nativeUpdate' } } } } }, then: { properties: { actor: { properties: { source: { const: 'native' } } } } } }];
defs.NativeCall.allOf = [when('state', 'prepared', { observedAt: nil, ownerPhase: nil, epoch: nil, evidence: nil, code: nil }),
  when('state', 'observed', { observedAt: timestamp, ownerPhase: defs.NativeCall.properties.ownerPhase.anyOf[0], evidence: ref('Artifact') })];

const document = { $schema: 'https://json-schema.org/draft/2020-12/schema', $id: 'https://ivy.invalid/schemas/task-board.schema.json',
  title: 'TaskBoard durable workflow, action and native-intent contracts', $defs: defs };
const path = 'specs/schemas/task-board.schema.json', content = JSON.stringify(document, null, 2) + '\n';
if (process.argv.includes('--check')) { if (readFileSync(path, 'utf8') !== content) throw new Error('Stale TaskBoard contract.'); }
else writeFileSync(path, content);
const operations = Object.fromEntries(defs.ActionInput.oneOf.map(item => {
  const name = item.$ref.slice('#/$defs/'.length), actionName = defs[name].properties.action.const;
  return [actionName, { input: '#/$defs/' + name, output: '#/$defs/ActionOutcome', mutation: true }];
}));
operations.operation = { input: '#/$defs/OperationQuery', output: '#/$defs/Operation', mutation: false };
operations.workspace = { input: '#/$defs/WorkspaceQuery', output: '#/$defs/WorkspaceInfo', mutation: false };
operations.configuration = { input: '#/$defs/ConfigurationQuery', output: '#/$defs/ConfigurationView', mutation: false };
operations.categories = { input: '#/$defs/CategoriesQuery', output: '#/$defs/CategoriesResult', mutation: false };
operations.read = { input: '#/$defs/TaskQuery', output: '#/$defs/TaskView', mutation: false };
operations.list = { input: '#/$defs/TaskListQuery', output: '#/$defs/TaskListResult', mutation: false };
operations.markCommentsRead = { input: '#/$defs/MarkCommentsReadRequest', output: '#/$defs/MarkCommentsReadResult', mutation: true };
operations.archive = { input: '#/$defs/ArchiveRequest', output: '#/$defs/ArchiveResult', mutation: true };
const catalogPath = 'specs/schemas/task-board.operations.json';
const catalogContent = JSON.stringify({ schemaVersion: 1, namespace: 'task-board', schema: document.$id,
  contracts: Object.entries({ ...contracts, 'task-key-sequence': 'TaskKeySequence', 'task-key-allocation': 'TaskKeyAllocation', 'comment-read-state': 'CommentReadState', operation: 'Operation', coordination: 'Coordination', 'native-evidence': 'NativeEvidence', 'native-read-evidence': 'NativeReadEvidence',
    'native-signals': 'NativeSignals', 'scan-cursor': 'ScanCursor', 'scheduler-attempt': 'SchedulerAttempt', 'native-turn-search': 'NativeTurnSearch', 'native-turn-snapshot': 'NativeTurnSnapshot', 'native-result-page': 'NativeResultPage', 'native-transcript': 'NativeTranscript', 'native-full-turn-transcript': 'NativeFullTurnTranscript' }).map(([key, name]) => ({ key: 'task-board/' + key, version: contractVersions[key] ?? '1.0.0', definition: '#/$defs/' + name })),
  binaryContracts: [{ key: 'task-board/attachment', version: '1.0.0', definitionFile: 'task-board.attachment.json' },
    { key: 'task-board/native-evidence-chunk', version: '1.0.0', definitionFile: 'task-board.evidence-chunk.json' },
    { key: 'task-board/native-read-evidence-chunk', version: '1.0.0', definitionFile: 'task-board.read-evidence-chunk.json' }],
  operations }, null, 2) + '\n';
if (process.argv.includes('--check')) { if (readFileSync(catalogPath, 'utf8') !== catalogContent) throw new Error('Stale TaskBoard operation catalog.'); }
else writeFileSync(catalogPath, catalogContent);
const chunkPath = 'specs/schemas/task-board.evidence-chunk.json';
const chunkContent = JSON.stringify({ key: 'task-board/native-evidence-chunk', version: '1.0.0', owner: { kind: 'service', serviceName: 'task-board' }, mediaType: 'application/octet-stream',
  specMarkdown: '# TaskBoard native evidence chunk 1.0.0\n\nExact binary bytes, at most 1 MiB per chunk. Evidence manifests and their chunks are direct children of their owning primary TaskBoard NativeCall or Run. The native-evidence JSON manifest pins every immutable chunk revision, hash and length, in order. Reassembled canonical UTF-8 JSON is at most 16 MiB and must validate as Agent.Operation and against its exact checked native catalog, original caller/node/request identity and method response. Unbound bytes are not native proof. No native dispatch is authorized by writing a chunk. Hive binary/JSON limits remain unchanged.' }, null, 2) + '\n';
if (process.argv.includes('--check')) { if (readFileSync(chunkPath, 'utf8') !== chunkContent) throw new Error('Stale TaskBoard evidence chunk contract.'); }
else writeFileSync(chunkPath, chunkContent);
const readChunkPath = 'specs/schemas/task-board.read-evidence-chunk.json';
const readChunkContent = JSON.stringify({ key: 'task-board/native-read-evidence-chunk', version: '1.0.0', owner: { kind: 'service', serviceName: 'task-board' }, mediaType: 'application/octet-stream',
  specMarkdown: '# TaskBoard native read evidence chunk 1.0.0\n\nExact bytes of a canonical Agent.ReadObservation, at most 1 MiB per chunk and 16 MiB for the complete observation. The native-read-evidence manifest pins every immutable chunk revision/hash/length in order. Manifest and chunks belong directly to the primary Run. Verify original caller, selected node, planned version, epoch, observation identity, request parameters/hash and checked native executable/catalog plus native input/output schemas. Reads are observations and never mutation receipts or resend authority. Existing native-evidence and native-evidence-chunk contracts are unchanged.' }, null, 2) + '\n';
if (process.argv.includes('--check')) { if (readFileSync(readChunkPath, 'utf8') !== readChunkContent) throw new Error('Stale TaskBoard read evidence chunk contract.'); }
else writeFileSync(readChunkPath, readChunkContent);
const attachmentPath = 'specs/schemas/task-board.attachment.json';
const attachmentContent = JSON.stringify({ key: 'task-board/attachment', version: '1.0.0', owner: { kind: 'service', serviceName: 'task-board' }, mediaType: 'application/octet-stream',
  retention: { objects: { mode: 'owned' }, revisions: { mode: 'current' } },
  specMarkdown: '# TaskBoard Task attachment 1.0.0\n\nExact immutable bytes owned by one retained TaskBoard Task. A Task revision references every published attachment. The Task embeds filename, declared media type, length, hash and uploader; unsupported media is download-only.' }, null, 2) + '\n';
if (process.argv.includes('--check')) { if (readFileSync(attachmentPath, 'utf8') !== attachmentContent) throw new Error('Stale TaskBoard attachment contract.'); }
else writeFileSync(attachmentPath, attachmentContent);
