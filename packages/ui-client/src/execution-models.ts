import type { Agent, BoundTool } from '../../sdk/src/client.js';
import { record, schemaNode } from './native.js';
import type { NativeModel } from './native.js';
import { nativeBackendKey, nativeBackendLabel, nativeModelKey, sameBackend } from './native-backends.js';
import type { NativeBackendStatus } from './native-backends.js';

export interface ExecutionScope {
  node: { serviceNodeId: string; hostId: string };
  backend?: Agent.BackendIdentity;
  status: NativeBackendStatus;
}
export interface ExecutionModels extends ExecutionScope { models: NativeModel[]; bindings: BoundTool[] }
export interface ExecutionModel extends NativeModel { key: string; backend?: Agent.BackendIdentity }
export type NativeServiceTier = NonNullable<Agent.AccountExecutionDefaults['serviceTier']>;
export const nativeServiceTier = (value: unknown): NativeServiceTier | undefined =>
  value === 'standard' || value === 'fast' || value === 'flex' ? value : undefined;

export function executionBackendChoices(scopes: ExecutionScope[]) {
  const hosts = [...new Set(scopes.map(scope => scope.node.hostId))];
  const choices = new Map<string, { key: string; backend: Agent.BackendIdentity; label: string; hosts: Set<string>; disabled: boolean }>();
  for (const scope of scopes) {
    if (!scope.backend) continue;
    const key = nativeBackendKey(scope.backend);
    const saved = choices.get(key) ?? { key, backend: scope.backend,
      label: nativeBackendLabel(scope.backend), hosts: new Set<string>(), disabled: false };
    if (scope.status.state === 'ready') saved.hosts.add(scope.node.hostId);
    choices.set(key, saved);
  }
  return [...choices.values()].map(({ hosts: available, ...choice }) => ({ ...choice, missing: hosts.filter(host => !available.has(host)) }));
}

/** Models intersect across eligible hosts; distinct accounts never share options. */
export function executionModels(agents: ExecutionModels[]): ExecutionModel[] {
  const hosts = new Map<string, Map<string, ExecutionModel>>();
  const unscoped = new Map<string, number>();
  for (const agent of agents) if (!agent.backend) unscoped.set(agent.node.hostId, (unscoped.get(agent.node.hostId) ?? 0) + 1);
  for (const agent of agents) {
    const models = hosts.get(agent.node.hostId) ?? new Map<string, ExecutionModel>();
    hosts.set(agent.node.hostId, models);
    if (!agent.backend && unscoped.get(agent.node.hostId)! > 1) continue;
    for (const model of agent.models) {
      const key = nativeModelKey(agent.backend, model.model, agent.node.serviceNodeId);
      models.set(key, { ...model, key, ...(agent.backend ? { backend: agent.backend } : {}) });
    }
  }
  const catalogs = [...hosts.values()];
  return [...(catalogs[0]?.values() ?? [])].filter(model => catalogs.every(catalog => catalog.has(model.key))).map(model => ({
    ...model, hidden: catalogs.every(catalog => catalog.get(model.key)!.hidden),
    efforts: model.efforts.filter(effort => catalogs.every(catalog => catalog.get(model.key)!.efforts.includes(effort))),
  }));
}

export function modelAvailability(agents: ExecutionModels[]) {
  const models = new Map<string, ExecutionModel & { hosts: Set<string> }>();
  for (const agent of agents) for (const model of agent.models) {
    const key = nativeModelKey(agent.backend, model.model, agent.node.serviceNodeId);
    const entry = models.get(key) ?? { ...model, key, ...(agent.backend ? { backend: agent.backend } : {}), hosts: new Set<string>() };
    entry.hosts.add(agent.node.hostId); entry.hidden &&= model.hidden; models.set(key, entry);
  }
  const hosts = [...new Set(agents.map(agent => agent.node.hostId))];
  return [...models.values()].map(({ hosts: available, ...entry }) => ({ ...entry, missing: hosts.filter(host => !available.has(host)).sort() }));
}

function allowsTierValue(value: unknown, root: unknown, tier: string | null, depth = 0): boolean {
  if (depth > 32) return false;
  const schema = schemaNode(value, root);
  if (Array.isArray(schema.enum)) return schema.enum.includes(tier);
  if (schema.const !== undefined) return schema.const === tier;
  if (Array.isArray(schema.anyOf)) return schema.anyOf.some(branch => allowsTierValue(branch, root, tier, depth + 1));
  if (Array.isArray(schema.oneOf)) return schema.oneOf.filter(branch => allowsTierValue(branch, root, tier, depth + 1)).length === 1;
  const type = tier === null ? 'null' : 'string';
  return schema.type === type || Array.isArray(schema.type) && schema.type.includes(type);
}
function allowsTier(binding: BoundTool, tier: 'standard' | 'fast' | 'flex'): boolean {
  const root = binding.definition.inputSchema, field = record(schemaNode(root, root).properties).serviceTier;
  return !!field && allowsTierValue(field, root, tier === 'standard' ? null : tier);
}
/** The pinned Claude adapter accepts a broad Codex-shaped schema but has no tier implementation. */
export function nativeSpeedAvailable(bindings: (BoundTool | null | undefined)[], tier: 'standard' | 'fast' | 'flex',
  provider: 'codex' | 'claude' | undefined): boolean {
  return provider === 'codex' && bindings.length > 0 && bindings.every(binding => !!binding && allowsTier(binding, tier));
}
export function nativeModelSpeedAvailable(models: NativeModel[], model: string, tier: string): boolean {
  return tier === 'standard' || !!models.find(value => value.model === model)?.serviceTiers?.includes(tier);
}
export function executionSpeedAvailable(agents: ExecutionModels[], model?: string, effort?: string,
  backend?: Agent.BackendIdentity, tier: 'fast' | 'flex' = 'fast'): boolean {
  if (!backend && new Set(agents.filter(agent => !agent.backend).map(agent => agent.node.serviceNodeId)).size > 1) return false;
  const hosts = new Set(agents.map(agent => agent.node.hostId));
  const supported = new Set(agents.filter(agent => sameBackend(agent.backend, backend) &&
    agent.models.some(value => (model ? value.model === model : value.isDefault) && (!effort || value.efforts.includes(effort)) &&
      (!agent.backend || nativeModelSpeedAvailable(agent.models, value.model, tier))) &&
    agent.bindings.length === 2 && nativeSpeedAvailable(agent.bindings, tier, agent.backend?.provider ?? ('serverType' in agent.status ? agent.status.serverType : undefined))).map(agent => agent.node.hostId));
  return hosts.size > 0 && [...hosts].every(host => supported.has(host));
}
