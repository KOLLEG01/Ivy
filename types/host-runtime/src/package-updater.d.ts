import type { Host } from '../../contracts/src/generated.js';
import { HostJournal } from './journal.js';
export interface BootstrapPackageComponent {
    instanceId: string;
    candidate: Host.Candidate;
    manifest: Host.ReleaseManifest;
}
export interface PackageSyncResult {
    bootstrap: BootstrapPackageComponent[];
}
export declare class PackageUpdater {
    readonly journal: HostJournal;
    readonly credential: string;
    private revision;
    private readonly pendingBootstrap;
    constructor(_config: Host.HostConfig, journal: HostJournal, credential: string);
    private get config();
    private importPackage;
    private result;
    private installedVersion;
    sync(): Promise<PackageSyncResult>;
}
