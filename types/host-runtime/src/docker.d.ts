import type { ProcessResult } from './process.js';
import type { Host } from '../../contracts/src/generated.js';
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
    startContainer: (host: Host.HostConfig, instance: Host.Instance, target: Host.RuntimeTarget, config: Host.InstanceConfig, candidate: Host.Candidate, manifest: Host.ReleaseManifest, allowRestart: boolean, resources?: Host.LinuxResourceScope) => Promise<ManagedProcess>;
}
export declare const containerName: (hostId: string, instanceId: string, recovery?: {
    runtimeRoot: string;
    backupId: string;
}) => string;
/** Docker restart policy is configured once at container creation; HostExecutor only adopts/observes it. */
export declare function dockerRestartPolicy(manifest: Host.ReleaseManifest): string;
export declare function createDockerAdapter(command?: DockerCommand): DockerAdapter;
export declare const defaultDockerAdapter: DockerAdapter;
export declare const inspectContainer: (host: Host.HostConfig, instanceId: string) => Promise<Host.ContainerState | null>;
export declare const stopContainer: (host: Host.HostConfig, instanceId: string, graceMs: number, expectedId?: string) => Promise<Host.ContainerState | null>;
export declare const verifyContainerImage: (host: Host.HostConfig, instance: Host.Instance, candidate: Host.Candidate, manifest: Host.ReleaseManifest) => Promise<void>;
export declare const startContainer: (host: Host.HostConfig, instance: Host.Instance, target: Host.RuntimeTarget, config: Host.InstanceConfig, candidate: Host.Candidate, manifest: Host.ReleaseManifest, allowRestart: boolean, resources?: Host.LinuxResourceScope) => Promise<ManagedProcess>;
