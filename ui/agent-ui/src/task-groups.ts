import type { Agent, Operation } from '../../../packages/sdk/src/client.js';
import { record } from '../../../packages/ui-client/src/native.js';
import { nativeProjectForThread, isInternalProject } from '../../../packages/sdk/src/native-project-membership.js';
export { normalizedProjectPath as normalizedPath, isInternalProject } from '../../../packages/sdk/src/native-project-membership.js';

/** Native approval reviews are internal sessions, not user tasks. */
export const isVisibleTask = (task: Operation.InventoryItem): boolean =>
  record(record(record(task.summary).source).subAgent).other !== 'guardian';

const orderedProjects = (projects: Agent.ProjectSummary[]) => [...projects].sort((a, b) =>
  (a.position ?? Number.MAX_SAFE_INTEGER) - (b.position ?? Number.MAX_SAFE_INTEGER));

export type ProjectChoice = { projectId: string; path: string; label: string };
export function projectChoices(projects: Agent.ProjectSummary[]): ProjectChoice[] {
  return orderedProjects(projects).flatMap(project => project.paths.map(path => ({
    projectId: project.nativeId, path, label: project.name + ' · ' + path,
  })));
}
export type ScopedProject = { serviceNodeId: string; project: Agent.ProjectSummary };
export function partitionInternalTasks(projects: ScopedProject[], tasks: Operation.InventoryItem[]) {
  const byHost = new Map<string, Agent.ProjectSummary[]>();
  for (const { serviceNodeId, project } of projects) byHost.set(serviceNodeId, [...(byHost.get(serviceNodeId) ?? []), project]);
  const recent: Operation.InventoryItem[] = [], internal: Operation.InventoryItem[] = [];
  for (const task of tasks) {
    const project = nativeProjectForThread(byHost.get(task.resourceRef.serviceNodeId) ?? [], record(task.summary));
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
