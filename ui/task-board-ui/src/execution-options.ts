import { IvyError } from '../../../packages/sdk/src/client.js';
import type { Agent } from '../../../packages/sdk/src/client.js';
import { validateAgent } from '../../../packages/contracts/src/agent-validation.js';
import { backendRead, nativeBackendModels, nativeBackendScopes, sameBackend } from '../../../packages/ui-client/src/native-backends';
import type { NativeBackendScope } from '../../../packages/ui-client/src/native-backends';
import { nativeHosts, nativeServiceNodes } from '../../../packages/ui-client/src/native-hosts';
import { client } from './runtime';
export { executionBackendChoices, executionModels, modelAvailability, executionSpeedAvailable } from '../../../packages/ui-client/src/execution-models';

async function executionNodes(signal: AbortSignal, hostId?: string) {
  return nativeServiceNodes(client, signal, hostId);
}
type Node = Awaited<ReturnType<typeof executionNodes>>[number];
const available = (node: Node) => node.connected && node.synced && node.ready && node.desiredEnabled;
const scoped = (node: Node, scope: NativeBackendScope) => ({ node, ...(scope.backend ? { backend: scope.backend } : {}), status: scope.status });
async function scopesFor(node: Node, signal: AbortSignal) {
  return (await nativeBackendScopes(client, node, signal)).map(scope => ({ scope, ...scoped(node, scope) }));
}
export async function executionHostOptions(signal: AbortSignal, hostId: string, serviceNodeId?: string) {
  if (!hostId) return { agents: [], backends: [] };
  const nodes = (await executionNodes(signal, hostId)).filter(node => available(node) && (!serviceNodeId || node.serviceNodeId === serviceNodeId));
  const backends = (await Promise.all(nodes.map(async node => {
    try { return await scopesFor(node, signal); } catch { signal.throwIfAborted(); return []; }
  }))).flat();
  const agents = (await Promise.all(backends.map(async owner => {
    if (owner.status.state !== 'ready') return null;
    try { return { ...scoped(owner.node, owner.scope), ...await nativeBackendModels(client, owner.scope, signal) }; }
    catch { signal.throwIfAborted(); return null; }
  }))).filter(agent => agent !== null);
  return { agents, backends };
}

export async function executionEnvironments(signal: AbortSignal, includeNativeOptions = true) {
  const nodes = await executionNodes(signal);
  const scopes = (await Promise.all(nodes.filter(available).map(async node => {
    try { return await scopesFor(node, signal); } catch { signal.throwIfAborted(); return []; }
  }))).flat();
  const agents = (await Promise.all(scopes.map(async owner => {
    if (owner.status.state !== 'ready') return null;
    try {
      const [profile, projects, options] = await Promise.all([
        backendRead(client, owner.scope, 'agent.capabilities', {}, signal),
        backendRead(client, owner.scope, 'agent.projects', {}, signal),
        includeNativeOptions ? nativeBackendModels(client, owner.scope, signal) : { models: [], bindings: [] },
      ]);
      validateAgent('CapabilityProfile', profile); validateAgent('ProjectsResult', projects);
      const capabilities = profile as Agent.CapabilityProfile;
      if (capabilities.serviceNodeId !== owner.node.serviceNodeId || capabilities.hostId !== owner.node.hostId ||
        !sameBackend(capabilities.backend, owner.backend) || !sameBackend((projects as Agent.ProjectsResult).backend, owner.backend))
        throw new IvyError('native_owner_mismatch', 'Projects and capabilities must retain the selected account.');
      return { ...scoped(owner.node, owner.scope), ...options, profile: profile as Agent.CapabilityProfile, projects: projects as Agent.ProjectsResult };
    } catch { signal.throwIfAborted(); return null; }
  }))).filter(agent => agent !== null);
  const capabilities = new Map<string, string>();
  const projects = new Map<string, { id: string; name: string; path: string; hosts: Set<string> }>();
  for (const observation of agents) {
    for (const value of observation.profile.capabilities) capabilities.set(value.key, value.label);
    for (const project of observation.projects.projects) for (const path of project.paths) {
      const key = JSON.stringify([observation.backend ?? observation.node.serviceNodeId, project.nativeId, path]);
      const saved = projects.get(key) ?? { id: project.nativeId, name: project.name, path, hosts: new Set<string>() };
      saved.hosts.add(observation.node.hostId); projects.set(key, saved);
    }
  }
  return { nodes, hosts: nativeHosts(nodes).map(host => host.hostId).sort(), backends: scopes,
    capabilities: [...capabilities].map(([key, label]) => ({ key, label })).sort((a, b) => a.key.localeCompare(b.key)),
    projects: [...projects].map(([key, value]) => ({ key, id: value.id, name: value.name, path: value.path, hosts: [...value.hosts].sort() })).sort((a, b) => a.name.localeCompare(b.name) || a.path.localeCompare(b.path)), agents };
}
export type ExecutionAgent = Awaited<ReturnType<typeof executionEnvironments>>['agents'][number];
