import test from "node:test";
import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import { callBound, HiveClient } from "../packages/sdk/src/client.js";
import type { BoundTool } from "../packages/sdk/src/client.js";
import {
  rpcBatchRequests,
  rpcBatchRequestBytes,
} from "../packages/contracts/src/limits.js";

type Frame = { id: string; method: string; params: Record<string, unknown> };
const frames = (body: unknown): Frame[] =>
  Array.isArray(body) ? body : [body as Frame];
const reply = (
  body: unknown,
  result: (frame: Frame) => unknown = (frame) => frame.params,
) => {
  const values = frames(body).map((frame) => ({
    jsonrpc: "2.0",
    id: frame.id,
    result: result(frame),
  }));
  return new Response(
    JSON.stringify(Array.isArray(body) ? values.reverse() : values[0]),
  );
};
const provider = (node: string) => ({
  qualifiedName: "fixture.read",
  serviceNodeId: node,
  expectedDefinitionHash: "sha256:" + "0".repeat(64),
  arguments: {},
});

test("browser read batches correlate independent results and failures, share concurrent reads and leave writes individual", async () => {
  const sent: unknown[] = [];
  const client = new HiveClient("http://127.0.0.1/", {
    batchReads: true,
    requestCalls: 16,
    providerCalls: 8,
    fetch: async (_url, init) => {
      const body = JSON.parse(String(init!.body));
      sent.push(body);
      if (!Array.isArray(body)) return reply(body);
      return new Response(
        JSON.stringify(
          frames(body)
            .reverse()
            .map((frame) =>
              frame.method === "objects.stat"
                ? {
                    jsonrpc: "2.0",
                    id: frame.id,
                    error: {
                      code: -32000,
                      message: "Missing object.",
                      data: { code: "not_found", outcome: "not_executed" },
                    },
                  }
                : { jsonrpc: "2.0", id: frame.id, result: frame.method },
            ),
        ),
      );
    },
  });
  const status = client.request("system.status", {}),
    same = client.request("system.status", {});
  const apps = client.request("uis.catalog", { limit: 50 });
  const missing = assert.rejects(
    client.request("objects.stat", { objectId: "missing" }),
    { code: "not_found" },
  );
  await Promise.all([missing, assert.doesNotReject(apps)]);
  assert.equal(await status, "system.status");
  assert.equal(await same, "system.status");
  assert.equal(sent.length, 1);
  assert.equal(frames(sent[0]).length, 3);
  await client.request("system.status", {});
  assert.equal(sent.length, 2, "completed observations are never cached");
  await Promise.all([
    client.request("tools.call", {
      ...provider("one"),
      operationId: "original-write",
    } as never),
    client.request("tools.call", {
      ...provider("one"),
      operationId: "original-write",
    } as never),
  ]);
  assert.equal(sent.length, 4, "writes never join or enter a read batch");
  assert.ok(!Array.isArray(sent[2]) && !Array.isArray(sent[3]));
});

test("pinned native read hints permit batching with operation IDs while effects remain individual", async () => {
  const sent: unknown[] = [];
  const client = new HiveClient("http://127.0.0.1/", { batchReads: true,
    fetch: async (_url, init) => {
      const body = JSON.parse(String(init!.body)); sent.push(body);
      return reply(body, frame => frame.params.operationId);
    } });
  const binding = (name: string, readOnlyHint?: boolean) => ({
    serviceNodeId: "one", qualifiedName: "codex." + name, definitionHash: "sha256:" + "0".repeat(64),
    definition: { annotations: readOnlyHint === undefined ? {} : { readOnlyHint } },
  }) as BoundTool;
  assert.deepEqual(await Promise.all([
    callBound(client, binding("thread/read", true), {}, "read-one"),
    callBound(client, binding("model/list", true), {}, "read-two"),
    callBound(client, binding("turn/start", false), {}, "original-effect"),
    callBound(client, binding("unknown"), {}, "original-effect"),
  ]), ["read-one", "read-two", "original-effect", "original-effect"]);
  const batches = sent.filter(Array.isArray);
  assert.equal(batches.length, 1);
  assert.deepEqual(batches[0]!.map(frame => frame.params.operationId), ["read-one", "read-two"]);
  assert.equal(sent.filter(body => !Array.isArray(body)).length, 2);
});

test("cancelling or timing out one read cannot cancel another consumer or another batch item", async () => {
  let release!: () => void,
    batchSignal: AbortSignal | null = null;
  const sent: unknown[] = [];
  const client = new HiveClient("http://127.0.0.1/", {
    batchReads: true,
    fetch: async (_url, init) => {
      batchSignal = init!.signal as AbortSignal;
      const body = JSON.parse(String(init!.body));
      sent.push(body);
      await new Promise<void>((resolve) => {
        release = resolve;
      });
      return reply(body);
    },
  });
  const cancelled = new AbortController();
  const cancelledRead = client.request(
    "system.status",
    {},
    { signal: cancelled.signal },
  );
  const cancelledResult = assert.rejects(cancelledRead, {
    code: "outcome_unknown",
  });
  const kept = client.request("system.status", {});
  const timed = assert.rejects(
    client.request("uis.catalog", {}, { timeoutMs: 35 }),
    { code: "outcome_unknown" },
  );
  await delay(20);
  cancelled.abort();
  await cancelledResult;
  await timed;
  assert.equal((batchSignal as AbortSignal | null)?.aborted, false);
  release();
  await kept;
  assert.equal(sent.length, 1);
  assert.equal(frames(sent[0]).length, 2);
  const before = new AbortController();
  const removed = client.request(
    "objects.stat",
    { objectId: "cancelled" },
    { signal: before.signal },
  );
  const refusal = assert.rejects(removed);
  before.abort();
  await refusal;
  await delay(20);
  assert.equal(sent.length, 1, "cancelled queued reads are never sent");
});

test("ordinary read batches pass a waiting provider and writes fence earlier observations", async () => {
  const sent: unknown[] = [],
    releases: Array<() => void> = [];
  const client = new HiveClient("http://127.0.0.1/", {
    batchReads: true,
    requestCalls: 4,
    providerCalls: 2,
    fetch: async (_url, init) => {
      const body = JSON.parse(String(init!.body));
      sent.push(body);
      if (
        frames(body).some(
          (frame) => frame.method === "tools.call" && !frame.params.operationId,
        )
      )
        await new Promise<void>((resolve) => releases.push(resolve));
      return reply(body);
    },
  });
  const read = (key: string) => ({ ...provider("one"), arguments: { key } });
  const first = client.request("tools.call", read("first") as never);
  const second = client.request("tools.call", read("second") as never);
  const queued = client.request("tools.call", read("third") as never);
  await delay(20);
  const core = await client.request("system.status", {});
  assert.deepEqual(core, {});
  assert.equal(releases.length, 1);
  await client.request("objects.archive", {
    objectId: "one",
    expectedRevision: 1,
    mutationId: "write",
    archived: true,
  } as never);
  const after = client.request("tools.call", read("first") as never);
  releases[0]!();
  await Promise.all([first, second]);
  await delay(20);
  assert.equal(releases.length, 2);
  assert.equal(
    frames(sent.at(-1)).length,
    2,
    "a post-write read cannot join the pre-write observation",
  );
  releases[1]!();
  await Promise.all([queued, after]);
});

test("read batch count and bytes are bounded without rejecting valid individual reads", async () => {
  const bodies: string[] = [];
  const client = new HiveClient("http://127.0.0.1/", {
    batchReads: true,
    requestCalls: 32,
    fetch: async (_url, init) => {
      const body = String(init!.body);
      bodies.push(body);
      return reply(JSON.parse(body));
    },
  });
  await Promise.all(
    Array.from({ length: 19 }, (_, i) =>
      client.request("objects.stat", { objectId: String(i) }),
    ),
  );
  assert.equal(bodies.length, 3);
  assert.ok(
    bodies.every((body) => frames(JSON.parse(body)).length <= rpcBatchRequests),
  );
  bodies.length = 0;
  await Promise.all([
    client.request("tools.call", {
      ...provider("one"),
      arguments: { document: "x".repeat(40000) },
    } as never),
    client.request("tools.call", {
      ...provider("one"),
      arguments: { document: "y".repeat(40000) },
    } as never),
  ]);
  assert.equal(bodies.length, 2);
  assert.ok(
    bodies.every(
      (body) =>
        !Array.isArray(JSON.parse(body)) ||
        Buffer.byteLength(body) <= rpcBatchRequestBytes,
    ),
  );
});

test("malformed batch correlations and HTTP-level authentication failures never replay reads", async () => {
  for (const mode of ["duplicate", "missing", "unauthenticated"]) {
    let calls = 0;
    const client = new HiveClient("http://127.0.0.1/", {
      batchReads: true,
      fetch: async (_url, init) => {
        calls++;
        const [first] = frames(JSON.parse(String(init!.body)));
        const frame = { jsonrpc: "2.0", id: first!.id, result: {} };
        return new Response(
          JSON.stringify(
            mode === "duplicate"
              ? [frame, frame]
              : mode === "missing"
                ? [frame]
                : {
                    jsonrpc: "2.0",
                    id: null,
                    error: {
                      code: -32000,
                      message: "Sign in.",
                      data: {
                        code: "unauthenticated",
                        outcome: "not_executed",
                      },
                    },
                  },
          ),
        );
      },
    });
    const failures = await Promise.allSettled([
      client.request("system.status", {}),
      client.request("uis.catalog", {}),
    ]);
    assert.ok(
      failures.every(
        (failure) =>
          failure.status === "rejected" &&
          failure.reason.code ===
            (mode === "unauthenticated" ? "unauthenticated" : "invalid_frame"),
      ),
    );
    assert.equal(calls, 1);
  }
});
