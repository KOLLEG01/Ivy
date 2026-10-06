import { execFile } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { open, readFile, lstat, unlink, rename } from 'node:fs/promises';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { hashJson } from '../../contracts/src/canonical.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import type { Host } from '../../contracts/src/generated.js';
import { runtimeEnvironment } from './process.js';
import { installationIdentity, validateLinuxResourceScope } from './linux-resources.js';
import { linuxUnit } from './bootstrap.js';
import { linuxProcessUser } from './linux-service-identity.js';

const exec = promisify(execFile);
const unitRoot = '/etc/systemd/system';
const boundedOutput = 65_536;

export type SystemdState = {
  loadState: string;
  activeState: string;
  subState: string;
  mainPid: number | null;
};

export type SystemdCommand = (config: Host.HostConfig, args: readonly string[]) => Promise<string>;

export interface SystemdAdapter {
  inspectUnit(config: Host.HostConfig, unit: string): Promise<SystemdState>;
  readUnit(config: Host.HostConfig, unit: string): Promise<string | null>;
  stopUnit(config: Host.HostConfig, unit: string, timeoutMs: number): Promise<SystemdState>;
  replaceUnit(config: Host.HostConfig, unit: string, content: string): Promise<void>;
  reload(config: Host.HostConfig): Promise<void>;
  startUnit(config: Host.HostConfig, unit: string): Promise<SystemdState>;
}

const requireUnit = (unit: string): string => {
  requireThat(/^ivy-next-[0-9a-f]{12}-[0-9a-f]{12}\.service$/.test(unit), 'target_conflict', 'Systemd unit is outside the Ivy installation.');
  return unit;
};

export function systemdUnitName(config: Host.HostConfig, instanceId: string): string {
  const instance = config.instances.find(value => value.instanceId === instanceId);
  requireThat(instance?.engine === 'process', 'unsupported_runtime', 'Only process instances have systemd units.');
  return requireUnit(installationIdentity(config) + '-' + hashJson(instanceId).slice(7, 19) + '.service');
}

function parseProperties(output: string): Record<string, string> {
  const values: Record<string, string> = {};
  for (const line of output.trim().split(/\r?\n/)) {
    const index = line.indexOf('=');
    if (index > 0) values[line.slice(0, index)] = line.slice(index + 1);
  }
  return values;
}

export function parseSystemdState(output: string): SystemdState {
  const values = parseProperties(output), pid = Number(values['MainPID'] ?? 0);
  return { loadState: values['LoadState'] ?? 'unknown', activeState: values['ActiveState'] ?? 'unknown',
    subState: values['SubState'] ?? 'unknown', mainPid: Number.isSafeInteger(pid) && pid > 0 ? pid : null };
}

const defaultCommand: SystemdCommand = async (config, args) => {
  requireThat(process.platform === 'linux', 'unsupported_runtime', 'Systemd is only available on Linux.');
  const executable = config.executables['systemctl'] ?? '/usr/bin/systemctl';
  requireThat(executable.startsWith('/') && !/[\x00-\x1f]/.test(executable), 'invalid_arguments', 'Systemd executable must be an absolute path.');
  try {
    const result = await exec(executable, [...args], { timeout: 30_000, maxBuffer: boundedOutput, env: runtimeEnvironment() });
    return result.stdout;
  } catch (error) {
    const failure = error as NodeJS.ErrnoException & { stdout?: string; stderr?: string; code?: string | number };
    const detail = typeof failure.stderr === 'string' ? failure.stderr.slice(-1024) : '';
    throw new IvyError('systemd_failed', 'Systemd command failed.' + (detail ? ' ' + detail : ''), 'unknown', { command: args[0] ?? 'systemctl', exitCode: failure.code ?? null });
  }
};

const state = async (command: SystemdCommand, config: Host.HostConfig, unit: string): Promise<SystemdState> => {
  const output = await command(config, ['show', '--no-pager', '--property=LoadState,ActiveState,SubState,MainPID', requireUnit(unit)]);
  const value = parseSystemdState(output);
  requireThat(value.loadState === 'loaded' || value.loadState === 'not-found', 'target_conflict', 'Systemd reported an unexpected unit load state.');
  return value;
};

async function readDefinition(unit: string): Promise<string | null> {
  const path = join(unitRoot, requireUnit(unit));
  try {
    const metadata = await lstat(path);
    requireThat(metadata.isFile() && !metadata.isSymbolicLink() && metadata.uid === 0 && !(metadata.mode & 0o022) && metadata.size <= boundedOutput,
      'target_conflict', 'Systemd unit must be a bounded administrator-owned regular file.');
    return await readFile(path, 'utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw error;
  }
}

async function writeDefinition(unit: string, content: string): Promise<void> {
  requireUnit(unit);
  requireThat(content.length > 0 && Buffer.byteLength(content) <= boundedOutput && !/[\x00]/.test(content), 'invalid_arguments', 'Systemd unit content is invalid or unbounded.');
  requireThat(process.platform === 'linux' && process.getuid?.() === 0, 'unsupported_runtime', 'Systemd unit replacement needs the systemd administrator.');
  const path = join(unitRoot, unit), temporary = path + '.' + randomUUID() + '.tmp';
  const file = await open(temporary, 'wx', 0o644);
  try { await file.writeFile(content, 'utf8'); await file.sync(); }
  finally { await file.close(); }
  try {
    await rename(temporary, path);
    const directory = await open(unitRoot, 'r');
    try { await directory.sync(); } finally { await directory.close(); }
  } catch (error) { await unlink(temporary).catch(() => undefined); throw error; }
}

export function createSystemdAdapter(command: SystemdCommand = defaultCommand): SystemdAdapter {
  return {
    inspectUnit: (config, unit) => state(command, config, unit),
    readUnit: async (_config, unit) => readDefinition(unit),
    stopUnit: async (config, unit, timeoutMs) => {
      requireThat(Number.isSafeInteger(timeoutMs) && timeoutMs >= 0 && timeoutMs <= 120_000, 'invalid_arguments', 'Systemd stop timeout is invalid.');
      const name = requireUnit(unit), current = await state(command, config, name);
      if (['inactive', 'failed'].includes(current.activeState)) return current;
      await command(config, ['stop', name]);
      const deadline = Date.now() + timeoutMs;
      let observed = await state(command, config, name);
      while (!['inactive', 'failed'].includes(observed.activeState) && Date.now() < deadline) {
        await new Promise(resolve => setTimeout(resolve, 100));
        observed = await state(command, config, name);
      }
      if (!['inactive', 'failed'].includes(observed.activeState)) throw new IvyError('outcome_unknown', 'Systemd did not establish a stopped unit outcome.', 'unknown');
      return observed;
    },
    replaceUnit: async (_config, unit, content) => writeDefinition(requireUnit(unit), content),
    reload: async (config) => { await command(config, ['daemon-reload']); },
    startUnit: async (config, unit) => { const name = requireUnit(unit); await command(config, ['start', name]); return state(command, config, name); },
  };
}

export const defaultSystemdAdapter = createSystemdAdapter();

export function renderSystemdUnit(config: Host.HostConfig, instanceId: string, target: Host.RuntimeTarget, candidate: Host.Candidate,
  manifest: Host.ReleaseManifest, resources?: Host.LinuxResourceScope): { unit: string; content: string } {
  requireThat(process.platform === 'linux' || candidate.platform.os === 'linux', 'unsupported_runtime', 'Systemd units require a Linux candidate.');
  const instance = config.instances.find(value => value.instanceId === instanceId);
  requireThat(instance?.engine === 'process', 'unsupported_runtime', 'Only process instances can render systemd units.');
  requireThat(candidate.platform.os === 'linux' && candidate.componentId === instance.componentId && target.instanceId === instanceId && target.candidateId === candidate.candidateId,
    'target_conflict', 'Systemd unit identity does not match the accepted target.');
  requireThat(manifest.componentId === instance.componentId && manifest.entrypoint, 'target_conflict', 'Systemd unit has no matching component entrypoint.');
  if (resources) validateLinuxResourceScope(resources, installationIdentity(config));
  const name = systemdUnitName(config, instanceId);
  const user = linuxProcessUser(instance.componentId, 'linux');
  const processEntry: Host.BootstrapPlan['processes'][number] = { instanceId, componentId: instance.componentId, name: name.slice(0, -'.service'.length),
    ...(user ? { user } : {}),
    candidateId: candidate.candidateId, artifactRoot: candidate.artifactRoot, entrypoint: manifest.entrypoint, configPath: target.configPath,
    ...(manifest.restart ? { restart: manifest.restart } : {}) };
  const plan: Host.BootstrapPlan = { schemaVersion: 1, os: 'linux', hostId: config.hostId, installationId: installationIdentity(config),
    candidateId: candidate.candidateId, artifactRoot: candidate.artifactRoot, configPath: target.configPath, configHash: hashJson(config), runtimeRoot: config.runtimeRoot,
    nodeExecutable: config.executables['node']!, processes: [processEntry], ...(resources ? { linuxResources: resources } : {}) };
  const content = linuxUnit(plan, processEntry, target.revision);
  return { unit: name, content };
}
