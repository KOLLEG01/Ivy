import { homedir } from 'node:os';
import { posix, win32 } from 'node:path';
import { requireThat } from '../../contracts/src/errors.js';

export interface CodexHomeSettings { codexHome?: string; nativeHome?: string }
export interface AppServerSettings extends CodexHomeSettings {
  appServer?: { mode: 'owned-stdio' } | { mode: 'external-proxy'; socketPath?: string; expectedCodexHome?: string } |
    { mode: 'claude-adapter' };
}
export const usesExternalAppServer = (settings: AppServerSettings): boolean =>
  settings.appServer?.mode !== 'owned-stdio' && settings.appServer?.mode !== 'claude-adapter';
export function expectedServerHome(settings: AppServerSettings): string {
  return settings.appServer?.mode === 'external-proxy' && settings.appServer.expectedCodexHome !== undefined
    ? settings.appServer.expectedCodexHome : resolveCodexHome(settings).path;
}
export interface CodexHomeResolution {
  path: string;
  source: 'host-configuration' | 'existing-native-home' | 'environment' | 'user-home';
}

/** Resolve for the executing account without mutating its environment or moving any data. */
export function resolveCodexHome(settings: CodexHomeSettings, options: {
  platform?: NodeJS.Platform; environment?: NodeJS.ProcessEnv; userHome?: string; cwd?: string;
} = {}): CodexHomeResolution {
  const platform = options.platform ?? process.platform, environment = options.environment ?? process.env;
  const paths = platform === 'win32' ? win32 : posix;
  const identity = (path: string) => platform === 'win32' ? paths.normalize(path).toLowerCase() : paths.normalize(path);
  requireThat(!(settings.codexHome && settings.nativeHome) || identity(settings.codexHome) === identity(settings.nativeHome),
    'invalid_arguments', 'codexHome and the existing explicit nativeHome must not select different homes.');
  const configured = settings.codexHome ?? settings.nativeHome;
  if (configured !== undefined) {
    requireThat(configured.trim().length > 0 && paths.isAbsolute(configured), 'invalid_arguments', 'Explicit Codex home must be an absolute path.');
    return { path: paths.normalize(configured), source: settings.codexHome !== undefined ? 'host-configuration' : 'existing-native-home' };
  }
  const inherited = environment['CODEX_HOME']?.trim();
  if (inherited) {
    return { path: paths.resolve(options.cwd ?? process.cwd(), inherited), source: 'environment' };
  }
  const userHome = options.userHome ?? environment[platform === 'win32' ? 'USERPROFILE' : 'HOME'] ?? homedir();
  requireThat(paths.isAbsolute(userHome), 'invalid_arguments', 'The executing account must have an absolute home directory.');
  return { path: paths.join(userHome, '.codex'), source: 'user-home' };
}

/** Ownership comes only from an explicit path within this instance, never from a default. */
export function instanceOwnedCodexHome(settings: CodexHomeSettings, dataRoot: string,
  platform: NodeJS.Platform = process.platform): string | null {
  if (settings.codexHome === undefined && settings.nativeHome === undefined) return null;
  const paths = platform === 'win32' ? win32 : posix;
  const home = resolveCodexHome(settings, { platform }).path, local = paths.relative(dataRoot, home);
  return local && local !== '..' && !local.startsWith('..' + paths.sep) && !paths.isAbsolute(local) ? home : null;
}
