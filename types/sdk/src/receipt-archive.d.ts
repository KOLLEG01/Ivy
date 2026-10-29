import type { DatabaseSync } from 'node:sqlite';
/** Uncertainty in a nested owner observation is still live recovery evidence. */
export declare function hasUnresolvedOutcome(value: unknown): boolean;
/** Receipts share the owner's database and transaction. SQLite owns atomicity and integrity. */
export declare class ReceiptArchive {
    private readonly db;
    readonly limits: {
        count: number;
        bytes: number;
    };
    private readonly lookup;
    private readonly insert;
    private readonly usage;
    constructor(db: DatabaseSync, limits?: {
        count: number;
        bytes: number;
    });
    read(kind: string, key: string): string | null;
    retain(kind: string, key: string, value: string): void;
    /** Only the owner can release a receipt after its dependencies have finished. */
    release(kind: string, key: string): void;
    status(): {
        records: number;
        bytes: number;
    };
}
