import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { lstat, readFile, open } from 'node:fs/promises';
import { hashJson } from '../../contracts/src/canonical.js';
import { requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import { runtimeEnvironment } from './process.js';
import type { Host } from '../../contracts/src/generated.js';

const exec = promisify(execFile);
export const installationIdentity = (config: Host.HostConfig): string => 'ivy-next-' + hashJson(config.restoredFrom
  ? { hostId: config.hostId, runtimeRoot: config.runtimeRoot, backupId: config.restoredFrom.backupId } : config.hostId).slice(7, 19);
export function linuxResourceScope(installationId: string, budget: Host.LinuxResourceBudget): Host.LinuxResourceScope {
  validateHost('LinuxResourceBudget', budget);
  requireThat(/^ivy-next-[a-f0-9]{12}$/.test(installationId) && budget.memoryHighBytes < budget.memoryMaxBytes,
    'invalid_arguments', 'Linux resources require an owned installation and memory high below memory max.');
  return { slice: 'ivynext' + installationId.slice(9) + hashJson(budget).slice(7, 19) + '.slice', budget: structuredClone(budget) };
}
export function validateLinuxResourceScope(scope: Host.LinuxResourceScope, installationId?: string): void {
  validateHost('LinuxResourceScope', scope);
  const identity = installationId ?? 'ivy-next-' + scope.slice.slice(7, 19);
  requireThat(hashJson(scope) === hashJson(linuxResourceScope(identity, scope.budget)), 'target_conflict', 'Linux resource scope differs from its installation or budget.');
}
export function linuxSlice(scope: Host.LinuxResourceScope): string {
  validateLinuxResourceScope(scope);
  return `[Unit]\nDescription=IvyNext resource boundary ${scope.slice}\n\n[Slice]\nMemoryAccounting=true\nMemoryHigh=${scope.budget.memoryHighBytes}\nMemoryMax=${scope.budget.memoryMaxBytes}\nMemorySwapMax=0\nTasksAccounting=true\nTasksMax=${scope.budget.tasksMax}\n`;
}
const slicePath = (scope: Host.LinuxResourceScope) => { validateLinuxResourceScope(scope); return '/etc/systemd/system/' + scope.slice; };
async function verifySliceFile(scope: Host.LinuxResourceScope): Promise<void> {
  const path = slicePath(scope), metadata = await lstat(path);
  requireThat(metadata.isFile() && !metadata.isSymbolicLink() && metadata.uid === 0 && !(metadata.mode & 0o022) && metadata.size <= 4096,
    'target_conflict', 'Linux resource definition must be a bounded administrator-owned regular file.');
  requireThat((await readFile(path)).equals(Buffer.from(linuxSlice(scope))), 'target_conflict', 'Linux resource definition differs from its immutable budget.');
}
export async function ensureLinuxSlice(scope: Host.LinuxResourceScope): Promise<void> {
  requireThat(process.platform === 'linux' && process.getuid?.() === 0, 'unsupported_runtime', 'Linux resource installation needs the systemd administrator.');
  const path = slicePath(scope);
  try { await lstat(path); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    try {
      const file = await open(path, 'wx', 0o644);
      try { await file.writeFile(linuxSlice(scope)); await file.sync(); } finally { await file.close(); }
      const directory = await open('/etc/systemd/system', 'r'); try { await directory.sync(); } finally { await directory.close(); }
    }
    catch (writeError) { if ((writeError as NodeJS.ErrnoException).code !== 'EEXIST') throw writeError; }
  }
  await verifySliceFile(scope);
}
export async function inspectLinuxSlice(scope: Host.LinuxResourceScope): Promise<void> {
  await verifySliceFile(scope);
  const { stdout } = await exec('/usr/bin/systemctl', ['show', scope.slice,
    '--property=FragmentPath,DropInPaths,Transient,LoadState,MemoryHigh,MemoryMax,MemorySwapMax,TasksMax'],
    { timeout: 10_000, maxBuffer: 16_384, env: runtimeEnvironment() });
  const values = Object.fromEntries(stdout.trim().split('\n').map(line => { const i = line.indexOf('='); return [line.slice(0, i), line.slice(i + 1)]; }));
  requireThat(values['FragmentPath'] === slicePath(scope) && values['DropInPaths'] === '' && values['Transient'] === 'no' && values['LoadState'] === 'loaded' &&
    values['MemoryHigh'] === String(scope.budget.memoryHighBytes) && values['MemoryMax'] === String(scope.budget.memoryMaxBytes) &&
    values['MemorySwapMax'] === '0' && values['TasksMax'] === String(scope.budget.tasksMax),
    'target_conflict', 'Loaded Linux resource definition has different ownership, overrides or limits.');
}
export function assertLinuxResourceMembership(scope: Host.LinuxResourceScope, unit: string, membership: string, values: Record<string, string>): void {
  validateLinuxResourceScope(scope);
  requireThat(/^ivy-next-[0-9a-f]{12}-[0-9a-f]{12}\.service$/.test(unit) && unit.startsWith('ivy-next-' + scope.slice.slice(7, 19) + '-') &&
    membership.trim() === '0::/' + scope.slice + '/' + unit && values['memory.high']?.trim() === String(scope.budget.memoryHighBytes) &&
    values['memory.max']?.trim() === String(scope.budget.memoryMaxBytes) && values['memory.swap.max']?.trim() === '0' && values['pids.max']?.trim() === String(scope.budget.tasksMax),
    'resource_scope_mismatch', 'Runtime owner is outside its expected cgroup-v2 resource boundary or the effective kernel limits changed.');
}
export async function verifyLinuxResourceMembership(config: Host.HostConfig, instanceId: string, scope: Host.LinuxResourceScope): Promise<void> {
  requireThat(process.platform === 'linux', 'unsupported_runtime', 'Linux resource scope cannot run on another platform.');
  const installationId = installationIdentity(config); validateLinuxResourceScope(scope, installationId);
  const unit = installationId + '-' + hashJson(instanceId).slice(7, 19) + '.service';
  const values = Object.fromEntries(await Promise.all(['memory.high', 'memory.max', 'memory.swap.max', 'pids.max'].map(async name =>
    [name, await readFile('/sys/fs/cgroup/' + scope.slice + '/' + name, 'utf8')])));
  assertLinuxResourceMembership(scope, unit, await readFile('/proc/self/cgroup', 'utf8'), values);
}
