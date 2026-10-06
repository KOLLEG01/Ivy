import { bundleAgentSchema as bundledSchema } from '../../../packages/sdk/src/node.js';
import { hashJson } from '../../../packages/sdk/src/node.js';
import { validateAgent } from '../../../packages/sdk/src/node.js';
import { nativeToolDefinition } from '../../../packages/sdk/src/node.js';
import type { Agent, Wire } from '../../../packages/sdk/src/node.js';
import { browserNoticeTopic } from '../../../packages/sdk/src/node.js';

export const agentSchema = (name: string) => bundledSchema('#/$defs/' + name, 'https://ivy.invalid/schemas/agent.schema.json');
export const nativeInventorySchemaVersion = '1.1.0';
export const nativeProjectSchemaVersion = '2.0.0';
const codexGroups: Record<string, string> = {
  codex: 'Native Codex operations using the selected host version.',
  'codex/tasks': 'Codex task lifecycle, content, settings and voice.',
  'codex/tasks/lifecycle': 'Create, find, resume, fork and archive Codex tasks.',
  'codex/tasks/content': 'Read task history, items and output.',
  'codex/tasks/settings': 'Task names, metadata, goals and configuration.',
  'codex/tasks/voice': 'Task realtime voice sessions and audio.',
  'codex/projects': 'Projects, workspaces and task organization.',
  'codex/turns': 'Start, steer and interrupt task turns and messages.',
  'codex/models-account': 'Models, account login and usage limits.',
  'codex/configuration': 'Settings, environments and permission profiles.',
  'codex/extensions': 'Plugins, MCP connections, marketplaces and skills.',
  'codex/files-commands': 'Native files, commands and processes.',
};
function codexGroup(name: string): string {
  const [head, action = ''] = name.split('/');
  if (head === 'thread') {
    if (action === 'realtime') return 'codex/tasks/voice';
    if (/^(read|items|turns|history|messages|loaded|output)/i.test(action)) return 'codex/tasks/content';
    if (/^(name|metadata|goal|settings|config|set)/i.test(action)) return 'codex/tasks/settings';
    return 'codex/tasks/lifecycle';
  }
  if (head === 'project' || head === 'threadSection') return 'codex/projects';
  if (head === 'turn') return 'codex/turns';
  if (/^(account|model|modelProvider|getAuthStatus)$/.test(head!)) return 'codex/models-account';
  if (/^(config|configRequirements|environment|permissionProfile|externalAgentConfig|windowsSandbox|experimentalFeature)$/.test(head!)) return 'codex/configuration';
  if (/^(ui|plugin|marketplace|skills|mcpServer|mcpServerStatus)$/.test(head!)) return 'codex/extensions';
  if (/^(fs|command|process|fuzzyFileSearch|gitDiffToRemote)$/.test(head!)) return 'codex/files-commands';
  return 'codex/' + head;
}
function codexSummary(name: string, group: string): string {
  const summaries: Record<string, string> = {
    'thread/start': 'Create a new Codex task.', 'thread/list': 'Find Codex tasks.', 'thread/read': 'Read a Codex task and its turns.',
    'thread/resume': 'Resume an existing Codex task.', 'thread/fork': 'Fork a Codex task.', 'thread/archive': 'Archive a Codex task.',
    'thread/unarchive': 'Restore an archived Codex task.', 'thread/realtime/start': 'Start a realtime voice session for a task.',
    'thread/realtime/stop': 'Stop a task voice session.', 'turn/start': 'Send input and start a task turn.',
    'turn/steer': 'Send steering input to a running turn.', 'turn/interrupt': 'Interrupt a running task turn.',
    'account/rateLimits/read': 'Read account usage and remaining rate limits.', 'model/list': 'List available models and reasoning levels.',
    'project/list': 'List Codex projects.', 'project/create': 'Create a Codex project.',
    'thread/name/set': 'Rename a Codex task.', 'thread/loaded/list': 'List tasks loaded in the native Codex process.',
    'mcpServerStatus/list': 'List MCP connections and their tools.', 'skills/list': 'List available Codex skills.',
    'plugin/list': 'List Codex plugins.', 'config/read': 'Read Codex configuration.',
  };
  return summaries[name] ?? name.replaceAll('/', ' / ').replace(/([a-z])([A-Z])/g, '$1 $2') + ': ' + (codexGroups[group] ?? 'Native Codex tools.');
}
function managementMcp(name: string): { name: string; surface: 'ivy' | 'ivy_dev' } | null {
  if (['catalog', 'frameLimits', 'environmentDefaults', 'resolveWorkspace', 'prevent', 'stageFile'].includes(name)) return null;
  if (['status', 'capabilities'].includes(name)) return { name: 'agent_manager_status', surface: 'ivy' };
  if (['read', 'operation'].includes(name)) return { name: 'agent_manager_read', surface: 'ivy' };
  if (['inputs', 'inputDefinition'].includes(name)) return { name: 'agent_manager_inputs', surface: 'ivy' };
  if (['configureCapabilities', 'interact', 'interaction', 'notifications'].includes(name))
    return { name: 'agent_manager_' + name.replace(/[A-Z]/g, letter => '_' + letter.toLowerCase()), surface: 'ivy_dev' };
  return { name: 'agent_manager_' + name.replace(/[A-Z]/g, letter => '_' + letter.toLowerCase()), surface: 'ivy' };
}
export function agentRegistry(catalog: Agent.Catalog): Wire.RegistrySync {
  validateAgent('Catalog', catalog);
  const nativeSchemaIdentity = hashJson(catalog);
  const nativeTools: Wire.ToolDefinition[] = catalog.clientRequests.map(value => {
    const definition = nativeToolDefinition(catalog, value, nativeSchemaIdentity), group = codexGroup(value.method);
    return { ...definition, discovery: { group, summary: codexSummary(value.method, group) } };
  });
  const discoveryGroups = new Map(Object.entries(codexGroups));
  for (const tool of nativeTools) if (tool.discovery?.group && !discoveryGroups.has(tool.discovery.group))
    discoveryGroups.set(tool.discovery.group, tool.discovery.group.replaceAll('/', ' / ') + ' native Codex tools.');
  const management: Wire.ToolDefinition[] = [
    ['status', 'StatusInput', 'Status', 'Check this host’s Codex version, connection identity and operation capacity.'],
    ['catalog', 'StatusInput', 'Catalog', 'Read the full installed Codex protocol catalog. Prefer agent_manager_discover for selected native methods; match this catalog’s hash to agent_manager_status.'],
    ['discover', 'NativeDiscoveryInput', 'NativeDiscovery', 'Discover version-dependent native Codex methods on this host when native Codex tools cannot reach it. Omit query to list methods, search by query, or pass an exact method to load its input/output schemas and expectedDefinitionHash for agent_manager_invoke or agent_manager_interact.'],
    ['frameLimits', 'FrameLimitsInput', 'FrameLimits', 'Read request and response size limits for this Codex connection.'],
    ['operation', 'OperationInput', 'Operation', 'Check the original outcome of this caller’s Codex action by operationId.'],
    ['invoke', 'PreventInput', 'Operation', 'Dispatch one idempotent native request and return its committed operation receipt.'],
    ['interact', 'PreventInput', 'Interaction', 'Send a direct native interaction without Hive persistence. Replies are cached briefly in this connection; use interaction lookup after uncertain delivery, never blindly replay mutations.'],
    ['interaction', 'OperationInput', 'Interaction', 'Read a caller-scoped recent interaction reply. Expiry requires reconciliation against native thread state.'],
    ['read', 'ReadInput', 'ReadObservation', 'Read fresh Codex task metadata, turns or items at the installed version without a durable operation.'],
    ['prevent', 'PreventInput', 'Operation', 'Retain an unexecuted failure for an absent original native request, or return its existing outcome unchanged.'],
    ['inputs', 'PendingInputQuery', 'PendingInputPage', 'Find Codex requests waiting for input or approval, or read an exact historical request.'],
    ['inputDefinition', 'InputDefinitionQuery', 'InputDefinition', 'Get the reply schema for a pending Codex input or approval request.'],
    ['answer', 'AnswerInput', 'Operation', 'Answer a pending Codex request using its reply schema and a stable operationId.'],
    ['notifications', 'NotificationQuery', 'NotificationPage', 'Read Codex event history, including any gaps in the retained sequence.'],
    ['projects', 'ProjectsInput', 'ProjectsResult', 'Read the current projects of the connected Codex instance. Codex is the sole project authority.'],
    ['resolveProject', 'ProjectResolveInput', 'ProjectLocation', 'Validate a native project directory or explicitly register a normal/existing directory with Codex. Internal and task directories use their already registered native project.'],
    ['capabilities', 'CapabilityProfileInput', 'CapabilityProfile', 'Read which kinds of agent work this host is configured to accept.'],
    ['configureCapabilities', 'ConfigureCapabilitiesInput', 'CapabilityProfile', 'Configure which kinds of agent work this host accepts, using the current profile revision.'],
    ['environmentDefaults', 'EnvironmentDefaultsInput', 'EnvironmentDefaults', 'Read the project-owned MCP and skill defaults used when no Hive-wide override exists.'],
    ['stageFile', 'StageFileInput', 'StagedFile', 'Store an image or file of up to 8 MiB on this host so a Codex message can reference it by path.'],
    ['resolveWorkspace', 'WorkspaceResolveInput', 'WorkspaceResolution', 'Inspect or prepare the deterministic Task workspace and resolve its intended project path without performing Git lifecycle actions.'],
  ].map(([name, input, output, description]) => ({ namespace: 'agent', name: name!, interfaceVersion: '1.0.0', description: description!,
    discovery: { ...(managementMcp(name!) ? { mcp: managementMcp(name!)! } : {}), keywords: ['Codex'] },
    inputSchema: agentSchema(input!), outputSchema: agentSchema(output!), annotations: { readOnlyHint: !['answer', 'invoke', 'interact', 'prevent', 'resolveProject', 'configureCapabilities', 'stageFile'].includes(name!) && name !== 'resolveWorkspace', idempotentHint: name !== 'interact' } }));
  return { discoveryHint: 'AgentManager provides host readiness, capabilities and fallback access to Codex. Use a direct native Codex function first when it reaches the owning host.', namespaces: [
    { namespace: 'codex', description: 'Codex tasks, projects, tools and integrations on this host', guideMarkdown: 'Use a direct native Codex function first when it reaches the host that owns the task or project; its typed arguments need no AgentManager discovery step. Use AgentManager for host capabilities and as fallback when a direct function cannot reach that owner. Discover the needed fallback method and use the exact returned native arguments and definition hash; available methods depend on the installed Codex version. '
      + 'Use thread/list and thread/read to inspect tasks, thread/start to create one and turn/start to send it work. Read-only methods need no outer operationId. For mutations, keep one outer operationId and inspect agent.operation after a lost reply. Pending inputs and approvals are handled through the agent namespace.',
      tools: nativeTools, discoveryGroups: [...discoveryGroups].map(([group, description]) => ({ group, description })), notifications: [], topics: [],
      inventoryKinds: [{ kind: 'thread', version: nativeInventorySchemaVersion, summarySchema: agentSchema('ThreadSummary'), searchPointers: ['/name', '/preview', '/cwd'], archivedPointer: '/archived', recencyPointer: '/recencyAt' },
        { kind: 'project', version: nativeProjectSchemaVersion, summarySchema: agentSchema('ProjectSummary'), searchPointers: ['/name'], recencyPointer: '/recencyAt' }] },
    { namespace: 'agent', description: 'Codex host readiness, work capabilities, outcomes and pending inputs',
      discoveryGroups: [{ group: 'agent', description: 'Check the Codex host, configure work capabilities and answer pending inputs.' }],
      guideMarkdown: 'Use a direct native Codex function first when it reaches the owning host; its typed arguments need no AgentManager discovery step. AgentManager supplies host capabilities and fallback access. Select a service node from the public tool schema, use agent_manager_status to read that node’s host, readiness and work capabilities, then use agent_manager_projects to read that host’s projects. Use agent_manager_discover to load an exact version-dependent native method. Pass its nativeVersion, method, expectedDefinitionHash and unchanged native params to agent_manager_invoke with a stable operationId. '
        + 'For pending input, use agent_manager_inputs for requests and their reply definitions, then submit agent_manager_answer to the same service node, connection epoch and request ID. Only the current owner can answer. '
        + 'Use agent_manager_read for fresh task state and original operation outcomes. Development-only direct interactions and notifications live on ivy_dev.',
      tools: management, notifications: [{ name: 'notification', version: '1.0.0', description: 'Transient hint for a new native notification; history remains authoritative.', payloadSchema: agentSchema('Notification') },
        { name: 'inputs', version: '1.0.0', description: 'Transient hint that a native request is waiting for input; reread current pending inputs.', payloadSchema: agentSchema('InputNotification') },
        { name: 'capabilities', version: '1.0.0', description: 'Transient hint that this node capability profile changed.', payloadSchema: agentSchema('CapabilityProfile') }],
      topics: [browserNoticeTopic('agent'), { topic: 'agent.notification', version: '1.0.0', title: 'Agent notification', description: 'A durable native AgentManager notification was retained.', payloadSchema: agentSchema('Notification'), eventKinds: [] },
        { topic: 'agent.capabilities', version: '1.0.0', title: 'Agent capabilities changed', description: 'The AgentManager capability profile changed.', payloadSchema: agentSchema('CapabilityProfile'), eventKinds: [] }], inventoryKinds: [] },
  ], contracts: [{ key: 'agent/instructions', version: '1.0.0', owner: { kind: 'service', serviceName: 'agent-manager' },
    mediaType: 'application/json', retention: { objects: { mode: 'retain' }, revisions: { mode: 'bounded', maximumCount: 100 } }, jsonSchema: agentSchema('InstructionsDocument'),
    specMarkdown: 'Versioned user-authored instruction documents, not runtime configuration or credentials. Hive-wide hostId=null; host documents bind an exact hostId. AgentUI writes with the user permissions and object CAS. Enabled host instructions follow enabled Hive instructions and take precedence. A disabled host document suppresses central instructions on that host; removing it inherits Hive again. See specs/AGENT-INSTRUCTIONS.md.' },
  { key: 'agent/mcp-configuration', version: '1.0.0', owner: { kind: 'service', serviceName: 'agent-manager' },
    mediaType: 'application/json', retention: { objects: { mode: 'retain' }, revisions: { mode: 'bounded', maximumCount: 100 } }, jsonSchema: agentSchema('McpDocument'),
    specMarkdown: 'Versioned secret-free MCP intent. AgentManager injects only its own host credential and only for its exact Hive MCP resource.' },
  { key: 'agent/skills', version: '1.0.0', owner: { kind: 'service', serviceName: 'agent-manager' },
    mediaType: 'application/json', retention: { objects: { mode: 'retain' }, revisions: { mode: 'bounded', maximumCount: 100 } }, jsonSchema: agentSchema('SkillsDocument'),
    specMarkdown: 'Versioned UTF-8 skill files installed into Ivy-owned user skill directories. Other user skills remain outside AgentManager ownership.' }], requiredContracts: [] };
}
