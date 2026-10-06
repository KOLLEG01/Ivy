import test from "node:test";
import type { TestContext } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { HiveKernel } from "../services/hive/src/kernel.js";
import { STORAGE_FORMAT } from "../services/hive/src/storage-schema.js";
import type { ConnectionContext } from "../services/hive/src/kernel.js";
import { LiveRouting } from "../services/hive/src/live-routing.js";
import {
  Registry,
  activeDiagnosticLimit,
  resolvedDiagnosticLimit,
} from "../services/hive/src/registry.js";
import type {
  Diagnostic,
  RegistrySync,
  ServiceNode,
  ToolDefinition,
} from "../services/hive/src/registry.js";
import {
  canonical,
  digest,
  hashJson,
} from "../packages/contracts/src/canonical.js";
import { toolDefinitionHash } from "../packages/contracts/src/tool-definition.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import { operationId } from "../packages/contracts/src/operation-id.js";
import type {
  ObjectWriteResult,
  Page,
} from "../packages/contracts/src/types.js";
import type { UiRelease } from "../services/hive/src/uis.js";
import { hiveMcpInstructions } from "../instructions/hive-mcp.js";
import type {
  Host,
  Operation,
  Wire,
} from "../packages/contracts/src/generated.js";

const principal = { principalId: "client", digest: digest("test-client") };
const servicePrincipal = {
  principalId: "service",
  digest: digest("test-service"),
};
const relocatedPrincipal = {
  principalId: "relocated-service",
  digest: digest("test-relocated-service"),
};
const client: ConnectionContext = {
  transport: "http",
  credentialDigest: principal.digest,
  principalId: principal.principalId,
};
const socket: ConnectionContext = {
  transport: "ws",
  credentialDigest: servicePrincipal.digest,
  principalId: servicePrincipal.principalId,
};
const relocatedSocket: ConnectionContext = {
  transport: "ws",
  credentialDigest: relocatedPrincipal.digest,
  principalId: relocatedPrincipal.principalId,
};
const tool: ToolDefinition = {
  namespace: "codex",
  name: "turn/ReadCase",
  nativeMethod: "turn/ReadCase",
  nativeSchemaIdentity: "synthetic-native-fixture",
  interfaceVersion: "1.0.0",
  description: "Synthetic protocol fixture, no real native execution.",
  inputSchema: {
    type: "object",
    properties: { threadId: { type: "string" } },
    required: ["threadId"],
    additionalProperties: false,
  },
  outputSchema: {
    type: "object",
    properties: { answer: { type: "string" } },
    required: ["answer"],
    additionalProperties: false,
  },
};
const catalog = (definition = tool): RegistrySync => ({
  namespaces: [
    {
      namespace: "codex",
      description: "Synthetic test provider",
      guideMarkdown: "Test catalog.",
      tools: [definition],
      topics: [],
      inventoryKinds: [
        {
          kind: "thread",
          version: "1.0.0",
          searchPointers: ["/name", "/preview", "/cwd"],
          archivedPointer: "/archived",
          recencyPointer: "/recencyAt",
          summarySchema: {
            type: "object",
            properties: {
              title: { type: "string" },
              name: { type: ["string", "null"] },
              preview: { type: "string" },
              cwd: { type: "string" },
              archived: { type: "boolean" },
              recencyAt: { type: ["integer", "null"] },
              updatedAt: { type: "string" },
            },
            required: ["title"],
            additionalProperties: false,
          },
        },
      ],
    },
  ],
  contracts: [],
  requiredContracts: [],
});
function fixture(t: TestContext, validateMessages = false) {
  const kernel = new HiveKernel({
    filename: ":memory:",
    publicBaseUrl: "https://hive.test/ivy",
    version: "0.1.0-test",
    buildId: digest("test-code"),
    credentials: [principal, servicePrincipal, relocatedPrincipal],
  });
  const routing = new LiveRouting(validateMessages);
  const issuedAt = Date.now(),
    mutations = new Map<string, string>();
  t.after(() => kernel.close());
  const call = <T>(
    context: ConnectionContext,
    method: string,
    params: unknown,
  ): T => {
    if (
      params &&
      typeof params === "object" &&
      typeof (params as { mutationId?: unknown }).mutationId === "string"
    ) {
      const input = (params as { mutationId: string }).mutationId;
      let mutationId = mutations.get(input);
      if (!mutationId) {
        mutationId = operationId(
          kernel.store.runtimeEpoch,
          issuedAt,
          digest(input).slice(7),
        );
        mutations.set(input, mutationId);
      }
      params = { ...params, mutationId };
    }
    const response = kernel.execute(context, {
      jsonrpc: "2.0",
      id: randomUUID(),
      method,
      params,
    });
    assert.equal(response.kind, "result");
    return (response as { value: T }).value;
  };
  const connect = (node: string, host = node): ConnectionContext => {
    const result = call<{ generation: number }>(socket, "service.connect", {
      serviceNodeId: node,
      hostId: host,
      serviceName: "agent-manager",
      version: "test-1",
      buildId: digest("provider-" + node),
      hiveProtocol: 1,
    });
    const liveCatalog = kernel.registry.liveCatalog(node);
    routing.update({
      node: kernel.registry.node(node),
      generation: result.generation,
      ...(liveCatalog ? { catalog: liveCatalog } : {}),
    });
    return { ...socket, serviceNodeId: node, generation: result.generation };
  };
  const ready = (context: ConnectionContext, definition = tool) => {
    call(context, "registry.sync", catalog(definition));
    call(context, "service.heartbeat", { ready: true, diagnostics: [] });
    const liveCatalog = kernel.registry.liveCatalog(context.serviceNodeId!);
    routing.update({
      node: kernel.registry.node(context.serviceNodeId!),
      ...(liveCatalog ? { catalog: liveCatalog } : {}),
    });
  };
  return { kernel, routing, call, connect, ready };
}

test("Hive instructions include each registered service hint once and follow registry updates", (t) => {
  const { call, connect } = fixture(t);
  assert.equal(call<Operation.DiscoveryInstructionsResult>(client, "discovery.instructions", {}).instructions, hiveMcpInstructions);
  const first = connect("hint-a"), second = connect("hint-b");
  const { nativeMethod: _method, nativeSchemaIdentity: _identity, ...base } = tool;
  const publicTool = { ...base, name: "test", discovery: { mcp: { name: "agent_manager_test" } } };
  const initial = { ...catalog(publicTool), discoveryHint: "AgentManager offers test capabilities." };
  call(first, "registry.sync", initial);
  call(second, "registry.sync", initial);
  const composed = call<Operation.DiscoveryInstructionsResult>(client, "discovery.instructions", {}).instructions;
  assert.equal(composed.split("- agent-manager: AgentManager offers test capabilities.").length - 1, 1);
  call(first, "registry.sync", { ...initial, discoveryHint: "AgentManager offers updated capabilities." });
  call(second, "registry.sync", { ...initial, discoveryHint: "AgentManager offers updated capabilities." });
  const updated = call<Operation.DiscoveryInstructionsResult>(client, "discovery.instructions", {}).instructions;
  assert.match(updated, /AgentManager offers updated capabilities\./);
  assert.doesNotMatch(updated, /AgentManager offers test capabilities\./);
});

test("permanent deletion is absent from MCP discovery and rejected over MCP transport", (t) => {
  const { call } = fixture(t);
  const found = call<Operation.DiscoveryListResult>(client, "discovery.list", {
    query: "objects.delete",
  });
  assert.ok(found.items.every((item) => item.name !== "hive.objects.delete"));
  assert.throws(() => call(client, "discovery.describe", {
    serviceName: "hive",
    tools: ["hive.objects.delete"],
  }), (error) => error instanceof IvyError && error.code === "not_found");
  assert.throws(() => call({ ...client, transport: "mcp" }, "objects.delete", {
    objectId: "any",
    expectedRevision: 1,
    mutationId: "mcp-delete",
  }), (error) => error instanceof IvyError && error.code === "forbidden");
});
const code = (expected: string) => (error: unknown) =>
  error instanceof IvyError && error.code === expected;

test("singleton admission follows the connected service name across hosts", (t) => {
  const { kernel, call } = fixture(t);
  const identity = (serviceNodeId: string, hostId: string, instanceMode: "singleton" | "multiple") => ({
    serviceNodeId,
    hostId,
    serviceName: "moveable-service",
    instanceMode,
    version: "1",
    buildId: digest(serviceNodeId),
    hiveProtocol: 1 as const,
  });
  const first = identity("first-node", "first-host", "singleton");
  const second = identity("second-node", "second-host", "singleton");
  call(socket, "service.connect", first);
  assert.equal(kernel.registry.node(first.serviceNodeId).instanceMode, "singleton");
  assert.throws(() => call(relocatedSocket, "service.connect", second), code("target_conflict"));
  assert.throws(() => call(relocatedSocket, "service.connect", { ...second, instanceMode: "multiple" }), code("target_conflict"));
  kernel.registry.disconnect(first.serviceNodeId);
  call(relocatedSocket, "service.connect", second);
  assert.equal(kernel.registry.node(second.serviceNodeId).hostId, "second-host");
  assert.throws(() => call(socket, "service.connect", first), code("target_conflict"));
  assert.throws(() => call(socket, "service.connect", { ...second, serviceNodeId: first.serviceNodeId }), code("target_conflict"));
  assert.throws(() => call(socket, "service.connect", { ...first, instanceMode: "unknown" }), code("invalid_arguments"));
});

test("all reads pass the same authenticated operation and exact output validation boundary", (t) => {
  const { kernel, call } = fixture(t);
  const status = call<{
    publicBaseUrl: string;
    ready: boolean;
    storageFormat: number;
  }>(client, "system.status", {});
  assert.equal(status.publicBaseUrl, "https://hive.test/ivy");
  assert.equal(status.ready, true);
  assert.equal(status.storageFormat, STORAGE_FORMAT);
  assert.deepEqual(
    call(client, "system.inspectStorage", {
      contracts: [{ key: "unknown/domain" }],
    }),
    {
      schemaVersion: 1,
      exists: true,
      format: STORAGE_FORMAT,
      contracts: [{ key: "unknown/domain", versions: [] }],
    },
  );
  for (const method of [
    "hosts.list",
    "serviceNodes.list",
    "namespaces.list",
    "hostConfigurations.list",
    "uis.list",
    "deployments.list",
    "system.diagnostics",
    "inventory.list",
  ])
    assert.deepEqual(call(client, method, {}), { items: [], nextCursor: null });
  assert.deepEqual(
    call<{ items: { key: string }[]; nextCursor: null }>(
      client,
      "contracts.list",
      {},
    ).items.map((value) => value.key),
    ["ivy/host-configuration", "ivy/package-catalog"],
  );
  assert.equal(
    call<{ callerPrincipalId: string }>(
      { ...client, credentialDigest: digest("already-bound") },
      "system.status",
      {},
    ).callerPrincipalId,
    principal.principalId,
  );
  for (const malformed of [
    null,
    [],
    "x",
    { jsonrpc: "2.0", id: 1, method: "objects.write", params: null },
  ])
    assert.throws(
      () => kernel.execute(client, malformed),
      code("invalid_frame"),
    );
  assert.throws(
    () =>
      call(client, "service.connect", {
        serviceNodeId: "a",
        hostId: "a",
        serviceName: "a",
        buildId: digest("a"),
        version: "v1",
        hiveProtocol: 1,
      }),
    code("stale_generation"),
  );
  assert.throws(
    () => call(socket, "service.connect", { hiveProtocol: 2 }),
    code("unsupported_protocol"),
  );
});

test("browser object reads project declared HostConfig secrets through the central boundary", (t) => {
  const { call } = fixture(t),
    browser = { ...client, sessionDigest: digest("browser-session") };
  const configuration: Host.HostConfig = {
    schemaVersion: 1,
    hostId: "BROWSER",
    runtimeRoot: "C:/ivy/runtime",
    artifactRoot: "C:/ivy/artifacts",
    stagingRoot: "C:/ivy/staging",
    publicBaseUrl: "https://hive.test/ivy",
    executables: { node: "C:/node.exe" },
    instances: [
      {
        instanceId: "worker",
        serviceNodeId: "worker",
        componentId: "fixture",
        enabled: true,
        engine: "process",
        credential: "zirconiumultrasecret",
        secretPaths: ["/settings/apiToken"],
        settings: {
          apiToken: "settings-value",
          authorization: "ordinary-visible-value",
        },
      },
    ],
  };
  const saved = call<Operation.HostConfiguration>(
    client,
    "hostConfigurations.put",
    {
      hostId: configuration.hostId,
      configuration,
      mutationId: "browser-config",
    },
  );
  const read = call<Operation.ObjectRead>(browser, "objects.read", {
    objectId: saved.objectId,
  });
  assert.equal(JSON.stringify(read).includes("zirconiumultrasecret"), false);
  assert.equal(JSON.stringify(read).includes("settings-value"), false);
  assert.equal(JSON.stringify(read).includes("ordinary-visible-value"), true);
  assert.throws(
    () =>
      call(browser, "hostConfigurations.get", { hostId: configuration.hostId }),
    code("forbidden"),
  );
  assert.throws(
    () =>
      call(browser, "objects.query", { contractKey: "ivy/host-configuration" }),
    code("forbidden"),
  );
  assert.throws(
    () =>
      call(browser, "objects.query", {
        contractKey: "ivy/host-configuration",
        includeContent: true,
      }),
    code("forbidden"),
  );
  assert.equal(
    call<{ items: unknown[] }>(client, "objects.search", { text: "Ivy" }).items
      .length,
    1,
  );
  assert.equal(
    call<{ items: unknown[] }>(browser, "objects.search", { text: "Ivy" }).items
      .length,
    0,
  );
});

test("host observations fence sequences and the live route, set disabled state before first connect and cannot cross hosts", (t) => {
  const { kernel, routing, call, connect } = fixture(t);
  const registration = {
    serviceNodeId: "manager",
    hostId: "own-host",
    serviceName: "service-manager",
    version: "test",
    buildId: digest("manager"),
    hiveProtocol: 1,
  };
  const connected = call<{ generation: number }>(
    socket,
    "service.connect",
    registration,
  );
  const manager = {
    ...socket,
    serviceNodeId: "manager",
    generation: connected.generation,
  };
  const snapshot: Host.ManagementSnapshot = {
    sequence: 1,
    status: {
      schemaVersion: 1,
      hostId: "own-host",
      observedAt: new Date().toISOString(),
      executor: null,
      unfinished: [],
      instances: [
        {
          instanceId: "worker",
          serviceNodeId: "worker",
          componentId: "agent-manager",
          engine: "process",
          desiredEnabled: false,
          installedBuild: null,
          installedCandidateId: null,
          observedBuild: null,
          observedState: "stopped",
          observedAt: new Date().toISOString(),
          code: null,
          message: "",
        },
      ],
    },
  };
  const first = call(manager, "hosts.report", snapshot);
  assert.deepEqual(call(manager, "hosts.report", snapshot), first);
  const worker = connect("worker", "own-host");
  assert.equal(
    call<ServiceNode>(client, "serviceNodes.get", { serviceNodeId: "worker" })
      .desiredEnabled,
    false,
  );
  assert.throws(
    () => call(worker, "hosts.report", { ...snapshot, sequence: 2 }),
    code("target_conflict"),
  );
  const conflict = structuredClone(snapshot);
  conflict.status.instances[0]!.desiredEnabled = true;
  assert.throws(
    () => call(manager, "hosts.report", conflict),
    code("mutation_conflict"),
  );
  conflict.sequence = 2;
  call(manager, "hosts.report", conflict);
  assert.equal(
    call<ServiceNode>(client, "serviceNodes.get", { serviceNodeId: "worker" })
      .desiredEnabled,
    true,
  );
  assert.throws(
    () => call(manager, "hosts.report", snapshot),
    code("revision_conflict"),
  );
  connect("foreign", "different-host");
  const crossing = structuredClone(conflict);
  crossing.sequence = 3;
  crossing.status.instances[0]!.desiredEnabled = false;
  crossing.status.instances.push({
    ...crossing.status.instances[0]!,
    instanceId: "foreign",
    serviceNodeId: "foreign",
  });
  assert.throws(
    () => call(manager, "hosts.report", crossing),
    code("target_conflict"),
  );
  assert.equal(
    call<ServiceNode>(client, "serviceNodes.get", { serviceNodeId: "worker" })
      .desiredEnabled,
    true,
    "failed complete report cannot partially disable another node",
  );
  const next = call<{ generation: number }>(
    socket,
    "service.connect",
    registration,
  );
  routing.update({
    node: kernel.registry.node("manager"),
    generation: next.generation,
  });
  assert.throws(() => routing.caller(manager), code("stale_generation"));
  kernel.registry.disconnect("manager");
  const observations = call<{ items: Host.ManagementObservation[] }>(
    client,
    "hosts.observations",
    { hostId: "own-host" },
  );
  assert.equal(observations.items[0]!.snapshot.sequence, 2);
  assert.equal(observations.items[0]!.stale, true);
});

test("cached deployment reports cannot regress same-millisecond phases or reinterpret a terminal result", (t) => {
  const { call, connect } = fixture(t),
    reporter = connect("reporter", "host");
  const now = new Date().toISOString(),
    build = digest("candidate");
  const record: Wire.DeploymentRecord = {
    deploymentId: "observed-operation",
    hostId: "host",
    instanceId: "instance",
    componentId: "fixture",
    requestHash: digest("original arguments"),
    previousBuild: null,
    targetBuild: build,
    observedBuild: null,
    phase: "verifying",
    createdAt: now,
    updatedAt: now,
    readiness: { state: "not_run", message: "Current launch pending." },
  };
  call(reporter, "deployments.report", { records: [record] });
  const completed = {
    ...record,
    phase: "succeeded",
    observedBuild: build,
    readiness: { state: "passed", message: "Actual launch verified." },
  };
  call(reporter, "deployments.report", { records: [completed] });
  assert.throws(
    () => call(reporter, "deployments.report", { records: [record] }),
    code("revision_conflict"),
  );
  assert.throws(
    () =>
      call(reporter, "deployments.report", {
        records: [
          { ...completed, requestHash: digest("other original arguments") },
        ],
      }),
    code("target_conflict"),
  );
  call(reporter, "deployments.report", { records: [completed] });
  const observed = call<{ record: Wire.DeploymentRecord }>(
    client,
    "deployments.get",
    { hostId: "host", deploymentId: record.deploymentId },
  );
  assert.deepEqual(observed.record, completed);
});

test("deployment reports are listed by operation update time instead of replay report time", (t) => {
  const { call, connect } = fixture(t),
    reporter = connect("reporter", "host");
  const build = digest("candidate"),
    at = (offset: number) =>
      new Date(Date.UTC(2026, 8, 13, 12, offset)).toISOString();
  const record = (
    deploymentId: string,
    updatedAt: string,
  ): Wire.DeploymentRecord => ({
    deploymentId,
    hostId: "host",
    instanceId: "instance",
    componentId: "fixture",
    requestHash: digest(deploymentId),
    previousBuild: null,
    targetBuild: build,
    observedBuild: build,
    phase: "succeeded",
    createdAt: updatedAt,
    updatedAt,
    readiness: { state: "passed", message: "Verified." },
  });
  call(reporter, "deployments.report", {
    records: [
      record("newest", at(3)),
      record("oldest", at(1)),
      record("middle", at(2)),
    ],
  });
  const listed = call<{ items: Array<{ record: Wire.DeploymentRecord }> }>(
    client,
    "deployments.list",
    { hostId: "host" },
  );
  assert.deepEqual(
    listed.items.map((item) => item.record.deploymentId),
    ["newest", "middle", "oldest"],
  );
});

test("live connection fencing, atomic registry failure and explicit readiness", (t) => {
  const { kernel, routing, call, connect, ready } = fixture(t);
  const first = connect("node");
  assert.throws(
    () => call(first, "service.heartbeat", { ready: true, diagnostics: [] }),
    code("service_not_ready"),
  );
  ready(first);
  const malformed = catalog();
  malformed.namespaces[0]!.tools.push({
    ...tool,
    name: "other",
    nativeMethod: "other",
    inputSchema: { unsupportedKeyword: true },
  });
  assert.throws(
    () => call(first, "registry.sync", malformed),
    code("registry_invalid"),
  );
  const mixedInterface = catalog();
  mixedInterface.namespaces[0]!.tools.push({
    ...tool,
    name: "other",
    nativeMethod: "other",
    interfaceVersion: "2.0.0",
  });
  assert.throws(
    () => call(first, "registry.sync", mixedInterface),
    code("registry_invalid"),
  );
  const duplicateKinds = catalog();
  duplicateKinds.namespaces[0]!.topics = [
    {
      topic: "codex.turn",
      version: "1.0.0",
      title: "Agent turn",
      description: "A synthetic turn event.",
      payloadSchema: true,
      eventKinds: [
        { kind: "completed", title: "Completed", description: "Done." },
        { kind: "completed", title: "Completed again", description: "Done again." },
      ],
    },
  ];
  assert.throws(
    () => call(first, "registry.sync", duplicateKinds),
    code("registry_invalid"),
  );
  const state = call<ServiceNode>(client, "serviceNodes.get", {
    serviceNodeId: "node",
  });
  assert.equal(state.ready, false);
  assert.equal(state.synced, false);
  assert.ok(state.lastFailedSyncAt);
  const old = call<{ toolCount: number }>(client, "namespaces.get", {
    namespace: "codex",
    serviceNodeId: "node",
  });
  assert.equal(old.toolCount, 1);
  assert.equal("tools" in old, false);
  const second = connect("node");
  assert.equal(second.generation, 2);
  assert.throws(() => routing.caller(first), code("stale_generation"));
  ready(second);
  const beforeHeartbeat = call<ServiceNode>(client, "serviceNodes.get", {
    serviceNodeId: "node",
  });
  assert.equal(beforeHeartbeat.ready, true);
  const stableStatus = String(
    kernel.store.get(
      "SELECT status_json FROM service_nodes WHERE id=?",
      "node",
    )!["status_json"],
  );
  const changesBeforeHeartbeat = Number(
    kernel.store.get("SELECT total_changes() AS value")!["value"],
  );
  while (Date.now() <= Date.parse(beforeHeartbeat.lastContactAt!)) {
    /* ensure an observable volatile timestamp */
  }
  call(second, "service.heartbeat", { ready: true, diagnostics: [] });
  const afterHeartbeat = call<ServiceNode>(client, "serviceNodes.get", {
    serviceNodeId: "node",
  });
  assert.ok(
    Date.parse(afterHeartbeat.lastContactAt!) >
      Date.parse(beforeHeartbeat.lastContactAt!),
  );
  assert.equal(
    Number(kernel.store.get("SELECT total_changes() AS value")!["value"]),
    changesBeforeHeartbeat,
    "an unchanged heartbeat must perform zero SQLite changes",
  );
  assert.equal(
    String(
      kernel.store.get(
        "SELECT status_json FROM service_nodes WHERE id=?",
        "node",
      )!["status_json"],
    ),
    stableStatus,
    "an unchanged heartbeat must not rewrite its durable observation",
  );
  const diagnostic = {
    code: "fixture",
    resource: { item: "same" },
    severity: "warning" as const,
    source: "ignored",
    firstObservedAt: new Date().toISOString(),
    lastObservedAt: new Date().toISOString(),
    status: "current" as const,
    message: "same issue",
  };
  call(second, "service.heartbeat", { ready: true, diagnostics: [diagnostic] });
  const diagnosticIdentity = hashJson({
    code: diagnostic.code,
    resource: diagnostic.resource,
    source: "service:node",
  });
  const diagnosticChanges = Number(
    kernel.store.get("SELECT total_changes() AS value")!["value"],
  );
  const laterDiagnosticAt = new Date(Date.now() + 1000).toISOString();
  call(second, "service.heartbeat", {
    ready: true,
    diagnostics: [{ ...diagnostic, lastObservedAt: laterDiagnosticAt }],
  });
  const visibleDiagnostic = call<{ items: Array<{ lastObservedAt: string }> }>(
    client,
    "system.diagnostics",
    {},
  ).items.find((value) => value.lastObservedAt === laterDiagnosticAt);
  assert.ok(
    visibleDiagnostic,
    "the diagnostic observation time must remain current",
  );
  assert.equal(
    Number(kernel.store.get("SELECT total_changes() AS value")!["value"]),
    diagnosticChanges + 1,
    "an updated observation time must be persisted once",
  );
  assert.equal(
    JSON.parse(
      String(
        kernel.store.get(
          "SELECT diagnostic_json FROM diagnostics WHERE identity=?",
          diagnosticIdentity,
        )!["diagnostic_json"],
      ),
    ).lastObservedAt,
    laterDiagnosticAt,
  );
  assert.equal(
    new Registry(kernel.store)
      .diagnostics("current")
      .find((value) => value.code === diagnostic.code)?.lastObservedAt,
    laterDiagnosticAt,
    "a restart must retain the effective observation time used by age collection",
  );
  call(second, "service.heartbeat", { ready: true, diagnostics: [] });
  const resolved = call<{
    items: Array<{ code: string; source: string; status: string }>;
  }>(client, "system.diagnostics", { status: "resolved" }).items;
  assert.ok(
    resolved.some(
      (value) =>
        value.code === diagnostic.code &&
        value.source === "service:node" &&
        value.status === "resolved",
    ),
  );
  assert.equal(
    call<{ items: unknown[] }>(client, "system.diagnostics", {
      status: "current",
    }).items.some(
      (value) => (value as { code?: string }).code === diagnostic.code,
    ),
    false,
    "a complete heartbeat resolves an omitted prior issue",
  );
  assert.equal(
    call<{ items: Array<{ code: string; status: string }> }>(
      client,
      "system.diagnostics",
      { status: "unresolved" },
    ).items.some(
      (value) => value.code === diagnostic.code || value.status === "resolved",
    ),
    false,
    "the unresolved selection excludes resolved diagnostics",
  );
  const now = new Date().toISOString();
  kernel.registry.diagnostic({
    code: "stale_generation",
    resource: { serviceNodeId: "node" },
    severity: "warning",
    source: "hive",
    firstObservedAt: now,
    lastObservedAt: now,
    status: "current",
    message: "Historical protocol observation.",
  });
  const recoveredRegistry = new Registry(kernel.store);
  assert.equal(
    recoveredRegistry
      .diagnostics("current")
      .some(
        (value) => value.code === "stale_generation" && value.source === "hive",
      ),
    false,
    "a restart must resolve protocol observations retained by an older Hive build",
  );
  assert.equal(
    recoveredRegistry
      .diagnostics("resolved")
      .some(
        (value) => value.code === "stale_generation" && value.source === "hive",
      ),
    true,
  );
  assert.throws(
    () =>
      call(second, "registry.sync", {
        namespaces: [null],
        contracts: [],
        requiredContracts: [],
      }),
    code("invalid_arguments"),
  );
  assert.equal(
    call<ServiceNode>(client, "serviceNodes.get", { serviceNodeId: "node" })
      .ready,
    false,
  );
});

test("diagnostic admission is bounded, permits replacement at capacity and collects resolved history deterministically", (t) => {
  const { kernel, call, connect, ready } = fixture(t),
    service = connect("diagnostic-node");
  ready(service);
  const observed = "2026-09-20T00:00:00.000Z",
    diagnostic = (
      index: number,
      status: Diagnostic["status"] = "current",
      lastObservedAt = observed,
    ): Diagnostic => ({
      code: "diagnostic-" + index,
      resource: { index },
      severity: "warning",
      source: "hive",
      firstObservedAt: observed,
      lastObservedAt,
      status,
      message: "Synthetic diagnostic.",
    });
  kernel.store.transaction(() => {
    for (let index = 0; index < activeDiagnosticLimit - 1; index++) {
      const value = diagnostic(index),
        identity = hashJson({
          code: value.code,
          resource: value.resource,
          source: value.source,
        });
      kernel.store.run(
        "INSERT INTO diagnostics VALUES (?,?)",
        identity,
        canonical(value),
      );
    }
  });
  const first = { ...diagnostic(activeDiagnosticLimit), source: "ignored" };
  call(service, "service.heartbeat", { ready: true, diagnostics: [first] });
  assert.equal(
    kernel.registry.diagnostics().filter((value) => value.status !== "resolved")
      .length,
    activeDiagnosticLimit,
  );
  const replacement = {
    ...diagnostic(activeDiagnosticLimit + 1),
    source: "ignored",
  };
  assert.doesNotThrow(
    () =>
      call(service, "service.heartbeat", {
        ready: true,
        diagnostics: [replacement],
      }),
    "resolving an omitted identity must make room for its replacement in the same batch",
  );
  assert.ok(
    kernel.registry
      .diagnostics("resolved")
      .some((value) => value.code === first.code),
  );
  assert.ok(
    kernel.registry
      .diagnostics("current")
      .some((value) => value.code === replacement.code),
  );
  const before = kernel.registry.diagnostics().length;
  assert.throws(
    () => kernel.registry.diagnostic(diagnostic(activeDiagnosticLimit + 2)),
    code("limit_exceeded"),
  );
  assert.equal(
    kernel.registry.diagnostics().length,
    before,
    "failed capacity admission must not partially change diagnostics",
  );

  kernel.store.run(
    "DELETE FROM diagnostics WHERE json_extract(diagnostic_json,'$.source')='hive'",
  );
  const now = Date.parse("2026-09-20T12:00:00.000Z"),
    cutoff = new Date(now - 7 * 24 * 60 * 60 * 1000).toISOString();
  kernel.store.transaction(() => {
    for (let index = 0; index < resolvedDiagnosticLimit + 2; index++) {
      const at =
          index === 0
            ? cutoff
            : new Date(Date.parse(cutoff) + index + 1).toISOString(),
        value = diagnostic(index, "resolved", at),
        identity = hashJson({
          code: value.code,
          resource: value.resource,
          source: value.source,
        });
      kernel.store.run(
        "INSERT INTO diagnostics VALUES (?,?)",
        identity,
        canonical(value),
      );
    }
  });
  kernel.registry.collectDiagnostics(now);
  const resolved = kernel.registry.diagnostics("resolved");
  assert.equal(resolved.length, resolvedDiagnosticLimit);
  assert.equal(
    resolved.some((value) => value.lastObservedAt === cutoff),
    false,
    "the exact seven-day boundary expires",
  );
  assert.equal(
    resolved.some((value) => value.code === "diagnostic-1"),
    false,
    "count pressure removes the next oldest identity",
  );
});

test("namespace discovery stays compact for large catalogs while selected tool schemas and hashes remain exact", (t) => {
  const { call, connect } = fixture(t),
    owner = connect("large-catalog");
  const definitions = Array.from({ length: 40 }, (_, index) => ({
    ...tool,
    name: "test/" + String(index).padStart(3, "0"),
    nativeMethod: "test/" + String(index).padStart(3, "0"),
    inputSchema: {
      ...(tool.inputSchema as Record<string, unknown>),
      description: "x".repeat(30000),
    },
  }));
  assert.ok(Buffer.byteLength(JSON.stringify(definitions)) > 1024 * 1024);
  const registry = catalog();
  registry.namespaces[0]!.tools = definitions;
  const topic: Wire.TopicDefinition = {
    topic: "codex.turn",
    version: "1.0.0",
    title: "Agent turn",
    description: "A retained synthetic Agent turn event.",
    payloadSchema: {
      type: "object",
      properties: { kind: { type: "string" } },
      required: ["kind"],
      additionalProperties: false,
    },
    eventKinds: [
      {
        kind: "completed",
        title: "Turn completed",
        description: "The synthetic turn completed.",
      },
    ],
  };
  registry.namespaces[0]!.topics = [topic];
  call(owner, "registry.sync", registry);
  call(owner, "service.heartbeat", { ready: true, diagnostics: [] });
  const overview = call<{
    toolCount: number;
    topicCount: number;
    provider: { node: ServiceNode };
  }>(
    client,
    "namespaces.get",
    { namespace: "codex", serviceNodeId: "large-catalog" },
  );
  assert.equal(overview.toolCount, definitions.length);
  assert.equal(overview.topicCount, 1);
  assert.equal(overview.provider.node.serviceNodeId, "large-catalog");
  assert.equal("tools" in overview, false);
  assert.ok(Buffer.byteLength(JSON.stringify(overview)) < 8192);
  const selected = call<{
    items: Array<{ definition: ToolDefinition; definitionHash: string }>;
    nextCursor: string | null;
  }>(client, "tools.list", {
    namespace: "codex",
    serviceNodeId: "large-catalog",
    namePrefix: "test/007",
    limit: 1,
  });
  assert.equal(selected.items.length, 1);
  assert.equal(selected.nextCursor, null);
  assert.deepEqual(selected.items[0]!.definition, definitions[7]);
  assert.equal(
    selected.items[0]!.definitionHash,
    toolDefinitionHash(definitions[7]!),
  );
  const selectedTopics = call<Operation.TopicsListResult>(
    client,
    "topics.list",
    {
      namespace: "codex",
      serviceNodeId: "large-catalog",
      topicPrefix: "codex.turn",
      limit: 1,
    },
  );
  assert.equal(selectedTopics.items[0]!.provider!.node.serviceNodeId, "large-catalog");
  assert.equal(selectedTopics.items[0]!.namespace, "codex");
  assert.deepEqual(selectedTopics.items.map(item => item.definition), [topic]);
  const allTopics = call<Operation.TopicsListResult>(client, "topics.list", {});
  assert.ok(allTopics.items.some(item => item.definition.topic === topic.topic && item.provider?.node.serviceNodeId === "large-catalog"));
  const coreTopics = call<Operation.TopicsListResult>(client, "topics.list", { serviceName: "hive" });
  assert.ok(coreTopics.items.some(item => item.provider === null && item.definition.topic === "hive.object.changed" && item.definition.payloadSchema));
  assert.equal(selectedTopics.nextCursor, null);
});

test("provider-specific hashes, target conflicts and in-flight definition snapshots", (t) => {
  const { kernel, routing, call, connect, ready } = fixture(t);
  const first = connect("host-b", "windows"),
    second = connect("hetzner", "linux");
  ready(first);
  const variant = { ...tool, description: "Different native schema build." };
  ready(second, variant);
  assert.throws(
    () => call(client, "namespaces.get", { namespace: "codex" }),
    code("ambiguous_service_node"),
  );
  assert.throws(
    () =>
      call(client, "namespaces.get", {
        namespace: "codex",
        serviceNodeId: "host-b",
        hostId: "linux",
      }),
    code("target_conflict"),
  );
  assert.throws(
    () =>
      call(client, "namespaces.get", {
        namespace: "codex",
        serviceNodeId: "host-b",
        resourceRef: {
          serviceNodeId: "hetzner",
          namespace: "codex",
          kind: "thread",
          nativeId: "same-id",
        },
      }),
    code("target_conflict"),
  );
  const caller = { ...client, principalId: "client" };
  const invoke = (node = "host-b", expected = hashJson(tool)) =>
    routing.prepare(caller, {
      qualifiedName: "codex.turn/ReadCase",
      serviceNodeId: node,
      expectedDefinitionHash: expected,
      arguments: { threadId: "same-id" },
    });
  const dispatched = invoke();
  assert.equal(dispatched.serviceNodeId, "host-b");
  assert.deepEqual(dispatched.arguments, { threadId: "same-id" });
  assert.throws(() => invoke("hetzner"), code("tool_definition_changed"));
  ready(first, { ...tool, outputSchema: { type: "integer" } });
  assert.deepEqual(
    routing.complete(dispatched, { answer: "old response shape" }),
    { answer: "old response shape" },
  );
  assert.deepEqual(routing.complete(dispatched, { wrong: true }), {
    wrong: true,
  });
  connect("host-b", "windows");
  assert.throws(
    () => routing.complete(dispatched, { answer: "late" }),
    code("stale_generation"),
  );
  assert.throws(() => invoke(), code("service_not_ready"));
});

test("debug message validation rejects a provider result without loading validators by default", (t) => {
  const { routing, connect, ready } = fixture(t, true),
    service = connect("debug");
  ready(service);
  const caller = { ...client, principalId: "client" };
  const prepared = routing.prepare(caller, {
    qualifiedName: "codex.turn/ReadCase",
    serviceNodeId: "debug",
    expectedDefinitionHash: hashJson(tool),
    arguments: { threadId: "thread" },
  });
  assert.throws(
    () =>
      routing.prepare(caller, {
        qualifiedName: "codex.turn/ReadCase",
        serviceNodeId: "debug",
        expectedDefinitionHash: hashJson(tool),
        arguments: { threadId: 1 } as unknown as Wire.Json,
      }),
    code("invalid_arguments"),
  );
  assert.throws(
    () => routing.complete(prepared, { wrong: true }),
    code("provider_contract_error"),
  );
  assert.deepEqual(routing.complete(prepared, { answer: "valid" }), {
    answer: "valid",
  });
  const large = "x".repeat(2 * 1024 * 1024),
    largePrepared = routing.prepare(caller, {
      qualifiedName: "codex.turn/ReadCase",
      serviceNodeId: "debug",
      expectedDefinitionHash: hashJson(tool),
      arguments: { threadId: large },
    });
  assert.equal(
    (routing.complete(largePrepared, { answer: large }) as { answer: string })
      .answer.length,
    large.length,
  );
});

test("revoked provider credential prevents dispatch even before a socket close is observed", (t) => {
  const { kernel, routing, connect, ready } = fixture(t);
  const node = connect("node");
  ready(node);
  kernel.store.configureCredentials([principal]);
  routing.disconnect("node", node.generation!);
  assert.throws(
    () =>
      routing.prepare(
        { ...client, principalId: "client" },
        {
          qualifiedName: "codex.turn/ReadCase",
          serviceNodeId: "node",
          expectedDefinitionHash: hashJson(tool),
          arguments: { threadId: "thread" },
        },
      ),
    code("service_unavailable"),
  );
});

test("inventory rejects invalid schemas before committing an immutable version", (t) => {
  const { call, connect, ready } = fixture(t, true);
  const service = connect("node"), invalid = catalog();
  invalid.namespaces[0]!.inventoryKinds[0]!.summarySchema = { type: "not-a-json-schema-type" };
  assert.throws(() => call(service, "registry.sync", invalid), code("registry_invalid"));
  ready(service);
  call(service, "inventory.sync", {
    namespace: "codex", kind: "thread", schemaVersion: "1.0.0", mode: "snapshot", snapshotRevision: 1,
    entries: [{ nativeId: "thread", summary: { title: "Valid" }, observedAt: new Date().toISOString() }],
  });
});

test("reported readiness survives a stale heartbeat observation", (t) => {
  const { kernel, call, ready, routing } = fixture(t);
  const connected = call<{ generation: number }>(socket, "service.connect", {
    serviceNodeId: "node", hostId: "host", serviceName: "service-manager",
    version: "test", buildId: digest("manager"), hiveProtocol: 1,
  });
  const service = { ...socket, serviceNodeId: "node", generation: connected.generation };
  routing.update({ node: kernel.registry.node("node"), generation: connected.generation });
  ready(service);
  call(service, "hosts.report", { sequence: 1, status: {
    schemaVersion: 1, hostId: "host", observedAt: new Date().toISOString(),
    executor: null, unfinished: [], instances: [],
  } });
  const resourceRef = { serviceNodeId: "node", namespace: "codex", kind: "thread", nativeId: "thread" };
  call(service, "inventory.sync", {
    namespace: "codex", kind: "thread", schemaVersion: "1.0.0", mode: "snapshot", snapshotRevision: 1,
    entries: [{ nativeId: "thread", summary: { title: "Thread" }, observedAt: new Date().toISOString() }],
  });
  const assertInventoryStale = (stale: boolean) => {
    assert.equal(call<Operation.InventoryItem>(client, "inventory.get", { resourceRef }).stale, stale);
    const { items } = call<{ items: Operation.InventoryItem[] }>(client, "inventory.list", {
      serviceNodeId: "node", namespace: "codex", kind: "thread",
    });
    assert.equal(items.length, 1);
    assert.equal(items[0]!.stale, stale);
  };
  const report = () => call<{ items: Host.ManagementObservation[] }>(client, "hosts.observations", { hostId: "host" }).items[0]!;
  assert.equal(report().stale, false);
  assertInventoryStale(false);
  t.mock.timers.enable({ apis: ["Date"], now: Date.now() + 10_001 });
  const node = call<ServiceNode>(client, "serviceNodes.get", { serviceNodeId: "node" });
  assert.equal(node.stale, true);
  assert.equal(node.ready, true);
  assert.doesNotThrow(() => routing.caller(service));
  assert.equal(report().stale, true);
  assertInventoryStale(true);
  call(service, "service.heartbeat", { ready: true, diagnostics: [] });
  assert.equal(report().stale, false);
  assertInventoryStale(false);
});

test("complete inventory snapshots are atomic, deltas detect gaps, offline cache is labeled", (t) => {
  const { kernel, call, connect, ready } = fixture(t);
  const service = connect("node");
  ready(service);
  const params = {
    namespace: "codex",
    kind: "thread",
    schemaVersion: "1.0.0",
    mode: "snapshot",
    snapshotRevision: 1,
    entries: [
      {
        nativeId: "thread",
        summary: { title: "Original" },
        observedAt: new Date().toISOString(),
      },
    ],
  };
  call(service, "inventory.sync", params);
  call(service, "inventory.sync", params);
  assert.throws(
    () =>
      call(service, "inventory.sync", {
        ...params,
        snapshotRevision: 2,
        entries: [
          {
            nativeId: "invalid",
            summary: {},
            observedAt: new Date().toISOString(),
          },
        ],
      }),
    code("invalid_arguments"),
  );
  const ref = {
    serviceNodeId: "node",
    namespace: "codex",
    kind: "thread",
    nativeId: "thread",
  };
  assert.equal(
    call<{ summary: { title: string } }>(client, "inventory.get", {
      resourceRef: ref,
    }).summary.title,
    "Original",
  );
  assert.throws(
    () =>
      call(service, "inventory.sync", {
        ...params,
        mode: "delta",
        snapshotRevision: 3,
        expectedRevision: 2,
        removedNativeIds: [],
        entries: [],
      }),
    code("revision_conflict"),
  );
  kernel.registry.disconnect("node");
  assert.equal(
    call<{ stale: boolean }>(client, "inventory.get", { resourceRef: ref })
      .stale,
    true,
  );
  assert.equal(
    call<Page<unknown>>(client, "inventory.list", { namespace: "codex" }).items
      .length,
    1,
  );
});

test("inventory task views filter archives and search before paging in native recency order", (t) => {
  const { call, connect, ready } = fixture(t),
    service = connect("node");
  ready(service);
  const observedAt = "2026-09-14T00:00:00.000Z";
  call(service, "inventory.sync", {
    namespace: "codex",
    kind: "thread",
    schemaVersion: "1.0.0",
    mode: "snapshot",
    snapshotRevision: 1,
    entries: [
      {
        nativeId: "older",
        summary: {
          title: "Older",
          name: "Alpha",
          preview: "first",
          cwd: "/alpha",
          archived: false,
          recencyAt: 10,
        },
        observedAt,
      },
      {
        nativeId: "newer",
        summary: {
          title: "Newer",
          name: "Beta match",
          preview: "second",
          cwd: "/beta",
          archived: false,
          recencyAt: 20,
        },
        observedAt,
      },
      {
        nativeId: "archived",
        summary: {
          title: "Archived",
          name: "Match archive",
          preview: "third",
          cwd: "/archive",
          archived: true,
          recencyAt: 30,
        },
        observedAt,
      },
    ],
  });
  const active = call<
    Page<
      import("../packages/contracts/src/generated.js").Operation.InventoryItem
    >
  >(client, "inventory.list", {
    serviceNodeId: "node",
    namespace: "codex",
    kind: "thread",
    archived: false,
    sort: "recency-desc",
  });
  assert.deepEqual(
    active.items.map((item) => item.resourceRef.nativeId),
    ["newer", "older"],
  );
  const searched = call<
    Page<
      import("../packages/contracts/src/generated.js").Operation.InventoryItem
    >
  >(client, "inventory.list", {
    serviceNodeId: "node",
    namespace: "codex",
    kind: "thread",
    archived: false,
    searchTerm: "MATCH",
    sort: "recency-desc",
  });
  assert.deepEqual(
    searched.items.map((item) => item.resourceRef.nativeId),
    ["newer"],
  );
});

test("inventory SQL paging traverses owners, schemas, equal recency, Unicode, archives and offline entries without gaps", (t) => {
  const { kernel, call, connect, ready } = fixture(t),
    nodeA = connect("node-a", "host-a");
  ready(nodeA);
  const nodeB = connect("node-b", "host-b"),
    second = structuredClone(catalog());
  second.namespaces[0]!.inventoryKinds[0]!.version = "2.0.0";
  second.namespaces[0]!.inventoryKinds[0]!.summarySchema = {
    type: "object",
    properties: {
      title: { type: "string" },
      name: { type: ["string", "null"] },
      preview: { type: "string" },
      cwd: { type: "string" },
      archived: { type: "boolean" },
      recencyAt: { type: ["string", "null"] },
    },
    required: ["title"],
    additionalProperties: false,
  };
  call(nodeB, "registry.sync", second);
  call(nodeB, "service.heartbeat", { ready: true, diagnostics: [] });
  const observedAt = new Date().toISOString();
  call(nodeA, "inventory.sync", {
    namespace: "codex",
    kind: "thread",
    schemaVersion: "1.0.0",
    mode: "snapshot",
    snapshotRevision: 1,
    entries: [
      {
        nativeId: "same",
        summary: {
          title: "Equal A",
          name: "plain",
          archived: false,
          recencyAt: 50,
        },
        observedAt,
      },
      {
        nativeId: "second",
        summary: {
          title: "Equal second",
          name: "plain",
          archived: false,
          recencyAt: 50,
        },
        observedAt,
      },
      {
        nativeId: "unicode",
        summary: {
          title: "Unicode",
          name: "Ärger in 日本語",
          archived: false,
          recencyAt: 40,
        },
        observedAt,
      },
      {
        nativeId: "archived",
        summary: {
          title: "Archived",
          name: "old",
          archived: true,
          recencyAt: 60,
        },
        observedAt,
      },
      {
        nativeId: "fallback",
        summary: { title: "Missing recency" },
        observedAt,
      },
    ],
  });
  call(nodeB, "inventory.sync", {
    namespace: "codex",
    kind: "thread",
    schemaVersion: "2.0.0",
    mode: "snapshot",
    snapshotRevision: 7,
    entries: [
      {
        nativeId: "same",
        summary: {
          title: "Equal B",
          name: "plain",
          archived: false,
          recencyAt: "1970-01-01T00:00:00.050Z",
        },
        observedAt,
      },
      {
        nativeId: "dated",
        summary: {
          title: "Dated",
          name: "plain",
          archived: false,
          recencyAt: "2026-09-20T00:00:00.000Z",
        },
        observedAt,
      },
    ],
  });
  kernel.registry.disconnect("node-b");
  const request = {
    namespace: "codex",
    kind: "thread",
    sort: "recency-desc" as const,
    limit: 2,
  };
  const found: Operation.InventoryItem[] = [];
  let cursor: string | undefined;
  do {
    const page = call<Page<Operation.InventoryItem>>(client, "inventory.list", {
      ...request,
      ...(cursor ? { cursor } : {}),
    });
    found.push(...page.items);
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
  assert.deepEqual(
    found.map(
      (item) =>
        `${item.resourceRef.nativeId}@${item.resourceRef.serviceNodeId}`,
    ),
    [
      "dated@node-b",
      "archived@node-a",
      "same@node-a",
      "same@node-b",
      "second@node-a",
      "unicode@node-a",
      "fallback@node-a",
    ],
  );
  assert.equal(
    new Set(found.map((item) => JSON.stringify(item.resourceRef))).size,
    found.length,
  );
  assert.deepEqual(
    found
      .filter((item) => item.resourceRef.serviceNodeId === "node-b")
      .map((item) => item.schemaVersion),
    ["2.0.0", "2.0.0"],
  );
  assert.ok(
    found
      .filter((item) => item.resourceRef.serviceNodeId === "node-b")
      .every((item) => item.stale),
  );
  const first = call<Page<Operation.InventoryItem>>(
    client,
    "inventory.list",
    request,
  );
  assert.ok(first.nextCursor);
  assert.throws(
    () =>
      call(client, "inventory.list", {
        ...request,
        hostId: "host-a",
        cursor: first.nextCursor,
      }),
    code("invalid_cursor"),
  );
  const unicode = call<Page<Operation.InventoryItem>>(
    client,
    "inventory.list",
    {
      serviceNodeId: "node-a",
      namespace: "codex",
      kind: "thread",
      archived: false,
      searchTerm: "ÄRGER",
    },
  );
  assert.deepEqual(
    unicode.items.map((item) => item.resourceRef.nativeId),
    ["unicode"],
  );
  const archived = call<Page<Operation.InventoryItem>>(
    client,
    "inventory.list",
    { hostId: "host-a", namespace: "codex", kind: "thread", archived: true },
  );
  assert.deepEqual(
    archived.items.map((item) => item.resourceRef.nativeId),
    ["archived"],
  );
});

test("large inventory materializes only page-sized rows and byte trimming advances its exact keyset", (t) => {
  const { kernel, call, connect, ready } = fixture(t),
    service = connect("large-node", "large-host");
  ready(service);
  const observedAt = new Date().toISOString();
  const sync = (revision: number, count: number) =>
    call(service, "inventory.sync", {
      namespace: "codex",
      kind: "thread",
      schemaVersion: "1.0.0",
      mode: "snapshot",
      snapshotRevision: revision,
      entries: Array.from({ length: count }, (_, index) => ({
        nativeId: "item-" + String(index).padStart(5, "0"),
        summary: { title: "Item " + index },
        observedAt,
      })),
    });
  sync(1, 10);
  const measure = () => {
    const original = {
      all: kernel.store.all.bind(kernel.store),
      get: kernel.store.get.bind(kernel.store),
    };
    let allCalls = 0,
      getCalls = 0,
      rows = 0,
      maximumRows = 0,
      inventorySql = "",
      inventoryParams: unknown[] = [];
    kernel.store.all = ((sql: string, ...params: never[]) => {
      allCalls++;
      const result = original.all(sql, ...params);
      rows += result.length;
      maximumRows = Math.max(maximumRows, result.length);
      if (sql.startsWith("SELECT i.*,s.revision AS snapshot_revision")) {
        inventorySql = sql;
        inventoryParams = params;
      }
      return result;
    }) as typeof kernel.store.all;
    kernel.store.get = ((sql: string, ...params: never[]) => {
      getCalls++;
      return original.get(sql, ...params);
    }) as typeof kernel.store.get;
    let page: Page<Operation.InventoryItem>;
    try {
      page = call(client, "inventory.list", {
        serviceNodeId: "large-node",
        namespace: "codex",
        kind: "thread",
        limit: 7,
      });
    } finally {
      kernel.store.all = original.all;
      kernel.store.get = original.get;
    }
    return {
      page,
      allCalls,
      getCalls,
      rows,
      maximumRows,
      inventorySql,
      inventoryParams,
    };
  };
  const small = measure();
  sync(2, 1200);
  const large = measure();
  assert.equal(large.page.items.length, 7);
  assert.ok(large.page.nextCursor);
  assert.deepEqual(
    {
      allCalls: large.allCalls,
      getCalls: large.getCalls,
      rows: large.rows,
      maximumRows: large.maximumRows,
    },
    {
      allCalls: small.allCalls,
      getCalls: small.getCalls,
      rows: small.rows,
      maximumRows: small.maximumRows,
    },
  );
  assert.ok(large.maximumRows <= 8);
  assert.ok(large.inventorySql.includes("LIMIT ?"));
  const plan = kernel.store.db
    .prepare("EXPLAIN QUERY PLAN " + large.inventorySql)
    .all(...(large.inventoryParams as never[]))
    .map((row) => String(row["detail"]));
  assert.ok(plan.some((detail) => /inventory/i.test(detail)));
  assert.ok(plan.some((detail) => /index/i.test(detail)));

  const payload = "x".repeat(750_000);
  call(service, "inventory.sync", {
    namespace: "codex",
    kind: "thread",
    schemaVersion: "1.0.0",
    mode: "snapshot",
    snapshotRevision: 3,
    entries: [0, 1, 2].map((index) => ({
      nativeId: "large-" + index,
      summary: {
        title: "Large " + index,
        preview: payload,
        archived: false,
        recencyAt: index,
      },
      observedAt,
    })),
  });
  const first = call<Page<Operation.InventoryItem>>(client, "inventory.list", {
    serviceNodeId: "large-node",
    namespace: "codex",
    kind: "thread",
    limit: 10,
  });
  assert.equal(first.items.length, 2);
  assert.ok(first.nextCursor);
  const secondPage = call<Page<Operation.InventoryItem>>(
    client,
    "inventory.list",
    {
      serviceNodeId: "large-node",
      namespace: "codex",
      kind: "thread",
      limit: 10,
      cursor: first.nextCursor!,
    },
  );
  assert.deepEqual(
    [...first.items, ...secondPage.items].map(
      (item) => item.resourceRef.nativeId,
    ),
    ["large-0", "large-1", "large-2"],
  );
  call(service, "inventory.sync", {
    namespace: "codex",
    kind: "thread",
    schemaVersion: "1.0.0",
    mode: "snapshot",
    snapshotRevision: 4,
    entries: [
      { nativeId: "oversized", summary: { title: "Oversized" }, observedAt },
    ],
  });
  kernel.store.run(
    "UPDATE inventory SET summary=? WHERE node_id=? AND namespace=? AND kind=? AND native_id=?",
    JSON.stringify({ title: "Oversized", preview: "x".repeat(2_200_000) }),
    "large-node",
    "codex",
    "thread",
    "oversized",
  );
  assert.throws(
    () =>
      call(client, "inventory.list", {
        serviceNodeId: "large-node",
        namespace: "codex",
        kind: "thread",
        limit: 10,
      }),
    code("result_too_large"),
  );
});

test("ui deployment retains private release files and rollback uses CAS, replay and compatibility", (t) => {
  const { kernel, call, connect, ready } = fixture(t);
  const node = connect("node");
  ready(node);
  call(client, "contracts.register", {
    mutationId: "html-contract",
    definition: {
      key: "test/html",
      version: "1.0.0",
      owner: { kind: "agent" },
      mediaType: "text/html",
      retention: { objects: { mode: "retain" }, revisions: { mode: "all" } },
      specMarkdown: "Test HTML",
    },
  });
  const first = Buffer.from("<!doctype html><p>release one</p>");
  const firstAsset = { path: "index.html", mediaType: "text/html",
    contentHash: digest(first), byteLength: first.length };
  kernel.uiFiles.stage("test-ui", "one", firstAsset, first);
  const release: UiRelease = {
    releaseId: "one",
    entryPath: "index.html",
    requirements: {
      hiveProtocol: 1,
      contracts: [
        { key: "test/html", readVersions: ["1.0.0"], writeVersions: [] },
      ],
      services: [
        {
          serviceName: "agent-manager",
          namespace: "codex",
          interfaceVersion: "1.0.0",
        },
      ],
    },
    assets: [firstAsset],
  };
  const metadata = {
    uiId: "test-ui",
    displayName: "Test ui",
    description: "Test fixture",
    iconKey: "ui",
  };
  const initial = {
    metadata,
    release,
    expectedReleaseId: null,
    mutationId: "deploy-one",
  };
  const committed = call(client, "uis.deploy", initial);
  const second = Buffer.from("<!doctype html><p>release two</p>");
  const secondAsset = { ...firstAsset, contentHash: digest(second), byteLength: second.length };
  kernel.uiFiles.stage("test-ui", "two", secondAsset, second);
  call(client, "uis.deploy", {
    ...initial,
    mutationId: "deploy-two",
    expectedReleaseId: "one",
    release: {
      ...release,
      releaseId: "two",
      assets: [secondAsset],
    },
  });
  assert.deepEqual(call(client, "uis.deploy", initial), committed);
  assert.equal(readFileSync(kernel.uiFiles.path("test-ui", "one", "index.html"), "utf8"),
    "<!doctype html><p>release one</p>");
  assert.throws(
    () =>
      call(client, "uis.rollback", {
        uiId: "test-ui",
        releaseId: "one",
        expectedReleaseId: "one",
        mutationId: "stale-rollback",
      }),
    code("release_conflict"),
  );
  ready(node, {
    ...tool,
    interfaceVersion: "2.0.0",
    description: "Changed contract",
  });
  assert.throws(
    () =>
      call(client, "uis.rollback", {
        uiId: "test-ui",
        releaseId: "one",
        expectedReleaseId: "two",
        mutationId: "incompatible-rollback",
      }),
    code("service_interface_changed"),
  );
  ready(node);
  call(client, "uis.rollback", {
    uiId: "test-ui",
    releaseId: "one",
    expectedReleaseId: "two",
    mutationId: "rollback",
  });
  assert.throws(
    () =>
      call(client, "uis.deploy", {
        ...initial,
        mutationId: "traversal",
        expectedReleaseId: "one",
        release: {
          ...release,
          releaseId: "invalid",
          assets: [{ ...release.assets[0], path: "../escape" }],
        },
      }),
    code("invalid_arguments"),
  );
  assert.equal(
    call<{ currentReleaseId: string }>(client, "uis.get", { uiId: "test-ui" })
      .currentReleaseId,
    "one",
  );
});
