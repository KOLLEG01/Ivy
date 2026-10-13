export interface DaemonObservation {
    status: 'started' | 'alreadyRunning';
    socketPath: string;
    managedCodexPath: string;
    managedCodexVersion: string;
    cliVersion: string;
    appServerVersion: string;
    pid: number | null;
}
export declare function initializedServerVersion(value: unknown): string;
export declare function daemonObservation(value: unknown, expectedVersion: string): DaemonObservation;
/** Only official idempotent start. No updater, restart, pairing, stop, or implicit stdio fallback. */
export declare function ensureSharedDaemon(input: {
    executable: string;
    executableHash: string;
    version: string;
    home: string;
    jobLauncher: string;
    environment: Record<string, string>;
}): Promise<DaemonObservation>;
