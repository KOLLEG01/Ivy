import test from "node:test";
import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import { mapConcurrent } from "../packages/ui-client/src/concurrency.js";

test("UI collections use four workers, retain order, and stop scheduling after cancellation", async () => {
  let active = 0,
    maximum = 0;
  const items = Array.from({ length: 150 }, (_, index) => index);
  const result = await mapConcurrent(items, async (index) => {
    active++;
    maximum = Math.max(maximum, active);
    await delay(index % 3);
    active--;
    return index * 2;
  });
  assert.equal(maximum, 4);
  assert.deepEqual(
    result,
    items.map((index) => index * 2),
  );
  const aborted = new AbortController();
  let started = 0;
  await assert.rejects(
    mapConcurrent(
      items,
      async () => {
        started++;
        await delay(0);
        aborted.abort();
      },
      aborted.signal,
    ),
    { name: "AbortError" },
  );
  await delay(0);
  assert.equal(started, 4);
});
