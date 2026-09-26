import { HostJournal } from "./journal.js";
import type { Host } from "../../contracts/src/generated.js";
export type BootstrapComponent = {
    candidate: Host.Candidate;
    manifest: Host.ReleaseManifest;
};
export declare function configurationBootstrapPlan(config: Host.HostConfig, path: string, candidate: Host.Candidate, resources?: Host.LinuxResourceBudget, components?: readonly BootstrapComponent[]): Host.BootstrapPlan;
/** Select one explicit bootstrap release per process. Installed state wins; fresh hosts use the newest retained candidate. */
export declare function bootstrapComponents(config: Host.HostConfig, journal: HostJournal, hostCandidate: Host.Candidate, preferred?: readonly BootstrapComponent[]): BootstrapComponent[];
export declare function retainedBootstrapPlans(config: Host.HostConfig): Promise<Host.BootstrapPlan[]>;
export declare function retainBootstrapPlan(plan: Host.BootstrapPlan): Promise<void>;
export declare function bootstrapPlan(path: string, candidateId: string, resources?: Host.LinuxResourceBudget, preferred?: readonly BootstrapComponent[]): Promise<Host.BootstrapPlan>;
export declare function linuxUnit(plan: Host.BootstrapPlan, instance: Host.BootstrapPlan["processes"][number], launchId?: string): string;
type BootstrapInstallOptions = {
    replaceLegacy?: boolean;
};
/** Current Ivy-owned units can be replaced during every update; only adopting the removed guardian form needs explicit legacy authority. */
export declare function replaceableSystemdOwner(content: string, allowLegacy?: boolean): boolean;
export declare function installBootstrap(plan: Host.BootstrapPlan, bootstrapRoot: string, options?: BootstrapInstallOptions): Promise<Host.CliOutput>;
/** The OS definition and its protected launch JSON are one selected release.
 * Maintenance calls this only with every bootstrap owner stopped. */
export declare function publishBootstrapInstanceConfigurations(plan: Host.BootstrapPlan, config: Host.HostConfig, journal: HostJournal): Promise<void>;
export {};
