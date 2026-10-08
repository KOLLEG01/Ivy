import {
  hashJson,
  canonical,
} from "../../../packages/contracts/src/canonical.js";
import { storageInspection } from "./inspect-storage.js";
import { STORAGE_FORMAT } from "./storage-schema.js";
import {
  IvyError,
  fail,
  requireThat,
} from "../../../packages/contracts/src/errors.js";
import type {
  DataContract,
  EventFilter,
  ObjectWrite,
  ObjectQuery,
  ResourceRef,
} from "../../../packages/contracts/src/types.js";
import {
  operations,
  validateInput,
  validateOutput,
  validateRequest,
  validateShared,
} from "../../../packages/contracts/src/core-validation.js";
import type { RpcRequest } from "../../../packages/contracts/src/core-validation.js";
import { Uis } from "./uis.js";
import { Events } from "./events.js";
import { Inventory } from "./inventory.js";
import type { InventoryListInput } from "./inventory.js";
import { Objects } from "./objects.js";
import { Retention } from "./retention.js";
import { pruneObjectRevisions } from "./object-retention.js";
import type { PageInput } from "./objects.js";
import { catalogPage, utc } from "./paging.js";
import { queryObjects } from "./query.js";
import { Registry } from "./registry.js";
import { Discovery } from "./discovery.js";
import { wikiHandlers } from "./wiki.js";
import type {
  Diagnostic,
  RegistrySync,
  ServiceConnect,
  Target,
} from "./registry.js";
import { HiveStore } from "./store.js";
import { Configurations } from "./configurations.js";
import { PackageCatalogObject } from "./package-catalog-object.js";
import { UiFiles } from "./ui-files.js";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { randomUUID } from "node:crypto";
import { rmSync } from "node:fs";
import type { AuthenticatedContext, ServiceContext } from "./store.js";
import type {
  Operation,
  OperationName,
  Params,
  Result,
  Wire,
} from "../../../packages/contracts/src/generated.js";

export interface ConnectionContext extends AuthenticatedContext {
  transport: "http" | "ws" | "mcp";
  serviceNodeId?: string;
  generation?: number;
}
export interface KernelOptions {
  filename: string;
  uiRoot?: string;
  publicBaseUrl: string;
  version: string;
  buildId: string;
  credentials: { principalId: string; digest: string }[];
  validateMessages?: boolean;
  resetBootstrapCredentialDigest?: string;
}
export type KernelResult = { kind: "result"; value: unknown };
type Handler = (
  context: ConnectionContext,
  params: Record<string, unknown>,
) => unknown;
const resetBootstrapMethods = new Set([
  "uis.inspect",
  "uis.stageAsset",
  "contracts.register",
  "objects.write",
  "objects.read",
  "uis.deploy",
]);

export class HiveKernel {
  readonly store: HiveStore;
  readonly objects: Objects;
  readonly registry: Registry;
  readonly discovery: Discovery;
  readonly events: Events;
  readonly inventory: Inventory;
  readonly uis: Uis;
  readonly uiFiles: UiFiles;
  private readonly temporaryUiRoot: boolean;
  readonly retention: Retention;
  readonly configurations: Configurations;
  readonly packageCatalog: PackageCatalogObject;
  private readonly handlers = new Map<string, Handler>();
  private connectionGeneration = 0;

  constructor(readonly options: KernelOptions) {
    this.store = new HiveStore(options.filename);
    this.temporaryUiRoot = options.filename === ":memory:" && !options.uiRoot;
    this.uiFiles = new UiFiles(options.uiRoot ?? (this.temporaryUiRoot
      ? join(tmpdir(), "ivy-ui-" + randomUUID())
      : join(dirname(options.filename), "ui-releases")));
    try {
      this.store.configureCredentials(options.credentials);
    } catch (error) {
      this.store.close();
      throw error;
    }
    this.objects = new Objects(this.store);
    this.uiFiles.migrateLegacy(this.store);
    this.registry = new Registry(this.store);
    this.discovery = new Discovery(this.registry);
    this.events = new Events(this.store);
    this.inventory = new Inventory(this.store, this.registry);
    this.uis = new Uis(this.store, this.registry, this.uiFiles);
    this.retention = new Retention(this.store, this.registry);
    this.configurations = new Configurations(this.store, this.objects);
    this.packageCatalog = new PackageCatalogObject(this.store, this.objects, () => {
      const builds = new Set<string>([this.options.buildId]);
      for (const node of this.registry.nodes()) builds.add(node.buildId);
      // Last observations also protect disconnected hosts until they report a replacement.
      for (const { snapshot } of this.registry.hostObservations()) {
        if (snapshot.status.executor?.buildId) builds.add(snapshot.status.executor.buildId);
        for (const instance of snapshot.status.instances)
          for (const build of [instance.installedBuild, instance.observedBuild]) if (build) builds.add(build);
        for (const operation of snapshot.status.unfinished)
          for (const build of [operation.previousBuild, operation.targetBuild, operation.observedBuild]) if (build) builds.add(build);
      }
      return builds;
    });
    this.registerHandlers();
    const serverOperations = new Set([
      "tools.call",
      "discovery.call",
      "notifications.subscribe",
      "packages.catalog",
      "packages.authorizeUpload",
    ]);
    requireThat(
      [...Object.keys(operations)].every(
        (method) => serverOperations.has(method) || this.handlers.has(method),
      ),
      "internal_error",
      "Canonical operation catalog is incomplete.",
    );
  }
  close(): void {
    this.store.close();
    if (this.temporaryUiRoot) rmSync(this.uiFiles.root, { recursive: true, force: true });
  }
  private add<P>(
    method: string,
    handler: (context: ConnectionContext, params: P) => unknown,
  ): void {
    requireThat(
      operations[method] && !this.handlers.has(method),
      "internal_error",
      "Invalid canonical handler registration.",
    );
    this.handlers.set(method, (context, params) =>
      handler(context, params as P),
    );
  }
  private service(context: ConnectionContext): ServiceContext {
    requireThat(
      context.transport === "ws" && context.serviceNodeId && context.generation,
      "stale_generation",
      "This operation requires the current registered service connection.",
    );
    return {
      ...context,
      serviceNodeId: context.serviceNodeId,
      generation: context.generation,
    };
  }
  execute(context: ConnectionContext, value: unknown): KernelResult {
    if (context.serviceNodeId !== undefined) this.service(context);
    validateRequest(value);
    if (this.options.resetBootstrapCredentialDigest)
      requireThat(
        context.transport === "http" &&
          context.credentialDigest ===
            this.options.resetBootstrapCredentialDigest &&
          resetBootstrapMethods.has(value.method),
        "maintenance_active",
        "Hive accepts only the configured ui bootstrap while the runtime reset is held.",
      );
    if (
      value.method === "service.connect" &&
      value.params["hiveProtocol"] !== 1
    )
      fail("unsupported_protocol", "This Hive supports protocol major 1.");
    try {
      validateInput(value.method, value.params);
    } catch (error) {
      if (
        value.method === "registry.sync" &&
        context.serviceNodeId !== undefined
      ) {
        const node = this.registry.node(context.serviceNodeId),
          now = new Date().toISOString();
        this.registry.save({
          ...node,
          ready: false,
          synced: false,
          lastFailedSyncAt: now,
        });
        this.registry.diagnostic({
          code: "registry_invalid",
          resource: { serviceNodeId: node.serviceNodeId },
          severity: "error",
          source: "hive",
          firstObservedAt: now,
          lastObservedAt: now,
          status: "current",
          message: "Registry input failed contract validation.",
        });
      }
      throw error;
    }
    if (
      operations[value.method]!.access === "service" &&
      value.method !== "service.connect"
    )
      this.service(context);
    if (value.method === "objects.delete")
      requireThat(context.transport !== "mcp", "forbidden", "Permanent deletion is unavailable through MCP.");
    if (value.method === "discovery.call") {
      const params = value.params as Operation.DiscoveryCallParams;
      requireThat(
        params.expectedCallerPrincipalId === undefined ||
          params.expectedCallerPrincipalId === context.principalId,
        "caller_changed",
        "Return to the original caller before reconciling or repeating this action.",
      );
      return this.execute(context, {
        ...value,
        ...this.discovery.resolveCall(params),
      });
    }
    return this.store.withBudget(
      () => {
        requireThat(
          value.method !== "tools.call",
          "internal_error",
          "Tool routing is owned by the live connection table.",
        );
        const handler = this.handlers.get(value.method);
        requireThat(handler, "not_found", "Unknown Hive operation.");
        const result = handler(context, value.params);
        if (this.options.validateMessages === true) {
          try {
            validateOutput(value.method, result);
          } catch {
            fail(
              "internal_error",
              "The completed operation produced an invalid server result.",
              "completed",
            );
          }
        }
        return { kind: "result", value: result };
        // Large retained object families still need the same bounded query work,
        // but the normal one-second budget turns a valid paginated recovery scan
        // into a permanent retry loop once the Reference Hive has grown. Keep the
        // longer allowance specific to reads; writes and other operations retain
        // their tighter deadline.
      },
      value.method === "registry.sync"
        ? 25_000
        : value.method === "objects.query"
          ? 15_000
          : value.method === "objects.delete"
            ? 15_000
          : 1000,
    );
  }
  private page<T>(
    method: string,
    params: PageInput,
    records: T[],
    key: (record: T) => string,
    extra?: unknown,
  ) {
    const { cursor: _cursor, ...identity } = params;
    return catalogPage(
      this.store,
      records,
      {
        method,
        ...identity,
        ...(extra === undefined ? {} : { snapshot: extra }),
      },
      key,
      params,
    );
  }
  private registerHandlers(): void {
    const wiki = (context: ConnectionContext) => wikiHandlers(<M extends OperationName>(name: M, input: Params<M>) =>
      this.execute(context, { jsonrpc: '2.0', id: 'wiki', method: name, params: input }).value as Result<M>,
      params => this.objects.write(context, { mutationId: params.mutationId, objectId: params.objectId,
        expectedRevision: params.expectedRevision, contractVersion: '1.0.0', references: {},
        content: { encoding: 'text', value: params.markdown } }, true));
    this.add('wiki.search', (context, params: Operation.WikiSearchParams) => wiki(context)['wiki.search'](params));
    this.add('wiki.list', (context, params: Operation.WikiListParams) => wiki(context)['wiki.list'](params));
    this.add('wiki.read', (context, params: Operation.WikiReadParams) => wiki(context)['wiki.read'](params));
    this.add('wiki.create', (context, params: Operation.WikiCreateParams) => wiki(context)['wiki.create'](params));
    this.add('wiki.update', (context, params: Operation.WikiUpdateParams) => wiki(context)['wiki.update'](params));
    this.add('wiki.history', (context, params: Operation.WikiHistoryParams) => wiki(context)['wiki.history'](params));
    this.add('wiki.move', (context, params: Operation.WikiMoveParams) => wiki(context)['wiki.move'](params));
    this.add('wiki.archive', (context, params: Operation.WikiArchiveParams) => wiki(context)['wiki.archive'](params));
    this.add(
      "discovery.list",
      (context, params: Operation.DiscoveryListParams) =>
        this.discovery.list(params, context.transport === 'mcp'),
    );
    this.add("discovery.instructions", () => this.discovery.instructions());
    this.add("system.instructions", (_context, params: Operation.SystemInstructionsParams) => this.discovery.instructionsPage(params));
    this.add("system.toolSchema", (_context, params: Operation.SystemToolSchemaParams) => this.discovery.toolSchema(params));
    this.add(
      "discovery.describe",
      (context, params: Operation.DiscoveryDescribeParams) =>
        this.discovery.describe(params, context.transport === 'mcp'),
    );
    this.add("system.status", (context) => ({
      callerPrincipalId: context.principalId,
      version: this.options.version,
      buildId: this.options.buildId,
      hiveProtocol: 1,
      runtimeEpoch: this.store.runtimeEpoch,
      ready: true,
      serverTime: new Date().toISOString(),
      storageFormat: STORAGE_FORMAT,
      publicBaseUrl: this.options.publicBaseUrl,
    }));
    this.add("objects.writeReceipt", (context, params: { mutationId: string; expectedRequestHash: string }) =>
      this.store.objectWriteReceipt(context, params.mutationId, params.expectedRequestHash));
    this.add(
      "system.inspectStorage",
      (
        _context,
        params: import("../../../packages/contracts/src/generated.js").Operation.SystemInspectStorageParams,
      ) => storageInspection(this.store.db, params.contracts),
    );
    this.add(
      "system.diagnostics",
      (_context, params: PageInput & { status?: Diagnostic["status"] | "unresolved" }) => {
        const diagnostics = this.registry.diagnostics(params.status);
        return this.page("system.diagnostics", params, diagnostics, (value) =>
          hashJson({
            resource: value.resource,
            code: value.code,
            source: value.source,
          }),
        );
      },
    );
    this.add(
      "contracts.register",
      (context, params: { definition: DataContract; mutationId: string }) =>
        this.objects.registerAgent(context, params),
    );
    this.add(
      "contracts.get",
      (_context, params: { key: string; version?: string }) =>
        this.objects.getContract(params.key, params.version),
    );
    this.add(
      "contracts.validate",
      (
        _context,
        params: {
          key: string;
          contractVersion: string;
          content: ObjectWrite["content"];
        },
      ) => {
        const { contentHash, byteLength } = this.objects.decode(
          this.store.contract(params.key, params.contractVersion),
          params.content,
        );
        return { valid: true, contentHash, byteLength };
      },
    );
    const contractRecords = (params: PageInput & { key?: string; allVersions?: boolean }): DataContract[] => {
        let records = this.store
          .all(
            "SELECT definition FROM contracts WHERE (? IS NULL OR key=?)",
            params.key ?? null,
            params.key ?? null,
          )
          .map((row) => JSON.parse(String(row["definition"])) as DataContract);
        if (!params.allVersions)
          records = [...new Set(records.map((record) => record.key))].map(
            (key) => this.objects.getContract(key),
          );
        return records;
    };
    this.add(
      "contracts.list",
      (
        _context,
        params: PageInput & { key?: string; allVersions?: boolean },
      ) => {
        return this.page(
          "contracts.list",
          params,
          contractRecords(params),
          (record) => record.key + "@" + record.version,
        );
      },
    );
    this.add("contracts.summaries", (_context, params: PageInput & { key?: string; allVersions?: boolean }) =>
      this.page("contracts.summaries", params,
        contractRecords(params).map(record => ({
          key: record.key,
          version: record.version,
          description: record.specMarkdown.replace(/\s+/g, " ").trim().slice(0, 512) || record.key,
          owner: record.owner,
          mediaType: record.mediaType,
          hasJsonSchema: record.jsonSchema !== undefined,
        })),
        record => record.key + "@" + record.version));
    this.add(
      "objects.stat",
      (_context, params: Parameters<Objects["stat"]>[0]) =>
        this.objects.stat(params),
    );
    this.add(
      "objects.read",
      (context, params: Parameters<Objects["read"]>[0]) => {
        const read = this.objects.read(params);
        return context.sessionDigest
          ? this.configurations.projectRead(read)
          : read;
      },
    );
    this.add("objects.write", (context, params: ObjectWrite) =>
      this.objects.write(context, params),
    );
    this.add("objects.pruneRevisions", (context, params: Parameters<typeof pruneObjectRevisions>[2]) =>
      pruneObjectRevisions(this.store, context as Parameters<typeof pruneObjectRevisions>[1], params));
    this.add(
      "objects.move",
      (context, params: Parameters<Objects["move"]>[1]) =>
        this.objects.move(context, params),
    );
    this.add(
      "objects.reorder",
      (context, params: Parameters<Objects["reorder"]>[1]) =>
        this.objects.reorder(context, params),
    );
    this.add(
      "objects.archive",
      (context, params: Parameters<Objects["archive"]>[1]) =>
        this.objects.archive(context, params),
    );
    this.add(
      "objects.delete",
      (context, params: Parameters<Objects["delete"]>[1]) =>
        this.objects.delete(context, params),
    );
    this.add(
      "objects.list",
      (_context, params: Parameters<Objects["list"]>[0]) =>
        this.objects.list(params),
    );
    this.add(
      "objects.history",
      (_context, params: Parameters<Objects["history"]>[0]) =>
        this.objects.history(params),
    );
    this.add(
      "objects.tree",
      (_context, params: Parameters<Objects["tree"]>[0]) =>
        this.objects.tree(params),
    );
    this.add(
      "objects.search",
      (context, params: Parameters<Objects["search"]>[0]) =>
        this.objects.search({
          ...params,
          ...(context.sessionDigest
            ? { excludeContractKey: this.configurations.contractKey }
            : {}),
        }),
    );
    this.add("objects.query", (context, params: ObjectQuery) => {
      requireThat(
        !context.sessionDigest ||
          !this.configurations.isConfigurationContract(params.contractKey),
        "forbidden",
        "Browser sessions cannot query host configuration fields.",
      );
      return queryObjects(this.store, params);
    });
    this.add("retention.preview", () => this.retention.preview());
    this.add("retention.status", () => this.retention.status());
    this.add("service.connect", (context, params: ServiceConnect) => {
      requireThat(
        context.transport === "ws" && !context.serviceNodeId,
        "stale_generation",
        "service.connect must initialize a new WebSocket session.",
      );
      const generation =
        context.generation ??
        (this.connectionGeneration === Number.MAX_SAFE_INTEGER
          ? 1
          : this.connectionGeneration + 1);
      this.connectionGeneration = Math.max(
        this.connectionGeneration,
        generation,
      );
      return this.registry.connect({ ...context, generation }, params);
    });
    this.add("registry.sync", (context, params: RegistrySync) =>
      this.registry.sync(this.service(context), params),
    );
    this.add(
      "service.heartbeat",
      (context, params: Parameters<Registry["heartbeat"]>[1]) =>
        this.registry.heartbeat(this.service(context), params),
    );
    this.add(
      "hosts.report",
      (context, params: Parameters<Registry["reportHost"]>[1]) =>
        this.registry.reportHost(this.service(context), params),
    );
    this.add(
      "hosts.observations",
      (_context, params: PageInput & { hostId?: string }) => {
        const observations = this.registry.hostObservations(params.hostId);
        requireThat(
          observations.length <= 1000,
          "limit_exceeded",
          "Host observation catalog exceeds its operating bound.",
        );
        return this.page(
          "hosts.observations",
          params,
          observations,
          (value) => value.snapshot.status.hostId,
        );
      },
    );
    this.add("hostConfigurations.list", (_context, params: PageInput) =>
      this.page(
        "hostConfigurations.list",
        params,
        this.configurations.list(),
        (value) => value.hostId,
      ),
    );
    this.add(
      "hostConfigurations.get",
      (context, params: Operation.HostConfigurationsGetParams) => {
        requireThat(
          !context.sessionDigest,
          "forbidden",
          "Browser sessions use the redacted host configuration editor.",
        );
        return this.configurations.get(params.hostId, params.revision);
      },
    );
    this.add(
      "hostConfigurations.put",
      (context, params: Operation.HostConfigurationsPutParams) => {
        requireThat(
          !context.sessionDigest,
          "forbidden",
          "Browser sessions use redacted host configuration saves.",
        );
        return this.configurations.put(context, params);
      },
    );
    this.add(
      "hostConfigurations.edit",
      (_context, params: Operation.HostConfigurationsEditParams) =>
        this.configurations.edit(params.hostId, params.revision),
    );
    this.add(
      "hostConfigurations.save",
      (context, params: Operation.HostConfigurationsSaveParams) =>
        this.configurations.save(context, params),
    );
    this.add(
      "hostConfigurations.history",
      (_context, params: Operation.HostConfigurationsHistoryParams) =>
        this.configurations.history(params),
    );
    this.add("hosts.list", (_context, params: PageInput) => {
      const nodes = this.registry.nodes();
      const hosts = [...new Set(nodes.map((node) => node.hostId))].map(
        (hostId) => ({
          hostId,
          serviceNodeIds: nodes
            .filter((node) => node.hostId === hostId)
            .map((node) => node.serviceNodeId),
          connected: nodes.some(
            (node) => node.hostId === hostId && node.connected,
          ),
        }),
      );
      return this.page("hosts.list", params, hosts, (host) => host.hostId);
    });
    this.add(
      "serviceNodes.list",
      (
        _context,
        params: PageInput & { hostId?: string; serviceName?: string },
      ) =>
        this.page(
          "serviceNodes.list",
          params,
          this.registry
            .nodes()
            .filter(
              (node) =>
                (!params.hostId || node.hostId === params.hostId) &&
                (!params.serviceName ||
                  node.serviceName === params.serviceName),
            ),
          (node) => node.serviceNodeId,
        ),
    );
    this.add(
      "serviceNodes.get",
      (_context, params: { serviceNodeId: string }) =>
        this.registry.node(params.serviceNodeId),
    );
    this.add(
      "serviceNodes.contracts",
      (_context, params: Operation.ServiceNodesContractsParams) => {
        const node = this.registry.node(params.serviceNodeId),
          catalog = this.registry.getRegistry(params.serviceNodeId);
        const records = (catalog?.requiredContracts ?? [])
          .map((value, index) => ({ value, index }))
          .filter((item) => !params.key || item.value.key === params.key);
        const result = this.page(
          "serviceNodes.contracts",
          params,
          records,
          (item) => String(item.index).padStart(8, "0"),
          hashJson({ capturedAt: node.lastSuccessfulSyncAt, records }),
        );
        return {
          provider: { node, eligible: this.registry.eligible(node) },
          hasCatalog: catalog !== null,
          capturedAt: node.lastSuccessfulSyncAt,
          items: result.items.map((item) => item.value),
          nextCursor: result.nextCursor,
        };
      },
    );
    this.add("namespaces.list", (_context, params: PageInput) => {
      const namespaces = new Map<
        string,
        { node: ReturnType<Registry["node"]>; eligible: boolean }[]
      >();
      for (const node of this.registry.nodes())
        for (const ns of this.registry.getRegistry(node.serviceNodeId)
          ?.namespaces ?? [])
          namespaces.set(ns.namespace, [
            ...(namespaces.get(ns.namespace) ?? []),
            { node, eligible: this.registry.eligible(node) },
          ]);
      return this.page(
        "namespaces.list",
        params,
        [...namespaces].map(([namespace, providers]) => ({
          namespace,
          providers,
        })),
        (ns) => ns.namespace,
      );
    });
    this.add(
      "namespaces.get",
      (_context, params: Target & { namespace: string }) =>
        this.registry.snapshot(params.namespace, params),
    );
    this.add(
      "tools.list",
      (
        _context,
        params: Target & PageInput & { namespace: string; namePrefix?: string },
      ) => {
        const snapshot = this.registry.snapshot(params.namespace, params);
        const tools = this.registry.bindings(
          snapshot.provider.node.serviceNodeId,
          this.registry.namespace(
            snapshot.provider.node.serviceNodeId,
            params.namespace,
          ),
        );
        const catalogIdentity = {
          nodeId: snapshot.provider.node.serviceNodeId,
          tools,
          guide: snapshot.guideMarkdown,
        };
        const page = this.page(
          "tools.list",
          { ...params },
          tools.filter(
            (tool) =>
              !params.namePrefix ||
              tool.definition.name.startsWith(params.namePrefix),
          ),
          (tool) => tool.qualifiedName,
          hashJson(catalogIdentity),
        );
        return {
          provider: snapshot.provider,
          guideMarkdown: snapshot.guideMarkdown,
          ...page,
        };
      },
    );
    this.add("topics.list", (_context, params: Target & PageInput & { namespace?: string; topicPrefix?: string }) => {
      const nodeId = params.serviceNodeId ?? params.resourceRef?.serviceNodeId;
      const namespace = params.namespace ?? params.resourceRef?.namespace;
      requireThat(!params.serviceNodeId || !params.resourceRef || params.serviceNodeId === params.resourceRef.serviceNodeId,
        "target_conflict", "Resource and node selectors disagree.");
      requireThat(!params.namespace || !params.resourceRef || params.namespace === params.resourceRef.namespace,
        "target_conflict", "Resource and namespace selectors disagree.");
      const nodes = nodeId ? [this.registry.node(nodeId)] : this.registry.nodes();
      if (nodeId) {
        const node = nodes[0]!;
        requireThat((!params.hostId || node.hostId === params.hostId) && (!params.serviceName || node.serviceName === params.serviceName),
          "target_conflict", "The selected resource has a different owner.");
        if (namespace) this.registry.namespace(nodeId, namespace);
      }
      const items: { provider: Operation.Provider | null; namespace: string; definition: Wire.TopicDefinition }[] = nodes.filter(node => (!params.hostId || node.hostId === params.hostId) && (!params.serviceName || node.serviceName === params.serviceName))
        .flatMap(node => (this.registry.getRegistry(node.serviceNodeId)?.namespaces ?? [])
          .filter(entry => !namespace || entry.namespace === namespace)
          .flatMap(entry => entry.topics.filter(topic => !params.topicPrefix || topic.topic.startsWith(params.topicPrefix))
            .map(definition => ({ provider: { node, eligible: this.registry.eligible(node) }, namespace: entry.namespace, definition }))));
      if (!nodeId && !params.hostId && (!namespace || namespace === "hive") && (!params.serviceName || params.serviceName === "hive")) {
        for (const row of this.store.all("SELECT definition FROM topics WHERE owner='hive'")) {
          const topic = JSON.parse(String(row["definition"])) as Pick<Wire.TopicDefinition, "topic" | "version" | "payloadSchema">;
          if (params.topicPrefix && !topic.topic.startsWith(params.topicPrefix)) continue;
          items.push({ provider: null, namespace: "hive", definition: { ...topic, title: "Object changes", description: "A Hive object is written, moved, archived or deleted according to the selected topic version.", eventKinds: [] } });
        }
      }
      return this.page("topics.list", params, items,
        item => [item.provider?.node.serviceNodeId ?? "hive", item.namespace, item.definition.topic, item.definition.version].join("\u001f"),
        hashJson(items.map(item => ({ serviceNodeId: item.provider?.node.serviceNodeId ?? "hive", namespace: item.namespace, definition: item.definition }))));
    });
    this.add(
      "inventory.sync",
      (context, params: Parameters<Inventory["sync"]>[1]) =>
        this.inventory.sync(this.service(context), params),
    );
    this.add(
      "inventory.get",
      (_context, params: { resourceRef: ResourceRef }) =>
        this.inventory.get(params.resourceRef),
    );
    this.add(
      "inventory.list",
      (_context, params: PageInput & InventoryListInput) =>
        this.inventory.list(params),
    );
    this.add(
      "events.publish",
      (context, params: Parameters<Events["publish"]>[1]) =>
        this.events.publish(this.service(context), params),
    );
    this.add(
      "events.read",
      (
        _context,
        params: { afterSequence: number; filter: EventFilter; limit?: number },
      ) => this.events.read(params),
    );
    this.add("events.head", () => this.events.head());
    this.add(
      "events.subscribe",
      (context, params: Parameters<Events["subscribe"]>[1]) =>
        this.events.subscribe(this.service(context), params),
    );
    this.add("events.ack", (context, params: Parameters<Events["ack"]>[1]) =>
      this.events.ack(this.service(context), params),
    );
    this.add(
      "events.unsubscribe",
      (context, params: Parameters<Events["unsubscribe"]>[1]) =>
        this.events.unsubscribe(this.service(context), params),
    );
    this.add("uis.list", (_context, params: PageInput) =>
      this.page("uis.list", params, this.uis.list(), (ui) => ui.metadata.uiId),
    );
    this.add("uis.get", (_context, params: { uiId: string }) =>
      this.uis.get(params.uiId),
    );
    this.add("uis.catalog", (_context, params: Operation.UisCatalogParams) =>
      this.uis.catalog(params),
    );
    this.add("uis.releases", (_context, params: Operation.UisReleasesParams) =>
      this.uis.releases(params),
    );
    this.add("uis.inspect", (_context, params: Operation.UisInspectParams) =>
      this.uis.inspect(params),
    );
    this.add("uis.deploy", (context, params: Parameters<Uis["deploy"]>[1]) =>
      this.uis.deploy(context, params),
    );
    this.add("uis.stageAsset", (context, params: Parameters<Uis["stageAsset"]>[1]) =>
      this.uis.stageAsset(context, params),
    );
    this.add(
      "uis.rollback",
      (context, params: Parameters<Uis["rollback"]>[1]) =>
        this.uis.rollback(context, params),
    );
    this.add(
      "deployments.report",
      (context, params: { records: Record<string, unknown>[] }) => {
        const node = this.registry.node(this.service(context).serviceNodeId),
          now = new Date().toISOString();
        this.store.transaction(() => {
          for (const record of params.records) {
            validateShared("DeploymentRecord", record);
            utc(String(record["createdAt"]));
            utc(String(record["updatedAt"]));
            requireThat(
              record["hostId"] === node.hostId,
              "target_conflict",
              "Deployment snapshot belongs to another host.",
            );
            const prior = this.store.get(
              "SELECT record_json FROM deployment_reports WHERE host_id=? AND deployment_id=?",
              node.hostId,
              String(record["deploymentId"]),
            );
            if (prior) {
              const previous = JSON.parse(
                String(prior["record_json"]),
              ) as Record<string, unknown>;
              requireThat(
                [
                  "deploymentId",
                  "hostId",
                  "instanceId",
                  "componentId",
                  "requestHash",
                  "previousBuild",
                  "createdAt",
                ].every((key) => previous[key] === record[key]) &&
                  (previous["targetBuild"] === null ||
                    previous["targetBuild"] === record["targetBuild"]),
                "target_conflict",
                "Deployment observation cannot reinterpret its original local identity.",
              );
              const phases = [
                "preparing",
                "prepared",
                "checking",
                "draining",
                "activating",
                "verifying",
                "rolling_back",
              ];
              const before = phases.indexOf(String(previous["phase"])),
                after = phases.indexOf(String(record["phase"]));
              requireThat(
                String(previous["updatedAt"]) <= String(record["updatedAt"]) &&
                  (before < 0
                    ? canonical(previous) === canonical(record)
                    : after < 0 || after >= before),
                "revision_conflict",
                "Deployment observation cannot regress a phase or replace a terminal result.",
              );
            }
            this.store.run(
              "INSERT INTO deployment_reports VALUES (?,?,?,?,?) ON CONFLICT(host_id,deployment_id) DO UPDATE SET node_id=excluded.node_id,reported_at=excluded.reported_at,record_json=excluded.record_json",
              node.hostId,
              String(record["deploymentId"]),
              node.serviceNodeId,
              now,
              canonical(record),
            );
          }
        });
        return { reportedAt: now };
      },
    );
    const deployments = () =>
      this.store.all("SELECT * FROM deployment_reports").map((row) => ({
        record: JSON.parse(String(row["record_json"])) as Record<
          string,
          unknown
        >,
        serviceNodeId: String(row["node_id"]),
        reportedAt: String(row["reported_at"]),
        stale: !this.registry.node(String(row["node_id"])).connected,
      }));
    this.add(
      "deployments.list",
      (
        _context,
        params: PageInput & { hostId?: string; instanceId?: string },
      ) =>
        this.page(
          "deployments.list",
          params,
          deployments().filter(
            (item) =>
              (!params.hostId || params.hostId === item.record["hostId"]) &&
              (!params.instanceId ||
                params.instanceId === item.record["instanceId"]),
          ),
          (item) => {
            // A retained operation can be reported again long after it finished (for example after a
            // ServiceManager reconnect). Order by the operation's own update time, not report arrival,
            // so an old failure cannot look like the newest rollout in the console.
            const newestFirst = String(
              Number.MAX_SAFE_INTEGER -
                Date.parse(String(item.record["updatedAt"])),
            ).padStart(16, "0");
            return canonical([
              newestFirst,
              item.record["hostId"],
              item.record["deploymentId"],
            ]);
          },
        ),
    );
    this.add(
      "deployments.get",
      (_context, params: { hostId: string; deploymentId: string }) => {
        const record = deployments().find(
          (item) =>
            item.record["hostId"] === params.hostId &&
            item.record["deploymentId"] === params.deploymentId,
        );
        requireThat(record, "not_found", "Deployment snapshot not found.");
        return record;
      },
    );
  }
}

export function rpcFailure(error: unknown, id: RpcRequest["id"] | null) {
  const ivy = IvyError.from(error);
  return {
    status: ivy.httpStatus,
    body: { jsonrpc: "2.0" as const, id, error: ivy.toWire() },
  };
}
export function validRequestId(value: unknown): string | number | null {
  return typeof value === "string" && value.length > 0 && value.length <= 256
    ? value
    : typeof value === "number" && Number.isSafeInteger(value) && value >= 0
      ? value
      : null;
}
