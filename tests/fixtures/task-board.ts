import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { HiveKernel } from "../../services/hive/src/kernel.js";
import type { ConnectionContext } from "../../services/hive/src/kernel.js";
import { LiveRouting } from "../../services/hive/src/live-routing.js";
import type { PreparedCall } from "../../services/hive/src/live-routing.js";
import { digest, hashJson } from "../../packages/contracts/src/canonical.js";
import { toolDefinitionHash } from "../../packages/contracts/src/tool-definition.js";
import type {
  Agent,
  TaskBoard,
  Wire,
} from "../../packages/contracts/src/generated.js";
import type {
  OperationName,
  Params,
  Result,
  RpcClient,
} from "../../packages/sdk/src/client.js";
import { agentRegistry, nativeProjectSchemaVersion } from "../../services/agent-manager/src/registry.js";
import { taskBoardContracts } from "../../services/task-board/src/runtime/schema.js";
import { taskBoardRegistry } from "../../services/task-board/src/runtime/registry.js";
import { browserNoticeTopic } from "../../packages/contracts/src/browser-notice.js";
import { TaskBoardStore } from "../../services/task-board/src/runtime/store.js";
import { TaskBoardEngine } from "../../services/task-board/src/runtime/engine.js";
import { checkedNativeContract } from "../../packages/contracts/src/checked-native-contract.js";

export const fields: TaskBoard.TaskFields = {
  title: "Isolated workflow",
  description: "A real Hive-persisted workflow fixture.",
  acceptanceCriteria: ["Retain results and history."],
  category: null,
  control: "user",
  priority: 2,
  executionRequirement: { kind: "host", hostId: "fixture-host" },
  workspaceRequirement: { kind: "task_workspace" },
  dependencies: [],
  userContact: "ticket",
  nextReviewAt: null,
  dueAt: null,
};
export const content: TaskBoard.ResultContent = {
  summary: "Saved material result.",
  artifacts: [],
  checks: [
    {
      name: "Fixture check",
      status: "passed",
      detail: "Only an isolated fixture.",
      evidence: [],
    },
  ],
  codeReferences: [],
  repositoryResult: null,
  limitations: [],
};
export const context = (operationId: string, callerPrincipalId = "user") => ({
  callerPrincipalId,
  operationId,
  generation: 1,
  signal: new AbortController().signal,
});
export const fixtureModels = () => ({ data: ['fixture-model', 'gpt-fixture', 'gpt-6.1-sol'].map((model, index) => ({ id: model, model, displayName: 'Fixture model',
  description: 'Isolated model catalog', hidden: false, isDefault: index === 0, defaultReasoningEffort: 'high',
  supportedReasoningEfforts: [{ reasoningEffort: 'high', description: 'High' }, { reasoningEffort: 'low', description: 'Low' }, { reasoningEffort: 'xhigh', description: 'Extra high' }] })), nextCursor: null });

export function taskBoardFixture(
  t: { after: (fn: () => void) => void },
  nativeVersion: "0.154.0" = "0.154.0",
  catalogSourceHash?: string,
) {
  const dataRoot = mkdtempSync(join(tmpdir(), "ivy-task-board-"));
  const credentialDigest = digest("task-board-engine-fixture"),
    kernel = new HiveKernel({
      filename: ":memory:",
      publicBaseUrl: "https://workflow.test/ivy",
      version: "test",
      buildId: digest("workflow-test"),
      credentials: [
        { principalId: "task-board-owner", digest: credentialDigest },
      ],
    });
  const routing = new LiveRouting(true);
  t.after(() => {
    first.store.close();
    second.store.close();
    kernel.close();
    rmSync(dataRoot, { recursive: true, force: true });
  });
  const execute = <M extends OperationName>(
    connection: ConnectionContext,
    method: M,
    params: Params<M>,
  ): Result<M> => {
    const result = kernel.execute(connection, {
      jsonrpc: "2.0",
      id: randomUUID(),
      method,
      params,
    });
    assert.equal(result.kind, "result");
    return (result as { kind: "result"; value: Result<M> }).value;
  };
  let afterWrite:
    | ((
        params: Params<"objects.write">,
        result: Result<"objects.write">,
      ) => void | Promise<void>)
    | null = null;
  let afterRead:
    | ((
        params: Params<"objects.read">,
        result: Result<"objects.read">,
      ) => void | Promise<void>)
    | null = null;
  let nativeTool: ((call: PreparedCall) => Promise<unknown>) | null = null;
  const retainedOperations = new Map<string, Agent.Operation>();
  const clients = new Map<
    string,
    { connection: ConnectionContext; client: RpcClient }
  >();
  const client = (node: string, agent = false) => {
    const initial: ConnectionContext = {
      credentialDigest,
      principalId: "task-board-owner",
      transport: "ws",
    };
    const connected = execute(initial, "service.connect", {
      serviceNodeId: node,
      serviceName: agent ? "agent-manager" : "task-board",
      hostId: "fixture-host",
      version: "test",
      buildId: digest(node),
      hiveProtocol: 1,
      ...(agent ? { nativeVersion } : {}),
    });
    const connection = {
        ...initial,
        serviceNodeId: node,
        generation: connected.generation,
      },
      callerContext = { ...initial, principalId: "task-board-owner" },
      contracts = taskBoardContracts();
    execute(
      connection,
      "registry.sync",
      agent
        ? agentRegistry(native)
        : {
            namespaces: [{ namespace: "task-board", description: "TaskBoard fixture", guideMarkdown: "", tools: [], inventoryKinds: [], topics: [browserNoticeTopic("task-board")] }],
            contracts,
            requiredContracts: taskBoardRegistry().requiredContracts,
          },
    );
    execute(connection, "service.heartbeat", { ready: true, diagnostics: [] });
    const liveCatalog = kernel.registry.liveCatalog(node);
    routing.update({
      node: kernel.registry.node(node),
      generation: connected.generation,
      ...(liveCatalog ? { catalog: liveCatalog } : {}),
    });
    const rpc: RpcClient = {
      async request<M extends OperationName>(method: M, params: Params<M>) {
        if (
          method === "tools.call" &&
          (nativeTool || retainedOperations.size)
        ) {
          const call = routing.prepare(
            callerContext,
            params as unknown as Parameters<LiveRouting["prepare"]>[1],
          );
          const retained =
            call.definition.name === "operation"
              ? retainedOperations.get(
                  String(
                    (call.arguments as { operationId?: string }).operationId,
                  ),
                )
              : undefined;
          assert.ok(
            retained || nativeTool,
            "Fixture must provide the authoritative native operation",
          );
          return routing.complete(
            call,
            retained ? structuredClone(retained) : call.definition.name === "projects"
              ? { source: "native", observedAt: new Date().toISOString(),
                  projects: [{ nativeId: "fixture-task-project", source: "native", name: "Internal", paths: [dataRoot] }],
                  defaults: { projectRoot: dataRoot, internalProjectRoot: dataRoot } }
              : await nativeTool!(call),
          ) as Result<M>;
        }
        const value = execute(connection, method, params);
        if (method === "registry.sync" || method === "service.heartbeat") {
          const liveCatalog = kernel.registry.liveCatalog(node);
          routing.update({
            node: kernel.registry.node(node),
            ...(liveCatalog ? { catalog: liveCatalog } : {}),
          });
        }
        if (method === "objects.write")
          await afterWrite?.(
            params as Params<"objects.write">,
            value as Result<"objects.write">,
          );
        if (method === "objects.read")
          await afterRead?.(
            params as Params<"objects.read">,
            value as Result<"objects.read">,
          );
        return value;
      },
    };
    clients.set(node, { connection, client: rpc });
    return rpc;
  };
  const native = (catalogSourceHash
    ? checkedNativeContract(nativeVersion, { sourceHash: catalogSourceHash })
    : checkedNativeContract(nativeVersion)).catalog;
  const settings: TaskBoard.Settings = {
    principalId: "task-board-owner",
    rootObjectId: null,
    scheduler: { enabled: true, intervalMs: 1000, pageSize: 50 },

    phoneTarget: null,
  };
  const one = client("task-board-one"),
    two = client("task-board-two");
  client("native-agent", true);
  execute(clients.get("native-agent")!.connection, "inventory.sync", {
    namespace: "codex", kind: "project", schemaVersion: nativeProjectSchemaVersion, mode: "snapshot", snapshotRevision: 1,
    entries: [{ nativeId: "fixture-task-project", summary: { nativeId: "fixture-task-project", source: "native",
      name: "Internal", paths: [dataRoot] }, observedAt: new Date().toISOString() }],
  });
  const first = new TaskBoardEngine(
    new TaskBoardStore(one, null, dataRoot),
    settings,
    { serviceNodeId: "task-board-one", generation: 1 },
  );
  const second = new TaskBoardEngine(
    new TaskBoardStore(two, null, dataRoot),
    settings,
    { serviceNodeId: "task-board-two", generation: 1 },
  );
  const tools = agentRegistry(native).namespaces[0]!.tools;
  const definition = (name: string) =>
    toolDefinitionHash(tools.find((value) => value.name === name)!);
  const workspace: TaskBoard.WorkspaceResolution = {
    hostId: "fixture-host",
    serviceNodeId: "native-agent",
    canonicalCwd: dataRoot,
    taskRoot: dataRoot,
    bootstrapPath: dataRoot,
    intendedPath: dataRoot,
    sourcePath: null,
    project: null,
    useWorktree: false,
    repository: null,
  };
  const target: TaskBoard.Target = {
    hostId: "fixture-host",
    serviceNodeId: "native-agent",
  };
  const plan = {
    nativeVersion,
    catalogSourceHash: native.sourceHash,
    location: {
      ...target,
      kind: "internal" as const,
      cwd: dataRoot,
      projectId: "fixture-task-project",
    },
    definitions: {
      threadStart: definition("thread/start"),
      threadResume: definition("thread/resume"),
      turnStart: definition("turn/start"),
      turnInterrupt: definition("turn/interrupt"),
    },
    threadStart: { cwd: dataRoot },
    threadResume: { cwd: dataRoot },
    turnStart: {
      input: [
        { type: "text", text: "Perform the isolated task.", text_elements: [] },
      ],
    },
  } as TaskBoard.NativePlanDraft;
  const nativeManagement = async (call: PreparedCall) => {
    if (call.definition.name === 'read') {
      const request = call.arguments as Agent.ReadInput, threadId = (request.params as { threadId: string }).threadId;
      if (request.method !== 'thread/read') throw new Error('Unexpected fixture native read.');
      return { schemaVersion: 1, observationId: randomUUID(), callerPrincipalId: call.callerPrincipalId, serviceNodeId: 'native-agent',
        nativeVersion, nativeExecutableHash: native.nativeExecutableHash, catalogHash: hashJson(native), epoch: 'fixture-epoch', requestId: randomUUID(),
        observedAt: new Date().toISOString(), method: request.method, params: request.params, requestHash: hashJson({ method: request.method, params: request.params }),
        reply: { result: { thread: { cliVersion: nativeVersion, createdAt: 1788690000, cwd: String(plan.threadStart.cwd), ephemeral: false,
          id: threadId, modelProvider: 'openai', preview: 'Fixture', projectId: null, sessionId: threadId, source: 'appServer',
          status: { type: 'idle' }, turns: [], updatedAt: 1788690000, path: '/fixture/sessions/' + threadId + '.jsonl' } } } } satisfies Agent.ReadObservation;
    }
    if (call.definition.name === 'model/list') return fixtureModels();
    if (call.definition.name === "resolveProject")
      return {
        kind: "task",
        cwd: dataRoot,
        project: {
          nativeId: "fixture-task-project",
          source: "native",
          name: "Fixture task",
          paths: [dataRoot],
          kind: "task",
        },
      } satisfies Agent.ProjectLocation;
    if (call.definition.name === "resolveWorkspace")
      return workspace as unknown as Wire.Json;
    if (call.definition.name === "capabilities")
      return {
        serviceNodeId: "native-agent",
        hostId: "fixture-host",
        revision: 1,
        capabilities: [],
        updatedAt: new Date().toISOString(),
        operationId: null,
      } satisfies Agent.CapabilityProfile;
    if (call.definition.name === "catalog")
      return native as unknown as Wire.Json;
    if (call.definition.name === "status")
      return {
        serviceNodeId: "native-agent",
        hostId: "fixture-host",
        serverType: "codex",
        nativeVersion,
        nativeExecutableHash: native.nativeExecutableHash,
        catalogHash: hashJson(native),
        epoch: "fixture-epoch",
        pid: null,
        state: "ready",
        observedAt: new Date().toISOString(),
        code: null,
        initialized: null,
        pendingInputs: 0,
        operations: {
          retained: 0,
          maximum: 1000,
          bytes: 0,
          maximumBytes: 1048576,
        },
        observedMethods: [],
        capabilities: {
          serviceNodeId: "native-agent",
          hostId: "fixture-host",
          revision: 1,
          capabilities: [],
          updatedAt: new Date().toISOString(),
          operationId: null,
        },
      } satisfies Agent.Status;
    throw new Error(
      "Fixture has no response for agent." + call.definition.name,
    );
  };
  nativeTool = nativeManagement;
  return {
    first,
    second,
    settings,
    plan,
    kernel,
    clients,
    intercept: (handler: typeof afterWrite) => {
      afterWrite = handler;
    },
    nativeTools: (handler: typeof nativeTool) => {
      nativeTool = handler;
    },
    nativeManagement,
    retainOperation: (operation: Agent.Operation) =>
      retainedOperations.set(operation.operationId, structuredClone(operation)),
    interceptRead: (handler: typeof afterRead) => {
      afterRead = handler;
    },
    invoke: (
      request: TaskBoard.ActionInput | TaskBoard.SchedulerRequest,
      caller = "user",
      engine = first,
    ) =>
      request.action === "start" || request.action === "continue" || request.action === "recoverThread"
        ? engine.schedule(request)
        : engine.invoke(request, context(request.operationId, caller)),
    worker: async (
      request: TaskBoard.DurableRequest,
      caller = "worker",
      engine = first,
    ) => {
      const operation = await engine.operations.begin(request, {
        principalId: caller,
        ...engine.owner,
        source: "worker",
      });
      return engine.recoverOperation(operation.pin.objectId);
    },
    create: (
      overrides: Partial<TaskBoard.TaskFields> = {},
      operationId = randomUUID(),
    ) =>
      first.invoke(
        { action: "create", operationId, fields: { ...fields, ...overrides } },
        context(operationId),
      ),
    async planned(control: TaskBoard.TaskFields["control"] = "agent", workspaceRequirement = fields.workspaceRequirement) {
      const id = randomUUID(),
        result = await first.invoke(
          { action: "savePlan", operationId: id, plan },
          context(id),
        );
      const createId = randomUUID(),
        created = await first.invoke(
          {
            action: "create",
            operationId: createId,
            fields: { ...fields, control, workspaceRequirement },
          },
          context(createId),
        );
      const readyId = randomUUID(),
        ready = await first.invoke(
          {
            action: "transition",
            operationId: readyId,
            taskId: created.task!.objectId,
            expectedRevision: created.task!.revision,
            workflowState: "todo",
            detail: null,
          },
          context(readyId),
        );
      return { plan: result.plan!, task: ready.task!, target, workspace };
    },
  };
}
