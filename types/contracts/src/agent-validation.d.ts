export declare const agentSchema: Record<string, unknown>;
export declare function validateAgent(name: string, value: unknown): void;
/** Native JSONL has already been decoded and size-bounded by the transport. Keep this check structural. */
export declare function validateAgentFrame(value: unknown, kind?: 'request-id' | 'reply'): void;
