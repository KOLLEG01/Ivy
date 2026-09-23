import type { Host } from '../../contracts/src/generated.js';
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
export declare function systemdUnitName(config: Host.HostConfig, instanceId: string): string;
export declare function parseSystemdState(output: string): SystemdState;
export declare function createSystemdAdapter(command?: SystemdCommand): SystemdAdapter;
export declare const defaultSystemdAdapter: SystemdAdapter;
export declare function renderSystemdUnit(config: Host.HostConfig, instanceId: string, target: Host.RuntimeTarget, candidate: Host.Candidate, manifest: Host.ReleaseManifest, resources?: Host.LinuxResourceScope): {
    unit: string;
    content: string;
};
