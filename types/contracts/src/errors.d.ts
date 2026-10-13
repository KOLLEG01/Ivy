export type Outcome = "not_executed" | "unknown" | "completed";
export interface WireError {
    code: number;
    message: string;
    data: {
        code: string;
        outcome: Outcome;
        details?: unknown;
    };
}
export declare class IvyError extends Error {
    readonly code: string;
    readonly outcome: Outcome;
    readonly details?: unknown | undefined;
    constructor(code: string, message: string, outcome?: Outcome, details?: unknown | undefined);
    get httpStatus(): number;
    toWire(): WireError;
    static from(error: unknown): IvyError;
}
export declare function fail(code: string, message: string, outcome?: Outcome): never;
export declare function requireThat(value: unknown, code: string, message: string): asserts value;
