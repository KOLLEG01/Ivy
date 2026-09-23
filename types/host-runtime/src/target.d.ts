import { HostJournal } from './journal.js';
import type { Host } from '../../contracts/src/generated.js';
/** Bind release-owned native bytes to the selected candidate, never to stale mutable host configuration. */
export declare function candidateBoundSettings(componentId: string, value: Host.InstanceConfig['settings'], artifactRoot: string): Promise<Host.InstanceConfig['settings']>;
export declare function materializeTarget(journal: HostJournal, instanceId: string, candidateId: string, enabled: boolean, basis?: Host.InstanceConfig): Promise<{
    target: Host.RuntimeTarget;
    config: Host.InstanceConfig;
}>;
export declare function stoppedTarget(target: Host.RuntimeTarget): Host.RuntimeTarget;
