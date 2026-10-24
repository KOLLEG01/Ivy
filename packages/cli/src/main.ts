import { spawn } from 'node:child_process';
import { resolve, basename, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Host } from '../../contracts/src/generated.js';

export interface CliResult { exitCode: number; output: Host.CliOutput }

/** Compatibility for existing JS callers. The packaged native CLI owns every
 * local action, its operation identity, configuration and output/exit policy. */
export async function cli(args: string[], environment: NodeJS.ProcessEnv = process.env, onProgress?: (message: string) => void): Promise<CliResult> {
  const moduleRoot = fileURLToPath(new URL('../../../', import.meta.url));
  const root = basename(moduleRoot) === 'dist' ? dirname(moduleRoot) : moduleRoot;
  const executable = resolve(root, 'dist/native', `${process.platform}-${process.arch}`, process.platform === 'win32' ? 'ivy.exe' : 'ivy');
  const failure = (code: string, message: string, outcome: 'not_executed' | 'unknown'): CliResult => ({ exitCode: 1,
    output: { schemaVersion: 1, ok: false, code, data: { message, outcome } } });
  return new Promise(resolveResult => {
    const child = spawn(executable, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...environment, IVY_NODE_EXECUTABLE: environment['IVY_NODE_EXECUTABLE'] ?? process.execPath } });
    let stdout = '', pending = '';
    child.stdout.setEncoding('utf8').on('data', (text: string) => { stdout += text; });
    const progress = (message: string) => { try { onProgress?.(message); } catch { /* Display only. */ } };
    child.stderr.setEncoding('utf8').on('data', (text: string) => {
      const lines = (pending + text).split(/\r?\n/); pending = lines.pop()!;
      for (const line of lines) progress(line);
    });
    child.on('error', () => resolveResult(failure('unsupported_runtime',
      'The native Ivy CLI is unavailable. Build or install the HostExecutor package for this platform.', 'not_executed')));
    child.on('close', (exitCode, signal) => {
      if (pending) progress(pending);
      try {
        if (exitCode === null || signal) throw Error('Interrupted');
        resolveResult({ exitCode, output: JSON.parse(stdout) as Host.CliOutput });
      } catch { resolveResult(failure('outcome_unknown', 'The native Ivy CLI did not return a complete result.', 'unknown')); }
    });
  });
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const result = await cli(process.argv.slice(2), process.env, message => process.stderr.write(message + '\n'));
  process.stdout.write(JSON.stringify(result.output) + '\n'); process.exitCode = result.exitCode;
}
