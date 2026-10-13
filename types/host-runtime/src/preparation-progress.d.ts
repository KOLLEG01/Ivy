import type { Host } from '../../contracts/src/generated.js';
/** Bounded diagnostic state, separate from authoritative deployment transitions. No output contents. */
export declare class PreparationProgress {
    private value;
    private pending;
    private timer;
    private path;
    constructor(config: Host.HostConfig, componentId: string, snapshotId: string);
    step(step: string, options?: {
        command?: string;
        filePath?: string;
    }): Promise<void>;
    output: (_stream: "stdout" | "stderr", text: string) => void;
    private publish;
    close(): Promise<void>;
}
export declare function preparationProgress(config: Host.HostConfig, entry: Host.JournalEntry): Promise<{
    elapsedMs: number;
    stepElapsedMs: number;
    heartbeatAgeMs: number;
    componentId: string;
    snapshotId: string;
    step: string;
    startedAt: string;
    stepStartedAt: string;
    observedAt: string;
    lastOutputAt: string | null;
    outputBytes: number;
    workerPid: number;
    command: string | null;
    filePath: string | null;
    fileBytes: number | null;
} | null>;
