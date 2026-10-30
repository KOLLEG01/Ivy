export interface AppToolsSettings {
    /** Legacy explicit paths remain usable for local fixtures. Bundled paths resolve afresh. */
    nodeExecutable?: string;
    nodeExecutableHash?: string;
    serverPath?: string;
    serverHash?: string;
    pipePath: string;
    actorThreadId: string;
    internalProjectRoot?: string;
}
export interface AppThreadObservation {
    id: string;
    hostId: string;
    kind: string;
    status: {
        type: string;
        activeFlags: string[];
    };
}
export interface AppThreadSummary {
    id: string;
    hostId: "local";
    kind: "codex";
    status: string;
}
/** Resolve a complete local bundled release each time a new MCP connection starts. */
export declare function latestBundledAppToolsServer(root?: string): string;
export declare function resolveAppToolsLaunch(settings: AppToolsSettings, bundleRoot?: string): {
    nodeExecutable: string;
    serverPath: string;
};
export declare function appThreadObservation(result: unknown, threadId: string): AppThreadObservation;
/** Ordinary bundled MCP server, owned by the service. No Desktop pipe protocol,
 * debug hook, App Server process or fabricated live turn. The actor is an explicitly
 * assigned App-owned controller task. Domain journals own all mutation attempts. */
export declare class CodexAppTools {
    private client;
    private actorVerified;
    private readonly settings;
    private readonly inputSchemas;
    private connecting;
    private closed;
    private inspectionOnly;
    private connectedServer;
    get isClosed(): boolean;
    get isCurrentInstallation(): boolean;
    constructor(settings: AppToolsSettings);
    /** Explicit read-only installation setup verifies the current App Tools actor. */
    static inspect(settings: AppToolsSettings): Promise<AppToolsSettings>;
    private connect;
    private pipePaths;
    private invoke;
    readThread(threadId: string): Promise<AppThreadObservation>;
    verifyActor(): Promise<AppThreadObservation>;
    navigateToThread(threadId: string): Promise<void>;
    /** Move the Voice session that Desktop just started into the prepared local task.
     * New Desktop builds may resume their most recent Voice task despite navigation. */
    transferVoiceCall(sourceThreadId: string, threadId: string): Promise<void>;
    listThreads(limit?: number): Promise<AppThreadSummary[]>;
    internalProjectId(): Promise<string>;
    /** The App creates a user-owned local task. A lost acknowledgement cannot authorize
     * another creation; the caller journals the original attempt before submission. */
    createLocalThread(prompt: string, selection: {
        model: string;
        reasoningEffort: string;
    }, operationId: string, beforeSubmit: () => Promise<void>): Promise<string>;
    /** A successful MCP result is the App-owned acknowledgement. The caller records intent
     * before submission and must treat every later failure as unknown. */
    sendMessage(threadId: string, prompt: string, operationId: string, beforeSubmit: () => Promise<void>, selection?: {
        model?: string;
        reasoningEffort?: string;
    }): Promise<{
        threadId: string;
        sent: true;
    }>;
    /** Caller durably records intent immediately before this callback permits submission.
     * All exceptions after the callback are unknown; no retry or replacement operation. */
    archiveThread(threadId: string, operationId: string, beforeSubmit: () => Promise<void>): Promise<{
        threadId: string;
        archived: true;
    }>;
    close(): Promise<void>;
}
