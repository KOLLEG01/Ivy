import type { DatabaseSync } from 'node:sqlite';
/** Receipts share the owner's database and transaction. SQLite owns atomicity and integrity. */
export declare class ReceiptArchive {
    private readonly lookup;
    private readonly insert;
    private readonly usage;
    constructor(db: DatabaseSync);
    read(kind: string, key: string): string | null;
    retain(kind: string, key: string, value: string): void;
    status(): {
        records: number;
        bytes: number;
    };
}
