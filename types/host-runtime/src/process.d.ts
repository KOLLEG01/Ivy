import type { ChildProcess } from 'node:child_process';
import type { Host } from '../../contracts/src/generated.js';
export type Command = NonNullable<Host.BuildPlan['entrypoint']>;
export interface ProcessResult {
    exitCode: number | null;
    signal: string | null;
    errorCode: string | null;
    stdout: string;
    stderr: string;
    truncated: boolean;
}
export interface RunningProcess {
    child: ChildProcess;
    completion: Promise<ProcessResult>;
    stop: (graceMs?: number) => Promise<ProcessResult>;
}
export declare function runtimeEnvironment(extra?: Record<string, string>): NodeJS.ProcessEnv;
export declare function resolveCommand(command: Command, root: string, executables: Record<string, string>): Promise<{
    executable: string;
    cwd: string;
    args: string[];
}>;
export declare function startProcess(command: Command, root: string, executables: Record<string, string>, options?: {
    environment?: Record<string, string>;
    jobLauncher?: string;
    redact?: string[];
    onOutput?: (stream: 'stdout' | 'stderr', text: string) => void;
    captureOutput?: boolean;
    /** Explicit bounded capture for trusted helper output that is larger than diagnostics. */
    maxOutputBytes?: number;
    executionCwd?: string;
    /** Read-only helper probes can opt out of the service job; service processes stay job-owned. */
    useJobLauncher?: boolean;
    /** Only the shared Codex lifecycle may explicitly detach a descendant from this job. */
    allowWindowsBreakaway?: boolean;
}): Promise<RunningProcess>;
export declare function runCommand(command: Command, root: string, executables: Record<string, string>, options?: Parameters<typeof startProcess>[3] & {
    input?: string;
}): Promise<ProcessResult>;
