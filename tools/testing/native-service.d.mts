import type { ChildProcess } from 'node:child_process';
import type { Host } from '../../packages/contracts/src/generated.js';

export function artifactRoot(component: string, supplied?: string): string;
export function nativeExecutable(artifact: string, name: string): string;
export function writeJson(path: string, value: unknown): Promise<void>;
export interface NativeService {
  config: Host.InstanceConfig;
  path: string;
  child: ChildProcess;
  closed: Promise<{ code: number | null; signal: NodeJS.Signals | null }>;
  launchId: string;
  readonly pid: number;
  readonly logs: string;
  ready(timeout?: number): Promise<Host.Health>;
  close(): Promise<{ code: number | null; signal: NodeJS.Signals | null }>;
  kill(): Promise<{ code: number | null; signal: NodeJS.Signals | null }>;
}
export function startNativeService(config: Omit<Host.InstanceConfig, 'artifactRoot' | 'version' | 'buildId'>,
  options?: { artifact?: string; env?: NodeJS.ProcessEnv; ready?: boolean; parentPid?: number }): Promise<NativeService>;
export function startTestHive(options: { root: string; artifact?: string; credentials: Array<{ principalId: string; token: string }>;
  port?: number; settings?: Record<string, unknown>; consoleRoot?: string }): Promise<NativeService>;
export function crashStorageWorker(owner: NativeService): Promise<number>;
