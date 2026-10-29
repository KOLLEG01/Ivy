/** Stop admitting work on failure and drain every started operation before returning to its owner. */
export declare function mapConcurrent<T, R>(items: readonly T[], concurrency: number, action: (item: T, index: number) => Promise<R>): Promise<R[]>;
