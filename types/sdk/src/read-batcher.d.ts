export interface ReadFrame {
    jsonrpc: "2.0";
    id: string;
    method: string;
    params: Record<string, unknown>;
}
/** No result cache or replay: only concurrent observations share work, with independent cancellation. */
export declare class ReadBatcher {
    private readonly acquire;
    private readonly exchange;
    private readonly response;
    private readonly reads;
    private readonly waiting;
    private timer;
    constructor(acquire: (provider: boolean, signal: AbortSignal) => Promise<() => void>, exchange: (body: string, signal: AbortSignal) => Promise<unknown>, response: (frame: unknown, id: string) => unknown);
    /** A read begun after a write must not join an observation from before that write. */
    invalidate(): void;
    request(method: string, params: Record<string, unknown>, signal: AbortSignal, sent: () => void): Promise<unknown>;
    private admit;
    private flush;
    private send;
    private finish;
}
