import type { Agent } from '../../contracts/src/generated.js';
export declare const normalizedProjectPath: (value: string) => string;
/** Explicit native membership wins over paths, including explicit projectless tasks. */
export declare function nativeProjectForThread(projects: Agent.ProjectSummary[], thread: {
    projectId?: unknown;
    cwd?: unknown;
}): Agent.ProjectSummary | undefined;
export declare const isInternalProject: (project: Agent.ProjectSummary) => boolean;
