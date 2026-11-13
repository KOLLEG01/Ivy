import test from "node:test";
import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import { mapConcurrent } from "../packages/ui-client/src/concurrency.js";
import { RetainedRequests } from "../packages/ui-client/src/retained-requests.js";

test("Outstanding UI originals survive reload and cannot be replaced or completed by another operation", () => {
  const values = new Map<string, string>();
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  } as unknown as Storage;
  const first = new RetainedRequests<{ operationId: string; payload: string }>(
    "originals",
    storage,
  );
  first.retain("a", {
    operationId: "original",
    payload: "exact revision and payload",
  });
  const reloaded = new RetainedRequests<{
    operationId: string;
    payload: string;
  }>("originals", storage);
  assert.deepEqual(reloaded.read(), first.read());
  assert.throws(() =>
    reloaded.retain("a", { operationId: "replacement", payload: "changed" }),
  );
  assert.throws(() => reloaded.complete("a", "replacement"));
  for (let i = 1; i < RetainedRequests.maximumCount; i++)
    reloaded.retain(String(i), { operationId: String(i), payload: "" });
  const full = values.get("originals");
  assert.throws(() =>
    reloaded.retain("extra", { operationId: "extra", payload: "" }),
  );
  assert.equal(values.get("originals"), full);
  reloaded.complete("1", "1");
  assert.throws(() =>
    reloaded.retain("large", {
      operationId: "large",
      payload: "ü".repeat(RetainedRequests.maximumBytes / 2),
    }),
  );
  assert.equal(reloaded.read().a!.operationId, "original");
  values.set("originals", "malformed");
  assert.throws(() => reloaded.read());
  assert.throws(() =>
    reloaded.retain("a", { operationId: "new", payload: "" }),
  );
  assert.equal(values.get("originals"), "malformed");
});

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
