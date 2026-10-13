import type { Host } from '../../contracts/src/generated.js';
export interface BootstrapDefinition {
    content: string;
    hash: string;
}
export type BootstrapOwner = Host.BootstrapSnapshot['owners'][number] & {
    definition: BootstrapDefinition;
};
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
export declare function bootstrapOs(distribution: string): BootstrapOs;
