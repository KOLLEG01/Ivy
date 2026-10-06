/** Load a collection with a small fixed number of requests, preserving its order. */
export async function mapConcurrent<T, R>(
  items: readonly T[],
  load: (item: T) => Promise<R>,
  signal?: AbortSignal,
): Promise<R[]> {
  const results: R[] = new Array(items.length);
  let next = 0,
    failed = false;
  const worker = async () => {
    while (!failed && next < items.length) {
      signal?.throwIfAborted();
      const index = next++;
      try {
        results[index] = await load(items[index]!);
      } catch (error) {
        failed = true;
        throw error;
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(4, items.length) }, worker));
  return results;
}
