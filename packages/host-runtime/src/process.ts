import { spawn } from 'node:child_process';
import type { ChildProcess } from 'node:child_process';
import { isAbsolute, resolve, join } from 'node:path';
import { realpath, stat } from 'node:fs/promises';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { inside } from './config.js';
import type { Host } from '../../contracts/src/generated.js';

export type Command = NonNullable<Host.BuildPlan['entrypoint']>;
export interface ProcessResult { exitCode: number | null; signal: string | null; errorCode: string | null; stdout: string; stderr: string; truncated: boolean }
export interface RunningProcess { child: ChildProcess; completion: Promise<ProcessResult>; stop: (graceMs?: number) => Promise<ProcessResult> }
export function runtimeEnvironment(extra: Record<string, string> = {}): NodeJS.ProcessEnv {
  const allowed = new Set(['systemroot', 'windir', 'comspec', 'path', 'pathext', 'temp', 'tmp', 'tmpdir', 'userprofile', 'home', 'codex_home', 'appdata', 'localappdata', 'programdata', 'allusersprofile', 'programfiles', 'programfiles(x86)', 'lang', 'lc_all', 'timezone']);
  const overrides = new Set(Object.keys(extra).map(key => key.toLowerCase()));
  const env: NodeJS.ProcessEnv = {};
  for (const [key, value] of Object.entries(process.env)) if (value !== undefined && allowed.has(key.toLowerCase()) && !overrides.has(key.toLowerCase())) env[key] = value;
  return { ...env, NODE_NO_WARNINGS: '1', ...extra };
}
export async function resolveCommand(command: Command, root: string, executables: Record<string, string>): Promise<{ executable: string; cwd: string; args: string[] }> {
  const base = await realpath(root), cwd = await realpath(resolve(base, command.cwd ?? '.'));
  requireThat(inside(base, cwd), 'invalid_arguments', 'Command working directory leaves its artifact/source root.');
  const configured = Object.hasOwn(executables, command.executable) ? executables[command.executable]! : null;
  const requested = configured ?? resolve(base, command.executable);
  const executable = await realpath(requested);
  requireThat((configured !== null && isAbsolute(configured)) || (!isAbsolute(command.executable) && inside(base, executable)), 'invalid_arguments', 'Command executable must be inside the artifact or explicitly mapped by host configuration.');
  requireThat((await stat(executable)).isFile() && !/\.(?:cmd|bat|ps1)$/i.test(executable), 'invalid_arguments', 'Commands need a direct executable; shell scripts require an explicit interpreter and argument array.');
  return { executable, cwd, args: command.args };
}
export async function startProcess(command: Command, root: string, executables: Record<string, string>, options: {
  environment?: Record<string, string>; jobLauncher?: string; redact?: string[]; onOutput?: (stream: 'stdout' | 'stderr', text: string) => void;
  captureOutput?: boolean;
  /** Explicit bounded capture for trusted helper output that is larger than diagnostics. */
  maxOutputBytes?: number;
  executionCwd?: string;
  /** Read-only helper probes can opt out of the service job; service processes stay job-owned. */
  useJobLauncher?: boolean;
  /** Stable Windows job identity for observing and excluding a previous owned tree. */
  jobName?: string;
  /** Only the shared Codex lifecycle may explicitly detach a descendant from this job. */
  allowWindowsBreakaway?: boolean;
} = {}): Promise<RunningProcess> {
  const secrets = (options.redact ?? []).filter(Boolean);
  requireThat(secrets.every(secret => secret.length <= 65536), 'invalid_arguments', 'Redaction values exceed their bounded length.');
  const maxOutputBytes = options.maxOutputBytes ?? 65_536;
  requireThat(Number.isSafeInteger(maxOutputBytes) && maxOutputBytes >= 65_536 && maxOutputBytes <= 16 * 1024 * 1024,
    'invalid_arguments', 'Captured command output needs an explicit bounded size.');
  const resolved = await resolveCommand(command, root, executables);
  const windows = process.platform === 'win32';
  const useJobLauncher = windows && options.useJobLauncher !== false;
  requireThat(options.jobName === undefined || (useJobLauncher && /^Global\\Ivy\.[a-zA-Z0-9._-]{1,128}$/.test(options.jobName)),
    'invalid_arguments', 'A named process job requires the Windows job launcher.');
  const launcher = useJobLauncher ? await realpath(options.jobLauncher ?? join(root, 'dist', 'native', 'ivy-job.exe')) : resolved.executable;
  const args = useJobLauncher ? ['--parent', String(process.pid), ...(options.allowWindowsBreakaway ? ['--allow-breakaway'] : []),
    ...(options.jobName ? ['--job-name', options.jobName] : []), resolved.executable, ...resolved.args] : resolved.args;
  const cwd = options.executionCwd === undefined ? resolved.cwd : await realpath(options.executionCwd);
  const child = spawn(launcher, args, { cwd, env: runtimeEnvironment(options.environment), windowsHide: true, detached: !windows, stdio: ['pipe', 'pipe', 'pipe'] });
  let ended = false, errorCode: string | null = null, truncated = false;
  const output = { stdout: Buffer.alloc(0), stderr: Buffer.alloc(0) };
  const retained = Math.max(1, ...secrets.map(secret => secret.length)) - 1;
  const tails = { stdout: '', stderr: '' }, decoders = { stdout: new TextDecoder(), stderr: new TextDecoder() };
  const append = (stream: 'stdout' | 'stderr', value: string) => {
    const safe = Buffer.from(secrets.reduce((result, secret) => result.replaceAll(secret, '[redacted]'), value));
    output[stream] = Buffer.concat([output[stream], safe]);
    if (output[stream].length > maxOutputBytes) { output[stream] = output[stream].subarray(-maxOutputBytes); truncated = true; }
    try { options.onOutput?.(stream, safe.toString('utf8')); } catch { /* Log sinks cannot crash supervision. */ }
  };
  const received = (stream: 'stdout' | 'stderr', chunk?: Buffer) => {
    if (ended) return;
    if (options.captureOutput === false) return; // A native protocol owner reads raw pipes; payloads never become diagnostic logs.
    const text = tails[stream] + decoders[stream].decode(chunk, { stream: chunk !== undefined });
    let boundary = chunk === undefined ? text.length : Math.max(0, text.length - retained);
    for (const secret of secrets) {
      let at = text.indexOf(secret);
      while (at >= 0 && at < boundary) { if (at + secret.length > boundary) boundary = at; at = text.indexOf(secret, at + 1); }
    }
    append(stream, text.slice(0, boundary)); tails[stream] = text.slice(boundary);
  };
  child.stdout!.on('data', (bytes: Buffer) => received('stdout', bytes));
  child.stderr!.on('data', (bytes: Buffer) => received('stderr', bytes));
  const signal = (hard: boolean) => {
    if (ended || !child.pid) return;
    try { if (windows) child.kill(); else process.kill(-child.pid, hard ? 'SIGKILL' : 'SIGTERM'); } catch { /* Completion/observation determines the outcome. */ }
  };
  let finishCompletion!: (result: ProcessResult) => void;
  const completion = new Promise<ProcessResult>(resolve => { finishCompletion = resolve; });
  const finish = (exitCode: number | null, exitSignal: string | null) => {
    if (ended) return;
    received('stdout'); received('stderr'); ended = true;
    finishCompletion({ exitCode, signal: exitSignal, errorCode, stdout: output.stdout.toString('utf8'), stderr: output.stderr.toString('utf8'), truncated });
  };
  child.on('error', () => { if (!ended) errorCode = 'spawn_failed'; });
  const exited = (exitCode: number | null, exitSignal: NodeJS.Signals | null) => {
    if (ended) return; // A late close cannot turn an unknown stop into a proved clean exit.
    if (!windows && child.pid) { try { process.kill(-child.pid, 'SIGKILL'); } catch { /* Empty owned process group. */ } }
    finish(exitCode, exitSignal);
  };
  // On Windows the spawned process is the sole handle owner for a kill-on-close
  // Job Object. Its process exit is therefore the authoritative tree boundary;
  // waiting for `close` can hang on inherited pipes from a native child that is
  // already being terminated by that job.
  if (useJobLauncher) child.once('exit', exited);
  child.once('close', exited); // Also completes spawn failures, which need not emit `exit`.
  let stopping: Promise<ProcessResult> | null = null;
  const stop = (graceMs = 1500): Promise<ProcessResult> => {
    requireThat(Number.isSafeInteger(graceMs) && graceMs >= 0 && graceMs <= 60_000, 'invalid_arguments', 'Process stop grace must be finite and at most one minute.');
    if (stopping || ended) return stopping ?? completion;
    signal(false);
    const hard = setTimeout(() => signal(true), graceMs);
    const final = setTimeout(() => {
      signal(true); errorCode = 'outcome_unknown'; truncated = true;
      finish(child.exitCode, child.signalCode);
      // Relinquish only our handles. Pipe closure does not prove escaped descendants terminated.
      child.stdin?.destroy(); child.stdout?.destroy(); child.stderr?.destroy(); child.unref();
    }, graceMs + 2000);
    stopping = completion.finally(() => { clearTimeout(hard); clearTimeout(final); });
    return stopping;
  };
  return { child, completion, stop };
}
/** Absence of the exact named job proves that its handles and descendants have ended. */
export async function windowsJobStopped(name: string, root: string): Promise<boolean> {
  requireThat(process.platform === 'win32' && /^Global\\Ivy\.[a-zA-Z0-9._-]{1,128}$/.test(name),
    'invalid_arguments', 'Invalid Windows process job identity.');
  const result = await runCommand({ executable: 'dist/native/ivy-job.exe', args: ['--probe-job', name], timeoutMs: 5000 },
    root, {}, { useJobLauncher: false });
  const observed = JSON.parse(result.stdout) as { stopped?: unknown };
  requireThat(typeof observed.stopped === 'boolean', 'invalid_response', 'Process job observation is unavailable.');
  return observed.stopped;
}
export async function runCommand(command: Command, root: string, executables: Record<string, string>, options: Parameters<typeof startProcess>[3] & { input?: string } = {}): Promise<ProcessResult> {
  requireThat(options.input === undefined || Buffer.byteLength(options.input) <= 65536, 'limit_exceeded', 'Command input exceeds its bounded size.');
  const process = await startProcess(command, root, executables, options);
  let timedOut = false;
  const deadline = setTimeout(() => { timedOut = true; void process.stop(250); }, command.timeoutMs);
  try {
    if (options.input !== undefined) {
      // A child may reject the request and close stdin early. Its actual completion/response
      // remains authoritative; an EPIPE must not terminate the supervising parent.
      process.child.stdin!.on('error', () => undefined);
      process.child.stdin!.end(options.input);
    }
    const result = await process.completion;
    if (timedOut) throw new IvyError('deadline_exceeded', 'Command exceeded its finite deadline.', 'unknown');
    if (result.exitCode !== 0 || result.errorCode) throw new IvyError(result.errorCode ?? 'command_failed', 'Command failed; bounded diagnostics record the outcome.',
      result.errorCode === 'spawn_failed' && process.child.pid === undefined ? 'not_executed' : 'unknown',
      { exitCode: result.exitCode, signal: result.signal });
    return result;
  } finally { clearTimeout(deadline); }
}
