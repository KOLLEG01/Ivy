import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  readdirSync,
  rmSync,
  writeFileSync,
  readFileSync,
} from "node:fs";
import { join, relative } from "node:path";
import { tmpdir } from "node:os";
import { createServer } from "node:net";
import type { Worker } from "node:worker_threads";
import { setTimeout as delay } from "node:timers/promises";
import { HiveServer } from "../services/hive/src/server.js";
import { digest } from "../packages/contracts/src/canonical.js";
import {
  HiveClient,
  IvyError,
  discover,
  callBound,
  nativeServiceTools,
  serviceTools,
  BoundToolClient,
  BrowserNotifications,
  newOperationId,
  scopedOperationId,
  binaryObjectContentBytes,
  connectionInFlightRequests,
  consumerInFlightRequests,
  jsonObjectContentBytes,
  managementFrameBytes,
  textObjectContentBytes,
} from "../packages/sdk/src/client.js";
import type { Wire, RpcClient } from "../packages/sdk/src/client.js";
import { ServiceClient, consumeEvents } from "../packages/sdk/src/service.js";
import type { ServiceConnection } from "../packages/sdk/src/service.js";
import { connectionOwner } from "../packages/sdk/src/connection-owner.js";

async function until(
  check: () => boolean | Promise<boolean>,
  timeout = 12_000,
): Promise<void> {
  const end = Date.now() + timeout;
  while (!(await check())) {
    assert.ok(Date.now() < end, "condition deadline exceeded");
    await delay(30);
  }
}
const definition: Wire.ToolDefinition = {
  namespace: "example",
  name: "echo",
  interfaceVersion: "1.0.0",
  description: "Echo supplied content.",
  inputSchema: {
    type: "object",
    properties: { value: { type: "string" } },
    required: ["value"],
    additionalProperties: false,
  },
  outputSchema: { type: "string" },
};
const registry: Wire.RegistrySync = {
  namespaces: [
    {
      namespace: "example",
      description: "SDK test provider",
      guideMarkdown: "",
      tools: [definition],
      topics: [],
      inventoryKinds: [],
    },
  ],
  contracts: [],
  requiredContracts: [],
};

test("public SDK exports the canonical consumer limits", () => {
  assert.deepEqual(
    {
      jsonObjectContentBytes,
      textObjectContentBytes,
      binaryObjectContentBytes,
      managementFrameBytes,
      connectionInFlightRequests,
      consumerInFlightRequests,
    },
    {
      jsonObjectContentBytes: 1_048_576,
      textObjectContentBytes: 1_048_576,
      binaryObjectContentBytes: 8_388_608,
      managementFrameBytes: 33_554_432,
      connectionInFlightRequests: 64,
      consumerInFlightRequests: 256,
    },
  );
});

test("service and browser implementations depend on the public SDK boundary", () => {
  const files: string[] = [],
    directTransports = new Set<string>(),
    visit = (path: string) => {
      for (const entry of readdirSync(path, { withFileTypes: true })) {
        const selected = join(path, entry.name);
        if (entry.isDirectory()) visit(selected);
        else if (/\.(?:ts|vue|mjs)$/.test(entry.name)) files.push(selected);
      }
    };
  visit("ui");
  visit(join("packages", "ui-client"));
  for (const service of readdirSync("services", { withFileTypes: true }))
    if (service.isDirectory() && service.name !== "hive")
      visit(join("services", service.name));
  for (const file of files) {
    const source = readFileSync(file, "utf8");
    const normalized = file.replaceAll("\\", "/");
    assert.doesNotMatch(
      source,
      /packages\/contracts\/src|services\/hive\/src|\/api\/v1\//,
      `${file} bypasses the SDK boundary`,
    );
    if (/\bfetch\s*\(/.test(source))
      directTransports.add(normalized + ":fetch");
    if (/\bnew\s+WebSocket\s*\(/.test(source))
      directTransports.add(normalized + ":websocket");
    if (/\+\s*['"]\/mcp['"]/.test(source))
      directTransports.add(normalized + ":mcp-endpoint");
    for (const occurrence of source.matchAll(/serviceTools\(([^)]*)\)/g))
      assert.match(
        occurrence[1]!,
        /interfaceVersion/,
        `${file} calls a stable service namespace without its SDK interface version`,
      );
  }
  assert.deepEqual(
    [...directTransports].sort(),
    [
      "services/agent-manager/src/proxy-transport.ts:websocket",
      "services/chat-bridge/src/whatsapp/media.ts:fetch",
    ],
    "direct product transports must stay limited to the documented service-owned exceptions",
  );
});

test("provider calls beyond the client limit wait, take over finished slots and leave when aborted", async () => {
  const started: Array<() => void> = [];
  const client = new HiveClient("http://127.0.0.1/", {
    providerCalls: 2,
    fetch: async (_url, init) => {
      const { id } = JSON.parse(String(init!.body)) as { id: string };
      await new Promise<void>((resolve) => started.push(resolve));
      return new Response(
        JSON.stringify({ jsonrpc: "2.0", id, result: { ok: true } }),
      );
    },
  });
  const call = (signal?: AbortSignal) =>
    client.request(
      "tools.call",
      {
        qualifiedName: "fixture.read",
        serviceNodeId: "node",
        expectedDefinitionHash: "sha256:" + "0".repeat(64),
        arguments: {},
      } as never,
      signal ? { signal } : {},
    );
  const first = call(),
    second = call(),
    aborted = new AbortController(),
    third = call(aborted.signal),
    fourth = call();
  await delay(10);
  assert.equal(
    started.length,
    2,
    "only the configured number of provider calls is sent",
  );
  aborted.abort();
  await assert.rejects(third);
  started[0]!();
  await first;
  await delay(10);
  assert.equal(
    started.length,
    3,
    "a finished call hands its slot to the next live waiter",
  );
  started[1]!();
  started[2]!();
  await Promise.all([second, fourth]);
});

test("client admission bounds all requests and lets ordinary reads pass waiting providers", async () => {
  const started: Array<{ method: string; finish: () => void }> = [];
  const client = new HiveClient("http://127.0.0.1/", {
    requestCalls: 3,
    providerCalls: 1,
    queuedRequests: 2,
    fetch: async (_url, init) => {
      const { id, method } = JSON.parse(String(init!.body)) as {
        id: string;
        method: string
      };
      await new Promise<void>((resolve) =>
        started.push({ method, finish: resolve }),
      );
      return new Response(
        JSON.stringify({ jsonrpc: "2.0", id, result: { ok: true } }),
      );
    },
  });
  const provider = () =>
    client.request("tools.call", {
        qualifiedName: "fixture.read",
        serviceNodeId: "node",
        expectedDefinitionHash: "sha256:" + "0".repeat(64),
        arguments: {},
      } as never);
  const core = (signal?: AbortSignal) =>
    client.request(
      "objects.stat",
      { objectId: "object" },
      signal ? { signal } : {},
    );
  const first = provider(),
    waitingProvider = provider(),
    read1 = core(),
    read2 = core();
  const cancelled = new AbortController(),
    waitingRead = core(cancelled.signal);
  await delay(0);
  assert.deepEqual(
    started.map((item) => item.method),
    ["tools.call", "objects.stat", "objects.stat"],
  );
  await assert.rejects(core(), {
    code: "limit_exceeded",
    outcome: "not_executed",
    details: { budget: "client_queue", active: 2, limit: 2 },
  });
  cancelled.abort();
  await assert.rejects(waitingRead);
  const read3 = core();
  started[1]!.finish();
  await read1;
  await delay(0);
  assert.equal(
    started[3]!.method,
    "objects.stat",
    "a waiting provider does not block free ordinary capacity",
  );
  started[0]!.finish();
  await first;
  await delay(0);
  assert.equal(started[4]!.method, "tools.call");
  started[2]!.finish();
  started[3]!.finish();
  started[4]!.finish();
  await Promise.all([read2, read3, waitingProvider]);
  assert.equal(started.length, 5, "rejected and cancelled work was never sent");
});

test("waiting mutations expire before sending and sent cancellations are never retried", async () => {
  const started: Array<() => void> = [];
  const client = new HiveClient("http://127.0.0.1/", {
    requestCalls: 1,
    fetch: async (_url, init) => {
      const { id } = JSON.parse(String(init!.body)) as { id: string };
      await new Promise<void>((resolve, reject) => {
        started.push(resolve);
        init!.signal!.addEventListener(
          "abort",
          () => reject(init!.signal!.reason),
          { once: true },
        );
      });
      return new Response(
        JSON.stringify({ jsonrpc: "2.0", id, result: { ok: true } }),
      );
    },
  });
  const active = client.request("objects.stat", { objectId: "object" });
  await assert.rejects(
    client.request(
      "objects.write",
      { mutationId: "original-operation" } as never,
      { timeoutMs: 10 },
    ),
    { code: "deadline_exceeded", outcome: "not_executed" },
  );
  assert.equal(started.length, 1);
  started[0]!();
  await active;
  const cancelled = new AbortController();
  const sent = client.request(
    "objects.write",
    { mutationId: "original-operation" } as never,
    { signal: cancelled.signal },
  );
  await delay(0);
  cancelled.abort();
  await assert.rejects(sent, { code: "outcome_unknown", outcome: "unknown" });
  assert.equal(started.length, 2);
  const next = client.request("objects.stat", { objectId: "object" });
  await delay(0);
  started[2]!();
  await next;
  assert.equal(
    started.length,
    3,
    "the cancelled request released its capacity without replay",
  );
});

test("browser notifications correlate versioned subscription setup and can restart after the final listener leaves", (t) => {
  const locationDescriptor = Object.getOwnPropertyDescriptor(
    globalThis,
    "location",
  );
  const webSocketDescriptor = Object.getOwnPropertyDescriptor(
    globalThis,
    "WebSocket",
  );
  const cleanup: Array<() => void> = [];
  type Listener = (event: { data?: string }) => void;
  class FakeWebSocket {
    static blocked = false;
    static readonly CONNECTING = 0;
    static readonly OPEN = 1;
    static readonly CLOSED = 3;
    static readonly instances: FakeWebSocket[] = [];
    readyState = FakeWebSocket.CONNECTING;
    failSend = false;
    readonly sent: string[] = [];
    private readonly listeners = new Map<string, Listener[]>();
    constructor(readonly url: URL) {
      if (FakeWebSocket.blocked) throw new Error("Browser policy blocked WebSocket construction.");
      FakeWebSocket.instances.push(this);
    }
    addEventListener(name: string, listener: Listener): void {
      this.listeners.set(name, [...(this.listeners.get(name) ?? []), listener]);
    }
    send(value: string | ArrayBufferLike | ArrayBufferView): void {
      if (this.failSend) throw new Error("Socket closed while updating the subscription.");
      this.sent.push(
        typeof value === "string"
          ? value
          : new TextDecoder().decode(
              value instanceof ArrayBuffer
                ? value
                : ArrayBuffer.isView(value)
                  ? value
                  : new Uint8Array(value),
            ),
      );
    }
    open(): void {
      this.readyState = FakeWebSocket.OPEN;
      this.emit("open");
    }
    message(value: unknown): void {
      this.emit("message", JSON.stringify(value));
    }
    close(): void {
      if (this.readyState === FakeWebSocket.CLOSED) return;
      this.readyState = FakeWebSocket.CLOSED;
      this.emit("close");
    }
    private emit(name: string, data?: string): void {
      for (const listener of this.listeners.get(name) ?? [])
        listener({ ...(data === undefined ? {} : { data }) });
    }
  }
  Object.defineProperty(globalThis, "location", {
    configurable: true,
    value: new URL("https://hive.test/console"),
  });
  Object.defineProperty(globalThis, "WebSocket", {
    configurable: true,
    value: FakeWebSocket,
  });
  t.after(() => {
    for (const stop of cleanup.splice(0)) stop();
    if (locationDescriptor)
      Object.defineProperty(globalThis, "location", locationDescriptor);
    else delete (globalThis as { location?: unknown }).location;
    if (webSocketDescriptor)
      Object.defineProperty(globalThis, "WebSocket", webSocketDescriptor);
    else delete (globalThis as { WebSocket?: unknown }).WebSocket;
  });
  const notifications = new BrowserNotifications("https://hive.test/ivy"),
    received: unknown[] = [];
  const filter = {
    namespace: "agent",
    name: "notification",
    version: "1.0.0",
    serviceNodeId: "agent-one",
  } as const;
  const unsubscribe = notifications.subscribe(filter, (frame) =>
    received.push(frame),
  );
  cleanup.push(unsubscribe);
  const first = FakeWebSocket.instances[0]!;
  first.open();
  const request = JSON.parse(first.sent[0]!) as {
    id: string;
    method: string;
    params: { filters: unknown[] };
  };
  assert.equal(request.method, "notifications.subscribe");
  assert.deepEqual(request.params.filters, [filter]);
  first.message({ jsonrpc: "2.0", id: request.id, result: { subscribed: 1 } });
  first.message({
    jsonrpc: "2.0",
    method: "notifications.provider",
    params: { ...filter, version: "2.0.0", generation: 1, payload: {} },
  });
  first.message({
    jsonrpc: "2.0",
    method: "notifications.provider",
    params: { ...filter, generation: 1, payload: {} },
  });
  assert.equal(received.length, 1);
  unsubscribe();
  assert.equal(first.readyState, FakeWebSocket.CLOSED);
  const stop = notifications.subscribe(filter, (frame) => received.push(frame));
  cleanup.push(stop);
  const second = FakeWebSocket.instances[1]!;
  second.open();
  assert.equal(second.sent.length, 1);
  stop();

  t.mock.timers.enable({ apis: ["setTimeout"] });
  const statuses: boolean[] = [], changed: string[][] = [];
  cleanup.push(notifications.onStatus(value => statuses.push(value)));
  const stopChanges = notifications.subscribeChanges(["objects/wiki", "services"], scopes => changed.push(scopes));
  cleanup.push(stopChanges);
  const third = FakeWebSocket.instances.at(-1)!;
  third.open();
  assert.equal(notifications.connected, false, "an open transport is not an acknowledged subscription");
  const setup = JSON.parse(third.sent[0]!);
  assert.deepEqual(setup.params.changes, ["objects/wiki", "services"]);
  third.message({ jsonrpc: "2.0", id: setup.id, result: { subscribed: 0, changes: 2 } });
  assert.equal(notifications.connected, true);
  third.message({ jsonrpc: "2.0", method: "notifications.changed", params: { scopes: ["objects/task-board/task"] } });
  third.message({ jsonrpc: "2.0", method: "notifications.changed", params: { scopes: ["objects/wiki/page"] } });
  third.message({ jsonrpc: "2.0", method: "notifications.changed", params: { scopes: ["objects"] } });
  assert.deepEqual(changed, [["objects/wiki/page"], ["objects"]]);
  t.mock.timers.tick(30_000);
  third.message({ jsonrpc: "2.0", method: "notifications.changed", params: { scopes: [] } });
  t.mock.timers.tick(30_000);
  assert.equal(notifications.connected, true, "empty heartbeats do not invalidate data");
  assert.equal(changed.length, 2);
  t.mock.timers.tick(5_001);
  assert.equal(notifications.connected, false, "a silent broken connection enables fallback");
  t.mock.timers.tick(500);
  const fourth = FakeWebSocket.instances.at(-1)!;
  fourth.open();
  t.mock.timers.tick(5_001);
  assert.equal(fourth.readyState, FakeWebSocket.CLOSED, "a missing subscription acknowledgement times out");
  assert.deepEqual(statuses, [false, true, false]);
  stopChanges();
  const stopFirst = notifications.subscribeChanges(["objects"], () => undefined);
  cleanup.push(stopFirst);
  const fifth = FakeWebSocket.instances.at(-1)!;
  fifth.open();
  const pendingSetup = JSON.parse(fifth.sent[0]!);
  const stopNext = notifications.subscribeChanges(["services"], () => undefined);
  cleanup.push(stopNext);
  fifth.failSend = true;
  fifth.message({ jsonrpc: "2.0", id: pendingSetup.id, result: { subscribed: 0, changes: 1 } });
  assert.equal(fifth.readyState, FakeWebSocket.CLOSED);
  assert.equal(notifications.connected, false, "failed resubscription cannot report a closed socket as ready");
  assert.deepEqual(statuses, [false, true, false]);
  stopNext();
  stopFirst();
  FakeWebSocket.blocked = true;
  assert.doesNotThrow(() => {
    cleanup.push(notifications.subscribeChanges(["objects"], () => undefined));
  }, "a browser policy failure leaves HTTP fallback usable");
});

test("connection ownership is read once and readiness waits for the Hive acknowledgement", async (t) => {
  const f = await fixture(t),
    request = f.server.worker.request.bind(f.server.worker);
  let release!: () => void,
    entered!: () => void,
    first = true,
    reads = 0;
  const held = new Promise<void>((resolve) => (release = resolve)),
    started = new Promise<void>((resolve) => (entered = resolve));
  t.mock.method(
    f.server.worker,
    "request",
    async (...args: Parameters<typeof request>) => {
      const action = args[0],
        method =
          action.action === "execute"
            ? (action.request as { method: string }).method
            : "";
      if (method === "service.heartbeat" && first) {
        first = false;
        entered();
        await held;
      }
      if (method === "serviceNodes.get") reads++;
      return request(...args);
    },
  );
  f.service.start();
  await started;
  assert.equal(f.service.ready, false);
  release();
  await f.service.waitReady();
  const connection = f.service.connection,
    generation = connection.generation,
    owner = await connectionOwner(connection, "sdk-test");
  assert.equal(reads, 1);
  for (let i = 0; i < 10; i++)
    assert.deepEqual(await connectionOwner(connection, "sdk-test"), owner);
  assert.equal(reads, 1);
  connection.close();
  await assert.rejects(connectionOwner(connection, "sdk-test"));
  assert.equal(reads, 1);
  await f.service.waitReady();
  assert.ok(f.service.connection.generation > generation);
  const next = await connectionOwner(f.service.connection, "sdk-test");
  assert.equal(next.serviceNodeId, owner.serviceNodeId);
  assert.equal(reads, 2);
});

test(
  "bound SDK Tools share discovery while every invocation retains Hive readiness and definition fences",
  { timeout: 30000 },
  async (t) => {
    const f = await fixture(t);
    f.service.start();
    await f.service.waitReady();
    let discoveries = 0;
    const client = new Proxy(f.client, {
      get(target, key) {
        if (key !== "request") return Reflect.get(target, key);
        return (method: string, params: never, options: never) => {
          if (method === "tools.list") discoveries++;
          return target.request(method as never, params, options);
        };
      },
    }) as RpcClient;
    const requirement = [
      { namespace: "example", interfaceVersion: "1.0.0" },
    ] as const;
    const tools = serviceTools(client, "sdk-test", requirement);
    assert.equal(serviceTools(client, "sdk-test", requirement), tools);
    assert.throws(() => serviceTools(client, "sdk-test", []), {
      code: "invalid_arguments",
    });
    await assert.rejects(
      nativeServiceTools(client, "sdk-test").binding("example.echo"),
      { code: "invalid_arguments" },
    );
    await assert.rejects(
      serviceTools(client, "sdk-test", [
        { namespace: "example", interfaceVersion: "2.0.0" },
      ]).binding("example.echo"),
      { code: "contract_version_conflict" },
    );
    assert.equal(discoveries, 1);
    const bindings = await Promise.all([
      tools.binding("example.echo"),
      tools.binding("example.echo"),
    ]);
    assert.equal(bindings[0], bindings[1]);
    assert.equal(discoveries, 2);
    assert.ok(Object.isFrozen(bindings[0]!.definition));
    assert.deepEqual(
      await Promise.all(
        ["one", "two"].map((value) => tools.call("example.echo", { value })),
      ),
      ["one", "two"],
    );
    assert.equal(discoveries, 2);
    assert.equal(f.invocations(), 2);
    const changed = {
      ...definition,
      description: "Changed exact Tool contract.",
    };
    const replacement = {
      ...registry,
      namespaces: [{ ...registry.namespaces[0]!, tools: [changed] }],
    };
    f.service.connection.setRegistry(replacement);
    await f.service.connection.request("registry.sync", replacement);
    await assert.rejects(tools.call("example.echo", { value: "forbidden" }), {
      code: "tool_definition_changed",
    });
    await assert.rejects(
      tools.call("example.echo", { value: "still forbidden" }),
      { code: "tool_definition_changed" },
    );
    assert.equal(discoveries, 2);
    assert.equal(f.invocations(), 2);
    // Explicitly creating a new binding context admits the newly reviewed definition.
    const fresh = new BoundToolClient(client, "sdk-test", requirement);
    assert.equal(
      await fresh.call("example.echo", { value: "new contract" }),
      "new contract",
    );
    await f.service.stop();
    await assert.rejects(fresh.call("example.echo", { value: "offline" }));
    assert.equal(f.invocations(), 3);
  },
);

test('cached read Tools recover definition updates but cannot retry mutations or uncertain responses', async t => {
  const f = await fixture(t);
  f.service.start(); await f.service.waitReady();
  const published = structuredClone(registry), read = published.namespaces[0]!.tools[0]!;
  read.annotations = { readOnlyHint: true };
  const sync = async () => { f.service.connection.setRegistry(published); await f.service.connection.request('registry.sync', published); };
  await sync();
  const tools = serviceTools(f.client, 'sdk-test', [{ namespace: 'example', interfaceVersion: '1.0.0' }]);
  assert.equal(await tools.read('example.echo', { value: 'warm read' }), 'warm read');
  read.description = 'Updated read contract.'; await sync();
  assert.equal(await tools.read('example.echo', { value: 'same request' }), 'same request');
  assert.equal(f.invocations(), 2, 'The rejected stale binding must never reach the provider.');
  const request = f.client.request.bind(f.client); let calls = 0;
  f.client.request = async (method, params, options) => {
    if (method === 'tools.call') { calls++; throw new IvyError('tool_definition_changed', 'Uncertain response.', 'unknown'); }
    return request(method, params, options);
  };
  await assert.rejects(tools.read('example.echo', { value: 'uncertain' }), { outcome: 'unknown' });
  assert.equal(calls, 1);
  f.client.request = request;
  read.annotations = { readOnlyHint: false }; await sync();
  await assert.rejects(tools.read('example.echo', { value: 'became a mutation' }), { code: 'tool_definition_changed' });
  await assert.rejects(tools.read('example.echo', { value: 'still a mutation' }), { code: 'invalid_arguments' });
  assert.equal(f.invocations(), 2);
});

async function fixture(t: TestContext, start = true, callTimeoutMs = 1000) {
  const root = mkdtempSync(join(tmpdir(), "ivy-sdk-test-"));
  const reservation = createServer();
  await new Promise<void>((resolve) =>
    reservation.listen(0, "127.0.0.1", resolve),
  );
  const port = (reservation.address() as { port: number }).port;
  await new Promise<void>((resolve) => reservation.close(() => resolve()));
  const base = `http://127.0.0.1:${port}/ivy`;
  const server = new HiveServer({
    filename: join(root, "hive.sqlite"),
    publicBaseUrl: base,
    version: "test",
    buildId: digest("sdk-test"),
    listenPort: port,
    credentials: [
      { principalId: "client", digest: digest("test-client") },
      { principalId: "service", digest: digest("test-service") },
    ],
    callTimeoutMs,
  });
  if (start) await server.start();
  const client = new HiveClient(base, { credential: "test-client" });
  const states: string[] = [];
  let reconciled = 0,
    invocations = 0;
  const service = new ServiceClient({
    publicBaseUrl: base,
    credential: () => "test-service",
    identity: {
      serviceNodeId: "sdk-test",
      serviceName: "example",
      hostId: "fixture",
      version: "1.0.0",
      buildId: digest("sdk"),
      hiveProtocol: 1,
    },
    registry: () => registry,
    handlers: {
      "example.echo": (args) => {
        invocations++;
        return args["value"]!;
      },
    },
    reconcile: async (connection) => {
      reconciled++;
      await connection.request("system.status", {});
    },
    onState: (state) => states.push(state.status),
    reconnectMinMs: 50,
    reconnectMaxMs: 200,
    heartbeatMs: 100,
  });
  t.after(async () => {
    await service.stop();
    await server.close();
    assert.ok(relative(tmpdir(), root).startsWith("ivy-sdk-test-"));
    rmSync(root, { recursive: true, force: true });
  });
  return {
    root,
    base,
    server,
    client,
    service,
    states,
    reconciled: () => reconciled,
    invocations: () => invocations,
  };
}

test(
  "SDK recovers initial refusal, reconciles before readiness, pins discovery and stops its one loop",
  { timeout: 30_000 },
  async (t) => {
    const f = await fixture(t, false);
    f.service.start();
    f.service.start();
    await until(() => f.states.includes("offline"));
    await f.server.start();
    await f.service.waitReady();
    assert.equal(f.reconciled(), 1);
    const bound = await discover(f.client, "example.echo");
    await assert.rejects(
      callBound(
        f.client,
        bound,
        { value: "never dispatched under another caller" },
        "old-caller-intent",
        { expectedCallerPrincipalId: "unrelated-original-caller" },
      ),
      { code: "caller_changed" },
    );
    assert.equal(f.invocations(), 0);
    assert.equal(
      await callBound(
        f.client,
        bound,
        { value: "through SDK" },
        "stable-intent",
      ),
      "through SDK",
    );
    assert.equal(f.invocations(), 1);
    await assert.rejects(
      callBound(
        f.client,
        { ...bound, definitionHash: digest("changed") },
        { value: "never sent" },
      ),
      (error: unknown) =>
        error instanceof IvyError &&
        error.code === "tool_definition_changed" &&
        error.outcome === "not_executed",
    );
    assert.equal(f.invocations(), 1);
    const generation = f.service.connection.generation;
    f.service.connection.socket.terminate();
    await until(
      () => f.service.ready && f.service.connection.generation > generation,
    );
    assert.equal(f.reconciled(), 2);
    await f.service.stop();
    const count = f.states.length;
    await delay(300);
    assert.equal(f.states.length, count);
    assert.equal(f.service.ready, false);
  },
);

test(
  "HTTP and provider WebSocket carry complete fourteen-MiB management evidence and refuse oversized outgoing requests before invocation",
  { timeout: 30_000 },
  async (t) => {
    const f = await fixture(t, true, 20000);
    f.service.start();
    await f.service.waitReady();
    const binding = await discover(f.client, "example.echo"),
      payload = "x".repeat(14 * 1024 * 1024 - 4) + "🧪";
    assert.equal(Buffer.byteLength(payload), 14 * 1024 * 1024);
    assert.equal(
      await callBound(
        f.client,
        binding,
        { value: payload },
        "large-management",
      ),
      payload,
    );
    assert.equal(f.invocations(), 1);
    assert.equal(f.service.ready, true);
    await assert.rejects(
      callBound(
        f.client,
        binding,
        { value: "x".repeat(managementFrameBytes) },
        "oversized-management",
      ),
      (error: unknown) =>
        error instanceof IvyError &&
        error.code === "content_too_large" &&
        error.outcome === "not_executed",
    );
    assert.equal(f.invocations(), 1);
    assert.equal(
      await callBound(f.client, binding, { value: "still-ready" }),
      "still-ready",
    );
    // A slow reader leaves a legal large frame in the provider's write buffer.
    // Small replies and heartbeat requests must still share that connection.
    const connection = f.service.connection;
    const transport = (connection.socket as unknown as {
      _socket: import("node:net").Socket;
    })._socket;
    const before = f.invocations();
    transport.cork();
    const large = callBound(
      f.client,
      binding,
      { value: payload },
      "buffered-large-evidence",
    );
    void large.catch(() => undefined);
    let small: Promise<Wire.Json> | undefined;
    try {
      await until(() => connection.socket.bufferedAmount > 1024 * 1024);
      small = callBound(
        f.client,
        binding,
        { value: "concurrent-small-reply" },
        "buffered-small-reply",
      );
      void small.catch(() => undefined);
      await until(() => f.invocations() === before + 2);
      await delay(200);
      assert.equal(f.service.ready, true);
      assert.equal(f.service.connection, connection);
    } finally {
      transport.uncork();
    }
    assert.deepEqual(await Promise.all([large, small!]), [
      payload,
      "concurrent-small-reply",
    ]);
    assert.equal(f.service.connection, connection);
  },
);

test(
  "storage worker death preserves mutations and SDK obtains a new reconciled generation",
  { timeout: 30_000 },
  async (t) => {
    const f = await fixture(t);
    f.service.start();
    await f.service.waitReady();
    await f.client.request("contracts.register", {
      mutationId: await newOperationId(f.client),
      definition: {
        key: "test/text",
        version: "1.0.0",
        mediaType: "text/plain",
        owner: { kind: "agent" },
        retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
        specMarkdown: "Test fixture.",
      },
    });
    const request = {
      mutationId: await newOperationId(f.client),
      contractVersion: "1.0.0",
      references: {},
      create: {
        contractKey: "test/text",
        parentId: null,
        ownerObjectId: null,
        name: "saved",
      },
      content: { encoding: "text" as const, value: "survives" },
    };
    const saved = await f.client.request("objects.write", request);
    const generation = f.service.connection.generation;
    // Fault injection at the actual worker boundary, never exposed as a product API.
    await (f.server.worker as unknown as { worker: Worker }).worker.terminate();
    await until(
      () => f.service.ready && f.service.connection.generation > generation,
      20_000,
    );
    assert.deepEqual(await f.client.request("objects.write", request), saved);
    assert.equal(
      (await f.client.request("objects.read", { objectId: saved.object.id }))
        .content.encoding,
      "text",
    );
  },
);

test(
  "native JSON arguments retain exact values and operation context through HTTP and provider WebSockets without repeated payload validation",
  { timeout: 30_000 },
  async (t) => {
    const f = await fixture(t),
      received: { args: Wire.Json; caller: string; operationId?: string }[] =
        [];
    const jsonDefinition: Wire.ToolDefinition = {
      ...definition,
      name: "json",
      inputSchema: true,
      outputSchema: true,
    };
    const nullDefinition: Wire.ToolDefinition = {
      ...jsonDefinition,
      name: "null",
      inputSchema: { type: "null" },
      outputSchema: { type: "null" },
    };
    const objectDefinition: Wire.ToolDefinition = {
      ...jsonDefinition,
      name: "object-api",
    };
    const declared: Wire.RegistrySync = {
      ...registry,
      namespaces: [
        {
          ...registry.namespaces[0]!,
          tools: [definition, jsonDefinition, nullDefinition, objectDefinition],
        },
      ],
    };
    f.service.options.registry = () => declared;
    f.service.options.jsonHandlers = {
      "example.json": (args, context) => {
        received.push({
          args,
          caller: context.callerPrincipalId,
          ...(context.operationId ? { operationId: context.operationId } : {}),
        });
        return args;
      },
      "example.null": (args) => args,
    };
    let objectCalls = 0;
    f.service.options.handlers["example.object-api"] = (args) => {
      objectCalls++;
      return args;
    };
    f.service.start();
    const connection = await f.service.waitReady();
    const json = await discover(f.client, "example.json"),
      nil = await discover(f.client, "example.null");
    const values: Wire.Json[] = [
      null,
      false,
      4.5,
      "native scalar",
      ["a", null, { nested: [true, 7] }],
      { value: null },
    ];
    for (const [index, value] of values.entries())
      assert.deepEqual(
        await callBound(f.client, json, value, "native-value-" + index),
        value,
      );
    assert.deepEqual(
      received,
      values.map((args, index) => ({
        args,
        caller: "client",
        operationId: "native-value-" + index,
      })),
    );
    assert.equal(
      await callBound(connection, nil, null, "service-native-null"),
      null,
    );
    assert.deepEqual(await callBound(f.client, nil, {}), {});
    const echo = await discover(f.client, "example.echo");
    assert.equal(f.invocations(), 0);
    const object = await discover(f.client, "example.object-api");
    assert.deepEqual(await callBound(f.client, object, []), []);
    assert.equal(objectCalls, 1);
    assert.deepEqual(await callBound(f.client, object, { untouched: true }), {
      untouched: true,
    });
    assert.equal(objectCalls, 2);
    f.service.options.jsonHandlers["example.echo"] = (args) => args;
    assert.throws(
      () => connection.setRegistry(declared),
      (error: unknown) =>
        error instanceof IvyError && error.code === "registry_invalid",
    );
    delete f.service.options.jsonHandlers["example.echo"];
    assert.equal(
      await callBound(f.client, echo, { value: "existing object API" }),
      "existing object API",
    );
  },
);

test(
  "event polling releases empty batches, persists before ack and exposes poison replay",
  { timeout: 30_000 },
  async (t) => {
    const f = await fixture(t);
    f.service.start();
    await f.service.waitReady();
    await f.client.request("contracts.register", {
      mutationId: await newOperationId(f.client),
      definition: {
        key: "test/text",
        version: "1.0.0",
        mediaType: "text/plain",
        owner: { kind: "agent" },
        retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
        specMarkdown: "Test.",
      },
    });
    const connection = f.service.connection,
      subscription = { name: "saved-results", filter: {} };
    const idle = await connection.request("events.subscribe", subscription);
    assert.equal(idle.items.length, 0);
    await connection.request("events.ack", {
      name: subscription.name,
      throughSequence: idle.throughSequence,
    });
    const removal = {
      name: subscription.name,
      mutationId: await newOperationId(connection),
    };
    assert.deepEqual(await connection.request("events.unsubscribe", removal), {
      removed: true,
    });
    assert.deepEqual(await connection.request("events.unsubscribe", removal), {
      removed: true,
    });
    await f.client.request("objects.write", {
      mutationId: await newOperationId(f.client),
      contractVersion: "1.0.0",
      references: {},
      create: {
        contractKey: "test/text",
        parentId: null,
        ownerObjectId: null,
        name: "event",
      },
      content: { encoding: "text", value: "durable event" },
    });
    let poisonSeen = 0;
    await assert.rejects(
      consumeEvents(
        connection,
        subscription,
        async () => {
          throw new Error("consumer storage unavailable");
        },
        {
          onFailure: (_error, batch) => {
            poisonSeen = batch.items.length;
          },
        },
      ),
    );
    assert.equal(poisonSeen, 1);
    const outstanding = await connection.request(
      "events.subscribe",
      subscription,
    );
    assert.equal(outstanding.items.length, 1);
    let persisted = 0,
      waiting = false;
    const waitForEvents = connection.waitForEvents.bind(connection);
    t.mock.method(
      connection,
      "waitForEvents",
      async (...args: Parameters<ServiceConnection["waitForEvents"]>) => {
        waiting = true;
        try {
          await waitForEvents(...args);
        } finally {
          waiting = false;
        }
      },
    );
    const runner = consumeEvents(
      connection,
      subscription,
      async (batch) => {
        if (batch.items.length) {
          writeFileSync(
            join(f.root, "consumer-result.json"),
            JSON.stringify(batch),
          );
          persisted++;
        }
      },
      { pollMs: 10_000 },
    ).catch(() => undefined);
    await until(() => persisted === 1 && waiting);
    assert.equal(
      JSON.parse(readFileSync(join(f.root, "consumer-result.json"), "utf8"))
        .throughSequence,
      outstanding.throughSequence,
    );
    const startedAt = Date.now();
    await f.client.request("objects.write", {
      mutationId: await newOperationId(f.client),
      contractVersion: "1.0.0",
      references: {},
      create: {
        contractKey: "test/text",
        parentId: null,
        ownerObjectId: null,
        name: "event-wakeup",
      },
      content: { encoding: "text", value: "wake immediately" },
    });
    await until(() => persisted === 2, 2_000);
    assert.ok(
      Date.now() - startedAt < 2_000,
      "WebSocket hint bypassed the ten-second fallback poll",
    );
    await f.service.stop();
    await runner;
    assert.equal(persisted, 2);
  },
);

test(
  "registration pins definitions and handler functions; payloads pass without runtime schema validation",
  { timeout: 15000 },
  async (t) => {
    const f = await fixture(t);
    f.service.start();
    const connection = await f.service.waitReady();
    const declared = structuredClone(registry);
    const pending = connection.prepareRegistry(declared);
    declared.namespaces[0]!.tools[0]!.outputSchema = { type: "integer" };
    const snapshot = await pending;
    assert.deepEqual(snapshot.namespaces[0]!.tools[0]!.outputSchema, {
      type: "string",
    });
    assert.equal(
      Object.isFrozen(snapshot.namespaces[0]!.tools[0]!.outputSchema),
      true,
    );
    await connection.request("registry.sync", snapshot);
    const bound = await discover(f.client, "example.echo");
    assert.equal(
      await callBound(f.client, bound, {
        value: "original admitted definition",
      }),
      "original admitted definition",
    );
    f.service.options.handlers["example.echo"] = () => 123;
    assert.equal(
      await callBound(f.client, bound, { value: "bound handler" }),
      "bound handler",
    );
    connection.setRegistry(registry);
    assert.equal(
      await callBound(f.client, bound, { value: "opaque result" }),
      123,
    );
    delete f.service.options.handlers["example.echo"];
    await assert.rejects(connection.prepareRegistry(registry), {
      code: "registry_invalid",
    });
  },
);

test(
  "HTTP SDK exposes pre-correlation authentication refusal and never retries a timed-out effect",
  { timeout: 20_000 },
  async (t) => {
    const f = await fixture(t);
    await assert.rejects(
      new HiveClient(f.base, { credential: "invalid" }).request(
        "system.status",
        {},
      ),
      (error: unknown) =>
        error instanceof IvyError &&
        error.code === "unauthenticated" &&
        error.outcome === "not_executed",
    );
    f.service.options.handlers["example.echo"] = async () => {
      await delay(250);
      return "late";
    };
    f.service.start();
    await f.service.waitReady();
    let executed = 0;
    f.service.options.handlers["example.echo"] = async () => {
      executed++;
      await delay(250);
      return "late";
    };
    f.service.connection.setRegistry(registry);
    const bound = await discover(f.client, "example.echo");
    await assert.rejects(
      callBound(f.client, bound, { value: "test" }, "one-effect", {
        timeoutMs: 50,
      }),
      (error: unknown) =>
        error instanceof IvyError && error.outcome === "unknown",
    );
    await delay(350);
    assert.equal(executed, 1);
  },
);

test(
  "a service request deadline preserves its connection and unrelated calls without replay",
  { timeout: 15_000 },
  async (t) => {
    const f = await fixture(t);
    const executed: string[] = [];
    f.service.options.handlers["example.echo"] = async (args) => {
      const value = args["value"] as string;
      executed.push(value);
      if (value === "slow") await delay(300);
      return value;
    };
    f.service.start();
    await f.service.waitReady();
    const connection = f.service.connection;
    const binding = await discover(f.client, "example.echo");
    await assert.rejects(
      connection.request(
        "tools.call",
        {
          qualifiedName: binding.qualifiedName,
          serviceNodeId: binding.serviceNodeId,
          expectedDefinitionHash: binding.definitionHash,
          arguments: { value: "slow" },
          operationId: "single-slow-effect",
        },
        { timeoutMs: 100 },
      ),
      (error: unknown) =>
        error instanceof IvyError &&
        error.code === "deadline_exceeded" &&
        error.outcome === "unknown",
    );
    assert.equal(f.service.ready, true);
    assert.equal(f.service.connection, connection);
    assert.equal(
      await callBound(f.client, binding, { value: "still-ready" }),
      "still-ready",
    );
    await delay(350);
    assert.deepEqual(executed, ["slow", "still-ready"]);
    assert.equal(
      f.service.connection,
      connection,
      "a late reply does not replace the connection",
    );
  },
);

test(
  "service offline status retains the connection failure during heartbeat sleep",
  { timeout: 15_000 },
  async (t) => {
    const f = await fixture(t);
    const observations: Array<{
      status: string;
      code?: string;
      message?: string;
      phase?: string;
    }> = [];
    f.service.options.onState = (state) => observations.push(state);
    f.service.start();
    await f.service.waitReady();
    const connection = f.service.connection;
    connection.close(
      new IvyError("invalid_frame", "Malformed provider transport.", "unknown"),
    );
    await until(() => observations.some((state) => state.status === "offline"));
    const offline = observations.find((state) => state.status === "offline")!;
    assert.equal(offline.code, "invalid_frame");
    assert.equal(offline.message, "Malformed provider transport.");
    assert.equal(offline.phase, "heartbeat.wait");
    await until(() => f.service.ready && f.service.connection !== connection);
  },
);

test(
  "SDK shutdown cancels a hung initial credential dependency",
  { timeout: 5000 },
  async (t) => {
    const f = await fixture(t, false);
    f.service.options.credential = () => new Promise<string>(() => undefined);
    f.service.start();
    await delay(20);
    const started = Date.now();
    await f.service.stop();
    assert.ok(Date.now() - started < 500);
    assert.equal(f.states.at(-1), "stopped");
  },
);

test("scoped operation identities stay available beyond 10,000 live scopes", async () => {
  let statusReads = 0;
  const client = { request: async (method: string) => {
    assert.equal(method, "system.status");
    statusReads++;
    return { runtimeEpoch: "sdk-cache-fixture" };
  } } as unknown as RpcClient;
  const ids = await Promise.all(Array.from({ length: 10_050 }, (_, index) =>
    scopedOperationId(client, ["scope", index])));
  assert.equal(new Set(ids).size, ids.length);
  assert.equal(await scopedOperationId(client, ["scope", 0]), ids[0]);
  assert.equal(statusReads, 1);
});
