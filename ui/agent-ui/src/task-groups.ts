import type { Agent, Operation } from '../../../packages/sdk/src/client.js';
import { record } from '../../../packages/ui-client/src/native.js';
import { nativeProjectForThread, isInternalProject } from '../../../packages/sdk/src/native-project-membership.js';
import { canonical } from '../../../packages/sdk/src/client.js';
import type { NativeHost } from '../../../packages/ui-client/src/native-hosts.js';
import { nativeOwnerKey } from '../../../packages/ui-client/src/native-route.js';
import type { NativeRouteOwner } from '../../../packages/ui-client/src/native-route.js';
export { normalizedProjectPath as normalizedPath, isInternalProject } from '../../../packages/sdk/src/native-project-membership.js';

/** Native approval reviews and ephemeral service contexts (no saved history) are not user tasks. */
export const isVisibleTask = (task: Operation.InventoryItem): boolean =>
  record(task.summary).ephemeral !== true &&
  record(record(record(task.summary).source).subAgent).other !== 'guardian';

const orderedProjects = (projects: Agent.ProjectSummary[]) => [...projects].sort((a, b) =>
  (a.position ?? Number.MAX_SAFE_INTEGER) - (b.position ?? Number.MAX_SAFE_INTEGER));

export type ProjectChoice = { projectId: string; path: string; label: string };
export function projectChoices(projects: Agent.ProjectSummary[]): ProjectChoice[] {
  return orderedProjects(projects).flatMap(project => project.paths.map(path => ({
    projectId: project.nativeId, path, label: project.name + ' · ' + path,
  })));
}
export type ScopedProject = { serviceNodeId: string; namespace?: string; backend?: Agent.BackendIdentity | undefined; project: Agent.ProjectSummary };
const projectScope = (project: ScopedProject) => canonical([project.serviceNodeId, project.namespace ?? 'codex']);
const taskScope = (task: Operation.InventoryItem) => canonical([task.resourceRef.serviceNodeId, task.resourceRef.namespace]);
export function partitionInternalTasks(projects: ScopedProject[], tasks: Operation.InventoryItem[]) {
  const byHost = new Map<string, Agent.ProjectSummary[]>();
  for (const scoped of projects) { const key = projectScope(scoped); const own = byHost.get(key) ?? []; own.push(scoped.project); byHost.set(key, own); }
  const recent: Operation.InventoryItem[] = [], internal: Operation.InventoryItem[] = [];
  for (const task of tasks) {
    const project = nativeProjectForThread(byHost.get(taskScope(task)) ?? [], record(task.summary));
    (project && isInternalProject(project) ? internal : recent).push(task);
  }
  return { recent, internal };
}
export type TaskGroup = { id: string; name: string; path: string; paths: string[]; tasks: Operation.InventoryItem[] };
export function groupTasks(projects: Agent.ProjectSummary[], tasks: Operation.InventoryItem[]): TaskGroup[] {
  const groups = orderedProjects(projects).map(project => ({ id: project.nativeId, name: project.name,
    path: project.paths[0] ?? '', paths: [...project.paths], tasks: [] as Operation.InventoryItem[] }));
  const byId = new Map(groups.map(group => [group.id, group]));
  const remaining: Operation.InventoryItem[] = [];
  for (const task of tasks) {
    const project = nativeProjectForThread(projects, record(task.summary)), group = project ? byId.get(project.nativeId) : undefined;
    (group?.tasks ?? remaining).push(task);
  }
  if (remaining.length) groups.push({ id: 'other', name: 'No project', path: '', paths: [], tasks: remaining });
  return groups;
}
export type ScopedTaskGroup = TaskGroup & { projectId: string; owner: NativeRouteOwner };
export type HostSection = { hostId: string; serviceNodeId: string; groups: ScopedTaskGroup[]; internal: Operation.InventoryItem[]; hasInternal: boolean };
/** Every host keeps its own project tree: listed hosts first, then any other host that still has projects or tasks. */
export function hostSections(hosts: NativeHost[], projects: ScopedProject[], tasks: Operation.InventoryItem[]): HostSection[] {
  const { recent, internal } = partitionInternalTasks(projects, tasks);
  const known = new Set(hosts.flatMap(host => host.nodes.map(node => node.serviceNodeId)));
  const extra = [...new Set([...projects.map(value => value.serviceNodeId), ...tasks.map(task => task.resourceRef.serviceNodeId)])].filter(node => !known.has(node));
  return [...hosts, ...extra.map(serviceNodeId => ({ hostId: serviceNodeId, serviceNodeId, nodes: [{ serviceNodeId }] }))].map(host => {
    const nodes = new Set(host.nodes.map(node => node.serviceNodeId));
    const own = projects.filter(value => nodes.has(value.serviceNodeId));
    const mine = (task: Operation.InventoryItem) => nodes.has(task.resourceRef.serviceNodeId);
    const scopes = new Map<string, NativeRouteOwner>();
    for (const value of own) scopes.set(projectScope(value), { serviceNodeId: value.serviceNodeId, namespace: value.namespace ?? 'codex', ...(value.backend ? { backend: value.backend } : {}) });
    for (const task of recent.filter(mine)) if (!scopes.has(taskScope(task))) scopes.set(taskScope(task), { serviceNodeId: task.resourceRef.serviceNodeId, namespace: task.resourceRef.namespace });
    const groups = [...scopes].flatMap(([key, owner]) => groupTasks(own.filter(value => projectScope(value) === key && !isInternalProject(value.project)).map(value => value.project), recent.filter(task => taskScope(task) === key))
      .map(group => ({ ...group, projectId: group.id, id: canonical([nativeOwnerKey(owner), group.id]), owner })));
    return { hostId: host.hostId, serviceNodeId: host.serviceNodeId, groups, internal: internal.filter(mine), hasInternal: own.some(value => isInternalProject(value.project)) };
  });
}
