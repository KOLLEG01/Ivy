import { createReadStream } from 'node:fs';
import { realpath, stat } from 'node:fs/promises';
import { basename, dirname, isAbsolute, resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { runCommand } from '../../../packages/host-runtime/src/process.js';
import { requireThat } from '../../../packages/sdk/src/node.js';
import { validateAgent } from '../../../packages/sdk/src/node.js';
import type { Agent } from '../../../packages/sdk/src/node.js';

const probe = `
$ErrorActionPreference='Stop'
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class IvyNativeShellProbe { [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern int GetCurrentPackageFullName(ref uint length, IntPtr name); }'
[uint32]$ivyPackageLength=0
$ivyPackageStatus=[IvyNativeShellProbe]::GetCurrentPackageFullName([ref]$ivyPackageLength,[IntPtr]::Zero)
@{packageStatus=$ivyPackageStatus;version=$PSVersionTable.PSVersion.ToString();executable=[System.Diagnostics.Process]::GetCurrentProcess().MainModule.FileName}|ConvertTo-Json -Compress
`;

/** Store/AppX shells may escape the native Job; the owning host supplies a pinned portable pwsh. */
export async function verifyWindowsShell(settings: Agent.WindowsShell | undefined, cwd: string, jobLauncher: string): Promise<Agent.WindowsShellStatus> {
  requireThat(process.platform === 'win32' && settings, 'native_shell_required', 'Windows native execution requires an explicitly configured unpackaged PowerShell.');
  validateAgent('WindowsShell', settings);
  requireThat(isAbsolute(settings.executable), 'native_shell_mismatch', 'The native shell executable must be absolute.');
  const executable = await realpath(settings.executable);
  requireThat(basename(executable).toLowerCase() === 'pwsh.exe' && (await stat(executable)).isFile(), 'native_shell_mismatch', 'The configured native shell must be a direct pwsh.exe executable.');
  const hash = createHash('sha256'); for await (const bytes of createReadStream(executable)) hash.update(bytes as Buffer);
  requireThat('sha256:' + hash.digest('hex') === settings.executableHash, 'native_shell_mismatch', 'Native shell bytes differ from the configured digest.');
  const result = await runCommand({ executable: 'shell', args: ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', probe], timeoutMs: 15_000 }, cwd,
    { shell: executable }, { jobLauncher });
  let value: unknown;
  try { value = JSON.parse(result.stdout); } catch { value = null; }
  requireThat(value && typeof value === 'object' && !Array.isArray(value), 'native_shell_mismatch', 'Native shell identity probe returned no valid observation.');
  const observed = value as Record<string, unknown>;
  requireThat(observed['packageStatus'] === 15700, 'native_shell_packaged', 'Packaged PowerShell is outside the supported native process lifetime boundary; configure an unpackaged distribution.');
  requireThat(typeof observed['version'] === 'string' && /^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(observed['version']) &&
    typeof observed['executable'] === 'string' && resolve(observed['executable']).toLowerCase() === resolve(executable).toLowerCase(),
  'native_shell_mismatch', 'The launched shell does not match its configured identity.');
  const status: Agent.WindowsShellStatus = { executable, executableHash: settings.executableHash, version: observed['version'], packageIdentity: 'unpackaged' };
  validateAgent('WindowsShellStatus', status); return status;
}

export function windowsShellEnvironment(shell: Agent.WindowsShellStatus): Record<string, string> {
  const inherited = Object.entries(process.env).find(([key]) => key.toLowerCase() === 'path')?.[1] ?? '';
  return { PATH: dirname(shell.executable) + (inherited ? ';' + inherited : '') };
}
