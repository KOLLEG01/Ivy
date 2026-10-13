/** Admission only: each request is sent once, and cancelled waiting work is removed. */
export declare class RequestQueue {
    readonly maximum: number;
    readonly providerMaximum: number;
    readonly waitingMaximum: number;
    private active;
    private providers;
    private readonly waiting;
    constructor(maximum: number, providerMaximum: number, waitingMaximum?: number);
    acquire(provider: boolean, signal: AbortSignal): Promise<() => void>;
    private enter;
}
