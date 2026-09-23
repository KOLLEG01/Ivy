export interface AppToolsSettings {
    nodeExecutable: string;
    nodeExecutableHash: string;
    serverPath: string;
    serverHash: string;
    pipePath: string;
    actorThreadId: string;
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
export declare function appThreadObservation(result: unknown, threadId: string): AppThreadObservation;
/** Ordinary bundled MCP server, owned by the service. No Desktop pipe protocol,
 * debug hook, App Server process or fabricated live turn. The actor is an explicitly
 * assigned App-owned controller task. Domain journals own all mutation attempts. */
export declare class CodexAppTools {
    private client;
    private readonly settings;
    private readonly inputSchemas;
    private connecting;
    private closed;
    private inspectionOnly;
    constructor(settings: AppToolsSettings);
    /** Explicit read-only installation setup verifies the current App Tools actor. */
    static inspect(settings: AppToolsSettings): Promise<AppToolsSettings>;
    private verifyFiles;
    private connect;
    private pipePaths;
    private invoke;
    readThread(threadId: string): Promise<AppThreadObservation>;
    verifyActor(): Promise<AppThreadObservation>;
    navigateToThread(threadId: string): Promise<void>;
    listThreads(limit?: number): Promise<AppThreadSummary[]>;
    /** A successful MCP result is the App-owned acknowledgement. The caller records intent
     * before submission and must treat every later failure as unknown. */
    sendMessage(threadId: string, prompt: string, operationId: string, beforeSubmit: () => Promise<void>): Promise<{
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
