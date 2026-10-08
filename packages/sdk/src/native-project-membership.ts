import type { Agent } from '../../contracts/src/generated.js';

const windowsPath = (value: string) => /^[a-zA-Z]:[\\/]/.test(value) || value.startsWith('\\\\');
export const normalizedProjectPath = (value: string): string => {
  const slash = value.replace(/^\\\\\?\\UNC\\/i, '\\\\').replace(/^\\\\\?\\/, '').replace(/\\/g, '/').replace(/\/+$/, '');
  return windowsPath(value) || slash.startsWith('//') ? slash.toLowerCase() : slash;
};
const containsPath = (root: string, cwd: string): boolean => {
  const parent = normalizedProjectPath(root), child = normalizedProjectPath(cwd);
  return child === parent || child.startsWith(parent + '/');
};

/** Explicit native membership wins over paths, including explicit projectless tasks. */
export function nativeProjectForThread(projects: Agent.ProjectSummary[], thread: { projectId?: unknown; cwd?: unknown }): Agent.ProjectSummary | undefined {
  if (typeof thread.projectId === 'string' && thread.projectId) return projects.find(project => project.nativeId === thread.projectId);
  if ('projectId' in thread || typeof thread.cwd !== 'string') return undefined;
  return projects.flatMap(project => project.paths.filter(path => containsPath(path, thread.cwd as string))
    .map(path => ({ project, length: normalizedProjectPath(path).length }))).sort((a, b) => b.length - a.length)[0]?.project;
}
export const isInternalProject = (project: Agent.ProjectSummary): boolean => project.name === 'IvyInternal';
