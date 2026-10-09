import { bundledSchema } from '../../../../packages/sdk/src/node.js';
import { validateTaskBoard } from '../../../../packages/sdk/src/node.js';
import type { TaskBoard, Wire } from '../../../../packages/sdk/src/node.js';
import catalog from '../../../../specs/schemas/task-board.operations.json' with { type: 'json' };
import evidenceChunk from '../../../../specs/schemas/task-board.evidence-chunk.json' with { type: 'json' };
import readEvidenceChunk from '../../../../specs/schemas/task-board.read-evidence-chunk.json' with { type: 'json' };
import attachment from '../../../../specs/schemas/task-board.attachment.json' with { type: 'json' };
import { upgradeConfigurationRecord, upgradeTaskRecord } from '../../../../packages/sdk/src/client.js';

export const version = '1.0.0' as const;
export const contractVersion = (key: ContractKey): '1.0.0' | '1.1.0' | '1.2.0' | '1.7.0' => key === 'task-board/task' ? '1.7.0' : key === 'task-board/configuration' ? '1.2.0' : key === 'task-board/run' || key === 'task-board/delivery' ? '1.1.0' : version;
export const readableVersions = (key: ContractKey): string[] => key === 'task-board/task' ? ['1.0.0', '1.1.0', '1.2.0', '1.3.0', '1.4.0', '1.5.0', '1.6.0', '1.7.0']
  : key === 'task-board/configuration' ? ['1.0.0', '1.1.0', '1.2.0'] : key === 'task-board/run' || key === 'task-board/delivery' ? ['1.0.0', '1.1.0'] : [version];
// Before task 1.5.0 and configuration 1.1.0, an execution requirement could name one capability instead of a host.
const capabilityRequirement = { type: 'object', properties: { kind: { const: 'capability' }, capabilityKey: { $ref: '#/$defs/WireIdentifier' } }, required: ['kind', 'capabilityKey'], additionalProperties: false };
function withCapabilityRequirement(contract: Wire.DataContract, from: string, to: string, description?: string): Wire.DataContract {
  const result = structuredClone(contract);
  result.version = to;
  result.specMarkdown = result.specMarkdown.replace(contract.key + ' ' + from, contract.key + ' ' + to);
  const definitions = (result.jsonSchema as { $defs: Record<string, { description?: string; oneOf: unknown[] }> }).$defs;
  const requirement = definitions.TaskBoardExecutionRequirement!;
  requirement.oneOf.splice(1, 0, capabilityRequirement);
  if (description) definitions.TaskBoardExecutionRequirement = { description, oneOf: requirement.oneOf };
  return result;
}
export const contractNames = { 'task-board/configuration': 'Configuration', "task-board/native-signals": "NativeSignals", 'task-board/task': 'Task', 'task-board/run': 'Run', 'task-board/result': 'Result', 'task-board/review': 'Review',
  'task-board/delivery': 'Delivery',
  'task-board/task-key-sequence': 'TaskKeySequence', 'task-board/task-key-allocation': 'TaskKeyAllocation', 'task-board/comment-read-state': 'CommentReadState',
  'task-board/history': 'History', 'task-board/native-plan': 'NativePlan', 'task-board/native-call': 'NativeCall', 'task-board/operation': 'Operation', 'task-board/coordination': 'Coordination', 'task-board/native-evidence': 'NativeEvidence', 'task-board/native-read-evidence': 'NativeReadEvidence',
  'task-board/scan-cursor': 'ScanCursor', 'task-board/scheduler-attempt': 'SchedulerAttempt', 'task-board/native-turn-search': 'NativeTurnSearch', 'task-board/native-turn-snapshot': 'NativeTurnSnapshot', 'task-board/native-result-page': 'NativeResultPage', 'task-board/native-transcript': 'NativeTranscript', 'task-board/native-full-turn-transcript': 'NativeFullTurnTranscript' } as const;
export interface ContractValues {
  'task-board/configuration': TaskBoard.Configuration;
  'task-board/delivery': TaskBoard.Delivery;
  'task-board/task-key-sequence': TaskBoard.TaskKeySequence; 'task-board/task-key-allocation': TaskBoard.TaskKeyAllocation; 'task-board/comment-read-state': TaskBoard.CommentReadState;
  'task-board/task': TaskBoard.Task; 'task-board/run': TaskBoard.Run; 'task-board/result': TaskBoard.Result;
  'task-board/review': TaskBoard.Review; 'task-board/history': TaskBoard.History;
  'task-board/native-plan': TaskBoard.NativePlan; 'task-board/native-call': TaskBoard.NativeCall; 'task-board/operation': TaskBoard.Operation; 'task-board/coordination': TaskBoard.Coordination; 'task-board/native-evidence': TaskBoard.NativeEvidence;
  'task-board/native-read-evidence': TaskBoard.NativeReadEvidence;
  'task-board/native-signals': TaskBoard.NativeSignals;
  'task-board/scan-cursor': TaskBoard.ScanCursor; 'task-board/scheduler-attempt': TaskBoard.SchedulerAttempt;
  'task-board/native-turn-search': TaskBoard.NativeTurnSearch;
  'task-board/native-turn-snapshot': TaskBoard.NativeTurnSnapshot; 'task-board/native-result-page': TaskBoard.NativeResultPage; 'task-board/native-transcript': TaskBoard.NativeTranscript;
  'task-board/native-full-turn-transcript': TaskBoard.NativeFullTurnTranscript;
}
export type ContractKey = keyof ContractValues;
export const localContractKeys = new Set<ContractKey>(['task-board/native-signals', 'task-board/native-plan', 'task-board/native-call', 'task-board/operation',
  'task-board/coordination', 'task-board/native-evidence', 'task-board/native-read-evidence', 'task-board/scan-cursor', 'task-board/scheduler-attempt',
  'task-board/native-turn-search', 'task-board/native-turn-snapshot', 'task-board/native-result-page', 'task-board/native-transcript', 'task-board/native-full-turn-transcript']);
/** Reads records from before the capability requirement was replaced by required capabilities. */
export function upgradeRecord<K extends ContractKey>(key: K, value: ContractValues[K]): ContractValues[K] {
  return (key === 'task-board/task' ? upgradeTaskRecord(value as TaskBoard.Task)
    : key === 'task-board/configuration' ? upgradeConfigurationRecord(value as TaskBoard.Configuration) : value) as ContractValues[K];
}
export function validateDomain<K extends ContractKey>(key: K, value: unknown): asserts value is ContractValues[K] { validateTaskBoard(contractNames[key], value); }
export const taskBoardSchema = (name: string) => bundledSchema('#/$defs/' + name, catalog.schema);
function publishedTaskSchema(schema: Record<string, unknown>): Record<string, unknown> {
  // Tool-facing descriptions may evolve, but the published Task contracts are immutable.
  const definitions = schema.$defs as Record<string, { description?: string; properties?: Record<string, { description?: string }> }>;
  delete definitions.TaskBoardExecutionRequirement?.description;
  delete definitions.TaskBoardWorkspaceRequirement?.description;
  const fields = definitions.TaskBoardTaskFields?.properties;
  for (const name of ['control', 'priority', 'executionRequirement', 'workspaceRequirement', 'requiredCapabilities'])
    delete fields?.[name]?.description;
  return schema;
}
export function taskBoardContracts(): Wire.DataContract[] {
  const retention = (key: string): Wire.RetentionPolicy => key === 'task-board/task' ? { objects: { mode: 'retain' }, revisions: { mode: 'bounded', maximumCount: 1000 } } :
    key === 'task-board/result' ? { objects: { mode: 'retain' }, revisions: { mode: 'current' } } : { objects: { mode: 'retain' }, revisions: { mode: 'current' } };
  const finalKeys = new Set(['task-board/configuration', 'task-board/task', 'task-board/result', 'task-board/run', 'task-board/review', 'task-board/history', 'task-board/delivery', 'task-board/task-key-sequence', 'task-board/task-key-allocation', 'task-board/comment-read-state']);
  const contracts: Wire.DataContract[] = catalog.contracts.filter(value => finalKeys.has(value.key)).map(value => ({ key: value.key, version: value.version, owner: { kind: 'service', serviceName: 'task-board' }, mediaType: 'application/json', retention: retention(value.key), jsonSchema: value.key === 'task-board/task' ? publishedTaskSchema(bundledSchema(value.definition, catalog.schema)) : bundledSchema(value.definition, catalog.schema),
    specMarkdown: `# ${value.key} ${value.version}\n\nTaskBoard owns this exact workflow contract. Primary domain Objects have the configured workflow root as their direct parent. Immutable prepared payloads use the same exact contract below their owning Operation; they are evidence, not primary Tasks or Runs. Apply the state, actor, reference and recovery rules from the TaskBoard specification and decision 0011. Every write uses CAS or a retained create mutation ID. Native side effects require the saved claim, exact native plan and owner outcome evidence; this data schema alone does not authorize execution.` }));
  // Retained revisions in this installation still require their immutable published schemas.
  const deliveryPrevious = structuredClone(contracts.find(value => value.key === 'task-board/delivery')!);
  deliveryPrevious.version = '1.0.0';
  deliveryPrevious.specMarkdown = deliveryPrevious.specMarkdown.replace('task-board/delivery 1.1.0', 'task-board/delivery 1.0.0');
  // Main admission renamed this definition and widened Channel.adapter in 1.1.0.
  deliveryPrevious.jsonSchema = JSON.parse(JSON.stringify(deliveryPrevious.jsonSchema).replaceAll('ChatAdmittedNotifyRequest', 'ChatNotifyRequest'));
  (deliveryPrevious.jsonSchema as { $defs: { ChatChannel: { properties: { adapter: unknown } } } }).$defs.ChatChannel.properties.adapter = { enum: ['webchat', 'whatsapp'] };
  contracts.push(deliveryPrevious);
  for (const [key, from, to] of [['task-board/task', '1.7.0', '1.6.0'], ['task-board/run', '1.1.0', '1.0.0']] as const) {
    const previous = structuredClone(contracts.find(value => value.key === key)!);
    previous.version = to; previous.specMarkdown = previous.specMarkdown.replace(key + ' ' + from, key + ' ' + to);
    const schema = previous.jsonSchema as { properties: Record<string, any>; $defs: Record<string, any> };
    if (key === 'task-board/task') delete schema.$defs.TaskBoardTaskFields?.properties.allowParallel;
    else {
      delete schema.$defs.TaskBoardContinueRequest?.properties.coordinationOnly;
      delete schema.properties.calls.properties.steer;
    }
    contracts.push(previous);
  }
  const configPrevious = structuredClone(contracts.find(value => value.key === 'task-board/configuration')!);
  configPrevious.version = '1.1.0';
  configPrevious.specMarkdown = configPrevious.specMarkdown.replace('task-board/configuration 1.2.0', 'task-board/configuration 1.1.0');
  const configDefaults = (configPrevious.jsonSchema as { $defs: Record<string, { properties: Record<string, unknown> }> }).$defs.TaskBoardExecutionDefaults!.properties;
  delete configDefaults.userContact; delete configDefaults.useWorktree; delete configDefaults.allowParallel;
  contracts.push(configPrevious);
  for (const [key, from, to] of [['task-board/task', '1.6.0', '1.5.0']] as const) {
    const previous = structuredClone(contracts.find(value => value.key === key && value.version === from)!);
    previous.version = to; previous.specMarkdown = previous.specMarkdown.replace(key + ' ' + from, key + ' ' + to);
    const schema = previous.jsonSchema as { properties: Record<string, any>; $defs: Record<string, any> };
    delete schema.$defs.TaskBoardContinueRequest?.properties.coordinationOnly;
    if (key === 'task-board/task') {
      delete schema.properties.blocker; delete schema.$defs.TaskBoardBlocker;
      delete schema.$defs.TaskBoardComment.properties.sourceTaskId;
    }
    contracts.push(previous);
  }
  // Configuration 1.1.0 dropped the capability requirement that 1.0.0 still admits.
  contracts.push(withCapabilityRequirement(configPrevious, '1.1.0', '1.0.0',
    'Select one exact AgentManager host or any ready host with the named capability. Use null in task fields for the next available execution PC.'));
  // Task 1.5.0 replaced the capability requirement by required capabilities; 1.4.0 is the Task before that.
  const capabilityTask = withCapabilityRequirement(contracts.find(value => value.key === 'task-board/task' && value.version === '1.5.0')!, '1.5.0', '1.4.0');
  delete ((capabilityTask.jsonSchema as { $defs: Record<string, { properties: Record<string, unknown> }> }).$defs.TaskBoardTaskFields!.properties).requiredCapabilities;
  contracts.push(capabilityTask);
  const currentTask = structuredClone(capabilityTask);
  currentTask.version = '1.3.0';
  currentTask.specMarkdown = currentTask.specMarkdown.replace('task-board/task 1.4.0', 'task-board/task 1.3.0');
  // Already published contracts are immutable, including those used by retained ticket revisions.
  const prior = currentTask.jsonSchema as { allOf: unknown[]; $defs: Record<string, Record<string, unknown>> };
  const commentProperties = prior.$defs.TaskBoardComment!.properties as Record<string, unknown>;
  delete commentProperties.purpose;
  delete commentProperties.nativeInput;
  const pin = { $ref: '#/$defs/TaskBoardObjectPin' };
  prior.allOf.push(
    { if: { properties: { workflowState: { const: 'review' } } }, then: { properties: { latestResult: pin } } },
    { if: { properties: { workflowState: { const: 'done' }, fields: { properties: { control: { const: 'agent' } } } } }, then: { properties: { latestResult: pin, acceptedReview: pin } } },
  );
  const used = new Set<string>();
  const visit = (value: unknown): void => {
    if (!value || typeof value !== 'object') return;
    if (Array.isArray(value)) { value.forEach(visit); return; }
    for (const [key, child] of Object.entries(value)) {
      if (key === '$defs') continue;
      if (key === '$ref' && typeof child === 'string' && child.startsWith('#/$defs/')) {
        const name = child.slice('#/$defs/'.length);
        if (!used.has(name)) { used.add(name); visit(prior.$defs[name]); }
      } else visit(child);
    }
  };
  visit(prior);
  for (const name of Object.keys(prior.$defs)) if (!used.has(name)) delete prior.$defs[name];
  contracts.push(currentTask);
  const previousTask = structuredClone(currentTask);
  previousTask.version = '1.2.0';
  previousTask.specMarkdown = previousTask.specMarkdown.replace('task-board/task 1.3.0', 'task-board/task 1.2.0');
  const previousDefinitions = (previousTask.jsonSchema as { $defs: {
    TaskBoardExecutionRequirement: { oneOf: { properties: { kind: { const: string } } }[] };
    TaskBoardNativeOptions: { properties: { serviceTier: { anyOf: { enum?: string[] }[] } } };
  } }).$defs;
  previousDefinitions.TaskBoardExecutionRequirement.oneOf = previousDefinitions.TaskBoardExecutionRequirement.oneOf.filter(value => value.properties.kind.const !== 'automatic');
  previousDefinitions.TaskBoardNativeOptions.properties.serviceTier.anyOf[0]!.enum = ['fast', 'flex'];
  const legacyTask = structuredClone(previousTask);
  legacyTask.version = '1.1.0';
  legacyTask.specMarkdown = legacyTask.specMarkdown.replace('task-board/task 1.2.0', 'task-board/task 1.1.0');
  type TaskSchema = {
    properties: { fields: { $ref: string } };
    $defs: Record<string, {
      properties?: Record<string, unknown>;
      oneOf?: Array<{ properties?: { kind?: { const?: string } } }>;
    }>;
  };
  const legacySchema = legacyTask.jsonSchema as TaskSchema;
  const requirement = legacySchema.$defs.TaskBoardWorkspaceRequirement!;
  requirement.oneOf = requirement.oneOf!.filter(value => value.properties?.kind?.const !== 'directory_path');
  const originalTask = structuredClone(legacyTask);
  originalTask.version = '1.0.0';
  originalTask.specMarkdown = originalTask.specMarkdown.replace('task-board/task 1.1.0', 'task-board/task 1.0.0');
  const originalSchema = originalTask.jsonSchema as typeof legacySchema;
  const fieldsName = originalSchema.properties.fields.$ref.slice('#/$defs/'.length);
  delete originalSchema.$defs[fieldsName]!.properties!.nativeOptions;
  delete originalSchema.$defs.TaskBoardNativeOptions;
  contracts.push(previousTask, legacyTask, originalTask);
  contracts.push(structuredClone(attachment) as Wire.DataContract);
  return contracts;
}
