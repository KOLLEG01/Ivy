import type { Agent, Operation } from '../../../packages/sdk/src/client.js';
import { record } from '../../../packages/ui-client/src/native.js';

/** Native approval reviews are internal sessions, not user tasks. */
export const isVisibleTask = (task: Operation.InventoryItem): boolean =>
  record(record(record(task.summary).source).subAgent).other !== 'guardian';

const windowsPath = (value: string) => /^[a-zA-Z]:[\\/]/.test(value) || value.startsWith('\\\\');
export const normalizedPath = (value: string): string => {
  const slash = value.replace(/^\\\\\?\\UNC\\/i, '\\\\').replace(/^\\\\\?\\/, '').replace(/\\/g, '/').replace(/\/+$/, '');
  return windowsPath(value) || slash.startsWith('//') ? slash.toLowerCase() : slash;
};
const containsPath = (root: string, cwd: string): boolean => {
  const parent = normalizedPath(root), child = normalizedPath(cwd);
  return child === parent || child.startsWith(parent + '/');
};
const orderedProjects = (projects: Agent.ProjectSummary[]) => [...projects].sort((a, b) =>
  (a.position ?? Number.MAX_SAFE_INTEGER) - (b.position ?? Number.MAX_SAFE_INTEGER));

export type ProjectChoice = { projectId: string; path: string; label: string };
export function projectChoices(projects: Agent.ProjectSummary[]): ProjectChoice[] {
  return orderedProjects(projects).flatMap(project => project.paths.map(path => ({
    projectId: project.nativeId, path, label: project.name + ' · ' + path,
  })));
}
function taskProject(projects: Agent.ProjectSummary[], task: Operation.InventoryItem): Agent.ProjectSummary | undefined {
  const summary = record(task.summary);
  if (typeof summary.projectId === 'string' && summary.projectId) return projects.find(project => project.nativeId === summary.projectId);
  if (typeof summary.cwd !== 'string') return undefined;
  return projects.flatMap(project => project.paths.filter(path => containsPath(path, summary.cwd as string))
    .map(path => ({ project, length: normalizedPath(path).length }))).sort((a, b) => b.length - a.length)[0]?.project;
}
export type ScopedProject = { serviceNodeId: string; project: Agent.ProjectSummary };
export const isInternalProject = (project: Agent.ProjectSummary): boolean => project.name === 'IvyInternal';
export function partitionInternalTasks(projects: ScopedProject[], tasks: Operation.InventoryItem[]) {
  const byHost = new Map<string, Agent.ProjectSummary[]>();
  for (const { serviceNodeId, project } of projects) byHost.set(serviceNodeId, [...(byHost.get(serviceNodeId) ?? []), project]);
  const recent: Operation.InventoryItem[] = [], internal: Operation.InventoryItem[] = [];
  for (const task of tasks) {
    const project = taskProject(byHost.get(task.resourceRef.serviceNodeId) ?? [], task);
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
    const project = taskProject(projects, task), group = project ? byId.get(project.nativeId) : undefined;
    (group?.tasks ?? remaining).push(task);
  }
  if (remaining.length) groups.push({ id: 'other', name: 'Other tasks', path: '', paths: [], tasks: remaining });
  return groups;
}
