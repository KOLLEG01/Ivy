import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";
import { createHash, randomUUID } from "node:crypto";
import { once } from "node:events";
import { WebSocket } from "ws";
import {
  Client,
  StreamableHTTPClientTransport,
} from "@modelcontextprotocol/client";
import type { Transport } from "@modelcontextprotocol/client";
import { Ajv2020 } from "ajv/dist/2020.js";
import type { AnySchema } from "ajv";
import { HiveServer } from "../services/hive/src/server.js";
import { digest } from "../packages/contracts/src/canonical.js";
import { toolDefinitionHash } from '../packages/contracts/src/tool-definition.js';
import { coreMcpNames, publicCoreMcpMethods } from '../services/hive/src/core-mcp.js';
import { mcpDiscoveryResultBytes } from '../packages/contracts/src/limits.js';
import { operationId } from "../packages/contracts/src/operation-id.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import { managementFrameBytes } from "../packages/contracts/src/limits.js";
import { validateToolRouting } from "../packages/contracts/src/transport-validation.js";
import type { WireError } from "../packages/contracts/src/errors.js";
import type { ToolDefinition } from "../services/hive/src/registry.js";
import { HiveClient } from "../packages/sdk/src/client.js";
import { Agent as HttpAgent, request as httpRequest } from "node:http";
import { HiveOAuth } from "../services/hive/src/oauth.js";
import { mcpSurfaceBindings } from "../services/hive/src/mcp.js";
import type { McpCallObservation } from "../services/hive/src/mcp.js";
import type { McpBinding } from "../services/hive/src/discovery.js";
import { hiveMcpInstructions } from "../instructions/hive-mcp.js";

const clientToken = "synthetic-http-client",
  providerToken = "synthetic-live-provider";
const credentials = [
  { principalId: "client", digest: digest(clientToken) },
  { principalId: "provider", digest: digest(providerToken) },
];

test("explicit user tools suppress stale development bindings with the same MCP name", () => {
  const binding = (name: string, surface: "ivy" | "ivy_dev" | undefined, serviceNodeId: string) => ({
    serviceName: "agent-manager",
    qualifiedName: "management." + name,
    definition: {
      namespace: "management",
      name,
      interfaceVersion: "1.0.0",
      description: name,
      discovery: { mcp: { name, ...(surface ? { surface } : {}) } },
      inputSchema: { type: "object", additionalProperties: false },
      outputSchema: { type: "object", additionalProperties: false },
    },
    definitionHash: digest(serviceNodeId + name),
    provider: { serviceNodeId, hostId: serviceNodeId.split(".")[0]!, available: true },
    guideMarkdown: "",
  }) as McpBinding;
  const bindings = [
    binding("agent_manager_status", "ivy", "HOST-B.agent-manager"),
    binding("agent_manager_status", undefined, "HOST-A.agent-manager"),
    binding("agent_manager_configure", "ivy_dev", "HOST-B.agent-manager"),
  ];
  assert.deepEqual(
    mcpSurfaceBindings(bindings, "ivy").map(value => value.provider?.serviceNodeId),
    ["HOST-B.agent-manager"],
  );
  assert.deepEqual(
    mcpSurfaceBindings(bindings, "ivy_dev").map(value => value.definition.discovery?.mcp?.name),
    ["agent_manager_configure"],
  );
});
const tool: ToolDefinition = {
  namespace: "sample",
  name: "EchoCase/read",
  interfaceVersion: "1.0.0",
  description: "Loopback transport fixture, no external side effect.",
  inputSchema: {
    type: "object",
    properties: { value: { type: "string" } },
    required: ["value"],
    additionalProperties: false,
  },
  outputSchema: {
    type: "object",
    properties: { echoed: { type: "string" } },
    required: ["echoed"],
    additionalProperties: false,
  },
  annotations: { readOnlyHint: true },
    discovery: { mcp: { name: 'fixture_provider_echo' } },
};
const valueTools: ToolDefinition[] = [
  {
    namespace: "sample",
    name: "Null/read",
    interfaceVersion: "1.0.0",
    description: "Return null.",
    inputSchema: { type: "object", additionalProperties: false },
    outputSchema: { type: "null" },
    annotations: { readOnlyHint: true },
    discovery: { mcp: { name: 'fixture_provider_null' } },
  },
  {
    namespace: "sample",
    name: "Empty/read",
    interfaceVersion: "1.0.0",
    description: "Acknowledge completion without data.",
    inputSchema: { type: "object", additionalProperties: false },
    outputSchema: { type: "object", properties: {}, additionalProperties: false },
    annotations: { readOnlyHint: true },
    discovery: { mcp: { name: 'fixture_provider_empty' } },
  },
  {
    namespace: "sample",
    name: "Nullable/read",
    interfaceVersion: "1.0.0",
    description: "Return a value or null when absent.",
    inputSchema: { type: "object", additionalProperties: false },
    outputSchema: { anyOf: [{ type: "string" }, { type: "null" }] },
    annotations: { readOnlyHint: true },
    discovery: { mcp: { name: 'fixture_provider_nullable' } },
  },
  {
    namespace: "sample",
    name: "Array/read",
    interfaceVersion: "1.0.0",
    description: "Return an array.",
    inputSchema: { type: "object", additionalProperties: false },
    outputSchema: { type: "array", items: { type: "number" } },
    annotations: { readOnlyHint: true },
    discovery: { mcp: { name: 'fixture_provider_array' } },
  },
  {
    namespace: "sample",
    name: "Scalar/read",
    interfaceVersion: "1.0.0",
    description: "Return a scalar.",
    inputSchema: { type: "object", additionalProperties: false },
    outputSchema: { type: "string" },
    annotations: { readOnlyHint: true },
    discovery: { mcp: { name: 'fixture_provider_scalar' } },
  },
];
const registry = {
  namespaces: [
    {
      namespace: "sample",
      description: "Loopback",
      guideMarkdown: "",
      tools: [tool],
      notifications: [
        {
          name: "progress",
          version: "1.0.0",
          description: "Transient loopback progress.",
          payloadSchema: {
            type: "object",
            properties: { active: { type: "boolean" } },
            required: ["active"],
            additionalProperties: false,
          },
        },
      ],
      topics: [],
      inventoryKinds: [],
    },
  ],
  contracts: [],
  requiredContracts: [],
};
interface Envelope {
  jsonrpc: "2.0";
  id: string | number | null;
  result?: unknown;
  error?: WireError;
  method?: string;
  params?: Record<string, unknown>;
}

test("manual routing checks use the canonical service and resource limits", () => {
  const common = {
    qualifiedName: "sample.read",
    expectedDefinitionHash: digest("definition"),
    arguments: {},
  };
  assert.doesNotThrow(() =>
    validateToolRouting(
      {
        ...common,
        serviceName: "valid-service",
        resourceRef: {
          serviceNodeId: "node",
          namespace: "sample",
          kind: "thread",
          nativeId: "x".repeat(300),
        },
      },
      false,
    ),
  );
  assert.throws(
    () =>
      validateToolRouting({ ...common, serviceName: "s".repeat(65) }, false),
    { code: "invalid_arguments" },
  );
  assert.throws(
    () =>
      validateToolRouting(
        {
          ...common,
          resourceRef: {
            serviceNodeId: "node",
            namespace: "sample",
            kind: "thread",
            nativeId: "x".repeat(1025),
          },
        },
        false,
      ),
    { code: "invalid_arguments" },
  );
});

test("runtime reset bootstrap keeps public Hive fenced to its ui publisher", async (t) => {
  const directory = mkdtempSync(join(tmpdir(), "ivy-reset-fence-"));
  const server = new HiveServer({
    filename: join(directory, "hive.sqlite"),
    publicBaseUrl: "https://hive.test/ivy",
    version: "test",
    buildId: digest("reset-fence"),
    credentials,
    listenPort: 0,
    resetBootstrapCredentialDigest: credentials[0]!.digest,
  });
  const address = await server.start(),
    base = `http://127.0.0.1:${address.port}/ivy`;
  t.after(async () => {
    await server.close();
    rmSync(directory, { recursive: true, force: true });
  });
  const rpc = async (token: string, method: string) => {
    const response = await fetch(base + "/api/v1/rpc", {
      method: "POST",
      headers: {
        Authorization: "Bearer " + token,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method,
        params: { uiId: "missing" },
      }),
    });
    return (await response.json()) as Envelope;
  };
  assert.equal(
    (await rpc(providerToken, "uis.inspect")).error?.data.code,
    "maintenance_active",
  );
  assert.equal(
    (await rpc(clientToken, "system.status")).error?.data.code,
    "maintenance_active",
  );
  assert.equal(
    (await rpc(clientToken, "uis.inspect")).error?.data.code,
    "not_found",
  );
  const root = await fetch(base + "/");
  assert.equal(root.status, 400);
  assert.equal(
    ((await root.json()) as { error: WireError }).error.data.code,
    "maintenance_active",
  );
  assert.equal((await fetch(base + "/health/ready")).status, 503);
});

test("MCP rejects every supplied noncanonical Origin before dispatch", async (t) => {
  const { base } = await fixture(t);
  const initialize = JSON.stringify({
    jsonrpc: "2.0",
    id: "origin-check",
    method: "initialize",
    params: {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "origin-check", version: "1" },
    },
  });
  const request = (
    origin?: string,
    credential = clientToken,
    body = initialize,
  ) =>
    fetch(base + "/mcp", {
      method: "POST",
      headers: {
        Authorization: "Bearer " + credential,
        "Content-Type": "application/json",
        Accept: "application/json, text/event-stream",
        ...(origin === undefined ? {} : { Origin: origin }),
      },
      body,
    });
  for (const origin of ["https://other.test", "null", ":malformed"]) {
    const rejected = await request(origin, clientToken, "not-json");
    assert.equal(rejected.status, 403);
    assert.equal(
      ((await rejected.json()) as Envelope).error?.data.code,
      "forbidden",
    );
  }
  assert.equal((await request()).status, 200);
  assert.equal((await request("https://hive.test")).status, 200);
  const unauthenticated = await request(undefined, "wrong-token");
  assert.equal(unauthenticated.status, 401);
  assert.match(
    unauthenticated.headers.get("www-authenticate") ?? "",
    /scope="hive"/,
  );
});

test("HTTP keep-alive and WebSocket authenticate once while retaining distinct callers on a shared socket", async (t) => {
  const { server, base } = await fixture(t),
    original = server.worker.request.bind(server.worker);
  let checks = 0;
  t.mock.method(
    server.worker,
    "request",
    async (...args: Parameters<typeof server.worker.request>) => {
      if (args[0].action === "authenticate") checks++;
      return original(...args);
    },
  );
  const agent = new HttpAgent({ keepAlive: true, maxSockets: 1 });
  t.after(() => agent.destroy());
  const status = (token: string) =>
    new Promise<Envelope>((resolve, reject) => {
      const body = JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "system.status",
        params: {},
      });
      const req = httpRequest(
        base + "/api/v1/rpc",
        {
          agent,
          method: "POST",
          headers: {
            Authorization: "Bearer " + token,
            "Content-Type": "application/json",
            "Content-Length": Buffer.byteLength(body),
          },
        },
        (res) => {
          let data = "";
          res.setEncoding("utf8");
          res.on("data", (part) => (data += part));
          res.on("end", () => resolve(JSON.parse(data) as Envelope));
        },
      );
      req.on("error", reject);
      req.end(body);
    });
  for (let i = 0; i < 3; i++)
    assert.equal((await status(clientToken)).error, undefined);
  assert.equal(checks, 1);
  assert.equal(
    ((await status(providerToken)).result as { callerPrincipalId: string })
      .callerPrincipalId,
    "provider",
  );
  assert.equal(
    ((await status(clientToken)).result as { callerPrincipalId: string })
      .callerPrincipalId,
    "client",
  );
  assert.equal(checks, 2);
  const connected = await peer(t, base, {
    Authorization: "Bearer " + clientToken,
  });
  assert.equal(checks, 3);
  for (let i = 0; i < 3; i++) await connected.call("system.status", {});
  assert.equal(checks, 3);
});

test("immutable JSON ui bytes preserve canonical numeric-key ordering and the pinned hash after later revisions", async (t) => {
  const { base } = await fixture(t),
    client = new HiveClient(base, { credential: clientToken });
  const mutate = async (nonce: string) =>
    operationId(
      (await client.request("system.status", {})).runtimeEpoch,
      Date.now(),
      nonce,
    );
  for (const [key, mediaType] of [
    ["fixture/html", "text/html"],
    ["fixture/json", "application/json"],
  ] as const) {
    await client.request("contracts.register", {
      mutationId: await mutate(key),
      definition: {
        key,
        version: "1.0.0",
        owner: { kind: "agent" },
        mediaType,
        retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
        specMarkdown: "Immutable asset byte fixture.",
        ...(mediaType === "application/json"
          ? {
              jsonSchema: {
                type: "object",
                additionalProperties: { type: "number" },
              },
            }
          : {}),
      },
    });
  }
  await client.request("objects.write", {
    mutationId: await mutate("html"),
    references: {},
    create: {
      contractKey: "fixture/html",
      parentId: null,
      ownerObjectId: null,
      name: "index",
    },
    contractVersion: "1.0.0",
    content: { encoding: "text", value: "<h1>Fixture</h1>" },
  });
  const json = await client.request("objects.write", {
    mutationId: await mutate("json"),
    references: {},
    create: {
      contractKey: "fixture/json",
      parentId: null,
      ownerObjectId: null,
      name: "data",
    },
    contractVersion: "1.0.0",
    content: { encoding: "json", value: { "2": 2, "10": 10, z: 0 } },
  });
  const stage = async (releaseId: string, path: string, mediaType: string, content: string) => {
    const bytes = Buffer.from(content);
    const asset = { path, mediaType, contentHash: digest(bytes), byteLength: bytes.length };
    await client.request("uis.stageAsset", {
      uiId: "bytes", releaseId, asset, base64: bytes.toString("base64"),
      mutationId: await mutate("stage-" + releaseId + "-" + path),
    });
    return asset;
  };
  const htmlAsset = await stage("r1", "index.html", "text/html", "<h1>Fixture</h1>");
  const firstJsonAsset = await stage("r1", "data.json", "application/json", '{"10":10,"2":2,"z":0}');
  await client.request("uis.deploy", {
    mutationId: await mutate("deploy"),
    expectedReleaseId: null,
    metadata: {
      uiId: "bytes",
      slug: "byte-view",
      displayName: "Bytes",
      description: "Exact bytes",
      iconKey: "ui",
    },
    release: {
      releaseId: "r1",
      entryPath: "index.html",
      requirements: { hiveProtocol: 1, contracts: [], services: [] },
      assets: [htmlAsset, firstJsonAsset],
    },
  });
  await client.request("objects.write", {
    mutationId: await mutate("later-json"),
    objectId: json.object.id,
    expectedRevision: 1,
    contractVersion: "1.0.0",
    references: {},
    content: { encoding: "json", value: { changed: 99 } },
  });
  const nextHtmlAsset = await stage("r2", "index.html", "text/html", "<h1>Fixture</h1>");
  const nextJsonAsset = await stage("r2", "data.json", "application/json", '{"changed":99}');
  const url = base + "/ui/bytes/releases/r1/data.json",
    headers = { Authorization: "Bearer " + clientToken };
  const response = await fetch(url, { headers }),
    body = await response.text();
  assert.equal(response.status, 200);
  assert.equal(body, '{"10":10,"2":2,"z":0}');
  assert.equal(digest(body), json.revision.contentHash);
  assert.equal(response.headers.get("etag"), '"' + digest(body) + '"');
  assert.equal(
    Number(response.headers.get("content-length")),
    Buffer.byteLength(body),
  );
  assert.equal(
    (
      await fetch(url, {
        headers: { ...headers, "If-None-Match": response.headers.get("etag")! },
      })
    ).status,
    304,
  );
  const stableUrl = base + "/ui/bytes/data.json";
  const stable = await fetch(stableUrl, { headers });
  assert.equal(stable.status, 200);
  assert.equal(stable.headers.get("cache-control"), "private, no-cache");
  assert.equal(await stable.text(), body);
  const entry = await fetch(base + "/ui/bytes/", { headers, redirect: "manual" });
  assert.equal(entry.status, 200);
  assert.equal(await entry.text(), "<h1>Fixture</h1>");
  const bare = await fetch(base + '/byte-view?mode=test', { headers, redirect: 'manual' });
  assert.equal(bare.status, 303);
  assert.equal(bare.headers.get('location'), new URL(base).pathname + '/byte-view/?mode=test');
  const shortEntry = await fetch(base + '/byte-view/', { headers });
  assert.equal(await shortEntry.text(), '<h1>Fixture</h1>');
  const shortAsset = await fetch(base + '/byte-view/data.json', { headers });
  assert.equal(await shortAsset.text(), body);
  assert.equal(shortAsset.headers.get('etag'), stable.headers.get('etag'));
  assert.equal((await fetch(base + '/byte-view/data.json', { headers: { ...headers, 'If-None-Match': shortAsset.headers.get('etag')! } })).status, 304);
  const unauthenticated = await new Promise<{ status: number | undefined; location: string | undefined }>((resolve, reject) => {
    const request = httpRequest(base + '/byte-view/', { headers: { 'sec-fetch-mode': 'navigate', 'sec-fetch-dest': 'document' } }, response => {
      response.resume();
      response.on('end', () => resolve({ status: response.statusCode, location: response.headers.location }));
    });
    request.on('error', reject); request.end();
  });
  assert.equal(unauthenticated.status, 303);
  assert.match(unauthenticated.location!, /\/login\?returnTo=/);
  assert.equal((await fetch(base + '/byte-view/data.json')).status, 401);
  await client.request("uis.deploy", {
    mutationId: await mutate("deploy-r2"),
    expectedReleaseId: "r1",
    metadata: {
      uiId: "bytes",
      slug: "byte-view",
      displayName: "Bytes",
      description: "Exact bytes",
      iconKey: "ui",
    },
    release: {
      releaseId: "r2",
      entryPath: "index.html",
      requirements: { hiveProtocol: 1, contracts: [], services: [] },
      assets: [nextHtmlAsset, nextJsonAsset],
    },
  });
  const current = await fetch(stableUrl, {
    headers: { ...headers, "If-None-Match": stable.headers.get("etag")! },
  });
  assert.equal(current.status, 200);
  assert.equal(await current.text(), '{"changed":99}');
  assert.equal(await (await fetch(base + '/byte-view/data.json', { headers })).text(), '{"changed":99}');
  assert.notEqual(current.headers.get("etag"), stable.headers.get("etag"));
  assert.equal(
    (await fetch(stableUrl, {
      headers: { ...headers, "If-None-Match": current.headers.get("etag")! },
    })).status,
    304,
  );
  assert.equal(await (await fetch(url, { headers })).text(), body);
});

async function fixture(
  t: TestContext,
  prefix = "/ivy",
  overrides: Partial<ConstructorParameters<typeof HiveServer>[0]> = {},
) {
  const directory = mkdtempSync(join(tmpdir(), "ivy-network-test-"));
  const server = new HiveServer({
    filename: join(directory, "hive.sqlite"),
    publicBaseUrl: "https://hive.test" + prefix,
    version: "0.1.0-test",
    buildId: digest("network-test"),
    credentials,
    listenPort: 0,
    callTimeoutMs: 200,
    ...overrides,
  });
  const address = await server.start();
  const base = `http://127.0.0.1:${address.port}${prefix}`;
  t.after(async () => {
    await server.close();
    assert.ok(relative(tmpdir(), directory).startsWith("ivy-network-test-"));
    rmSync(directory, { recursive: true, force: true });
  });
  const rpc = async (method: string, params: unknown, token = clientToken) => {
    const response = await fetch(base + "/api/v1/rpc", {
      method: "POST",
      headers: {
        Authorization: "Bearer " + token,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: randomUUID(),
        method,
        params,
      }),
    });
    const body = (await response.json()) as Envelope;
    return { response, body };
  };
  const status = await rpc("system.status", {}),
    runtimeEpoch = (status.body.result as { runtimeEpoch: string })
      .runtimeEpoch;
  const mutation = (nonce: string) =>
    operationId(runtimeEpoch, Date.now(), nonce);
  return { server, base, rpc, mutation };
}

async function oauthTokens(server: HiveServer) {
  const oauth = (server as unknown as { oauth: HiveOAuth }).oauth;
  const redirectUri = "http://127.0.0.1/callback",
    verifier = "v".repeat(64);
  const clientId = String(
    oauth.register({
      client_name: "Transport expiry fixture",
      redirect_uris: [redirectUri],
      token_endpoint_auth_method: "none",
    })["client_id"],
  );
  const authorize = new URL(
    oauth.base.href.replace(/\/$/, "") + "/oauth/authorize",
  );
  for (const [key, value] of Object.entries({
    response_type: "code",
    client_id: clientId,
    redirect_uri: redirectUri,
    state: "s".repeat(48),
    code_challenge: createHash("sha256").update(verifier).digest("base64url"),
    code_challenge_method: "S256",
    resource: oauth.resource,
    scope: "hive",
  }))
    authorize.searchParams.set(key, value);
  const transaction = oauth.begin(authorize).transaction;
  const code = new URL(
    oauth.authorize(transaction, credentials[0]!.digest),
  ).searchParams.get("code")!;
  const exchange = (form: URLSearchParams) =>
    oauth.token(form) as Promise<{
      access_token: string;
      refresh_token: string;
    }>;
  const initial = await exchange(
    new URLSearchParams({
      grant_type: "authorization_code",
      code,
      client_id: clientId,
      redirect_uri: redirectUri,
      code_verifier: verifier,
      resource: oauth.resource,
    }),
  );
  const refresh = (refreshToken: string) =>
    exchange(
      new URLSearchParams({
        grant_type: "refresh_token",
        refresh_token: refreshToken,
        client_id: clientId,
        resource: oauth.resource,
      }),
    );
  return { ...initial, refresh };
}

async function mcpJson(response: Response): Promise<Record<string, unknown>> {
  const text = await response.text(),
    data = /^data:\s*(.+)$/m.exec(text)?.[1];
  return JSON.parse(data ?? text) as Record<string, unknown>;
}

async function peer(
  t: TestContext,
  url: string,
  headers: Record<string, string> = {
    Authorization: "Bearer " + providerToken,
  },
) {
  const socket = new WebSocket(url.replace("http:", "ws:") + "/ws", {
    headers,
  });
  const pending = new Map<
    string,
    {
      resolve: (value: unknown) => void;
      reject: (error: Error) => void;
      timer: NodeJS.Timeout;
    }
  >();
  let invocation: ((frame: Envelope) => void) | null = null;
  socket.on("message", (bytes) => {
    const frame = JSON.parse(bytes.toString()) as Envelope;
    if (frame.method === "provider.invoke") {
      invocation?.(frame);
      return;
    }
    const item = pending.get(String(frame.id));
    if (!item) return;
    clearTimeout(item.timer);
    pending.delete(String(frame.id));
    if (frame.error)
      item.reject(
        new IvyError(
          frame.error.data.code,
          frame.error.message,
          frame.error.data.outcome,
        ),
      );
    else item.resolve(frame.result);
  });
  socket.on("error", () => undefined);
  socket.on("close", () => {
    for (const item of pending.values()) {
      clearTimeout(item.timer);
      item.reject(new Error("test peer disconnected"));
    }
    pending.clear();
  });
  await once(socket, "open");
  t.after(() => socket.terminate());
  const call = <T>(method: string, params: unknown): Promise<T> =>
    new Promise((resolve, reject) => {
      const id = randomUUID(),
        timer = setTimeout(() => {
          pending.delete(id);
          reject(new Error("test peer response timeout"));
        }, 5000);
      pending.set(id, {
        resolve: (value) => resolve(value as T),
        reject,
        timer,
      });
      socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
    });
  return {
    socket,
    call,
    onInvocation: (handler: (frame: Envelope) => void) => {
      invocation = handler;
    },
  };
}
async function ready(
  provider: Awaited<ReturnType<typeof peer>>,
  id = "loopback",
) {
  await provider.call("service.connect", {
    serviceNodeId: id,
    hostId: "test-host",
    serviceName: "fixture-provider",
    version: "test",
    buildId: digest("fixture"),
    hiveProtocol: 1,
  });
  await provider.call("registry.sync", registry);
  await provider.call("service.heartbeat", { ready: true, diagnostics: [] });
}

test('MCP calls typed service and Wiki tools directly while preserving owners and operation identities', { timeout: 30_000 }, async t => {
  const { base, mutation } = await fixture(t), first = await peer(t, base), second = await peer(t, base);
  await ready(first); await ready(second, 'second');
  const exposed = { ...tool, discovery: { mcp: { name: 'fixture_provider_echo' } } };
  const internal = { ...tool, name: 'internal' }; delete internal.discovery;
  const write = { ...exposed, name: 'write', annotations: { readOnlyHint: false }, discovery: { mcp: { name: 'fixture_provider_write' } },
    inputSchema: { type: 'object', properties: { operationId: { type: 'string' }, value: { type: 'string' } }, required: ['operationId', 'value'], additionalProperties: false } };
  const published = { ...registry, namespaces: [{ ...registry.namespaces[0]!, tools: [exposed, internal, write] }] };
  await first.call('registry.sync', published); await second.call('registry.sync', published);
  const frames: Envelope[] = [];
  first.onInvocation(frame => { frames.push(frame); first.socket.send(JSON.stringify({ jsonrpc: '2.0', id: frame.id, result: { echoed: 'direct' } })); });
  let secondCalls = 0; second.onInvocation(() => { secondCalls++; });
  const mcp = new Client({ name: 'direct-surface', version: '1' });
  await mcp.connect(new StreamableHTTPClientTransport(new URL(base + '/mcp-dev'), { requestInit: { headers: { Authorization: 'Bearer ' + clientToken } } }) as unknown as Transport);
  t.after(() => mcp.close());
  const userMcp = new Client({ name: 'direct-user-surface', version: '1' });
  await userMcp.connect(new StreamableHTTPClientTransport(new URL(base + '/mcp'), { requestInit: { headers: { Authorization: 'Bearer ' + clientToken } } }) as unknown as Transport);
  t.after(() => userMcp.close());
  const catalog = (await mcp.listTools()).tools;
  assert.equal(catalog.filter(item => item.name === 'fixture_provider_echo').length, 1);
  const reply = await mcp.callTool({ name: 'fixture_provider_echo', arguments: { serviceNodeId: 'loopback', input: { value: 'direct' } } });
  assert.deepEqual(reply.structuredContent, { result: { echoed: 'direct' } });
  assert.deepEqual(frames[0]!.params?.['arguments'], { value: 'direct' });
  const validator = new Ajv2020({ strict: false, validateFormats: false });
  assert.ok(validator.compile(catalog.find(item => item.name === 'fixture_provider_echo')!.outputSchema as AnySchema)(reply.structuredContent));
  const writeReply = await mcp.callTool({ name: 'fixture_provider_write', arguments: { serviceNodeId: 'loopback', input: { operationId: 'retained-action', value: 'direct' } } });
  assert.equal(writeReply.isError, undefined);
  assert.equal(frames[1]!.params?.['operationId'], 'retained-action');
  const absentOwner = await mcp.callTool({ name: 'fixture_provider_echo', arguments: { input: { value: 'direct' } } });
  assert.equal(absentOwner.isError, true); assert.equal(frames.length, 2);
  await assert.rejects(mcp.callTool({ name: 'fixture_provider_internal', arguments: { serviceNodeId: 'loopback', input: { value: 'hidden' } } }), (error: unknown) => (error as { code?: number }).code === -32601);
  await first.call('service.heartbeat', { ready: false, diagnostics: [] });
  const unavailable = await mcp.callTool({ name: 'fixture_provider_echo', arguments: { serviceNodeId: 'loopback', input: { value: 'direct' } } });
  assert.equal((unavailable.structuredContent as { error: WireError }).error.data.code, 'service_not_ready');
  assert.equal(secondCalls, 0);
  const pageContract = { key: 'wiki/page', version: '1.0.0', owner: { kind: 'agent' }, mediaType: 'text/markdown', specMarkdown: 'Wiki fixture', retention: { objects: { mode: 'retain' }, revisions: { mode: 'all' } } };
  assert.equal((await userMcp.callTool({ name: 'hive_schema_register', arguments: { mutationId: mutation('wiki-contract'), definition: pageContract } })).isError, undefined);
  const created = await userMcp.callTool({ name: 'wiki_create', arguments: { mutationId: mutation('wiki-create'), title: 'Direct Wiki', markdown: 'Markdown body' } });
  assert.equal(created.isError, undefined);
  const objectId = (created.structuredContent as { result: { object: { id: string } } }).result.object.id;
  const read = await userMcp.callTool({ name: 'wiki_read', arguments: { objectId, revision: 1 } });
  assert.equal(read.isError, undefined);
  assert.ok(validator.compile((await userMcp.listTools()).tools.find(item => item.name === 'wiki_read')!.outputSchema as AnySchema)(read.structuredContent));
});

test("direct service calls, replies and provider events bypass a blocked storage request on the same connection", async (t) => {
  const { server, base } = await fixture(t),
    provider = await peer(t, base),
    client = await peer(t, base, { Authorization: "Bearer " + clientToken }),
    ignored = await peer(t, base, { Authorization: "Bearer " + clientToken });
  await ready(provider);
  await ready(client, "consumer");
  await ignored.call("notifications.subscribe", {
    filters: [
      {
        namespace: "sample",
        name: "progress",
        version: "1.0.0",
        serviceNodeId: "another-provider",
      },
    ],
  });
  let leaked = false;
  ignored.socket.on("message", (bytes) => {
    const frame = JSON.parse(bytes.toString()) as Envelope;
    if (frame.method === "notifications.provider") leaked = true;
  });
  provider.onInvocation((frame) =>
    provider.socket.send(
      JSON.stringify({
        jsonrpc: "2.0",
        id: frame.id,
        result: { echoed: "immediate" },
      }),
    ),
  );
  const original = server.worker.request.bind(server.worker);
  let release!: () => void, entered!: () => void;
  const blocked = new Promise<void>((resolve) => {
      release = resolve;
    }),
    started = new Promise<void>((resolve) => {
      entered = resolve;
    });
  let storageCalls = 0;
  t.mock.method(
    server.worker,
    "request",
    async (...args: Parameters<typeof server.worker.request>) => {
      storageCalls++;
      const request = args[0];
      if (
        request.action === "execute" &&
        (request.request as { method?: string }).method === "system.status"
      ) {
        entered();
        await blocked;
      }
      return original(...args);
    },
  );
  const pending = client.call("system.status", {});
  await started;
  const before = storageCalls;
  try {
    assert.deepEqual(
      await client.call("notifications.subscribe", {
        filters: [
          {
            namespace: "sample",
            name: "progress",
            version: "1.0.0",
            serviceNodeId: "loopback",
          },
        ],
      }),
      { subscribed: 1 },
    );
    assert.deepEqual(
      await client.call("tools.call", {
        serviceNodeId: "loopback",
        qualifiedName: "sample.EchoCase/read",
        expectedDefinitionHash: toolDefinitionHash(tool),
        arguments: { value: "hello" },
      }),
      { echoed: "immediate" },
    );
    assert.deepEqual(
      await client.call("discovery.call", {
        serviceName: "fixture-provider",
        serviceNodeId: "loopback",
        qualifiedName: "sample.EchoCase/read",
        expectedDefinitionHash: toolDefinitionHash(tool),
        arguments: { value: "hello" },
      }),
      { echoed: "immediate" },
    );
    const event = new Promise<Envelope>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error("provider notification was blocked by storage")),
        1000,
      );
      client.socket.on("message", (bytes) => {
        const frame = JSON.parse(bytes.toString()) as Envelope;
        if (frame.method === "notifications.provider") {
          clearTimeout(timer);
          resolve(frame);
        }
      });
    });
    provider.socket.send(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "service.notification",
        params: {
          namespace: "sample",
          name: "progress",
          version: "1.0.0",
          payload: { active: true },
        },
      }),
    );
    assert.equal((await event).params?.["serviceNodeId"], "loopback");
    await new Promise((resolve) => {
      setTimeout(resolve, 50);
    });
    assert.equal(leaked, false);
    assert.equal(storageCalls, before);
  } finally {
    release();
    await pending;
  }
});

test("UI invalidations require an explicit subscription and filter committed object changes", async (t) => {
  const { base, mutation } = await fixture(t);
  const client = new HiveClient(base, { credential: clientToken });
  await client.request("contracts.register", {
    mutationId: mutation("ui-change-contract"),
    definition: { key: "test/ui-change", version: "1.0.0", owner: { kind: "agent" }, mediaType: "text/plain",
      retention: { objects: { mode: "retain" }, revisions: { mode: "all" } }, specMarkdown: "UI invalidation fixture." },
  });
  const browser = await peer(t, base, { Authorization: "Bearer " + clientToken });
  const ignored = await peer(t, base, { Authorization: "Bearer " + clientToken });
  const changes: string[][] = [], ignoredChanges: unknown[] = [];
  browser.socket.on("message", bytes => {
    const frame = JSON.parse(bytes.toString());
    if (frame.method === "notifications.changed" && frame.params.scopes.length) changes.push(frame.params.scopes);
  });
  ignored.socket.on("message", bytes => {
    const frame = JSON.parse(bytes.toString());
    if (frame.method === "notifications.changed" && frame.params.scopes.length) ignoredChanges.push(frame);
  });
  assert.deepEqual(await browser.call("notifications.subscribe", { filters: [], changes: ["objects/test"] }), { subscribed: 0, changes: 1 });
  await ignored.call("notifications.subscribe", { filters: [], changes: ["objects/wiki"] });
  const written = await client.request("objects.write", {
    mutationId: mutation("ui-change-write"), contractVersion: "1.0.0", references: {},
    create: { contractKey: "test/ui-change", parentId: null, ownerObjectId: null, name: "Changed" },
    content: { encoding: "text", value: "Committed content" },
  });
  for (let attempt = 0; !changes.length && attempt < 50; attempt++) await new Promise(resolve => setTimeout(resolve, 10));
  assert.deepEqual(changes, [["objects/test/ui-change"]]);
  assert.deepEqual(ignoredChanges, []);
  assert.equal((await client.request("objects.read", { objectId: written.object.id })).revision.revision, 1);
  await assert.rejects(client.request("objects.write", {
    mutationId: mutation("ui-change-conflict"), objectId: written.object.id, expectedRevision: 99, contractVersion: "1.0.0",
    references: {}, content: { encoding: "text", value: "Must not commit" },
  }));
  await new Promise(resolve => setTimeout(resolve, 50));
  assert.equal(changes.length, 1, "failed writes do not announce committed changes");
  await client.request("objects.write", {
    mutationId: mutation("ui-change-rename"), objectId: written.object.id, expectedRevision: 1,
    contractVersion: "1.0.0", name: "Renamed", references: {}, content: { encoding: "text", value: "Renamed content" },
  });
  for (let attempt = 0; !ignoredChanges.length && attempt < 50; attempt++) await new Promise(resolve => setTimeout(resolve, 10));
  assert.equal(ignoredChanges.length, 1, "atomic renames can change descendant paths watched by another contract");
});

test("durable event availability is a payload-free wakeup for registered services only", async (t) => {
  const { base, mutation } = await fixture(t),
    consumer = await peer(t, base),
    browser = await peer(t, base, { Authorization: "Bearer " + clientToken });
  await ready(consumer, "event-consumer");
  const client = new HiveClient(base, { credential: clientToken });
  await client.request("contracts.register", {
    mutationId: mutation("event-hint-contract"),
    definition: {
      key: "test/event-hint",
      version: "1.0.0",
      mediaType: "text/plain",
      owner: { kind: "agent" },
      retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
      specMarkdown: "Event hint fixture.",
    },
  });
  let browserReceived = false;
  browser.socket.on("message", (bytes) => {
    const frame = JSON.parse(bytes.toString()) as Envelope;
    if (frame.method === "events.available") browserReceived = true;
  });
  const wakeup = new Promise<Envelope>((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error("event availability wakeup timed out")),
      2_000,
    );
    consumer.socket.on("message", (bytes) => {
      const frame = JSON.parse(bytes.toString()) as Envelope;
      if (frame.method === "events.available") {
        clearTimeout(timer);
        resolve(frame);
      }
    });
  });
  await client.request("objects.write", {
    mutationId: mutation("event-hint-object"),
    contractVersion: "1.0.0",
    references: {},
    create: {
      contractKey: "test/event-hint",
      parentId: null,
      ownerObjectId: null,
      name: "event",
    },
    content: { encoding: "text", value: "saved first" },
  });
  const frame = await wakeup;
  await new Promise((resolve) => {
    setTimeout(resolve, 50);
  });
  assert.deepEqual(Object.keys(frame.params ?? {}), ["throughSequence"]);
  assert.ok(
    Number.isSafeInteger(frame.params?.["throughSequence"]) &&
      Number(frame.params?.["throughSequence"]) >= 1,
  );
  assert.equal(browserReceived, false);
});

test(
  "real HTTP root/subpath RPC and MCP share validation, errors and mutation identity",
  { timeout: 30_000 },
  async (t) => {
    for (const prefix of ["", "/ivy"]) {
      const { base, rpc, mutation } = await fixture(t, prefix);
      assert.equal((await fetch(base + "/health/live")).status, 200);
      assert.deepEqual(await (await fetch(base + "/health/ready")).json(), {
        ready: true,
      });
      const login = await fetch(base + "/", { redirect: "manual" });
      assert.equal(login.status, 303);
      assert.equal(login.headers.get("location"), prefix + "/login");
      const status = await rpc("system.status", {});
      assert.equal(status.response.status, 200);
      for (const body of [
        "null",
        "42",
        "[]",
        '"scalar"',
        "{",
        JSON.stringify({
          jsonrpc: "2.0",
          id: 1,
          method: "objects.write",
          params: [],
        }),
      ]) {
        const response = await fetch(base + "/api/v1/rpc", {
          method: "POST",
          headers: {
            Authorization: "Bearer " + clientToken,
            "Content-Type": "application/json",
          },
          body,
        });
        assert.equal(response.status, 400);
        assert.equal(
          ((await response.json()) as Envelope).error?.data.code,
          "invalid_frame",
        );
      }
      const badId = await fetch(base + "/api/v1/rpc", {
        method: "POST",
        headers: {
          Authorization: "Bearer " + clientToken,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          jsonrpc: "2.0",
          id: { invalid: "correlation" },
          method: "system.status",
          params: {},
        }),
      });
      assert.equal(badId.status, 400);
      assert.equal(((await badId.json()) as Envelope).id, null);
      const tooLarge = await fetch(base + "/api/v1/rpc", {
        method: "POST",
        headers: {
          Authorization: "Bearer " + clientToken,
          "Content-Type": "application/json",
        },
        body: " ".repeat(managementFrameBytes + 1),
      });
      assert.equal(tooLarge.status, 413);
      const mcp = new Client({ name: "ivy-isolated-test", version: "1.0.0" });
      const transport = new StreamableHTTPClientTransport(
        new URL(base + "/mcp"),
        {
          requestInit: { headers: { Authorization: "Bearer " + clientToken } },
        },
      );
      await mcp.connect(transport as unknown as Transport);
      t.after(() => mcp.close());
      const catalog = await mcp.listTools();
      assert.ok(
        catalog.tools.some((tool) => tool.name === "wiki_search"),
      );
      const firstPage = await mcp.listTools({ cursor: "" });
      assert.ok(firstPage.tools.length > 0);
      assert.ok(Buffer.byteLength(JSON.stringify(firstPage)) <= mcpDiscoveryResultBytes);
      assert.ok(
        catalog.tools.every(
          (tool) =>
            !JSON.stringify(tool.inputSchema).includes("https://ivy.invalid"),
        ),
      );
      await rpc("contracts.register", {
        mutationId: mutation("contract"),
        definition: {
          key: "test/text",
          version: "1.0.0",
          mediaType: "text/plain",
          owner: { kind: "agent" },
          retention: {
            objects: { mode: "retain" },
            revisions: { mode: "all" },
          },
          specMarkdown: "Test only",
        },
      });
      const arguments_ = {
        mutationId: mutation("cross-transport"),
        contractVersion: "1.0.0",
        references: {},
        create: {
          contractKey: "test/text",
          parentId: null,
          ownerObjectId: null,
          name: "saved",
        },
        content: { encoding: "text", value: "durable shared result" },
      };
      const saved = await rpc("objects.write", arguments_);
      assert.equal(saved.response.status, 200);
      const invoke = (args: Record<string, unknown>) => mcp.callTool({ name: "hive_object_write", arguments: args });
      const replay = await invoke(arguments_);
      assert.equal(replay.isError, undefined, JSON.stringify(replay.structuredContent));
      assert.deepEqual(
        (replay.structuredContent as { result: unknown }).result,
        saved.body.result,
      );
      const conflict = await invoke({
        ...arguments_,
        content: { encoding: "text", value: "changed" },
      });
      assert.equal(conflict.isError, true);
      assert.equal(
        (conflict.structuredContent as { error: WireError }).error.data.code,
        "mutation_conflict",
      );
      assert.equal((await rpc("system.status", {})).response.status, 200);
    }
  },
);

test(
  "raw 2026 MCP discovery, headers, private cache metadata and stateless calls are complete",
  { timeout: 20_000 },
  async (t) => {
    const { base } = await fixture(t);
    let sequence = 0;
    const revision = "2026-07-28";
    const envelope = (version = revision) => ({
      "io.modelcontextprotocol/protocolVersion": version,
      "io.modelcontextprotocol/clientInfo": {
        name: "ivy-raw-wire-test",
        version: "1",
      },
      "io.modelcontextprotocol/clientCapabilities": {},
    });
    const send = async (
      method: string,
      params: Record<string, unknown>,
      options: { version?: string; methodHeader?: string; name?: string } = {},
    ) => {
      const version = options.version ?? revision;
      const headers: Record<string, string> = {
        Authorization: "Bearer " + clientToken,
        "Content-Type": "application/json",
        Accept: "application/json, text/event-stream",
        "MCP-Protocol-Version": version,
        "Mcp-Method": options.methodHeader ?? method,
      };
      if (options.name) headers["Mcp-Name"] = options.name;
      const response = await fetch(base + "/mcp", {
        method: "POST",
        headers,
        body: JSON.stringify({
          jsonrpc: "2.0",
          id: "modern-" + ++sequence,
          method,
          params: { ...params, _meta: envelope(version) },
        }),
      });
      return { response, body: await mcpJson(response) };
    };
    const discovered = await send("server/discover", {});
    assert.equal(discovered.response.status, 200);
    const discovery = discovered.body["result"] as Record<string, unknown>;
    assert.ok((discovery["supportedVersions"] as string[]).includes(revision));
    assert.deepEqual(discovery["capabilities"], { tools: {} });
    assert.equal(discovery["ttlMs"], 0);
    assert.equal(discovery["cacheScope"], "private");
    assert.equal(discovery["resultType"], "complete");
    assert.equal(discovery["instructions"], hiveMcpInstructions);
    const initialized = await send("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "ivy-raw-wire-test", version: "1" },
    }, { version: "2025-06-18" });
    assert.equal(initialized.response.status, 200, JSON.stringify(initialized.body));
    assert.equal(
      (initialized.body["result"] as Record<string, unknown>)["instructions"],
      discovery["instructions"],
    );
    assert.deepEqual(
      (discovery["_meta"] as Record<string, unknown>)[
        "io.modelcontextprotocol/serverInfo"
      ],
      {
        name: "ivy",
        title: "Ivy",
        version: "0.1.0-test",
        description: "Personal back office with a wiki and connected services.",
        websiteUrl: "https://hive.test/ivy",
        icons: [
          {
            src: "https://hive.test/ivy/mcp-icon.svg",
            mimeType: "image/svg+xml",
            sizes: ["any"],
          },
        ],
      },
    );
    const icon = await fetch(base + "/mcp-icon.svg");
    assert.equal(icon.status, 200);
    assert.match(icon.headers.get("content-type") ?? "", /^image\/svg\+xml/);
    assert.match(await icon.text(), /aria-label="Ivy"/);

    const first = await send("tools/list", {}),
      second = await send("tools/list", {});
    for (const listed of [first, second]) {
      assert.equal(listed.response.status, 200);
      const result = listed.body["result"] as Record<string, unknown>;
      assert.equal(result["ttlMs"], 0);
      assert.equal(result["cacheScope"], "private");
      assert.equal(result["resultType"], "complete");
      const tools = result["tools"] as Array<Record<string, unknown>>;
      assert.ok(tools.length > 0);
      for (const tool of tools)
        assert.doesNotMatch(JSON.stringify(tool), /"securitySchemes"|"openai\/visibility"/);
    }
    assert.deepEqual(
      (first.body["result"] as Record<string, unknown>)["tools"],
      (second.body["result"] as Record<string, unknown>)["tools"],
      "stateless requests expose the same catalog without initialization state",
    );

    const called = await send(
      "tools/call",
      { name: "hive_status", arguments: {} },
      { name: "hive_status" },
    );
    assert.equal(called.response.status, 200);
    const callResult = called.body["result"] as Record<string, unknown>;
    assert.equal(callResult["resultType"], "complete");
    assert.equal(callResult["isError"], undefined);
    assert.ok(Array.isArray(callResult["content"]));

    const missingMethod = await fetch(base + "/mcp", {
      method: "POST",
      headers: {
        Authorization: "Bearer " + clientToken,
        "Content-Type": "application/json",
        Accept: "application/json",
        "MCP-Protocol-Version": revision,
      },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: "missing-method-header",
        method: "tools/list",
        params: { _meta: envelope() },
      }),
    });
    assert.equal(missingMethod.status, 400);
    assert.equal(
      ((await mcpJson(missingMethod))["error"] as { code: number }).code,
      -32020,
    );
    const wrongName = await send(
      "tools/call",
      { name: "hive_status", arguments: {} },
      { name: "hive_schema_list" },
    );
    assert.equal(wrongName.response.status, 400);
    assert.equal((wrongName.body["error"] as { code: number }).code, -32020);
    const unsupported = await send("tools/list", {}, { version: "2099-01-01" });
    assert.equal(unsupported.response.status, 400);
    assert.notEqual((unsupported.body["error"] as { code: number }).code, 0);
  },
);

test(
  "HTTP and WebSocket enforce the exact management received envelope bound",
  { timeout: 30000 },
  async (t) => {
    const { base } = await fixture(t),
      limit = managementFrameBytes;
    const frame = JSON.stringify({
      jsonrpc: "2.0",
      id: "exact-frame",
      method: "system.status",
      params: {},
    });
    const exact = frame + " ".repeat(limit - Buffer.byteLength(frame));
    const http = await fetch(base + "/api/v1/rpc", {
      method: "POST",
      headers: {
        Authorization: "Bearer " + clientToken,
        "Content-Type": "application/json",
      },
      body: exact,
    });
    assert.equal(http.status, 200);
    assert.equal(((await http.json()) as Envelope).id, "exact-frame");
    const ws = await peer(t, base, { Authorization: "Bearer " + clientToken });
    const reply = once(ws.socket, "message");
    ws.socket.send(exact);
    const [bytes] = await reply;
    assert.equal((JSON.parse(String(bytes)) as Envelope).id, "exact-frame");
    const closed = once(ws.socket, "close");
    ws.socket.send(exact + "x");
    const [code] = await closed;
    assert.equal(code, 1009);
    const httpOver = await fetch(base + "/api/v1/rpc", {
      method: "POST",
      headers: {
        Authorization: "Bearer " + clientToken,
        "Content-Type": "application/json",
      },
      body: exact + "x",
    });
    assert.equal(httpOver.status, 413);
  },
);

test(
  "browser login scope, CSRF checks and logout preserve shared API access",
  { timeout: 20_000 },
  async (t) => {
    const { base, rpc } = await fixture(t);
    const wrong = await fetch(base + "/login", {
      method: "POST",
      redirect: "manual",
      headers: {
        Origin: "https://other.test",
        "Content-Type": "application/x-www-form-urlencoded",
      },
      body: new URLSearchParams({ token: clientToken }),
    });
    assert.equal(wrong.status, 401);
    const signIn = async (returnTo = "/ivy/") =>
      fetch(base + "/login", {
        method: "POST",
        redirect: "manual",
        headers: {
          Origin: "https://hive.test",
          "Content-Type": "application/x-www-form-urlencoded",
        },
        body: new URLSearchParams({ token: clientToken, returnTo }),
      });
    assert.equal((await signIn("//other.test/path")).status, 400);
    const signedIn = await signIn();
    assert.equal(signedIn.status, 303);
    const setCookie = signedIn.headers.get("set-cookie")!;
    assert.match(
      setCookie,
      /Path=\/ivy; HttpOnly; Secure; SameSite=Lax; Max-Age=31536000/,
    );
    assert.ok(!setCookie.includes(clientToken));
    assert.ok(!setCookie.includes("Domain="));
    const cookie = setCookie.split(";")[0]!;
    const noOrigin = await fetch(base + "/api/v1/rpc", {
      method: "POST",
      headers: { Cookie: cookie, "Content-Type": "application/json" },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "system.status",
        params: {},
      }),
    });
    assert.equal(noOrigin.status, 401);
    const browserPeer = await peer(t, base, {
      Cookie: cookie,
      Origin: "https://hive.test",
    });
    const apiPeer = await peer(t, base, {
      Authorization: "Bearer " + clientToken,
    });
    const closed = once(browserPeer.socket, "close");
    const logout = await fetch(base + "/logout", {
      method: "POST",
      redirect: "manual",
      headers: { Cookie: cookie, Origin: "https://hive.test" },
    });
    assert.equal(logout.status, 303);
    await closed;
    assert.equal(apiPeer.socket.readyState, WebSocket.OPEN);
    assert.equal((await rpc("system.status", {})).response.status, 200);
    await apiPeer.call("system.status", {});
    assert.equal((await signIn()).status, 303);
  },
);

test(
  "browser cookie lifetime rolls on activity beyond its original validity window",
  { timeout: 20_000 },
  async (t) => {
    const originalNow = Date.now;
    let now = Date.parse("2026-01-01T00:00:00.000Z");
    Date.now = () => now;
    try {
      const { base } = await fixture(t);
      const login = await fetch(base + "/login", {
        method: "POST",
        redirect: "manual",
        headers: {
          Origin: "https://hive.test",
          "Content-Type": "application/x-www-form-urlencoded",
        },
        body: new URLSearchParams({ token: clientToken, returnTo: "/ivy/" }),
      });
      const issued = login.headers.get("set-cookie")!;
      assert.match(issued, /Max-Age=31536000/);
      const cookie = issued.split(";")[0]!;
      const authenticated = () =>
        fetch(base + "/api/v1/rpc", {
          method: "POST",
          headers: {
            Cookie: cookie,
            Origin: "https://hive.test",
            "Content-Type": "application/json",
          },
          body: JSON.stringify({
            jsonrpc: "2.0",
            id: 1,
            method: "system.status",
            params: {},
          }),
        });
      now += 364 * 24 * 60 * 60_000;
      const renewed = await authenticated();
      assert.equal(renewed.status, 200);
      assert.match(renewed.headers.get("set-cookie") ?? "", /Max-Age=31536000/);
      now += 2 * 24 * 60 * 60_000;
      const beyondOriginal = await authenticated();
      assert.equal(beyondOriginal.status, 200);
      assert.match(
        beyondOriginal.headers.get("set-cookie") ?? "",
        /Max-Age=31536000/,
      );
    } finally {
      Date.now = originalNow;
    }
  },
);

test(
  "OAuth expiry fences cached HTTP, queued dispatch and an existing WebSocket while refresh restores access",
  { timeout: 20_000 },
  async (t) => {
    const originalNow = Date.now;
    let now = originalNow();
    Date.now = () => now;
    try {
      const { server, base } = await fixture(t),
        initial = await oauthTokens(server);
      const expiry = now + 15 * 60_000,
        agent = new HttpAgent({ keepAlive: true, maxSockets: 1 });
      t.after(() => agent.destroy());
      const status = (token: string) =>
        new Promise<{
          response: import("node:http").IncomingMessage;
          body: Envelope;
        }>((resolve, reject) => {
          const body = JSON.stringify({
            jsonrpc: "2.0",
            id: 1,
            method: "system.status",
            params: {},
          });
          const request = httpRequest(
            base + "/api/v1/rpc",
            {
              agent,
              method: "POST",
              headers: {
                Authorization: "Bearer " + token,
                "Content-Type": "application/json",
                "Content-Length": Buffer.byteLength(body),
              },
            },
            (response) => {
              let data = "";
              response.setEncoding("utf8");
              response.on("data", (part) => (data += part));
              response.on("end", () =>
                resolve({ response, body: JSON.parse(data) as Envelope }),
              );
            },
          );
          request.on("error", reject);
          request.end(body);
        });
      now = expiry - 1;
      assert.equal(
        (await status(initial.access_token)).response.statusCode,
        200,
      );
      now = expiry;
      const expired = await status(initial.access_token);
      assert.equal(expired.response.statusCode, 401);
      assert.equal(expired.body.error?.data.code, "unauthenticated");
      const challenge = await fetch(base + "/mcp", {
        method: "POST",
        headers: {
          Authorization: "Bearer " + initial.access_token,
          "Content-Type": "application/json",
        },
        body: "{}",
      });
      assert.equal(challenge.status, 401);
      assert.match(
        challenge.headers.get("www-authenticate") ?? "",
        /scope="hive"/,
      );

      const refreshed = await initial.refresh(initial.refresh_token),
        refreshedExpiry = now + 15 * 60_000;
      assert.equal(
        (await status(refreshed.access_token)).response.statusCode,
        200,
        "a silently refreshed access token works on the cached HTTP connection",
      );
      now = refreshedExpiry - 10_000;
      const socket = await peer(t, base, {
        Authorization: "Bearer " + refreshed.access_token,
      });
      const socketClosed = once(socket.socket, "close");
      now = refreshedExpiry;
      await assert.rejects(
        socket.call("system.status", {}),
        /disconnected|expired|unauthenticated/,
      );
      await socketClosed;

      const queuedToken = await initial.refresh(refreshed.refresh_token),
        queuedExpiry = now + 15 * 60_000;
      now = queuedExpiry - 1;
      const original = server.worker.request.bind(server.worker);
      let release!: () => void,
        entered!: () => void,
        executeCalls = 0;
      const blocked = new Promise<void>((resolve) => {
          release = resolve;
        }),
        started = new Promise<void>((resolve) => {
          entered = resolve;
        });
      t.mock.method(
        server.worker,
        "request",
        async (...args: Parameters<typeof server.worker.request>) => {
          if (args[0].action === "execute") {
            executeCalls++;
            if (executeCalls === 1) {
              entered();
              await blocked;
            }
          }
          return original(...args);
        },
      );
      const blocker = fetch(base + "/api/v1/rpc", {
        method: "POST",
        headers: {
          Authorization: "Bearer " + clientToken,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          jsonrpc: "2.0",
          id: "blocker",
          method: "system.status",
          params: {},
        }),
      });
      await started;
      const queued = fetch(base + "/api/v1/rpc", {
        method: "POST",
        headers: {
          Authorization: "Bearer " + queuedToken.access_token,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          jsonrpc: "2.0",
          id: "queued",
          method: "system.status",
          params: {},
        }),
      });
      const gate = (
        server as unknown as { dispatchGate: { waiting: unknown[] } }
      ).dispatchGate;
      for (
        let attempts = 0;
        attempts < 100 && gate.waiting.length === 0;
        attempts++
      )
        await new Promise((resolve) => {
          setImmediate(resolve);
        });
      assert.equal(
        gate.waiting.length,
        1,
        "the OAuth request reached the server dispatch queue before expiry",
      );
      now = queuedExpiry;
      release();
      assert.equal((await blocker).status, 200);
      const fenced = await queued,
        fencedBody = (await fenced.json()) as Envelope;
      assert.equal(fenced.status, 401);
      assert.equal(fencedBody.error?.data.code, "unauthenticated");
      assert.equal(
        executeCalls,
        1,
        "expired queued work never reached storage execution",
      );
    } finally {
      Date.now = originalNow;
    }
  },
);

test(
  "storage execution rechecks OAuth expiry after work has entered the worker request path",
  { timeout: 20_000 },
  async (t) => {
    const { server, mutation } = await fixture(t);
    const request = {
      jsonrpc: "2.0",
      id: "expired-at-storage-start",
      method: "contracts.register",
      params: {
        mutationId: mutation("expired-at-storage-start"),
        definition: {
          key: "test/expired-at-storage-start",
          version: "1.0.0",
          owner: { kind: "agent" },
          mediaType: "text/plain",
          retention: {
            objects: { mode: "retain" },
            revisions: { mode: "all" },
          },
          specMarkdown: "This mutation must never commit.",
        },
      },
    };
    const original = server.worker.request.bind(server.worker);
    t.mock.method(
      server.worker,
      "request",
      async (...args: Parameters<typeof server.worker.request>) => {
        if (
          args[0].action === "execute" &&
          (args[0].request as { id?: unknown } | null)?.id ===
            "expired-at-storage-start"
        )
          await new Promise((resolve) => setTimeout(resolve, 75));
        return original(...args);
      },
    );
    await assert.rejects(
      server.invoke(
        {
          principalId: "client",
          credentialDigest: digest(clientToken),
          transport: "http",
          expiresAt: Date.now() + 25,
        },
        request,
      ),
      (error: unknown) =>
        error instanceof IvyError && error.code === "unauthenticated",
    );
    const listed = (await server.invoke(
      {
        principalId: "client",
        credentialDigest: digest(clientToken),
        transport: "http",
      },
      {
        jsonrpc: "2.0",
        id: "verify-expired-storage-mutation",
        method: "contracts.list",
        params: { limit: 100 },
      },
    )) as { items: Array<{ key: string }> };
    assert.equal(
      listed.items.some(
        (value) => value.key === "test/expired-at-storage-start",
      ),
      false,
    );
  },
);

test(
  "MCP tool-level expiry returns an actionable OAuth challenge without exposing credentials",
  { timeout: 20_000 },
  async (t) => {
    const originalNow = Date.now;
    let now = originalNow();
    Date.now = () => now;
    try {
      const { server, base } = await fixture(t),
        tokens = await oauthTokens(server),
        expiry = now + 15 * 60_000;
      const mcp = new Client({ name: "reauthorization-test", version: "1" });
      await mcp.connect(
        new StreamableHTTPClientTransport(new URL(base + "/mcp"), {
          requestInit: {
            headers: { Authorization: "Bearer " + tokens.access_token },
          },
        }) as unknown as Transport,
      );
      t.after(() => mcp.close());
      const original = server.invoke.bind(server);
      let expireOnInvocation = false;
      t.mock.method(
        server,
        "invoke",
        async (...args: Parameters<typeof server.invoke>) => {
          if (
            expireOnInvocation &&
            (args[1] as { method?: string } | null)?.method === "discovery.call"
          )
            now = expiry;
          return original(...args);
        },
      );
      now = expiry - 1;
      expireOnInvocation = true;
      const result = await mcp.callTool({
        name: "hive_status",
        arguments: {},
      });
      assert.equal(result.isError, true);
      assert.equal(
        (result.structuredContent as { error: WireError }).error.data.code,
        "unauthenticated",
      );
      const challenges = result._meta?.["mcp/www_authenticate"];
      assert.ok(Array.isArray(challenges) && challenges.length === 1);
      assert.match(
        String(challenges[0]),
        /^Bearer resource_metadata="https:\/\/hive\.test\/ivy\/\.well-known\/oauth-protected-resource", error="invalid_token", error_description="[^"]+"$/,
      );
      assert.doesNotMatch(
        JSON.stringify(result._meta),
        new RegExp(tokens.access_token.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")),
      );
    } finally {
      Date.now = originalNow;
    }
  },
);

test(
  "browser session remains valid across a Hive binary restart",
  { timeout: 20_000 },
  async (t) => {
    const directory = mkdtempSync(join(tmpdir(), "ivy-session-restart-")),
      filename = join(directory, "hive.sqlite");
    let server = new HiveServer({
      filename,
      publicBaseUrl: "https://hive.test/ivy",
      version: "before",
      buildId: digest("before"),
      credentials,
      listenPort: 0,
    });
    let address = await server.start(),
      base = `http://127.0.0.1:${address.port}/ivy`;
    t.after(async () => {
      await server.close();
      rmSync(directory, { recursive: true, force: true });
    });
    const login = await fetch(base + "/login", {
      method: "POST",
      redirect: "manual",
      headers: {
        Origin: "https://hive.test",
        "Content-Type": "application/x-www-form-urlencoded",
      },
      body: new URLSearchParams({ token: clientToken, returnTo: "/ivy/" }),
    });
    const cookie = login.headers.get("set-cookie")!.split(";")[0]!;
    assert.equal(login.status, 303);
    await server.close();
    server = new HiveServer({
      filename,
      publicBaseUrl: "https://hive.test/ivy",
      version: "after",
      buildId: digest("after"),
      credentials,
      listenPort: 0,
    });
    address = await server.start();
    base = `http://127.0.0.1:${address.port}/ivy`;
    const response = await fetch(base + "/api/v1/rpc", {
      method: "POST",
      headers: {
        Cookie: cookie,
        Origin: "https://hive.test",
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "system.status",
        params: {},
      }),
    });
    assert.equal(response.status, 200);
    assert.equal(((await response.json()) as Envelope).error, undefined);
  },
);

test(
  "real provider forwarding carries original arguments and durable identity, without retries",
  { timeout: 20_000 },
  async (t) => {
    const { base, rpc } = await fixture(t);
    const provider = await peer(t, base);
    await ready(provider);
    const frames: Envelope[] = [];
    provider.onInvocation((frame) => {
      frames.push(frame);
      provider.socket.send(
        JSON.stringify({
          jsonrpc: "2.0",
          id: frame.id,
          result: {
            echoed:
              frame.params?.["arguments"] &&
              (frame.params["arguments"] as { value: string }).value,
          },
        }),
      );
    });
    const params = {
      qualifiedName: "sample.EchoCase/read",
      serviceNodeId: "loopback",
      expectedDefinitionHash: toolDefinitionHash(tool),
      operationId: "durable-external-intent",
      arguments: { value: "exact payload" },
    };
    const result = await rpc("tools.call", params);
    assert.equal(result.response.status, 200);
    assert.deepEqual(result.body.result, { echoed: "exact payload" });
    assert.equal(frames.length, 1);
    assert.equal(frames[0]!.params?.["operationId"], "durable-external-intent");
    assert.equal(frames[0]!.params?.["callerPrincipalId"], "client");
    assert.deepEqual(frames[0]!.params?.["arguments"], {
      value: "exact payload",
    });
    const opaque = await rpc("tools.call", {
      ...params,
      arguments: { value: 7 },
    });
    assert.deepEqual(opaque.body.result, { echoed: 7 });
    assert.equal(
      frames.length,
      2,
      "normal routing does not recursively revalidate parsed tool payloads",
    );
    const mismatched = await rpc("tools.call", {
      ...params,
      expectedDefinitionHash: digest("wrong"),
    });
    assert.equal(mismatched.response.status, 409);
    assert.equal(mismatched.body.error?.data.outcome, "not_executed");
    assert.equal(frames.length, 2);
    provider.onInvocation((frame) => {
      frames.push(frame);
      provider.socket.send(
        JSON.stringify({
          jsonrpc: "2.0",
          id: frame.id,
          result: { invalid: true },
        }),
      );
    });
    const invalid = await rpc("tools.call", params);
    assert.deepEqual(invalid.body.result, { invalid: true });
    assert.equal(frames.length, 3);
    assert.equal((await rpc("system.status", {})).response.status, 200);
  },
);

test(
  "disconnect and timeout stay unknown, reconnect fences the prior live provider",
  { timeout: 20_000 },
  async (t) => {
    const { base, rpc } = await fixture(t);
    const provider = await peer(t, base);
    await ready(provider);
    const params = {
      qualifiedName: "sample.EchoCase/read",
      serviceNodeId: "loopback",
      expectedDefinitionHash: toolDefinitionHash(tool),
      arguments: { value: "test" },
    };
    let dispatched = 0;
    provider.onInvocation(() => {
      dispatched++;
    });
    const timed = await rpc("tools.call", params);
    assert.equal(timed.response.status, 504);
    assert.equal(timed.body.error?.data.outcome, "unknown");
    assert.equal(dispatched, 1);
    provider.onInvocation(() => {
      dispatched++;
      provider.socket.terminate();
    });
    const disconnected = await rpc("tools.call", params);
    assert.equal(disconnected.body.error?.data.code, "outcome_unknown");
    assert.equal(disconnected.body.error?.data.outcome, "unknown");
    assert.equal(dispatched, 2);
    const replacement = await peer(t, base);
    await ready(replacement);
    const closed = once(replacement.socket, "close");
    const latest = await peer(t, base);
    await latest.call("service.connect", {
      serviceNodeId: "loopback",
      hostId: "test-host",
      serviceName: "fixture-provider",
      version: "test",
      buildId: digest("fixture"),
      hiveProtocol: 1,
    });
    await closed;
    const unready = await rpc("tools.call", params);
    assert.equal(unready.body.error?.data.code, "service_not_ready");
  },
);

test("MCP paginates the complete typed catalog and routes exact providers without replay", { timeout: 30_000 }, async t => {
  const observations: McpCallObservation[] = [];
  const { base, mutation } = await fixture(t, "/ivy", { observeMcpCall: value => observations.push(value) });
  const mcp = new Client({ name: "flat-catalog-test", version: "1" });
  await mcp.connect(new StreamableHTTPClientTransport(new URL(base + "/mcp"), {
    requestInit: { headers: { Authorization: "Bearer " + clientToken } },
  }) as unknown as Transport);
  t.after(() => mcp.close());
  const devMcp = new Client({ name: "development-catalog-test", version: "1" });
  await devMcp.connect(new StreamableHTTPClientTransport(new URL(base + "/mcp-dev"), {
    requestInit: { headers: { Authorization: "Bearer " + clientToken } },
  }) as unknown as Transport);
  t.after(() => devMcp.close());
  const discover = async (path: string) => {
    const response = await fetch(base + path, {
      method: "POST", headers: { Authorization: "Bearer " + clientToken, "Content-Type": "application/json", Accept: "application/json, text/event-stream", "MCP-Protocol-Version": "2026-07-28", "Mcp-Method": "server/discover" },
      body: JSON.stringify({ jsonrpc: "2.0", id: path, method: "server/discover", params: { _meta: {
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { name: "ivy-split-test", version: "1" },
        "io.modelcontextprotocol/clientCapabilities": {},
      } } }),
    });
    const body = await mcpJson(response);
    assert.equal(response.status, 200, JSON.stringify(body));
    assert.ok(body["result"], JSON.stringify(body));
    return body["result"] as { instructions: string; _meta: Record<string, unknown> };
  };
  const development = await discover("/mcp-dev");
  assert.match(development.instructions, /Load `ivy_dev` only while developing/);
  assert.doesNotMatch(development.instructions, /Ivy \(MCP\) is a personal back office/);
  const developmentInfo = development._meta[
    "io.modelcontextprotocol/serverInfo"
  ] as { icons: Array<{ src: string }> };
  assert.equal(developmentInfo.icons[0]?.src, "https://hive.test/ivy/mcp-dev-icon.svg");
  const developmentIcon = await fetch(base + "/mcp-dev-icon.svg");
  assert.equal(developmentIcon.status, 200);
  assert.match(await developmentIcon.text(), /aria-label="Ivy Development"/);
  assert.match((await discover("/mcp")).instructions, /Ivy \(MCP\) is a personal back office/);
  const allTools = async (client: Client) => {
    const descriptors: Awaited<ReturnType<typeof mcp.listTools>>["tools"] = [];
    let cursor: string | undefined; let pages = 0;
    do {
      const page = await client.listTools({ cursor: cursor ?? "" });
      assert.ok(Buffer.byteLength(JSON.stringify(page)) <= mcpDiscoveryResultBytes, JSON.stringify({ bytes: Buffer.byteLength(JSON.stringify(page)), limit: mcpDiscoveryResultBytes, keys: Object.keys(page), toolKeys: Object.keys(page.tools[0]!), count: page.tools.length }));
      descriptors.push(...page.tools); cursor = page.nextCursor; pages++;
    } while (cursor);
    return { descriptors, pages };
  };
  const initialUserTools = (await allTools(mcp)).descriptors;
  const initialDevTools = (await allTools(devMcp)).descriptors;
  const initialTools = [...initialUserTools, ...initialDevTools];
  const wireInstructions = (await discover('/mcp')).instructions;
  let instructionsCursor: string | undefined, readInstructions = '';
  do {
    const response = await mcp.callTool({ name: 'ivy_instructions', arguments: instructionsCursor ? { cursor: instructionsCursor } : {} });
    const page = (response.structuredContent as { result: { view: string; text: string; sha256: string; totalBytes: number; complete: boolean; nextCursor: string | null } }).result;
    assert.equal(page.view, 'instructions');
    readInstructions += page.text;
    assert.equal(page.totalBytes, Buffer.byteLength(wireInstructions));
    assert.equal(page.sha256, digest(wireInstructions));
    instructionsCursor = page.nextCursor ?? undefined;
  } while (instructionsCursor);
  assert.equal(readInstructions, wireInstructions);
  const examplesResponse = await mcp.callTool({ name: 'ivy_instructions', arguments: { view: 'examples' } });
  const examplesPage = (examplesResponse.structuredContent as { result: { view: string; text: string; sha256: string; complete: boolean } }).result;
  assert.equal(examplesPage.view, 'examples');
  assert.match(examplesPage.text, /Public MCP workflow examples/);
  assert.equal(examplesPage.sha256, digest(examplesPage.text));
  assert.equal(examplesPage.complete, true);
  const wikiDescriptor = initialUserTools.find(tool => tool.name === 'wiki_update')!;
  const schemaResponse = await mcp.callTool({ name: 'ivy_tool_schema', arguments: { name: 'wiki_update' } });
  const schema = (schemaResponse.structuredContent as { result: { inputSchema: unknown; outputSchema: unknown; complete: boolean } }).result;
  assert.equal(schema.complete, true);
  assert.deepEqual(schema.inputSchema, wikiDescriptor.inputSchema);
  assert.deepEqual(schema.outputSchema, wikiDescriptor.outputSchema);
  assert.deepEqual(initialTools.map(tool => tool.name).sort(), [...new Set(Object.values(coreMcpNames))].sort());
  const publicCoreNames = new Set([...publicCoreMcpMethods].map(method => coreMcpNames[method]!));
  const standardPage = await mcp.listTools({ cursor: "" });
  assert.ok(standardPage.tools.every(tool => publicCoreNames.has(tool.name)));
  assert.ok(initialDevTools.every(tool => !publicCoreNames.has(tool.name)));
  await assert.rejects(mcp.callTool({ name: "hive_diagnostics", arguments: {} }), (error: unknown) => (error as { code?: number }).code === -32601);
  await assert.rejects(devMcp.callTool({ name: "wiki_read", arguments: {} }), (error: unknown) => (error as { code?: number }).code === -32601);
  for (const descriptor of initialTools) {
    assert.ok(descriptor.inputSchema && descriptor.outputSchema, descriptor.name);
    assert.equal(descriptor._meta?.["securitySchemes"], undefined);
    assert.equal(descriptor._meta?.["openai/visibility"], undefined);
    assert.equal(descriptor._meta?.["ui"], undefined);
    assert.doesNotMatch(JSON.stringify(descriptor), /noauth|synthetic-http-client|ivyoa2\./);
  }
  for (const path of ["/mcp", "/mcp-dev"]) {
    for (const authenticated of [false, true]) {
      const response = await fetch(base + path, {
        method: "POST",
        headers: { ...(authenticated ? { Authorization: "Bearer " + clientToken } : {}),
          "Content-Type": "application/json", Accept: "application/json, text/event-stream" },
        body: JSON.stringify({ jsonrpc: "2.0", id: "server-auth", method: "tools/list", params: {} }),
      });
      assert.equal(response.status, authenticated ? 200 : 401);
      if (authenticated) {
        const raw = (await mcpJson(response))["result"] as { tools: unknown[] };
        assert.ok(raw.tools.length);
        assert.doesNotMatch(JSON.stringify(raw.tools), /"securitySchemes"|"openai\/visibility"/);
      } else {
        assert.match(response.headers.get("www-authenticate") ?? "", /oauth-protected-resource/);
        assert.match(response.headers.get("www-authenticate") ?? "", /scope="hive"/);
        await response.arrayBuffer();
      }
    }
  }
  const first = await peer(t, base); await ready(first);
  const second = await peer(t, base); await ready(second, "other-host");
  const writeTool = { ...tool, name: "EchoCase/write", annotations: { readOnlyHint: false }, discovery: { mcp: { name: "fixture_provider_write" } } };
  const largeTools = Array.from({ length: 20 }, (_, index) => ({ ...tool, name: "large" + index,
    discovery: { mcp: { name: "fixture_provider_large_" + index } }, inputSchema: { ...tool.inputSchema as object, description: "x".repeat(30000) } }));
  const publish = (tools: ToolDefinition[]) => first.call("registry.sync", { ...registry, namespaces: [{ ...registry.namespaces[0]!, tools }] });
  await publish([tool, writeTool, ...valueTools, ...largeTools]);
  const firstPublishedPage = await devMcp.listTools({ cursor: "" });
  assert.ok(firstPublishedPage.tools.every(tool => !publicCoreNames.has(tool.name)));
  const { descriptors, pages } = await allTools(devMcp);
  assert.ok(pages > 1);
  const names = new Set(descriptors.map(tool => tool.name));
  assert.equal(names.size, descriptors.length);
  assert.equal(names.size, initialDevTools.length + 2 + valueTools.length + largeTools.length);
  for (const descriptor of descriptors) assert.doesNotMatch(JSON.stringify(descriptor.inputSchema), /https:\/\/ivy.invalid/);
  const echo = descriptors.find(tool => tool.name === "fixture_provider_echo")!;
  const validator = new Ajv2020({ strict: false, validateFormats: false });
  const validateInput = validator.compile(echo.inputSchema as AnySchema);
  assert.equal(validateInput({ serviceNodeId: "loopback", input: { value: "exact payload" } }), true);
  assert.equal(validateInput({ input: { value: "missing owner" } }), false);
  assert.equal(validateInput({ serviceNodeId: "other-host", input: { value: "second owner" } }), true);
  assert.equal(echo._meta, undefined);
  const page = await devMcp.listTools({ cursor: "" }); assert.ok(page.nextCursor);
  await first.call("service.heartbeat", { ready: true, diagnostics: [] });
  await devMcp.listTools({ cursor: page.nextCursor });
  await publish([tool, writeTool, ...valueTools]);
  await assert.rejects(devMcp.listTools({ cursor: page.nextCursor }), (error: unknown) => (error as { code?: number }).code === -32602);
  const frames: Envelope[] = []; let wrongProviderCalls = 0;
  second.onInvocation(() => { wrongProviderCalls++; });
  first.onInvocation(frame => {
    frames.push(frame); first.socket.send(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result: { echoed: "exact payload" } }));
  });
  const args = { serviceNodeId: "loopback", input: { value: "exact payload" }, operationId: "one-original-action" };
  const result = await devMcp.callTool({ name: "fixture_provider_write", arguments: args });
  assert.deepEqual(result.structuredContent, { result: { echoed: "exact payload" } });
  assert.equal(frames[0]!.params?.["operationId"], args.operationId);
  assert.deepEqual(frames[0]!.params?.["arguments"], args.input);
  const read = await devMcp.callTool({ name: "fixture_provider_echo", arguments: { serviceNodeId: args.serviceNodeId, input: args.input } });
  assert.equal(validator.compile(echo.outputSchema as AnySchema)(read.structuredContent), true);
  const primitiveValues = new Map<string, unknown>([["sample.Null/read", null], ["sample.Empty/read", {}],
    ["sample.Nullable/read", null], ["sample.Array/read", [1, 2]], ["sample.Scalar/read", "value"]]);
  first.onInvocation(frame => {
    frames.push(frame); first.socket.send(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result: primitiveValues.get(String(frame.params?.["qualifiedName"])) }));
  });
  for (const definition of valueTools) {
    const name = definition.discovery!.mcp!.name;
    const primitive = await devMcp.callTool({ name, arguments: { serviceNodeId: "loopback", input: {} } });
    const descriptor = descriptors.find(tool => tool.name === name)!;
    if (["Null/read", "Empty/read"].includes(definition.name)) {
      assert.equal(descriptor.outputSchema, undefined);
      assert.equal(primitive.structuredContent, undefined);
      assert.deepEqual(primitive.content, []);
      continue;
    }
    const envelope = { result: primitiveValues.get("sample." + definition.name) };
    assert.deepEqual(primitive.structuredContent, envelope);
    assert.equal(validator.compile(descriptor.outputSchema as AnySchema)(envelope), true);
    assert.deepEqual(JSON.parse(primitive.content[0]!.type === "text" ? primitive.content[0]!.text : ""), envelope);
  }
  const coreMutationId = mutation("mcp-core-contract-registration");
  const registered = await mcp.callTool({ name: "hive_schema_register", arguments: { mutationId: coreMutationId, definition: {
    key: "test/mcp-core-correlation", version: "1.0.0", owner: { kind: "agent" }, mediaType: "text/plain",
    retention: { objects: { mode: "retain" }, revisions: { mode: "all" } }, specMarkdown: "MCP call correlation fixture.",
  } } });
  assert.equal(registered.isError, undefined);
  const summaries = await mcp.callTool({ name: 'hive_schema_list', arguments: { key: 'test/mcp-core-correlation' } });
  const summary = (summaries.structuredContent as { result: { items: Record<string, unknown>[] } }).result.items[0]!;
  assert.equal(summary['key'], 'test/mcp-core-correlation');
  assert.equal(summary['description'], 'MCP call correlation fixture.');
  assert.equal('jsonSchema' in summary, false);
  // A direct call binds the current schema, and rejects obsolete arguments before dispatch.
  await publish([{ ...writeTool, inputSchema: { type: "object", properties: { replacement: { type: "string" } }, required: ["replacement"], additionalProperties: false } }, tool, ...valueTools]);
  const stale = await devMcp.callTool({ name: "fixture_provider_write", arguments: args });
  assert.equal((stale.structuredContent as { error: WireError }).error.data.outcome, "not_executed");
  assert.equal(frames.length, 2 + valueTools.length);
  await publish([tool, writeTool, ...valueTools]);
  first.onInvocation(frame => { frames.push(frame); });
  const timed = await devMcp.callTool({ name: "fixture_provider_write", arguments: { ...args, operationId: "original-timeout-action" } });
  assert.equal((timed.structuredContent as { error: WireError }).error.data.outcome, "unknown");
  assert.equal(frames.length, 3 + valueTools.length); assert.equal(wrongProviderCalls, 0);
  const missing = await devMcp.callTool({ name: "fixture_provider_echo", arguments: { serviceNodeId: "missing-provider", input: args.input } });
  assert.equal((missing.structuredContent as { error: WireError }).error.data.code, "invalid_arguments");
  for (const name of ["missing_root_tool", "hive_service_never-registered", "hive_search_tools", "hive_describe_tools", "hive_read_tool", "hive_call_tool"])
    await assert.rejects(devMcp.callTool({ name, arguments: {} }), (error: unknown) => (error as { code?: number }).code === -32601);
  const malformed = await fetch(base + "/mcp", {
    method: "POST", headers: { Authorization: "Bearer " + clientToken, "Content-Type": "application/json", Accept: "application/json, text/event-stream", "MCP-Protocol-Version": "2025-06-18" },
    body: JSON.stringify({ jsonrpc: "2.0", id: "malformed-tool-call", method: "tools/call", params: { name: 7, arguments: {} } }),
  });
  assert.equal(((await mcpJson(malformed))["error"] as { code: number }).code, -32602);
  assert.ok(observations.some(value => value.tool === "fixture_provider_write" && value.code === "ok" && value.outcome === "completed" && value.operationIdentity === "one-original-action" && value.internalRequestIds.length === 1));
  assert.ok(observations.some(value => value.tool === "hive_schema_register" && value.code === "ok" && value.operationIdentity === coreMutationId));
  assert.ok(observations.some(value => value.tool === "fixture_provider_write" && value.code === "invalid_arguments" && value.outcome === "not_executed" && value.internalRequestIds.length === 0));
  assert.ok(observations.some(value => value.tool === "fixture_provider_write" && value.outcome === "unknown" && value.operationIdentity === "original-timeout-action"));
  assert.ok(observations.some(value => value.tool === "missing_root_tool" && value.code === "method_not_found" && value.internalRequestIds.length === 0));
  assert.ok(observations.every(value => value.requestId !== null && value.latencyMs >= 0 && value.latencyMs <= 86_400_000));
  assert.doesNotMatch(JSON.stringify(observations), /exact payload|synthetic-http-client|Authorization|access_token|refresh_token/);
});

test(
  "credential revocation closes current HTTP and WebSocket access",
  { timeout: 20_000 },
  async (t) => {
    const { server, base, rpc } = await fixture(t);
    const connected = await peer(t, base, {
      Authorization: "Bearer " + clientToken,
    });
    const closed = once(connected.socket, "close");
    await server.setCredentials([credentials[1]!]);
    await closed;
    const response = await rpc("system.status", {});
    assert.equal(response.response.status, 401);
    assert.equal(response.body.error?.data.code, "unauthenticated");
  },
);

test("a replacement connection fences queued writes and can prune its own history", { timeout: 15_000 }, async (t) => {
  const { server, base, rpc, mutation } = await fixture(t),
    old = await peer(t, base), replacement = await peer(t, base);
  await ready(old);
  const catalog = { ...registry, contracts: [{
    key: "fixture/result", version: "1.0.0", owner: { kind: "service", serviceName: "fixture-provider" },
    mediaType: "text/plain", retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
    specMarkdown: "Result fixture.",
  }] };
  await old.call("registry.sync", catalog);
  const saved = await old.call<{ object: { id: string } }>("objects.write", {
    mutationId: mutation("initial"), contractVersion: "1.0.0", references: {},
    create: { contractKey: "fixture/result", parentId: null, ownerObjectId: null, name: "result" },
    content: { encoding: "text", value: "initial" },
  });
  const gate = (server as unknown as { dispatchGate: { waiting: unknown[]; run<T>(work: () => Promise<T>): Promise<T> } }).dispatchGate;
  let release!: () => void;
  const blocked = gate.run(() => new Promise<void>(resolve => { release = resolve; }));
  await new Promise<void>(resolve => setImmediate(resolve));
  const waitForQueue = async (size: number) => {
    const deadline = Date.now() + 2000;
    while (gate.waiting.length < size && Date.now() < deadline)
      await new Promise<void>(resolve => setImmediate(resolve));
    assert.equal(gate.waiting.length, size);
  };
  let connected: Promise<unknown>, stale: Promise<unknown>;
  try {
    connected = replacement.call("service.connect", {
      serviceNodeId: "loopback", hostId: "test-host", serviceName: "fixture-provider",
      version: "test", buildId: digest("fixture"), hiveProtocol: 1,
    });
    await waitForQueue(1);
    stale = old.call("objects.write", {
      objectId: saved.object.id, expectedRevision: 1, mutationId: mutation("stale"),
      contractVersion: "1.0.0", references: {}, content: { encoding: "text", value: "must not commit" },
    }).catch(error => error);
    await waitForQueue(2);
  } finally { release(); }
  await blocked;
  await connected!;
  await stale!;
  await gate.run(async () => undefined);
  const read = await rpc("objects.read", { objectId: saved.object.id });
  assert.equal((read.body.result as { revision: { revision: number } }).revision.revision, 1);
  await replacement.call("registry.sync", catalog);
  await replacement.call("service.heartbeat", { ready: true, diagnostics: [] });
  await replacement.call("objects.write", {
    objectId: saved.object.id, expectedRevision: 1, mutationId: mutation("new"),
    contractVersion: "1.0.0", references: {}, content: { encoding: "text", value: "new" },
  });
  assert.deepEqual(await replacement.call("objects.pruneRevisions", {
    objectId: saved.object.id, expectedRevision: 2, mutationId: mutation("prune"),
    maximumCount: 1, maximumAgeDays: 7, maximumBytes: 1000,
  }), { deleted: 1, protected: 0, remainingBytes: 3 });
});
