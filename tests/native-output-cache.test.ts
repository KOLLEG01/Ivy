import test from "node:test";
import assert from "node:assert/strict";
import {
  NativeOutputCache,
  nativeOutputCache,
} from "../packages/ui-client/src/native-output-cache.js";
import type { NativeOutputPage } from "../packages/ui-client/src/native-output-page.js";
import type { RpcClient } from "../packages/sdk/src/client.js";

const page = (text = "Saved message"): NativeOutputPage => ({
  mode: "items",
  items: [
    { turnId: "turn", item: { id: "message", type: "agentMessage", text } },
  ],
  nextCursor: null,
  detail: "",
  download: null,
});

test("output previews isolate native owners, threads, turns and clients and expire without extending freshness on read", () => {
  let now = 0;
  const cache = new NativeOutputCache(4096, 8, 100, () => now),
    saved = page();
  cache.write(["owner", "thread", ""], saved);
  assert.equal(cache.read(["owner", "thread", ""]), saved);
  for (const scope of [
    ["other", "thread", ""],
    ["owner", "other", ""],
    ["owner", "thread", "turn"],
  ] as const)
    assert.equal(cache.read(scope), undefined);
  now = 99;
  assert.equal(cache.read(["owner", "thread", ""]), saved);
  now = 100;
  assert.equal(cache.read(["owner", "thread", ""]), undefined);
  const client = {} as RpcClient;
  assert.equal(nativeOutputCache(client), nativeOutputCache(client));
  assert.notEqual(
    nativeOutputCache(client),
    nativeOutputCache({} as RpcClient),
  );
});

test("output previews evict by recency and size and never retain oversized native output or search cursors", () => {
  const cache = new NativeOutputCache(1000, 2),
    saved = page();
  cache.write(["owner", "a", ""], saved);
  cache.write(["owner", "b", ""], saved);
  cache.read(["owner", "a", ""]);
  cache.write(["owner", "c", ""], saved);
  assert.equal(cache.read(["owner", "b", ""]), undefined);
  assert.equal(cache.read(["owner", "a", ""]), saved);
  cache.write(["owner", "a", ""], page("x".repeat(1000)));
  assert.equal(cache.read(["owner", "a", ""]), undefined);
  cache.write(["owner", "c", ""], { ...saved, mode: "search" });
  assert.equal(cache.read(["owner", "c", ""]), undefined);
  const bytes = JSON.stringify(saved).length * 2;
  const bounded = new NativeOutputCache(bytes, 8);
  bounded.write(["owner", "a", ""], saved);
  bounded.write(["owner", "b", ""], saved);
  assert.equal(bounded.read(["owner", "a", ""]), undefined);
  assert.equal(bounded.read(["owner", "b", ""]), saved);
});

test("history invalidation removes all views of the affected thread without losing another owner or thread", () => {
  const cache = new NativeOutputCache(),
    saved = page();
  for (const scope of [
    ["owner", "a", ""],
    ["owner", "a", "turn"],
    ["owner", "b", ""],
    ["other", "a", ""],
  ] as const)
    cache.write(scope, saved);
  cache.invalidate("owner", "a");
  assert.equal(cache.read(["owner", "a", ""]), undefined);
  assert.equal(cache.read(["owner", "a", "turn"]), undefined);
  assert.equal(cache.read(["owner", "b", ""]), saved);
  assert.equal(cache.read(["other", "a", ""]), saved);
  cache.invalidate("owner");
  assert.equal(cache.read(["owner", "b", ""]), undefined);
  cache.invalidate();
  assert.equal(cache.read(["other", "a", ""]), undefined);
});

test("output previews drop full-turn downloads and clear only a changed native connection", () => {
  const cache = new NativeOutputCache(),
    saved = page();
  cache.write(["owner", "a", ""], {
    ...saved,
    download: { name: "turn.json", raw: "x".repeat(4 * 1024 * 1024) },
  });
  cache.write(["other", "a", ""], saved);
  assert.deepEqual(cache.read(["owner", "a", ""]), saved);
  assert.equal(cache.observeEpoch("owner", "first"), false);
  assert.equal(cache.observeEpoch("owner", "first"), false);
  assert.equal(cache.read(["owner", "a", ""])?.download, null);
  assert.equal(cache.observeEpoch("owner", "second"), true);
  assert.equal(cache.read(["owner", "a", ""]), undefined);
  assert.equal(cache.read(["other", "a", ""]), saved);
});
