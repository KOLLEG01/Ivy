import { readFile, realpath, stat } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { fileHash } from '../../../packages/host-runtime/src/artifact.js';
import { runCommand } from '../../../packages/host-runtime/src/process.js';
import { requireThat } from '../../../packages/sdk/src/node.js';

export interface DaemonObservation {
  status: 'started' | 'alreadyRunning'; socketPath: string; managedCodexPath: string;
  managedCodexVersion: string; cliVersion: string; appServerVersion: string; pid: number | null;
}
export function initializedServerVersion(value: unknown): string {
  const version = typeof value === 'string' ? /^[^/\s]+(?: [^/\s]+)*\/(\d+\.\d+\.\d+)(?:\s|$)/.exec(value)?.[1] : undefined;
  requireThat(version, 'native_server_version_unavailable', 'The server handshake must identify its actual exact version.');
  return version;
}
export function daemonObservation(value: unknown, expectedVersion: string): DaemonObservation {
  requireThat(value !== null && typeof value === 'object' && !Array.isArray(value), 'native_daemon_response_invalid', 'Official daemon startup did not return a lifecycle object.');
  const row = value as Record<string, unknown>;
  requireThat(['started', 'alreadyRunning'].includes(String(row['status'])) &&
    typeof row['socketPath'] === 'string' && isAbsolute(row['socketPath']) &&
    typeof row['managedCodexPath'] === 'string' && isAbsolute(row['managedCodexPath']) &&
    (row['pid'] === undefined || Number.isSafeInteger(row['pid']) && Number(row['pid']) > 0),
  'native_daemon_response_invalid', 'Official daemon startup returned an invalid endpoint or process identity.');
  requireThat(row['cliVersion'] === expectedVersion && row['managedCodexVersion'] === expectedVersion && row['appServerVersion'] === expectedVersion,
    'native_server_version_mismatch', 'The shared server, managed installation and selected CLI must have the configured exact version. Existing servers are not restarted automatically.');
  return { status: row['status'] as DaemonObservation['status'], socketPath: row['socketPath'], managedCodexPath: row['managedCodexPath'],
    managedCodexVersion: expectedVersion, cliVersion: expectedVersion, appServerVersion: expectedVersion, pid: row['pid'] as number | undefined ?? null };
}

/** Only official idempotent start. No updater, restart, pairing, stop, or implicit stdio fallback. */
export async function ensureSharedDaemon(input: {
  executable: string; executableHash: string; version: string; home: string; jobLauncher: string; environment: Record<string, string>;
}): Promise<DaemonObservation> {
  const filename = process.platform === 'win32' ? 'codex.exe' : 'codex';
  const current = join(input.home, 'packages', 'standalone', 'current');
  let managed = join(current, 'bin', filename);
  try { requireThat((await stat(managed)).isFile(), 'native_daemon_installation_missing', 'The selected home needs its verified standalone Codex installation.'); }
  catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; managed = join(current, filename); }
  let actual: string;
  try { actual = await realpath(managed); }
  catch { requireThat(false, 'native_daemon_installation_missing', 'Install the pinned standalone Codex package in the selected home before starting its shared daemon.'); }
  requireThat(await fileHash(actual!) === input.executableHash, 'native_daemon_installation_mismatch', 'The managed standalone server executable differs from the selected verified CLI.');
  let command = { executable: 'codex', args: ['app-server', 'daemon', 'start'], timeoutMs: 100_000 };
  const executables: Record<string, string> = { codex: input.executable };
  const environment = { ...input.environment, CODEX_HOME: input.home };
  // The shared server cannot remain in a runtime owner's kill-on-stop systemd cgroup.
  // A user scope owns this short official lifecycle invocation and its detached daemon.
  if (process.platform === 'linux' && /\/ivy-next-[^/\n]+\.service(?:\/|$)/m.test(await readFile('/proc/self/cgroup', 'utf8'))) {
    const uid = process.getuid!(), runtime = '/run/user/' + uid;
    requireThat((await stat(join(runtime, 'bus')).catch(() => null))?.isSocket(), 'native_daemon_user_bus_unavailable', 'A running user systemd manager is required to start a shared daemon outside this Ivy service cgroup.');
    executables['systemd-run'] = '/usr/bin/systemd-run';
    Object.assign(environment, { XDG_RUNTIME_DIR: runtime, DBUS_SESSION_BUS_ADDRESS: 'unix:path=' + join(runtime, 'bus') });
    command = { ...command, executable: 'systemd-run', args: ['--user', '--scope', '--quiet', '--unit=ivy-codex-start-' + randomUUID(), '--', input.executable, ...command.args] };
  }
  const result = await runCommand(command, input.home, executables, { environment, jobLauncher: input.jobLauncher, allowWindowsBreakaway: true });
  requireThat(result.exitCode === 0 && result.errorCode === null && !result.truncated,
    'native_daemon_start_failed', 'Official shared daemon startup failed or timed out. Reconnection may retry idempotent start; no accepted native request is replayed.');
  let value: unknown;
  try { value = JSON.parse(result.stdout); } catch { requireThat(false, 'native_daemon_response_invalid', 'Official daemon startup did not return one JSON lifecycle response.'); }
  const observed = daemonObservation(value, input.version);
  requireThat(await realpath(observed.managedCodexPath) === actual && await fileHash(actual!) === input.executableHash,
    'native_daemon_installation_mismatch', 'The lifecycle selected another standalone server installation.');
  return observed;
}
