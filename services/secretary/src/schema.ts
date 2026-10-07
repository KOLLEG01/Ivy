import { SchemaValidators, IvyError, hashJson, bundleLocalSchema } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import { browserNoticeTopic } from '../../../packages/sdk/src/node.js';
import schema from '../../../specs/schemas/secretary.schema.json' with { type: 'json' };
import catalog from '../../../specs/schemas/secretary.contracts.json' with { type: 'json' };

export type Pin = Chat.ObjectPin;
export interface Scope { secretaryId: string; rootObjectId: string }
export interface SourceDefinition { sourceId: string; accountId: string; kind: 'email' | 'teams' | 'whatsapp'; producerPrincipalIds: string[]; allowedSenderIds: string[] | null; ownSenderIds: string[] }
export interface Identity { scope: Scope; principalId: string; serviceNodeId: string; hostId: string }
export interface Policy { silentTime: { timeZone: string; start: string; end: string } | null; urgentBypass: boolean; whatsappQuestionsOnly: boolean; minimumMainUrgency: 'normal' | 'high' | 'critical'; researchMaxMinutes: number; voiceEscalation: { serviceNodeId: string; recipientId: string; requestDefinitionHash: string } | null }
export interface NoticeTarget { serviceNodeId: string; expectedBridge: Chat.ExpectedBridge; channel: Chat.Channel; notifyDefinitionHash?: string; noticeDefinitionHash?: string }
export interface Settings { identity: Identity; sources: SourceDefinition[]; policy: Policy; pollMs: number; recordsPerTick: number; followUpPrincipalIds?: string[] }
export interface ContactWindow { timeZone: string; start: string; end: string }
export interface ContactRule { enabled: boolean; minimumUrgency: 'normal' | 'high' | 'critical'; window: ContactWindow | null }
export interface WhatsAppRule extends ContactRule { questionsOnly: boolean }
export interface VoiceRule { enabled: boolean; minimumUrgency: 'critical'; window: ContactWindow | null; immediateOnly: boolean }
export interface GlobalRules { main: ContactRule; whatsapp: WhatsAppRule; voice: VoiceRule; researchMaxMinutes: number }
export interface ContactRuleOverride { enabled?: boolean; minimumUrgency?: 'normal' | 'high' | 'critical'; window?: ContactWindow | null }
export interface WhatsAppRuleOverride extends ContactRuleOverride { questionsOnly?: boolean }
export interface VoiceRuleOverride { enabled?: boolean; minimumUrgency?: 'critical'; window?: ContactWindow | null; immediateOnly?: boolean }
export interface RuleOverrides { main?: ContactRuleOverride; whatsapp?: WhatsAppRuleOverride; voice?: VoiceRuleOverride; researchMaxMinutes?: number }
export interface ExecutionTarget { serviceNodeId: string; threadCwd: string; model: string | null; effort: 'low' | 'medium' | 'high' | 'xhigh'; permissions: string }
export interface NativeTarget { serviceNodeId: string; hostId: string; nativeVersion: string; nativeExecutableHash: string; catalogHash: string }
export interface SecretaryConfiguration { schemaVersion: 1; rules: GlobalRules; execution: ExecutionTarget | null; updatedAt: string }
export interface ScheduleTrigger { kind: 'schedule'; timeZone: string; cadence: 'interval' | 'daily' | 'weekly'; intervalMinutes: number | null; localTime: string | null; weekdays: number[] }
export interface EventTrigger { kind: 'event'; topic: string; topicVersion: string; sourceServiceNodeId: string | null; eventKind: string | null }
export interface ObjectTrigger {
  kind: "object-change";
  objectIds: string[];
  paths: string[];
  delaySeconds: number;
  observation: {
    completePath: string;
    observedAtPath: string;
    maximumAgeSeconds: number;
  } | null;
}
export type AssignmentTrigger = ScheduleTrigger | EventTrigger | ObjectTrigger;
export interface Preflight { executable: string; args: string[]; timeoutMs: number }
export interface AssignmentExecution { model: string | null; effort: ExecutionTarget['effort'] | null; reuse: 'main' | 'assignment' | 'new' }
export interface Assignment { schemaVersion: 1; assignmentId: string; name: string; description: string; enabled: boolean; builtInKey?: string | null; minimumNotificationAgeMinutes?: number; trigger: AssignmentTrigger; prompt: string; preflight: Preflight | null; rules: RuleOverrides; execution?: AssignmentExecution; createdAt: string; updatedAt: string }
export interface ScheduleProgress { schemaVersion: 1; assignmentId: string; assignment: Pin; assignmentSnapshot: Assignment; checkedAt: string; updatedAt: string }
export interface EventProgress { schemaVersion: 1; assignmentId: string; assignment: Pin; assignmentSnapshot: Assignment; processedThrough: number; active: boolean; needsRebase: boolean; updatedAt: string }
export interface ExecutionTrigger { kind: 'schedule' | 'event'; key: string; occurredAt: string; payload: Wire.Json }
export interface NativeOperations { threadStart: string | null; turnStart: string | null; archive: string | null; delete: string | null }
export interface Execution { schemaVersion: 1; executionId: string; assignment: Pin; assignmentSnapshot: Assignment; trigger: ExecutionTrigger; effectiveRules: GlobalRules; executionTarget: ExecutionTarget;
  delivery?: import('./assignment-delivery.js').Handoff; voice?: import('./assignment-voice.js').VoiceRecord;
  phase: 'queued' | 'preflight' | 'starting' | 'running' | 'completed' | 'settled' | 'archiving' | 'archived' | 'deleting' | 'deleted' | 'failed'; serviceNodeId: string; nativeTarget: NativeTarget | null; threadId: string | null; turnId: string | null;
  nativeOperations: NativeOperations; preflightOutput: string | null; result: string | null; errorCode: string | null; createdAt: string; startedAt: string | null; completedAt: string | null; archivedAt: string | null; deleteAfter: string | null; deletedAt: string | null; updatedAt: string }
export interface Message { accountId: string; conversationId: string; messageId: string; nativeRevision: string; senderId: string; outgoing: boolean; attachments: 'none' | 'expected'; occurredAt: string; observedAt: string; title: string; text: string; url: string | null }
export interface Assessment { urgency: 'low' | 'normal' | 'high' | 'critical'; disposition: 'ignore' | 'record' | 'notify' | 'task'; notification: 'none' | 'main' | 'voice' | 'needs_attention'; summary: string; question: boolean; usefulIntel: boolean; task: Pin | null }
export interface Decision { assessment: Assessment; actor: string; operationId: string; decidedAt: string; policy: Policy }
export interface Notice { state: 'suppressed' | 'queued' | 'dispatching' | 'outcome_unknown' | 'confirmed'; reason: string | null; text: string; source: Pin; target: NoticeTarget | null; request: Chat.AdmittedNotifyRequest | null; preparedRevision: number | null; evidence: Pin | null; voice: { state: 'queued' | 'confirmed' | 'outcome_unknown'; operationId: string; callId: string | null; errorCode: string | null } | null; updatedAt: string }
export interface ItemMedia { contractKey: string; pin: Pin | null; contentHash: string | null; name: string | null; mediaType: string | null; errorCode: string | null }
export interface Item { schemaVersion: 1; identityHash: string; sourceId: string; messageHash: string; message: Message; media: ItemMedia[]; decision: Decision | null; notice: Notice | null }
export interface Source { schemaVersion: 1; definition: SourceDefinition; definitionHash: string; cursor: string | null; checkpointOperation: { principalId: string; operationId: string } | null; updatedAt: string | null }
interface BaseRequest { operationId: string; expectedScope: Scope }
export interface CaptureRequest extends BaseRequest { action: 'capture'; sourceId: string; message: Message }
export interface AssessRequest extends BaseRequest { action: 'assess'; item: Pin; assessment: Assessment }
export interface CheckpointRequest extends BaseRequest { action: 'checkpoint'; sourceId: string; previousCursor: string | null; nextCursor: string; captures: string[] }
export interface AttachRequest extends BaseRequest { action: 'attach'; sourceId: string; item: Pin; media: ItemMedia[] }
export type Request = CaptureRequest | AssessRequest | CheckpointRequest | AttachRequest;
export type Destination = { createName: string } | { object: Pin };
export type Prepared = { key: 'secretary/item'; destination: Destination; value: Item } | { key: 'secretary/source'; destination: Destination; value: Source };
export interface Accepted { schemaVersion: 1; action: Request['action']; operationId: string; callerPrincipalId: string; requestHash: string; request: Request | null; sourceId: string; source: Source; createdAt: string;
  prepared: Prepared | null; existingEffect: Pin | null; ignoredReason: 'own_sender' | 'sender_not_allowed' | 'outgoing' | null; captureIdentity: string | null;
  checkpointCaptures: Pin[]; phase: 'accepted' | 'succeeded' | 'failed'; effect: Pin | null; errorCode: string | null }
export interface Status { identity: Identity; sources: { object: Pin; data: Source }[]; policy: Policy; noticeConfigured: boolean; recoveryIssues: { objectId: string; code: string }[]; observedAt: string }
export interface ListAssignmentsRequest { expectedScope: Scope; assignmentId?: string; cursor?: string | null; limit?: number; enabled?: boolean; triggerKind?: AssignmentTrigger['kind'] }
export interface AssignmentEntry { pin: Pin; assignment: Assignment }
export interface AssignmentPage { assignments: AssignmentEntry[]; nextCursor: string | null; observedAt: string }
export interface GetAssignmentRequest { expectedScope: Scope; assignmentId: string }
export interface AssignmentEntryResponse extends AssignmentEntry { observedAt: string }
export interface OperationOutcome { operationId: string; action: Request['action'] | 'create_assignment' | 'update_assignment' | 'configure_execution'; phase: 'accepted' | 'succeeded' | 'failed' | 'unknown'; effect: Pin | null; errorCode: string | null; observedAt: string }
export interface SaveConfigurationRequest { operationId: string; configuration: Pin; value: SecretaryConfiguration }
export interface SaveAssignmentRequest { operationId: string; assignment: Pin | null; value: Assignment }
export interface FollowUpRequest { operationId: string; itemId: string; question: string }
export interface FollowUpResult { operationId: string; itemId: string; phase: 'prepared' | 'running' | 'succeeded' | 'outcome_unknown'; threadId: string | null; turnId: string | null; answer: string | null; errorCode: string | null }
export interface Values { 'secretary/item': Item; 'secretary/source': Source; 'secretary/operation': Accepted; 'secretary/configuration': SecretaryConfiguration; 'secretary/assignment': Assignment; 'secretary/schedule-progress': ScheduleProgress; 'secretary/event-progress': EventProgress; 'secretary/execution': Execution }
export const validators = new SchemaValidators();
export const bundled = (name: string) => bundleLocalSchema(schema, name);
export function validate(name: string, value: unknown): void { validators.validate(bundled(name), value); }
export function need(value: unknown, code: string, message: string): asserts value { if (!value) throw new IvyError(code, message); }
export const same = (a: unknown, b: unknown) => hashJson(a) === hashJson(b);
export const version = '1.0.0';
const histories: Record<string, string[]> = {
  'secretary/item': ['1.0.0', '1.1.0', '1.2.0', '1.3.0'],
  'secretary/assignment': ['1.0.0', '1.1.0', '1.2.0', '1.3.0', '1.4.0'],
  'secretary/schedule-progress': ['1.0.0', '1.1.0', '1.2.0'],
  'secretary/event-progress': ['1.0.0', '1.1.0', '1.2.0'],
  'secretary/execution': [
    '1.0.0',
    '1.1.0',
    '1.2.0',
    '1.3.0',
    '1.4.0',
    '1.5.0',
    "1.6.0",
  ],
};
export const readableVersions = (key: string): string[] => histories[key] ?? [version];
export const contractVersion = (key: string) => readableVersions(key).at(-1)!;

export function secretaryRegistry(): Wire.RegistrySync {
  const descriptions: Record<string, string> = {
    'secretary/item': 'Final selected message for the user-facing inbox. Source identity, assessment and useful notice state are materialized in its current revision. Source cursors and operation receipts live in the local Secretary journal.',
    'secretary/configuration': 'Global Secretary execution target and contact rules. Assignments inherit fields from this current revision.',
    'secretary/assignment': 'One revisioned system-originated schedule or event assignment, including its prompt, hidden optional preflight process and sparse rule overrides.',
    'secretary/schedule-progress': 'Durable per-assignment schedule cursor. A trigger is acknowledged only after its exact execution has been saved.',
    'secretary/event-progress': 'Durable per-assignment event position and assignment snapshot. Each Hive event is acknowledged after matching executions and progress have been saved.',
    'secretary/execution': 'Durable Secretary execution journal from admitted trigger through native task archival and seven-day deletion.'
  };
  const contracts: Wire.DataContract[] = catalog.contracts.flatMap(value => {
    const current: Wire.DataContract = { key: value.key, version: contractVersion(value.key), owner: { kind: 'service', serviceName: 'secretary' }, mediaType: 'application/json', jsonSchema: bundled(value.definition),
    retention: value.key === 'secretary/item' ? { objects: { mode: 'expire', maximumAgeDays: 90 }, revisions: { mode: 'all' } }
      : value.key === 'secretary/execution' ? { objects: { mode: 'retain' }, revisions: { mode: 'all' } }
      : value.key === 'secretary/assignment' ? { objects: { mode: 'retain' }, revisions: { mode: 'all' } }
      : { objects: { mode: 'retain' }, revisions: { mode: 'bounded', maximumCount: 100 } }, specMarkdown: descriptions[value.key]! };
    return [current];
  });
  const mcpTitles: Record<string, string> = { operation: 'Read inbox action outcome', operationRead: 'Read Secretary operation outcome', status: 'Check Secretary sources', binding: 'Read Secretary binding', listAssignments: 'List assignments', saveConfiguration: 'Configure assignments' };
  const tools: Wire.ToolDefinition[] = Object.entries(catalog.operations).map(
    ([name, operation]) => ({
      namespace: 'secretary',
      name,
      interfaceVersion: version,
      description: { capture: 'Capture one original source observation.', assess: 'Record the decision for one original unassessed item.', attach: 'Attach acquired media evidence to one original inbox item.', checkpoint: 'Advance one source cursor after saved capture receipts.', operation: 'Read the recorded outcome of an inbox action by its operation ID.', operationRead: 'Read the caller-owned outcome of an inbox or assignment operation by its original ID.', status: 'Read source collection checkpoints, notification settings, and recovery issues.', binding: 'Read the current service node, host and expected scope without a prior scope value.', listAssignments: 'List assignments triggered by schedules or events, with current revisions for updates.', getAssignment: 'Read one current Secretary assignment and its exact revision pin.', saveConfiguration: 'Set default execution targets and contact rules for Secretary assignments.', saveAssignment: 'Create or update one exact Secretary schedule, reminder or event-triggered assignment.' }[name]!,
      ...(name === 'listAssignments' || name === 'binding' || name === 'status' || name === 'saveConfiguration' || name === 'operation' || name === 'operationRead'
        ? { discovery: { keywords: [], mcp: {
      name: name === 'operation' ? 'secretary_history' : name === 'operationRead' ? 'secretary_operation_read' : name === 'saveConfiguration' ? 'secretary_configure_execution' : 'secretary_' + name.replace(/[A-Z]/g, letter => '_' + letter.toLowerCase()),
      surface: name === 'listAssignments' || name === 'binding' || name === 'operationRead' ? 'ivy' : 'ivy_dev',
    } } }
        : {}),
      inputSchema: bundled(operation.input),
      outputSchema: bundled(operation.output),
      annotations: { readOnlyHint: !operation.mutation, idempotentHint: true,
      ...(mcpTitles[name] ? { title: mcpTitles[name], openWorldHint: false } : {}) },
    }),
  );
  const assignmentInput = (creating: boolean) => {
    const schema = structuredClone(bundled('SaveAssignmentRequest')) as Record<string, unknown>;
    const definitions = schema['$defs'] as Record<string, Record<string, unknown>>;
    const properties = definitions['SaveAssignmentRequest']!['properties'] as Record<string, unknown>;
    const nullablePin = properties['assignment'] as { anyOf: unknown[] };
    properties['assignment'] = creating ? { type: 'null' } : nullablePin.anyOf[0]!;
    return schema;
  };
  tools.push(
    { discovery: { keywords: ['schedule', 'reminder', 'Zeitplan'], mcp: { name: 'secretary_create_assignment', surface: 'ivy' } }, namespace: 'secretary', name: 'createAssignment', interfaceVersion: version,
      description: 'Create an assignment that runs agent work on a schedule or a service event.', inputSchema: assignmentInput(true), outputSchema: bundled('Pin'), annotations: { title: 'Create assignment', readOnlyHint: false, destructiveHint: false, idempotentHint: true, openWorldHint: false } },
    { discovery: { keywords: [], mcp: { name: 'secretary_update_assignment', surface: 'ivy' } }, namespace: 'secretary', name: 'updateAssignment', interfaceVersion: version,
      description: "Change an assignment's prompt, schedule, or event trigger using its current revision.", inputSchema: assignmentInput(false), outputSchema: bundled('Pin'), annotations: { title: 'Update assignment', readOnlyHint: false, idempotentHint: true, openWorldHint: false } },
  );
  const followUp: Wire.ToolDefinition = { namespace: 'secretary', name: 'followUp', interfaceVersion: version, discovery: { keywords: ['follow-up'], mcp: { name: 'secretary_follow_up', surface: 'ivy' } }, description: 'Accept a Main or Voice question about a Secretary item or execution object ID. Returns promptly; poll secretary_follow_up_read with the original operationId for the answer.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['operationId', 'itemId', 'question'], properties: { operationId: { type: 'string', minLength: 1, maxLength: 256 }, itemId: { type: 'string', minLength: 1, maxLength: 256 }, question: { type: 'string', minLength: 1, maxLength: 16384 } } },
    outputSchema: { type: 'object', additionalProperties: false, required: ['operationId', 'itemId', 'phase', 'threadId', 'turnId', 'answer', 'errorCode'], properties: { operationId: { type: 'string' }, itemId: { type: 'string' }, phase: { enum: ['prepared', 'running', 'succeeded', 'outcome_unknown'] }, threadId: { type: ['string', 'null'] }, turnId: { type: ['string', 'null'] }, answer: { type: ['string', 'null'] }, errorCode: { type: ['string', 'null'] } } },
    annotations: { readOnlyHint: false, idempotentHint: true } };
  tools.push(followUp);
  tools.push({ ...followUp, name: 'followUpRead', description: 'Read the caller-owned result of an accepted Secretary follow-up using its original operationId.',
    discovery: { keywords: [], mcp: { name: 'secretary_follow_up_read', surface: 'ivy' } },
    inputSchema: { type: 'object', additionalProperties: false, required: ['operationId'], properties: { operationId: { type: 'string', minLength: 1, maxLength: 256 } } },
    annotations: { readOnlyHint: true, idempotentHint: true } });
  for (const tool of tools) if (tool.discovery?.mcp) tool.discovery.summary = tool.description;
  return { discoveryHint: 'Secretary manages scheduled and event-triggered assignments, the inbox and follow-up questions.', namespaces: [{ namespace: 'secretary', description: 'System assignment execution, durable inbox and agent triage',
    guideMarkdown: 'Start with secretary_binding for the selected serviceNodeId; copy its expectedScope into secretary_list_assignments to page current assignments or select an exact assignmentId. Create with secretary_create_assignment and update from the returned revision pin with secretary_update_assignment. The routed and inner operationId must match. After an uncertain reply, use secretary_operation_read with the original operationId; unknown or not_found does not prove that no effect occurred. Assignments start empty and run from schedules or Ivy service events. Use hive_event_topics to discover registered triggers, event kinds and payload schemas. Events advertising messageChannel are grouped by source and channel for five minutes; Secretary asks the source message-channel.unread tool for fresh read state before assessment. On ivy_dev, secretary_status reports source collection progress, secretary_history retrieves a full inbox action, and secretary_configure_execution sets assignment defaults. User follow-up continues through ChatBridge and the shared Main conversation.', topics: [browserNoticeTopic("secretary")], inventoryKinds: [], tools }], contracts,
    requiredContracts: catalog.contracts.map(value => ({ key: value.key, readVersions: readableVersions(value.key), writeVersions: [contractVersion(value.key)] })) } as unknown as Wire.RegistrySync;
}
