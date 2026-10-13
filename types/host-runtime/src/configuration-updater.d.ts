import { HostJournal } from "./journal.js";
export interface ConfigurationSyncResult {
    changed: boolean;
    revision: number | null;
    contentHash: string | null;
    bootstrapRequired: string[];
}
export declare class ConfigurationUpdater {
    readonly configPath: string;
    readonly journal: HostJournal;
    readonly credential: string;
    private readonly client;
    constructor(configPath: string, journal: HostJournal, credential: string);
    private remote;
    private apply;
    sync(): Promise<ConfigurationSyncResult>;
}
export declare function configurationFailure(error: unknown): string;
