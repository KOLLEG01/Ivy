import { setTimeout as delay } from 'node:timers/promises';

// Pace request starts across all local HTTP clients sharing one admission identity.
// A delayed timer never earns catch-up bursts. Network calls remain concurrent.
export function requestPacer(intervalMs = 25, clock = () => performance.now(), sleep = delay) {
  if (!Number.isFinite(intervalMs) || intervalMs <= 0) throw new Error('A positive request interval is required.');
  let previous = -Infinity, queue = Promise.resolve();
  return () => {
    const admitted = queue.then(async () => {
      let remaining;
      while ((remaining = previous + intervalMs - clock()) > 0) await sleep(remaining);
      previous = clock();
    });
    queue = admitted;
    return admitted;
  };
}
