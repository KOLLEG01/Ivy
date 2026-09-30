import type { Host } from '../../contracts/src/generated.js';
declare function restartWindowsOwner(plans: Host.BootstrapPlan[], name: string, distribution: string): Promise<void>;
/** A changed bootstrap instance file is the durable restart request. A new process
 * has a later health.startedAt, so crashes between publication and signaling do
 * not lose the request or cause repeated restarts after recovery. */
export declare function reconcileBootstrapConfigurationRestarts(config: Host.HostConfig, ownInstanceId?: string, distribution?: string, restartManager?: typeof restartWindowsOwner): Promise<boolean>;
export {};
