import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
const ref = name => ({ $ref: '#/$defs/' + name });
const object = properties => ({ type: 'object', properties, required: Object.keys(properties), additionalProperties: false });
const optionalObject = (properties, required = []) => ({ type: 'object', properties, required, additionalProperties: false });
const text = (maxLength = 256, minLength = 1) => ({ type: 'string', minLength, maxLength });
const integer = (minimum = 0, maximum = Number.MAX_SAFE_INTEGER) => ({ type: 'integer', minimum, maximum });
const nullable = value => ({ anyOf: [value, { type: 'null' }] });
const array = (items, maxItems, minItems = 0) => ({ type: 'array', items, minItems, maxItems });
const enumeration = (...values) => ({ enum: values });
const hash = { type: 'string', pattern: '^sha256:[0-9a-f]{64}$' };
const actionId = { ...text(), description: 'Use <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status, use current Unix milliseconds and a unique 1-128 character nonce without a colon. New writes require the current Hive epoch and expire after 24 hours; retain the ID for secretary_operation_read after an uncertain outcome.' };
const expectedScope = { ...ref('Scope'), description: 'Copy this value unchanged from secretary_binding for the selected serviceNodeId.' };
const stamp = { type: 'string', pattern: '^\\d{4}-\\d\\d-\\d\\dT\\d\\d:\\d\\d:\\d\\d\\.\\d{3}Z$' };
const time = { type: 'string', pattern: '^([01][0-9]|2[0-3]):[0-5][0-9]$' };
const defs = {
  Pin: object({ objectId: text(), revision: integer(1) }),
  Scope: object({ secretaryId: text(64), rootObjectId: text() }),
  SourceDefinition: object({ sourceId: text(64), accountId: text(), kind: enumeration('email', 'teams', 'whatsapp'),
    producerPrincipalIds: array(text(), 16, 1), allowedSenderIds: nullable(array(text(), 256)), ownSenderIds: array(text(), 32) }),
  Identity: object({ scope: ref('Scope'), principalId: text(), serviceNodeId: text(), hostId: text() }),
  Policy: object({ silentTime: nullable(object({ timeZone: text(128), start: time, end: time })), urgentBypass: { type: 'boolean' }, whatsappQuestionsOnly: { type: 'boolean' },
    minimumMainUrgency: enumeration('normal', 'high', 'critical'), researchMaxMinutes: integer(0, 5),
    voiceEscalation: nullable(object({ serviceNodeId: text(), recipientId: text(), requestDefinitionHash: hash })) }),
  ExpectedBridge: object({ principalId: text(), callerPrincipalId: text(), workspaceId: text(), definitionHash: hash }),
  Channel: object({ adapter: text(), accountId: text(), channelId: text() }),
  NoticeTarget: optionalObject({ serviceNodeId: text(), expectedBridge: ref('ExpectedBridge'), channel: ref('Channel'), notifyDefinitionHash: hash, noticeDefinitionHash: hash }, ['serviceNodeId', 'expectedBridge', 'channel']),
  NotifyRequest: object({ action: { const: 'notify' }, operationId: text(), expectedBridge: ref('ExpectedBridge'), channel: ref('Channel'), source: ref('Pin'), text: text(16384) }),
  Settings: optionalObject({ identity: ref('Identity'), sources: array(ref('SourceDefinition'), 32),
    policy: ref('Policy'), pollMs: integer(100, 60000), recordsPerTick: integer(1, 100), followUpPrincipalIds: array(text(), 32) },
    ['identity', 'sources', 'policy', 'pollMs', 'recordsPerTick']),
  ContactWindow: object({ timeZone: text(128), start: time, end: time }),
  ContactRule: object({ enabled: { type: 'boolean' }, minimumUrgency: enumeration('normal', 'high', 'critical'), window: nullable(ref('ContactWindow')) }),
  WhatsAppRule: object({ enabled: { type: 'boolean' }, minimumUrgency: enumeration('normal', 'high', 'critical'), window: nullable(ref('ContactWindow')), questionsOnly: { type: 'boolean' } }),
  VoiceRule: object({ enabled: { type: 'boolean' }, minimumUrgency: { const: 'critical' }, window: nullable(ref('ContactWindow')), immediateOnly: { type: 'boolean' } }),
  GlobalRules: object({ main: ref('ContactRule'), whatsapp: ref('WhatsAppRule'), voice: ref('VoiceRule'), researchMaxMinutes: integer(0, 5) }),
  ContactRuleOverride: optionalObject({ enabled: { type: 'boolean' }, minimumUrgency: enumeration('normal', 'high', 'critical'), window: nullable(ref('ContactWindow')) }),
  WhatsAppRuleOverride: optionalObject({ enabled: { type: 'boolean' }, minimumUrgency: enumeration('normal', 'high', 'critical'), window: nullable(ref('ContactWindow')), questionsOnly: { type: 'boolean' } }),
  VoiceRuleOverride: optionalObject({ enabled: { type: 'boolean' }, minimumUrgency: { const: 'critical' }, window: nullable(ref('ContactWindow')), immediateOnly: { type: 'boolean' } }),
  RuleOverrides: optionalObject({ main: ref('ContactRuleOverride'), whatsapp: ref('WhatsAppRuleOverride'), voice: ref('VoiceRuleOverride'), researchMaxMinutes: integer(0, 5) }),
  ExecutionTarget: object({ serviceNodeId: text(), threadCwd: text(4096), model: nullable(text(128)), effort: enumeration('low', 'medium', 'high', 'xhigh'), permissions: text(128) }),
  NativeTarget: object({ serviceNodeId: text(), hostId: text(), nativeVersion: text(), nativeExecutableHash: hash, catalogHash: hash }),
  SecretaryConfiguration: object({ schemaVersion: { const: 1 }, rules: ref('GlobalRules'), execution: nullable(ref('ExecutionTarget')), updatedAt: stamp }),
  ScheduleTrigger: object({ kind: { const: 'schedule' }, timeZone: text(128), cadence: enumeration('interval', 'daily', 'weekly'), intervalMinutes: nullable(integer(1, 10080)), localTime: nullable(time), weekdays: array(integer(0, 6), 7) }),
  EventTrigger: object({ kind: { const: 'event' }, topic: text(128), topicVersion: text(32), sourceServiceNodeId: nullable(text()), eventKind: nullable(text(256)) }),
  AssignmentTrigger: { oneOf: [ref('ScheduleTrigger'), ref('EventTrigger')] },
  Preflight: object({ executable: text(4096), args: array(text(4096, 0), 64), timeoutMs: integer(100, 120000) }),
  AssignmentExecution: object({ model: nullable(text(128)), effort: nullable(enumeration('low', 'medium', 'high', 'xhigh')), reuse: enumeration('main', 'assignment', 'new') }),
  Assignment: optionalObject({ schemaVersion: { const: 1 }, assignmentId: text(), name: text(256), description: text(2048, 0), enabled: { type: 'boolean' }, builtInKey: { ...nullable(text(128)), description: 'Retained provenance metadata with no execution behavior. New assignments omit it.' }, minimumNotificationAgeMinutes: integer(0, 1440), trigger: ref('AssignmentTrigger'), prompt: text(131072), preflight: nullable(ref('Preflight')), rules: ref('RuleOverrides'), execution: ref('AssignmentExecution'), createdAt: stamp, updatedAt: stamp },
    ['schemaVersion', 'assignmentId', 'name', 'description', 'enabled', 'trigger', 'prompt', 'preflight', 'rules', 'createdAt', 'updatedAt']),
  ExecutionTrigger: object({ kind: enumeration('schedule', 'event'), key: text(512), occurredAt: stamp, payload: {} }),
  ScheduleProgress: object({ schemaVersion: { const: 1 }, assignmentId: text(), assignment: ref('Pin'), assignmentSnapshot: ref('Assignment'), checkedAt: stamp, updatedAt: stamp }),
  EventProgress: object({ schemaVersion: { const: 1 }, assignmentId: text(), assignment: ref('Pin'), assignmentSnapshot: ref('Assignment'),
    processedThrough: integer(0), active: { type: 'boolean' }, needsRebase: { type: 'boolean' }, updatedAt: stamp }),
  NativeOperations: object({ threadStart: nullable(text()), turnStart: nullable(text()), archive: nullable(text()), delete: nullable(text()) }),
  DeliveryRequest: object({ action: { const: 'notify' }, operationId: text(), expectedBridge: ref('ExpectedBridge'), channel: ref('Channel'), source: ref('Pin'), text: text(16384) }),
  Handoff: optionalObject({ executionId: text(), resultHash: hash, operationId: nullable(text()), request: nullable(ref('DeliveryRequest')), state: enumeration('queued', 'dispatching', 'confirmed', 'suppressed', 'failed', 'outcome_unknown'), reason: nullable(text(1024, 0)), evidence: nullable(ref('Pin')), expiresAt: nullable(integer()), attempts: integer(), retryAt: integer(), terminal: { type: 'boolean' },
    target: nullable(object({ serviceNodeId: text(), channel: ref('Channel') })) }, ['executionId', 'resultHash', 'operationId', 'request', 'state', 'reason', 'evidence', 'expiresAt']),
  VoiceTarget: object({ serviceNodeId: text(), recipientId: text(), requestDefinitionHash: hash }),
  VoiceRequest: object({ operationId: text(), recipientId: text(), route: { const: 'voice' }, voicePrompt: text(4096) }),
  VoiceRecord: object({ executionId: text(), resultHash: hash, source: ref('Pin'), target: ref('VoiceTarget'), request: ref('VoiceRequest'), callId: nullable(text()), state: enumeration('dispatching', 'confirmed', 'failed', 'fallback', 'outcome_unknown'), reason: nullable(text()), expiresAt: nullable(integer()) }),
  Execution: optionalObject({ schemaVersion: { const: 1 }, executionId: text(), assignment: ref('Pin'), assignmentSnapshot: ref('Assignment'), trigger: ref('ExecutionTrigger'), effectiveRules: ref('GlobalRules'), executionTarget: ref('ExecutionTarget'),
    delivery: ref('Handoff'), voice: ref('VoiceRecord'),
    phase: enumeration('queued', 'preflight', 'starting', 'running', 'completed', 'settled', 'archiving', 'archived', 'deleting', 'deleted', 'failed'), serviceNodeId: text(), nativeTarget: nullable(ref('NativeTarget')), threadId: nullable(text()), turnId: nullable(text()),
    nativeOperations: ref('NativeOperations'), preflightOutput: nullable(text(65536, 0)), result: nullable(text(131072, 0)), errorCode: nullable(text(128)), createdAt: stamp, startedAt: nullable(stamp), completedAt: nullable(stamp), archivedAt: nullable(stamp), deleteAfter: nullable(stamp), deletedAt: nullable(stamp), updatedAt: stamp }, ['schemaVersion', 'executionId', 'assignment', 'assignmentSnapshot', 'trigger', 'effectiveRules', 'executionTarget', 'phase', 'serviceNodeId', 'nativeTarget', 'threadId', 'turnId', 'nativeOperations', 'preflightOutput', 'result', 'errorCode', 'createdAt', 'startedAt', 'completedAt', 'archivedAt', 'deleteAfter', 'deletedAt', 'updatedAt']),
  Message: object({ accountId: text(), conversationId: text(), messageId: text(), nativeRevision: text(), senderId: text(), outgoing: { type: 'boolean' }, attachments: enumeration('none', 'expected'),
    occurredAt: stamp, observedAt: stamp, title: text(1024, 0), text: text(32768, 0), url: nullable(text(4096)) }),
  Assessment: object({ urgency: enumeration('low', 'normal', 'high', 'critical'), disposition: enumeration('ignore', 'record', 'notify', 'task'),
    notification: enumeration('none', 'main', 'voice', 'needs_attention'), summary: text(8192), question: { type: 'boolean' }, usefulIntel: { type: 'boolean' }, task: nullable(ref('Pin')) }),
  ActionIdentity: object({ principalId: text(), operationId: text() }),
  Decision: object({ assessment: ref('Assessment'), actor: text(), operationId: text(), decidedAt: stamp, policy: ref('Policy') }),
  Notice: object({ state: enumeration('suppressed', 'queued', 'dispatching', 'outcome_unknown', 'confirmed'), reason: nullable(text()),
    text: text(8192), source: ref('Pin'), target: nullable(ref('NoticeTarget')), request: nullable(ref('NotifyRequest')), preparedRevision: nullable(integer(3)), evidence: nullable(ref('Pin')),
    voice: nullable(object({ state: enumeration('queued', 'confirmed', 'outcome_unknown'), operationId: text(), callId: { type: ['string', 'null'], maxLength: 256 }, errorCode: { type: ['string', 'null'], maxLength: 128 } })), updatedAt: stamp }),
  ItemMedia: object({ contractKey: text(192), pin: nullable(ref('Pin')), contentHash: nullable(hash), name: nullable(text(1024)), mediaType: nullable(text(256)), errorCode: nullable(text()) }),
  Item: object({ schemaVersion: { const: 1 }, identityHash: hash, sourceId: text(64), messageHash: hash, message: ref('Message'), media: array(ref('ItemMedia'), 64), decision: nullable(ref('Decision')), notice: nullable(ref('Notice')) }),
  Source: object({ schemaVersion: { const: 1 }, definition: ref('SourceDefinition'), definitionHash: hash, cursor: nullable(text(4096)), checkpointOperation: nullable(ref('ActionIdentity')), updatedAt: nullable(stamp) }),
  CaptureRequest: object({ action: { const: 'capture' }, operationId: text(), expectedScope: ref('Scope'), sourceId: text(64), message: ref('Message') }),
  AssessRequest: object({ action: { const: 'assess' }, operationId: text(), expectedScope: ref('Scope'), item: ref('Pin'), assessment: ref('Assessment') }),
  CheckpointRequest: object({ action: { const: 'checkpoint' }, operationId: text(), expectedScope: ref('Scope'), sourceId: text(64), previousCursor: nullable(text(4096)), nextCursor: text(4096), captures: array(text(), 100) }),
  AttachRequest: object({ action: { const: 'attach' }, operationId: text(), expectedScope: ref('Scope'), sourceId: text(64), item: ref('Pin'), media: array(ref('ItemMedia'), 64, 1) }),
  Request: { oneOf: [ref('CaptureRequest'), ref('AssessRequest'), ref('CheckpointRequest'), ref('AttachRequest')] },
  ReadRequest: object({ expectedScope, operationId: actionId }),
  OperationOutcome: object({ operationId: text(), action: enumeration('capture', 'assess', 'checkpoint', 'attach', 'create_assignment', 'update_assignment', 'configure_execution'),
    phase: enumeration('accepted', 'succeeded', 'failed', 'unknown'), effect: nullable(ref('Pin')), errorCode: nullable(text()), observedAt: stamp }),
  StatusRequest: object({ expectedScope }),
  BindingRequest: object({}),
  BindingResponse: object({ serviceNodeId: text(), hostId: text(), available: { const: true }, expectedScope: ref('Scope'), generation: integer(1),
    bindingHash: { ...hash, description: 'Changes when the service node, host, scope or connection generation changes; reread the binding before acting on a new owner.' }, observedAt: stamp }),
  ListAssignmentsRequest: optionalObject({ expectedScope, assignmentId: text(), cursor: nullable(text(4096)), limit: integer(1, 100), enabled: { type: 'boolean' }, triggerKind: enumeration('schedule', 'event') }, ['expectedScope']),
  AssignmentEntry: object({ pin: ref('Pin'), assignment: ref('Assignment') }),
  AssignmentPage: object({ assignments: array(ref('AssignmentEntry'), 100), nextCursor: nullable(text(4096)), observedAt: stamp }),
  GetAssignmentRequest: object({ expectedScope, assignmentId: text() }),
  AssignmentEntryResponse: object({ pin: ref('Pin'), assignment: ref('Assignment'), observedAt: stamp }),
  SaveConfigurationRequest: object({ operationId: actionId, configuration: ref('Pin'), value: ref('SecretaryConfiguration') }),
  SaveAssignmentRequest: object({ operationId: actionId, assignment: nullable(ref('Pin')), value: ref('Assignment') }),
  Destination: { oneOf: [object({ createName: text() }), object({ object: ref('Pin') })] },
  Prepared: { oneOf: [object({ key: { const: 'secretary/item' }, destination: ref('Destination'), value: ref('Item') }),
    object({ key: { const: 'secretary/source' }, destination: ref('Destination'), value: ref('Source') })] },
  Accepted: object({ schemaVersion: { const: 1 }, action: enumeration('capture', 'assess', 'checkpoint', 'attach'), operationId: text(), callerPrincipalId: text(), requestHash: hash, request: nullable(ref('Request')),
    sourceId: text(64), source: ref('Source'), createdAt: stamp, prepared: nullable(ref('Prepared')), existingEffect: nullable(ref('Pin')), ignoredReason: nullable(enumeration('own_sender', 'sender_not_allowed', 'outgoing')),
    captureIdentity: nullable(hash), checkpointCaptures: array(ref('Pin'), 100), phase: enumeration('accepted', 'succeeded', 'failed'), effect: nullable(ref('Pin')), errorCode: nullable(text()) }),
  Status: object({ identity: ref('Identity'), sources: array(object({ object: ref('Pin'), data: ref('Source') }), 32), policy: ref('Policy'), noticeConfigured: { type: 'boolean' }, recoveryIssues: array(object({ objectId: text(), code: text() }), 32), observedAt: stamp }),
};
const schema = { $schema: 'https://json-schema.org/draft/2020-12/schema', $id: 'https://ivy.invalid/schemas/secretary.schema.json', $defs: defs };
const catalog = { version: '1.0.0', contracts: [
  { key: 'secretary/item', definition: 'Item' },
  { key: 'secretary/configuration', definition: 'SecretaryConfiguration' },
  { key: 'secretary/assignment', definition: 'Assignment' },
  { key: 'secretary/schedule-progress', definition: 'ScheduleProgress' },
  { key: 'secretary/event-progress', definition: 'EventProgress' },
  { key: 'secretary/execution', definition: 'Execution' }
],
  operations: { capture: { input: 'CaptureRequest', output: 'Accepted', mutation: true }, assess: { input: 'AssessRequest', output: 'Accepted', mutation: true }, attach: { input: 'AttachRequest', output: 'Accepted', mutation: true },
    checkpoint: { input: 'CheckpointRequest', output: 'Accepted', mutation: true }, operation: { input: 'ReadRequest', output: 'Accepted', mutation: false }, operationRead: { input: 'ReadRequest', output: 'OperationOutcome', mutation: false }, status: { input: 'StatusRequest', output: 'Status', mutation: false }, binding: { input: 'BindingRequest', output: 'BindingResponse', mutation: false },
    listAssignments: { input: 'ListAssignmentsRequest', output: 'AssignmentPage', mutation: false },
    getAssignment: { input: 'GetAssignmentRequest', output: 'AssignmentEntryResponse', mutation: false },
    saveConfiguration: { input: 'SaveConfigurationRequest', output: 'Pin', mutation: true },
    saveAssignment: { input: 'SaveAssignmentRequest', output: 'Pin', mutation: true } } };
mkdirSync('specs/schemas', { recursive: true });
for (const [path, value] of [['specs/schemas/secretary.schema.json', schema], ['specs/schemas/secretary.contracts.json', catalog]]) {
  const bytes = JSON.stringify(value, null, 2) + '\n';
  if (process.argv.includes('--check')) { if (readFileSync(path, 'utf8') !== bytes) throw Error('Stale Secretary contract: ' + path); }
  else writeFileSync(path, bytes);
}
