import { requireThat } from '../../contracts/src/errors.js';

/** Stop admitting work on failure and drain every started operation before returning to its owner. */
export async function mapConcurrent<T, R>(items: readonly T[], concurrency: number, action: (item: T, index: number) => Promise<R>): Promise<R[]> {
  requireThat(Number.isSafeInteger(concurrency) && concurrency >= 1 && concurrency <= 8,
    'invalid_arguments', 'Owned concurrent work requires a bounded worker count.');
  const results = new Array<R>(items.length);
  let next = 0, failed = false, failure: unknown;
  const worker = async () => {
    while (!failed && next < items.length) {
      const index = next++;
      try { results[index] = await action(items[index]!, index); }
      catch (error) { if (!failed) { failed = true; failure = error; } }
    }
  };
  await Promise.all(Array.from({ length: Math.min(concurrency, items.length) }, worker));
  if (failed) throw failure;
  return results;
}
