import { mkdir } from 'node:fs/promises';
import { dirname, isAbsolute, resolve } from 'node:path';
import { isIP } from 'node:net';
import { setTimeout as delay } from 'node:timers/promises';
import { runCommand } from './process.js';
import { jsonFile } from './config.js';
import type { ProcessResult } from './process.js';
import { hashJson } from '../../contracts/src/canonical.js';
import { requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import type { Host } from '../../contracts/src/generated.js';
import { installationIdentity, validateLinuxResourceScope, verifyLinuxResourceMembership } from './linux-resources.js';

export interface ManagedProcess {
  completion: Promise<ProcessResult>;
  stop: (graceMs?: number) => Promise<ProcessResult>;
  verify?: () => Promise<void>;
  dispose?: () => void;
}

export type DockerCommand = (host: Host.HostConfig, args: string[], timeoutMs?: number, extraSecrets?: string[]) => Promise<ProcessResult>;

/**
 * The host executor uses this adapter as the one Docker ownership boundary.
 * Keeping the command boundary injectable makes the productive lifecycle
 * testable without weakening the production requirement for the local daemon.
 */
export interface DockerAdapter {
  inspectContainer: (host: Host.HostConfig, instanceId: string) => Promise<Host.ContainerState | null>;
  stopContainer: (host: Host.HostConfig, instanceId: string, graceMs: number, expectedId?: string) => Promise<Host.ContainerState | null>;
  verifyContainerImage: (host: Host.HostConfig, instance: Host.Instance, candidate: Host.Candidate, manifest: Host.ReleaseManifest) => Promise<void>;
  startContainer: (host: Host.HostConfig, instance: Host.Instance, target: Host.RuntimeTarget, config: Host.InstanceConfig,
    candidate: Host.Candidate, manifest: Host.ReleaseManifest, allowRestart: boolean, resources?: Host.LinuxResourceScope) => Promise<ManagedProcess>;
}

export const containerName = (hostId: string, instanceId: string, recovery?: { runtimeRoot: string; backupId: string }): string =>
  'ivy-next-' + hashJson({ hostId, instanceId, ...(recovery ? { recovery } : {}) }).slice(7, 31);

const ownedContainerName = (host: Host.HostConfig, instanceId: string) => containerName(host.hostId, instanceId,
  host.restoredFrom ? { runtimeRoot: host.runtimeRoot, backupId: host.restoredFrom.backupId } : undefined);

async function dockerCommand(host: Host.HostConfig, args: string[], timeoutMs = 10_000, extraSecrets: string[] = []): Promise<ProcessResult> {
  requireThat(process.platform === 'linux' && host.executables['docker'], 'unsupported_runtime', 'Docker ownership requires the configured Linux host daemon.');
  const redact = [...extraSecrets, ...host.instances.flatMap(value => [value.credential ?? '', ...(value.componentId === 'hive' ? (value.settings as unknown as Host.HiveSettings).credentials.map(value => value.token) : [])])];
  return runCommand({ executable: 'docker', args: ['--host', 'unix:///var/run/docker.sock', ...args], timeoutMs }, host.runtimeRoot, host.executables, { redact });
}

/** Docker restart policy is configured once at container creation; HostExecutor only adopts/observes it. */
export function dockerRestartPolicy(manifest: Host.ReleaseManifest): string {
  return manifest.restart?.policy ?? 'never';
}

export function createDockerAdapter(command: DockerCommand = dockerCommand): DockerAdapter {
  /** A successful exact-name query is the only negative existence observation. Daemon errors propagate. */
  const inspectContainer = async (host: Host.HostConfig, instanceId: string): Promise<Host.ContainerState | null> => {
    const name = ownedContainerName(host, instanceId);
    const listed = await command(host, ['container', 'ls', '--all', '--no-trunc', '--filter', 'name=^/' + name + '$', '--format', '{{.ID}}']);
    const ids = listed.stdout.trim().split(/\r?\n/).filter(Boolean);
    if (!ids.length) return null;
    requireThat(ids.length === 1 && /^[0-9a-f]{64}$/.test(ids[0]!), 'outcome_unknown', 'Owned Docker name has an ambiguous identity.');
    const inspected = await command(host, ['container', 'inspect', '--format', '{{json .}}', ids[0]!]);
    const raw = JSON.parse(inspected.stdout) as { Id: string; Name: string; Image: string; Config: { Labels: Record<string, string> }; HostConfig: { CgroupParent: string }; State: { Status: string; ExitCode: number; StartedAt: string } };
    const labels = raw.Config?.Labels;
    requireThat(labels && labels['dev.ivy.host'] === host.hostId && labels['dev.ivy.instance'] === instanceId && raw.Name === '/' + name, 'target_conflict', 'Existing Docker container is not owned by this Ivy instance.');
    const value: Host.ContainerState = { containerId: raw.Id, name, imageId: raw.Image, hostId: host.hostId, instanceId,
      candidateId: labels['dev.ivy.candidate']!, launchId: labels['dev.ivy.launch']!,
      state: raw.State.Status as Host.ContainerState['state'], exitCode: raw.State.ExitCode, startedAt: raw.State.StartedAt, cgroupParent: raw.HostConfig?.CgroupParent };
    validateHost('ContainerState', value);
    requireThat(value.containerId === ids[0], 'target_conflict', 'Docker identity changed during inspection.');
    return value;
  };

  const resultOf = async (host: Host.HostConfig, value: Host.ContainerState, configPath: string): Promise<ProcessResult> => {
    // Read reloadable credentials before collecting bounded container diagnostics.
    const config = await jsonFile<Host.InstanceConfig>(configPath).catch(() => null);
    const settings = config?.settings as unknown as Host.HiveSettings | undefined;
    const secrets = settings?.credentials?.map(value => value.token) ?? [];
    const logs = await command(host, ['container', 'logs', '--tail', '100', value.containerId], 5000, secrets).catch(() => null);
    return { exitCode: value.exitCode, signal: null, errorCode: null, stdout: logs?.stdout ?? '', stderr: logs?.stderr ?? '', truncated: logs?.truncated ?? false };
  };

  const stopContainer = async (host: Host.HostConfig, instanceId: string, graceMs: number, expectedId?: string): Promise<Host.ContainerState | null> => {
    const value = await inspectContainer(host, instanceId);
    if (!value) return null;
    requireThat(!expectedId || value.containerId === expectedId, 'outcome_unknown', 'The container to stop no longer has its observed identity.');
    if (['running', 'restarting'].includes(value.state)) {
      let failed: unknown;
      // The installed Docker CLI uses --time/-t; --timeout is not available there.
      try { await command(host, ['container', 'stop', '-t', String(Math.ceil(graceMs / 1000)), value.containerId], graceMs + 15_000); } catch (error) { failed = error; }
      const observed = await inspectContainer(host, instanceId);
      requireThat(observed && observed.containerId === value.containerId && ['exited', 'dead'].includes(observed.state), 'outcome_unknown', failed ? 'Docker stop result remains unknown after re-inspection.' : 'Docker did not establish the expected stopped container.');
      return observed;
    }
    requireThat(['created', 'exited', 'dead'].includes(value.state), 'outcome_unknown', 'Paused/removing container needs explicit owner reconciliation.');
    return value;
  };

  const verifyContainerImage = async (host: Host.HostConfig, instance: Host.Instance, candidate: Host.Candidate, manifest: Host.ReleaseManifest): Promise<void> => {
    requireThat(instance.componentId === 'hive' && instance.docker && candidate.dockerImage && manifest.entrypoint?.executable === 'node', 'unsupported_runtime', 'Docker activation requires a prepared Hive image with the pinned Node entry runtime.');
    requireThat(/^[a-z0-9]+(?:(?:[._-]|\/)[a-z0-9]+)*$/.test(instance.docker.imageRepository), 'invalid_arguments', 'Hive image repository must be an explicit local repository name without a tag.');
    for (const port of instance.docker.ports) requireThat(isIP(port.host) !== 0, 'invalid_arguments', 'Docker port bindings require explicit IP addresses.');
    const image = await command(host, ['image', 'inspect', '--format', '{{.Id}}', candidate.dockerImage]);
    requireThat(image.stdout.trim() === candidate.dockerImage, 'build_mismatch', 'Prepared immutable Docker image is unavailable.');
  };

  const startContainer = async (host: Host.HostConfig, instance: Host.Instance, target: Host.RuntimeTarget, config: Host.InstanceConfig,
    candidate: Host.Candidate, manifest: Host.ReleaseManifest, allowRestart: boolean, resources?: Host.LinuxResourceScope): Promise<ManagedProcess> => {
    requireThat(instance.componentId === 'hive' && instance.docker && candidate.dockerImage, 'unsupported_runtime', 'The Docker driver requires a prepared Hive image and explicit bindings.');
    requireThat(manifest.entrypoint?.executable === 'node', 'unsupported_runtime', 'This Hive image provides the pinned Node entry runtime.');
    const settings = config.settings as unknown as Host.HiveSettings;
    validateHost('HiveSettings', settings);
    if (resources) {
      validateLinuxResourceScope(resources, installationIdentity(host));
      await verifyLinuxResourceMembership(host, instance.instanceId, resources);
      const driver = await command(host, ['info', '--format', '{{.CgroupDriver}} {{.CgroupVersion}}']);
      requireThat(driver.stdout.trim() === 'systemd 2', 'unsupported_runtime', 'Shared Linux resources require Docker systemd cgroup v2.');
    }
    let current = await inspectContainer(host, instance.instanceId);
    const matches = (value: Host.ContainerState) => value.launchId === target.revision && value.candidateId === candidate.candidateId && value.imageId === candidate.dockerImage &&
      (!resources || value.cgroupParent === resources.slice);
    if (current && !matches(current)) {
      requireThat(['created', 'exited', 'dead'].includes(current.state), 'outcome_unknown', 'A different live launch must be explicitly stopped before replacement.');
      await command(host, ['container', 'rm', current.containerId]);
      current = null;
    }
    if (!current) {
      const name = ownedContainerName(host, instance.instanceId), mounts: string[] = [];
      const mount = (path: string, writable: boolean) => {
        requireThat(isAbsolute(path) && path !== '/' && !suspiciousPath(path), 'invalid_arguments', 'Docker mount needs an explicit unambiguous absolute path.');
        mounts.push('--mount', 'type=bind,src=' + path + ',dst=' + path + (writable ? '' : ',readonly'));
      };
      await mkdir(config.dataRoot, { recursive: true });
      await mkdir(settings.backup.directory, { recursive: true });
      mount(config.dataRoot, true); mount(settings.backup.directory, true); mount(config.artifactRoot, false); mount(dirname(target.configPath), false);
      for (const root of [config.workRoot, config.logsRoot]) if (root) { await mkdir(root, { recursive: true }); mount(root, true); }
      const ports = instance.docker.ports.flatMap(port => {
        requireThat(isIP(port.host) !== 0, 'invalid_arguments', 'Docker port bindings require an explicit IP address.');
        return ['--publish', (isIP(port.host) === 6 ? '[' + port.host + ']' : port.host) + ':' + port.port + ':' + port.containerPort];
      });
      const labels = { 'dev.ivy.host': host.hostId, 'dev.ivy.instance': instance.instanceId, 'dev.ivy.candidate': candidate.candidateId, 'dev.ivy.launch': target.revision };
      await command(host, ['image', 'tag', candidate.dockerImage, instance.docker.imageRepository + ':' + candidate.buildId.slice(7)]);
      let failure: unknown;
      try {
        await command(host, ['container', 'create', '--name', name, '--pull', 'never', '--restart', dockerRestartPolicy(manifest), '--read-only', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges=true',
          '--pids-limit', '128', '--memory', '768m', '--memory-swap', '768m', '--tmpfs', '/tmp:rw,nosuid,nodev,size=64m', '--log-opt', 'max-size=1m', '--log-opt', 'max-file=3',
          ...(resources ? ['--cgroup-parent', resources.slice] : []),
          ...Object.entries(labels).flatMap(([key, value]) => ['--label', key + '=' + value]), ...mounts, ...ports,
          '--env', 'IVY_INSTANCE_CONFIG=' + target.configPath, '--env', 'IVY_LAUNCH_ID=' + target.revision,
          '--workdir', config.workRoot ?? resolve(candidate.artifactRoot, manifest.entrypoint.cwd ?? '.'), '--entrypoint', '/usr/local/bin/node', candidate.dockerImage,
          ...(config.workRoot && manifest.entrypoint.args[0]?.startsWith('dist/') ? [resolve(candidate.artifactRoot, manifest.entrypoint.args[0]), ...manifest.entrypoint.args.slice(1)] : manifest.entrypoint.args)], 30_000);
      } catch (error) { failure = error; }
      current = await inspectContainer(host, instance.instanceId);
      requireThat(current && matches(current), 'outcome_unknown', failure ? 'Container creation needs reconciliation; no second identity was created.' : 'Created container does not match the accepted target.');
    }
    if (current.state === 'created' || (allowRestart && ['exited', 'dead'].includes(current.state))) {
      let failure: unknown;
      try { await command(host, ['container', 'start', current.containerId], 30_000); } catch (error) { failure = error; }
      const observed = await inspectContainer(host, instance.instanceId);
      requireThat(observed && matches(observed) && observed.containerId === current.containerId && ['running', 'exited', 'dead'].includes(observed.state) && !observed.startedAt.startsWith('0001-'), 'outcome_unknown', failure ? 'Docker start needs reconciliation against its existing identity.' : 'Docker start was not observed.');
      current = observed;
    }
    const identity = current.containerId;
    const observer = new AbortController();
    const unknown = (): ProcessResult => ({ exitCode: null, signal: null, errorCode: 'outcome_unknown', stdout: '', stderr: '', truncated: false });
    const verify = async () => {
      const observed = await inspectContainer(host, instance.instanceId);
      requireThat(observed && observed.containerId === identity && matches(observed) && observed.state === 'running', 'service_not_ready', 'Exact Docker launch is not observed running.');
    };
    const completion = (async (): Promise<ProcessResult> => {
      while (!observer.signal.aborted) {
        try {
          const observed = await inspectContainer(host, instance.instanceId);
          if (!observed || observed.containerId !== identity || !matches(observed)) return unknown();
          if (['created', 'exited', 'dead'].includes(observed.state)) return resultOf(host, observed, target.configPath);
        } catch { /* A failed daemon observation never proves that a running container exited. */ }
        try { await delay(1000, undefined, { signal: observer.signal }); } catch { break; }
      }
      return unknown();
    })();
    return { completion, verify, dispose: () => observer.abort(), stop: async (graceMs = 1000) => {
      const stopped = await stopContainer(host, instance.instanceId, graceMs, identity);
      return stopped ? resultOf(host, stopped, target.configPath) : unknown();
    } };
  };

  return { inspectContainer, stopContainer, verifyContainerImage, startContainer };
}

function suspiciousPath(value: string): boolean {
  return /[,\x00-\x1f]/.test(value);
}

export const defaultDockerAdapter = createDockerAdapter();
export const inspectContainer = (host: Host.HostConfig, instanceId: string) => defaultDockerAdapter.inspectContainer(host, instanceId);
export const stopContainer = (host: Host.HostConfig, instanceId: string, graceMs: number, expectedId?: string) => defaultDockerAdapter.stopContainer(host, instanceId, graceMs, expectedId);
export const verifyContainerImage = (host: Host.HostConfig, instance: Host.Instance, candidate: Host.Candidate, manifest: Host.ReleaseManifest) => defaultDockerAdapter.verifyContainerImage(host, instance, candidate, manifest);
export const startContainer = (host: Host.HostConfig, instance: Host.Instance, target: Host.RuntimeTarget, config: Host.InstanceConfig,
  candidate: Host.Candidate, manifest: Host.ReleaseManifest, allowRestart: boolean, resources?: Host.LinuxResourceScope) =>
  defaultDockerAdapter.startContainer(host, instance, target, config, candidate, manifest, allowRestart, resources);
