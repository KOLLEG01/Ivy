import { execFile } from 'node:child_process';
import { dirname } from 'node:path';
import { promisify } from 'node:util';
import { lstat } from 'node:fs/promises';
import { requireThat } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';
import { runtimeEnvironment } from './process.js';

const exec = promisify(execFile);
export const linuxAgentUser = 'ivy-agent';

export function linuxProcessUser(componentId: string, platform: NodeJS.Platform | Host.BootstrapPlan['os']): string | undefined {
  return platform === 'linux' && componentId === 'agent-manager' ? linuxAgentUser : undefined;
}

type ProcessIdentity = { componentId: string; user?: string; configPath: string; artifactRoot: string; nodeExecutable: string };

async function present(path: string): Promise<boolean> {
  try { await lstat(path); return true; }
  catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false; throw error; }
}

async function grantParents(user: string, path: string): Promise<void> {
  for (let current = dirname(path); current !== dirname(current); current = dirname(current))
    await exec('/usr/bin/setfacl', ['-m', `u:${user}:--x`, current], { timeout: 60_000, maxBuffer: 65_536, env: runtimeEnvironment() });
}

async function grantTree(user: string, path: string, writable: boolean): Promise<void> {
  if (!await present(path)) return;
  await grantParents(user, path);
  await exec('/usr/bin/setfacl', ['-R', '-m', `u:${user}:${writable ? 'rwX' : 'r-X'}`, path],
    { timeout: 60_000, maxBuffer: 65_536, env: runtimeEnvironment() });
  if (writable) await exec('/usr/bin/setfacl', ['-R', '-m', `d:u:${user}:rwx`, path],
    { timeout: 60_000, maxBuffer: 65_536, env: runtimeEnvironment() });
}

async function ownTree(user: string, path: string, uid: number, gid: number): Promise<void> {
  if (!await present(path)) return;
  const metadata = await lstat(path);
  if (metadata.uid === uid && metadata.gid === gid) return;
  await exec('/usr/bin/chown', ['-R', `${user}:${user}`, path], { timeout: 60_000, maxBuffer: 65_536, env: runtimeEnvironment() });
}

async function grantFile(user: string, path: string, permissions: 'r--' | 'r-x'): Promise<void> {
  requireThat(await present(path), 'not_found', 'A Linux service identity input file is missing.');
  await grantParents(user, path);
  await exec('/usr/bin/setfacl', ['-m', `u:${user}:${permissions}`, path], { timeout: 60_000, maxBuffer: 65_536, env: runtimeEnvironment() });
}

/** Keep model-reachable native execution on its own non-administrative OS identity. */
export async function ensureLinuxProcessIdentity(processIdentity: ProcessIdentity, runtime: Host.InstanceConfig): Promise<void> {
  if (!processIdentity.user) return;
  requireThat(process.platform === 'linux' && process.getuid?.() === 0 && processIdentity.componentId === 'agent-manager' && processIdentity.user === linuxAgentUser,
    'unsupported_runtime', 'Linux service identity setup requires the system administrator and the configured AgentManager identity.');
  let uid: number;
  try { uid = Number((await exec('/usr/bin/id', ['-u', processIdentity.user], { timeout: 10_000, maxBuffer: 65_536, env: runtimeEnvironment() })).stdout.trim()); }
  catch {
    await exec('/usr/sbin/useradd', ['--system', '--user-group', '--create-home', '--home-dir', '/var/lib/' + processIdentity.user, '--shell', '/usr/sbin/nologin', processIdentity.user],
      { timeout: 30_000, maxBuffer: 65_536, env: runtimeEnvironment() });
    uid = Number((await exec('/usr/bin/id', ['-u', processIdentity.user], { timeout: 10_000, maxBuffer: 65_536, env: runtimeEnvironment() })).stdout.trim());
  }
  const gid = Number((await exec('/usr/bin/id', ['-g', processIdentity.user], { timeout: 10_000, maxBuffer: 65_536, env: runtimeEnvironment() })).stdout.trim());
  requireThat(Number.isSafeInteger(uid) && uid > 0 && Number.isSafeInteger(gid) && gid > 0, 'access_denied', 'AgentManager must use a non-administrative Linux user.');

  await grantFile(processIdentity.user, processIdentity.configPath, 'r--');
  await grantFile(processIdentity.user, processIdentity.nodeExecutable, 'r-x');
  const nativeExecutable = runtime.settings['nativeExecutable'];
  if (typeof nativeExecutable === 'string') await grantFile(processIdentity.user, nativeExecutable, 'r-x');
  await grantTree(processIdentity.user, processIdentity.artifactRoot, false);

  const privateRoots = [runtime.dataRoot, runtime.workRoot, runtime.logsRoot].filter((value): value is string => typeof value === 'string');
  for (const path of new Set(privateRoots))
    await ownTree(processIdentity.user, path, uid, gid);

  const projects = runtime.settings['projects'];
  const writable = [runtime.dataRoot, runtime.workRoot, runtime.logsRoot, runtime.settings['projectRoot'], runtime.settings['internalProjectRoot'],
    ...(Array.isArray(projects) ? projects.flatMap(value => value && typeof value === 'object' && !Array.isArray(value) && typeof value['path'] === 'string' ? [value['path']] : []) : [])]
    .filter((value): value is string => typeof value === 'string');
  for (const path of new Set(writable)) await grantTree(processIdentity.user, path, true);
}
