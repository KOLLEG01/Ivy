import type { Agent } from '../../sdk/src/node.js';
/** Store/AppX shells may escape the native Job; the owning host supplies a pinned portable pwsh. */
export declare function verifyWindowsShell(settings: Agent.WindowsShell | undefined, cwd: string, jobLauncher: string): Promise<Agent.WindowsShellStatus>;
export declare function windowsShellEnvironment(shell: Agent.WindowsShellStatus): Record<string, string>;
