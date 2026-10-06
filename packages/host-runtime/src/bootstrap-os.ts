import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { lstat, open, readFile, rename, unlink } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { linuxUnit } from './bootstrap.js';
import { ensureLinuxSlice, inspectLinuxSlice } from './linux-resources.js';
import { runtimeEnvironment } from './process.js';
import { digest, hashJson } from '../../contracts/src/canonical.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';
import { instanceConfig } from './config.js';
import { ensureLinuxProcessIdentity } from './linux-service-identity.js';

export interface BootstrapDefinition { content: string; hash: string }
export type BootstrapOwner = Host.BootstrapSnapshot['owners'][number] & { definition: BootstrapDefinition };
/** The real implementation uses only verified task names or exact systemd unit paths. */
export interface BootstrapOs {
  inspect(plans: Host.BootstrapPlan[]): Promise<BootstrapOwner[]>;
  render(owner: BootstrapOwner, next: Host.BootstrapPlan): Promise<BootstrapDefinition>;
  check(definition: BootstrapDefinition): Promise<void>;
  apply(owner: BootstrapOwner, desired: BootstrapDefinition, plans: Host.BootstrapPlan[]): Promise<void>;
  reload(): Promise<void>;
  prepare?(next: Host.BootstrapPlan): Promise<void>;
  pause?(snapshot: Host.BootstrapSnapshot, plans: Host.BootstrapPlan[]): Promise<void>;
  resume?(snapshot: Host.BootstrapSnapshot, plans: Host.BootstrapPlan[]): Promise<void>;
  launchAutomatic?(plan: Host.BootstrapPlan, requestPath: string, operationId: string): Promise<void>;
}
const exec = promisify(execFile);
const boundedExec = async (file: string, args: string[]) => {
  try { return await exec(file, args, { timeout: 60_000, maxBuffer: 2 * 1024 * 1024, windowsHide: true, env: runtimeEnvironment() }); }
  catch { throw new IvyError('bootstrap_os_unavailable', 'The bounded OS bootstrap operation did not complete. Inspect the retained maintenance record.', 'unknown'); }
};
const unitPath = (name: string) => {
  requireThat(/^ivy-next-[0-9a-f]{12}-[0-9a-f]{12}$/.test(name), 'target_conflict', 'Invalid owned bootstrap unit name.');
  return join('/etc/systemd/system', name + '.service');
};
async function systemdOwner(instance: Host.BootstrapPlan['processes'][number], plans: Host.BootstrapPlan[]): Promise<BootstrapOwner> {
  const path = unitPath(instance.name), metadata = await lstat(path);
  requireThat(metadata.isFile() && !metadata.isSymbolicLink() && metadata.uid === 0 && !(metadata.mode & 0o022) && metadata.size <= 65536,
    'target_conflict', 'Bootstrap unit must be a bounded administrator-owned regular file.');
  const bytes = await readFile(path), content = bytes.toString('utf8');
  requireThat(Buffer.from(content, 'utf8').equals(bytes), 'target_conflict', 'Bootstrap unit must have exact UTF-8 bytes.');
  const candidates = [...new Map(plans.map(plan => [hashJson(plan), plan])).values()].flatMap(plan => {
    const process = plan.processes.find(value => value.instanceId === instance.instanceId);
    return process && linuxUnit(plan, process) === content ? [{ plan, process }] : [];
  });
  requireThat(candidates.length === 1, 'target_conflict', 'OS unit does not match one retained immutable bootstrap candidate.');
  const matched = candidates[0]!;
  if (matched.plan.linuxResources) await inspectLinuxSlice(matched.plan.linuxResources);
  const result = await boundedExec('/usr/bin/systemctl', ['show', instance.name + '.service', '--property=ActiveState,SubState,UnitFileState,LoadState,FragmentPath,DropInPaths,Transient']);
  const state = Object.fromEntries(result.stdout.trim().split('\n').map(line => { const index = line.indexOf('='); return [line.slice(0, index), line.slice(index + 1)]; }));
  requireThat(state['FragmentPath'] === path && state['DropInPaths'] === '' && state['LoadState'] === 'loaded' && state['Transient'] === 'no',
    'target_conflict', 'Loaded unit has unexpected ownership, overrides or load state.');
  requireThat(['enabled', 'disabled'].includes(state['UnitFileState'] ?? ''), 'target_conflict', 'Bootstrap unit has an unsupported scheduling state.');
  return { ...matched.process, candidateId: matched.process.candidateId ?? matched.plan.candidateId, definitionHash: digest(content), enabled: state['UnitFileState'] === 'enabled',
    running: !(['inactive', 'failed'].includes(state['ActiveState'] ?? '') && ['dead', 'failed'].includes(state['SubState'] ?? '')),
    definition: { content, hash: digest(content) } };
}
class SystemdBootstrap implements BootstrapOs {
  async prepare(next: Host.BootstrapPlan): Promise<void> {
    for (const process of next.processes.filter(value => value.user && value.configPath)) await ensureLinuxProcessIdentity({ componentId: process.componentId,
      user: process.user!, configPath: process.configPath!, artifactRoot: process.artifactRoot ?? next.artifactRoot, nodeExecutable: next.nodeExecutable }, await instanceConfig(process.configPath!));
    if (next.linuxResources) { await ensureLinuxSlice(next.linuxResources); await this.reload(); await inspectLinuxSlice(next.linuxResources); }
  }
  async inspect(plans: Host.BootstrapPlan[]): Promise<BootstrapOwner[]> {
    requireThat(plans.length > 0, 'not_found', 'No retained bootstrap candidate.');
    return Promise.all(plans[0]!.processes.map(instance => systemdOwner(instance, plans)));
  }
  async render(owner: BootstrapOwner, next: Host.BootstrapPlan): Promise<BootstrapDefinition> {
    const process = next.processes.find(value => value.instanceId === owner.instanceId);
    requireThat(process, 'target_conflict', 'Next bootstrap plan does not contain the owned process.');
    const content = linuxUnit(next, process); return { content, hash: digest(content) };
  }
  async check(definition: BootstrapDefinition): Promise<void> { requireThat(digest(definition.content) === definition.hash, 'storage_invalid', 'Retained bootstrap definition changed.'); }
  async apply(owner: BootstrapOwner, desired: BootstrapDefinition, plans: Host.BootstrapPlan[]): Promise<void> {
    requireThat(process.getuid?.() === 0, 'unsupported_runtime', 'Bootstrap maintenance needs the systemd administrator.');
    await this.check(desired);
    const current = await systemdOwner(owner, plans);
    requireThat(!current.running && [owner.definitionHash, desired.hash].includes(current.definitionHash), 'target_conflict', 'Bootstrap unit changed or started during maintenance.');
    if (current.definitionHash === desired.hash) return;
    const path = unitPath(owner.name), temporary = path + '.' + randomUUID() + '.tmp', file = await open(temporary, 'wx', 0o644);
    try { await file.writeFile(desired.content); await file.sync(); } finally { await file.close(); }
    try {
      // There is no multi-unit systemd CAS. Recheck immediately before this atomic file replacement.
      const rechecked = await systemdOwner(owner, plans);
      requireThat(!rechecked.running && rechecked.definitionHash === current.definitionHash, 'target_conflict', 'Bootstrap unit changed before publication.');
      await rename(temporary, path);
      const directory = await open(dirname(path), 'r'); try { await directory.sync(); } finally { await directory.close(); }
    } finally { await unlink(temporary).catch(error => { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }); }
  }
  async reload(): Promise<void> { await boundedExec('/usr/bin/systemctl', ['daemon-reload']); }
  private async exact(snapshot: Host.BootstrapSnapshot, plans: Host.BootstrapPlan[]): Promise<BootstrapOwner[]> {
    requireThat(snapshot.hostId === plans[0]?.hostId && snapshot.installationId === plans[0]?.installationId &&
      snapshot.owners.length === plans[0]?.processes.length, 'target_conflict', 'Bootstrap snapshot belongs to another installation.');
    const owners = await this.inspect(plans);
    requireThat(owners.every(owner => {
      const expected = snapshot.owners.find(value => value.instanceId === owner.instanceId && value.name === owner.name);
      return expected && expected.candidateId === owner.candidateId && expected.definitionHash === owner.definitionHash;
    }), 'target_conflict', 'Bootstrap owners changed after their exact snapshot.');
    return owners;
  }
  async pause(snapshot: Host.BootstrapSnapshot, plans: Host.BootstrapPlan[]): Promise<void> {
    const owners = await this.exact(snapshot, plans);
    for (const owner of [...owners].sort((left, right) => Number(left.componentId === 'host-executor') - Number(right.componentId === 'host-executor')))
      if (owner.running) await boundedExec('/usr/bin/systemctl', ['stop', owner.name + '.service']);
    requireThat((await this.inspect(plans)).every(owner => !owner.running), 'bootstrap_busy', 'Bootstrap owners did not stop for maintenance.');
  }
  async resume(snapshot: Host.BootstrapSnapshot, plans: Host.BootstrapPlan[]): Promise<void> {
    const owners = await this.inspect(plans);
    requireThat(owners.length === snapshot.owners.length && owners.every(owner => {
      const state = snapshot.owners.find(value => value.instanceId === owner.instanceId && value.name === owner.name);
      return state && state.enabled === owner.enabled;
    }), 'target_conflict', 'Bootstrap scheduling state changed during maintenance.');
    for (const state of snapshot.owners) if (state.running) await boundedExec('/usr/bin/systemctl', ['start', state.name + '.service']);
  }
  async launchAutomatic(plan: Host.BootstrapPlan, requestPath: string, operationId: string): Promise<void> {
    requireThat(process.getuid?.() === 0, 'unsupported_runtime', 'Automatic bootstrap maintenance needs the systemd administrator.');
    const unit = plan.installationId + '-maintenance-' + digest(operationId).slice(7, 19);
    const worker = join(plan.artifactRoot, 'dist/packages/host-runtime/src/bootstrap-auto-worker.js');
    const state = await exec('/usr/bin/systemctl', ['show', unit + '.service', '--property=LoadState,ActiveState'],
      { timeout: 10_000, maxBuffer: 65536, env: runtimeEnvironment() }).catch(() => ({ stdout: '' }));
    const values = Object.fromEntries(state.stdout.trim().split('\n').filter(Boolean).map(line => { const index = line.indexOf('='); return [line.slice(0, index), line.slice(index + 1)]; }));
    if (values['LoadState'] === 'loaded' && ['active', 'activating'].includes(values['ActiveState'] ?? '')) return;
    await boundedExec('/usr/bin/systemd-run', ['--unit', unit, '--collect', '--property=Type=exec', '--property=RuntimeMaxSec=15min', '--',
      plan.nodeExecutable, worker, '--config', plan.configPath, '--request', requestPath, '--distribution', plan.artifactRoot]);
  }
}
class WindowsBootstrap implements BootstrapOs {
  constructor(private readonly distribution: string) {}
  private async call<T>(action: string, data: unknown): Promise<T> {
    const file = join(process.env['SystemRoot'] ?? 'C:\\Windows', 'System32/WindowsPowerShell/v1.0/powershell.exe');
    return new Promise((resolve, reject) => {
      const child = execFile(file, ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', join(this.distribution, 'dist/packages/host-runtime/assets/bootstrap-maintenance-windows.ps1')],
        { timeout: 60_000, maxBuffer: 2 * 1024 * 1024, windowsHide: true, env: runtimeEnvironment(), encoding: 'utf8' }, (error, stdout) => {
          if (error) { reject(new IvyError('bootstrap_os_unavailable', 'The bounded scheduler operation did not complete. Inspect the retained maintenance record.', 'unknown')); return; }
          try {
            const result = JSON.parse(stdout) as { ok: boolean; value: T; code: string; message: string };
            if (!result.ok) reject(new IvyError(result.code, result.message)); else resolve(result.value);
          } catch { reject(new IvyError('bootstrap_os_unavailable', 'Scheduler returned an invalid result.', 'unknown')); }
        });
      child.stdin?.on('error', () => undefined); child.stdin?.end(JSON.stringify({ action, data }));
    });
  }
  inspect(plans: Host.BootstrapPlan[]): Promise<BootstrapOwner[]> { return this.call('inspect', { plans }); }
  render(owner: BootstrapOwner, next: Host.BootstrapPlan): Promise<BootstrapDefinition> { return this.call('render', { owner, next }); }
  async check(definition: BootstrapDefinition): Promise<void> { await this.call('check', { definition }); }
  async apply(owner: BootstrapOwner, desired: BootstrapDefinition, plans: Host.BootstrapPlan[]): Promise<void> { await this.call('apply', { owner, desired, plans }); }
  async reload(): Promise<void> {}
  async pause(snapshot: Host.BootstrapSnapshot, plans: Host.BootstrapPlan[]): Promise<void> { await this.call('pause', { snapshot, plans }); }
  async resume(snapshot: Host.BootstrapSnapshot, plans: Host.BootstrapPlan[]): Promise<void> { await this.call('resume', { snapshot, plans }); }
  async launchAutomatic(plan: Host.BootstrapPlan, requestPath: string, operationId: string): Promise<void> {
    await this.call('launch', { plan, requestPath, operationId });
  }
}
export function bootstrapOs(distribution: string): BootstrapOs {
  requireThat(['win32', 'linux'].includes(process.platform), 'unsupported_runtime', 'Bootstrap maintenance supports Windows and systemd Linux.');
  return process.platform === 'win32' ? new WindowsBootstrap(distribution) : new SystemdBootstrap();
}
