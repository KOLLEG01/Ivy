import { contractVersions } from "./contract-usage.js";
import {
  canonical,
  hashJson,
} from "../../../packages/contracts/src/canonical.js";
import {
  IvyError,
  fail,
  requireThat,
} from "../../../packages/contracts/src/errors.js";
import {
  admitSchema,
  scalarTypes,
} from "../../../packages/contracts/src/schema.js";
import type {
  DataContract,
  ResourceRef,
  Schema,
} from "../../../packages/contracts/src/types.js";
import { validateShared } from "../../../packages/contracts/src/core-validation.js";
import { Events } from "./events.js";
import { Objects } from "./objects.js";
import { utc } from "./paging.js";
import { HiveStore } from "./store.js";
import type { AuthenticatedContext, ServiceContext } from "./store.js";
import type { Host, Wire } from "../../../packages/contracts/src/generated.js";
import { toolDefinitionHash } from "../../../packages/contracts/src/tool-definition.js";

export interface ToolDefinition {
  namespace: string;
  name: string;
  interfaceVersion: string;
  description: string;
  inputSchema: Schema;
  outputSchema: Schema;
  annotations?: {
    title?: string;
    readOnlyHint?: boolean;
    destructiveHint?: boolean;
    idempotentHint?: boolean;
    openWorldHint?: boolean;
  };
  discovery?: NonNullable<Wire.ToolDefinition['discovery']>;
  nativeMethod?: string;
  nativeSchemaIdentity?: string;
}
export interface TopicEventKind {
  kind: string;
  title: string;
  description: string;
}
export interface TopicDefinition {
  topic: string;
  version: string;
  title: string;
  description: string;
  payloadSchema: Schema;
  eventKinds: TopicEventKind[];
}
export interface NamespaceDefinition {
  namespace: string;
  description: string;
  guideMarkdown: string;
  tools: ToolDefinition[];
  discoveryGroups?: { group: string; description: string }[];
  notifications?: {
    name: string;
    version: string;
    description: string;
    payloadSchema: Schema;
  }[];
  topics: TopicDefinition[];
  inventoryKinds: {
    kind: string;
    version: string;
    summarySchema: Schema;
    searchPointers?: string[];
    archivedPointer?: string;
    recencyPointer?: string;
  }[];
}
export type ContractRequirement = Wire.ContractRequirement;
export interface RegistrySync {
  discoveryHint?: string;
  mcpPrefix?: string;
  namespaces: NamespaceDefinition[];
  contracts: DataContract[];
  requiredContracts: ContractRequirement[];
}
export interface ServiceConnect {
  serviceNodeId: string;
  hostId: string;
  serviceName: string;
  instanceMode?: "singleton" | "multiple";
  version: string;
  buildId: string;
  hiveProtocol: 1;
  nativeVersion?: string;
}
export interface ServiceNode extends Omit<ServiceConnect, "nativeVersion" | "instanceMode"> {
  instanceMode: "singleton" | "multiple";
  principalId: string;
  nativeVersion: string | null;
  connected: boolean;
  synced: boolean;
  ready: boolean;
  stale: boolean;
  desiredEnabled: boolean;
  lastContactAt: string | null;
  lastSuccessfulSyncAt: string | null;
  lastFailedSyncAt: string | null;
  lastObservationAt: string | null;
  diagnostic: {
    code: string;
    severity: "info" | "warning" | "error";
    message: string;
    observedAt: string;
  } | null;
}
export interface Target {
  serviceNodeId?: string;
  hostId?: string;
  serviceName?: string;
  resourceRef?: ResourceRef;
}
export interface Diagnostic {
  code: string;
  resource: unknown;
  severity: "info" | "warning" | "error";
  source: string;
  firstObservedAt: string;
  lastObservedAt: string;
  status: "current" | "stale" | "unknown" | "resolved";
  message: string;
}
export interface ToolBinding {
  qualifiedName: string;
  definition: ToolDefinition;
  definitionHash: string;
}
export interface LiveCatalog {
  namespaces: string[];
  tools: ToolBinding[];
  notifications: {
    namespace: string;
    name: string;
    version: string;
    payloadSchema: Schema;
  }[];
}

export const resolvedDiagnosticLimit = 10_000;
export const activeDiagnosticLimit = 4_096;
const diagnosticHistoryMs = 7 * 24 * 60 * 60 * 1000;

export class Registry {
  readonly changedNodes = new Set<string>();
  readonly changedGenerations = new Map<string, number>();
  private readonly bindingCatalog = new Map<
    string,
    Map<string, ToolBinding[]>
  >();
  private readonly contactTimes = new Map<string, string>();
  constructor(readonly store: HiveStore) {
    // Protocol rejections describe a completed frame, not continuing service health.
    // Resolve rows written by older builds so an upgrade cannot retain a false alert.
    const now = new Date().toISOString();
    for (const row of this.store.all(
      "SELECT diagnostic_json FROM diagnostics",
    )) {
      const retained = JSON.parse(String(row["diagnostic_json"])) as Diagnostic;
      if (
        retained.source === "hive" &&
        retained.status === "current" &&
        ["invalid_frame", "stale_generation"].includes(retained.code)
      )
        this.diagnostic({
          ...retained,
          lastObservedAt: now,
          status: "resolved",
        });
    }
  }
  private currentNode(
    node: ServiceNode,
    diagnostics?: Diagnostic[],
  ): ServiceNode {
    const lastContactAt =
      this.contactTimes.get(node.serviceNodeId) ?? node.lastContactAt;
    const stale =
      node.connected &&
      (!lastContactAt || Date.now() - Date.parse(lastContactAt) > 10_000);
    let diagnostic: ServiceNode["diagnostic"] = null;
    for (const value of diagnostics ??
      this.store
        .all(
          "SELECT diagnostic_json FROM diagnostics WHERE json_extract(diagnostic_json,'$.resource.serviceNodeId')=?",
          node.serviceNodeId,
        )
        .map(
          (row) => JSON.parse(String(row["diagnostic_json"])) as Diagnostic,
        )) {
      if (
        value.source !== "service:" + node.serviceNodeId ||
        value.status !== "current"
      )
        continue;
      if (!diagnostic || value.lastObservedAt > diagnostic.observedAt)
        diagnostic = {
          code: value.code,
          severity: value.severity,
          message: value.message,
          observedAt: value.lastObservedAt,
        };
    }
    return {
      ...node,
      instanceMode: node.instanceMode ?? "multiple",
      lastContactAt,
      stale,
      ready: node.ready,
      diagnostic,
    };
  }
  node(serviceNodeId: string): ServiceNode {
    const row = this.store.get(
      "SELECT status_json FROM service_nodes WHERE id=?",
      serviceNodeId,
    );
    requireThat(row, "not_found", "Service Node not found.");
    return this.currentNode(
      JSON.parse(String(row["status_json"])) as ServiceNode,
    );
  }
  nodes(): ServiceNode[] {
    const diagnostics = this.store
      .all(
        "SELECT diagnostic_json FROM diagnostics WHERE json_extract(diagnostic_json,'$.status')='current'",
      )
      .map((row) => JSON.parse(String(row["diagnostic_json"])) as Diagnostic);
    return this.store
      .all("SELECT status_json FROM service_nodes ORDER BY id")
      .map((row) =>
        this.currentNode(
          JSON.parse(String(row["status_json"])) as ServiceNode,
          diagnostics,
        ),
      );
  }
  save(node: ServiceNode, notify = true): void {
    this.store.run(
      "UPDATE service_nodes SET status_json=? WHERE id=?",
      canonical(node),
      node.serviceNodeId,
    );
    this.changedNodes.add(node.serviceNodeId);
    if (notify) this.store.invalidate("services");
  }
  connect(
    context: AuthenticatedContext & { generation?: number },
    params: ServiceConnect,
  ): { serviceNodeId: string; generation: number } {
    requireThat(
      params.serviceName !== "hive",
      "invalid_arguments",
      "The Hive service name is reserved for core operations.",
    );
    const now = new Date().toISOString();
    const result = this.store.transaction(() => {
      const row = this.store.get(
        "SELECT status_json FROM service_nodes WHERE id=?",
        params.serviceNodeId,
      );
      const prior = row
        ? (JSON.parse(String(row["status_json"])) as ServiceNode)
        : null;
      requireThat(
        !prior ||
          (prior.principalId === context.principalId &&
            prior.hostId === params.hostId &&
            prior.serviceName === params.serviceName),
        "target_conflict",
        "A Service Node identity cannot change principal, host or service.",
      );
      const instanceMode = params.instanceMode ?? "multiple";
      const sameName = this.store.get(
        "SELECT id FROM service_nodes WHERE service_name=? AND id<>? AND json_extract(status_json,'$.connected')=1 AND (?='singleton' OR json_extract(status_json,'$.instanceMode')='singleton') LIMIT 1",
        params.serviceName,
        params.serviceNodeId,
        instanceMode,
      );
      requireThat(
        !sameName,
        "target_conflict",
        `Service ${params.serviceName} already has a connected singleton instance.`,
      );
      const generation = context.generation;
      const managed = this.store.get(
        "SELECT host_id,desired_enabled FROM managed_nodes WHERE node_id=?",
        params.serviceNodeId,
      );
      requireThat(
        !managed || managed["host_id"] === params.hostId,
        "target_conflict",
        "Managed Service Node belongs to another host.",
      );
      requireThat(
        Number.isSafeInteger(generation) && generation! >= 1,
        "stale_generation",
        "Service connection has no live routing generation.",
      );
      const node: ServiceNode = {
        ...params,
        instanceMode,
        principalId: context.principalId,
        nativeVersion: params.nativeVersion ?? null,
        connected: true,
        ready: false,
        stale: false,
        synced: false,
        diagnostic: null,
        desiredEnabled: managed
          ? managed["desired_enabled"] === 1
          : (prior?.desiredEnabled ?? true),
        lastContactAt: now,
        lastSuccessfulSyncAt: prior?.lastSuccessfulSyncAt ?? null,
        lastFailedSyncAt: prior?.lastFailedSyncAt ?? null,
        lastObservationAt: prior?.lastObservationAt ?? null,
      };
      if (prior) this.save(node);
      // Format 2 retains the former generation column physically. The constant is
      // only a required storage placeholder; live routing never reads or updates it.
      else
        this.store.run(
          "INSERT INTO service_nodes(id,principal_id,host_id,service_name,generation,status_json) VALUES (?,?,?,?,1,?)",
          params.serviceNodeId,
          context.principalId,
          params.hostId,
          params.serviceName,
          canonical(node),
        );
      this.changedNodes.add(node.serviceNodeId);
      this.store.invalidate("services");
      this.changedGenerations.set(node.serviceNodeId, generation!);
      this.store.run(
        "INSERT INTO service_connections VALUES (?,?,?) ON CONFLICT(node_id) DO UPDATE SET credential_digest=excluded.credential_digest,session_digest=excluded.session_digest",
        node.serviceNodeId,
        context.credentialDigest,
        context.sessionDigest ?? null,
      );
      return { serviceNodeId: params.serviceNodeId, generation: generation! };
    });
    this.contactTimes.set(params.serviceNodeId, now);
    return result;
  }
  disconnect(serviceNodeId: string): void {
    const node = this.node(serviceNodeId);
    this.save({ ...node, connected: false, synced: false, ready: false });
    this.contactTimes.delete(serviceNodeId);
  }
  eligible(node: ServiceNode): boolean {
    return node.connected && node.synced && node.ready && node.desiredEnabled;
  }
  reportHost(
    context: ServiceContext,
    snapshot: Host.ManagementSnapshot,
  ): { sequence: number; reportedAt: string } {
    const reporter = this.node(context.serviceNodeId),
      host = snapshot.status;
    requireThat(
      reporter.serviceName === "service-manager" &&
        reporter.hostId === host.hostId,
      "target_conflict",
      "Host observation requires its current local ServiceManager.",
    );
    utc(host.observedAt);
    requireThat(
      Date.parse(host.observedAt) <= Date.now() + 1000,
      "invalid_arguments",
      "Host observation is in the future.",
    );
    requireThat(
      new Set(host.instances.map((value) => value.instanceId)).size ===
        host.instances.length &&
        new Set(host.instances.map((value) => value.serviceNodeId)).size ===
          host.instances.length,
      "invalid_arguments",
      "Host snapshot contains duplicate instance/node identities.",
    );
    const encoded = canonical(snapshot, 1024 * 1024);
    return this.store.transaction(() => {
      const prior = this.store.get(
        "SELECT * FROM host_reports WHERE host_id=?",
        host.hostId,
      );
      if (prior) {
        requireThat(
          snapshot.sequence >= Number(prior["sequence"]),
          "revision_conflict",
          "Older host observation cannot replace a newer sequence.",
        );
        if (snapshot.sequence === prior["sequence"]) {
          requireThat(
            encoded === prior["snapshot_json"],
            "mutation_conflict",
            "Host observation sequence already identifies different content.",
          );
          return {
            sequence: snapshot.sequence,
            reportedAt: String(prior["reported_at"]),
          };
        }
      }
      for (const instance of host.instances) {
        const known = this.store.get(
          "SELECT status_json FROM service_nodes WHERE id=?",
          instance.serviceNodeId,
        );
        const node = known
          ? (JSON.parse(String(known["status_json"])) as ServiceNode)
          : null;
        const managed = this.store.get(
          "SELECT host_id FROM managed_nodes WHERE node_id=?",
          instance.serviceNodeId,
        );
        requireThat(
          (!node || node.hostId === host.hostId) &&
            (!managed || managed["host_id"] === host.hostId),
          "target_conflict",
          "Reported Service Node belongs to another host.",
        );
        this.store.run(
          "INSERT INTO managed_nodes VALUES (?,?,?) ON CONFLICT(node_id) DO UPDATE SET desired_enabled=excluded.desired_enabled",
          instance.serviceNodeId,
          host.hostId,
          Number(instance.desiredEnabled),
        );
        if (node)
          this.save({ ...node, desiredEnabled: instance.desiredEnabled }, node.desiredEnabled !== instance.desiredEnabled);
      }
      const reportedAt = new Date().toISOString();
      this.store.run(
        "INSERT INTO host_reports VALUES (?,?,?,?,?) ON CONFLICT(host_id) DO UPDATE SET node_id=excluded.node_id,sequence=excluded.sequence,reported_at=excluded.reported_at,snapshot_json=excluded.snapshot_json",
        host.hostId,
        reporter.serviceNodeId,
        snapshot.sequence,
        reportedAt,
        encoded,
      );
      this.store.invalidate("system");
      return { sequence: snapshot.sequence, reportedAt };
    });
  }
  hostObservations(hostId?: string): Host.ManagementObservation[] {
    return this.store
      .all(
        "SELECT * FROM host_reports WHERE (? IS NULL OR host_id=?) ORDER BY host_id LIMIT 1001",
        hostId ?? null,
        hostId ?? null,
      )
      .map((row) => {
        const snapshot = JSON.parse(
          String(row["snapshot_json"]),
        ) as Host.ManagementSnapshot;
        const reporter = this.node(String(row["node_id"]));
        return {
          snapshot,
          serviceNodeId: reporter.serviceNodeId,
          reportedAt: String(row["reported_at"]),
          stale: !reporter.connected || !reporter.ready || reporter.stale,
        };
      });
  }
  diagnostic(value: Diagnostic): void {
    this.commitDiagnostics([value]);
  }
  diagnostics(status?: Diagnostic["status"] | "unresolved"): Diagnostic[] {
    return this.store
      .all("SELECT diagnostic_json FROM diagnostics ORDER BY identity")
      .map((row) => JSON.parse(String(row["diagnostic_json"])) as Diagnostic)
      .filter((diagnostic) =>
        status === "unresolved"
          ? diagnostic.status !== "resolved"
          : !status || diagnostic.status === status,
      );
  }
  private commitDiagnostics(values: Diagnostic[], now = Date.now()): void {
    const updates = new Map<string, Diagnostic>();
    for (const value of values) {
      validateShared("Diagnostic", value);
      utc(value.firstObservedAt);
      utc(value.lastObservedAt);
      const identity = hashJson({
        code: value.code,
        resource: value.resource,
        source: value.source,
      });
      requireThat(
        !updates.has(identity),
        "invalid_arguments",
        "A diagnostic identity may be reported once per heartbeat.",
      );
      updates.set(identity, value);
    }
    const work = () => {
      const retained = new Map(
        this.store
          .all("SELECT identity,diagnostic_json FROM diagnostics")
          .map(
            (row) =>
              [
                String(row["identity"]),
                JSON.parse(String(row["diagnostic_json"])) as Diagnostic,
              ] as const,
          ),
      );
      const originals = new Map(retained);
      for (const [identity, value] of updates) {
        const prior = retained.get(identity);
        retained.set(identity, {
          ...value,
          firstObservedAt: prior?.firstObservedAt ?? value.firstObservedAt,
        });
      }
      requireThat(
        [...retained.values()].filter((value) => value.status !== "resolved")
          .length <= activeDiagnosticLimit,
        "limit_exceeded",
        "Active diagnostic capacity is full.",
      );
      const resolved = [...retained]
        .filter(([, value]) => value.status === "resolved")
        .sort(
          ([leftId, left], [rightId, right]) =>
            left.lastObservedAt.localeCompare(right.lastObservedAt) ||
            leftId.localeCompare(rightId),
        );
      const removed = new Set(
        resolved
          .filter(
            ([, value]) =>
              Date.parse(value.lastObservedAt) <= now - diagnosticHistoryMs,
          )
          .map(([identity]) => identity),
      );
      for (const [identity] of resolved
        .filter(([identity]) => !removed.has(identity))
        .slice(
          0,
          Math.max(0, resolved.length - removed.size - resolvedDiagnosticLimit),
        ))
        removed.add(identity);
      for (const identity of removed) {
        retained.delete(identity);
        this.store.run("DELETE FROM diagnostics WHERE identity=?", identity);
        this.store.invalidate("services");
      }
      for (const [identity] of updates) {
        const next = retained.get(identity);
        if (next) {
          const prior = originals.get(identity);
          if (!prior || prior.status !== next.status || prior.message !== next.message || prior.severity !== next.severity)
            this.store.invalidate("services");
          const encoded = canonical(next);
          if (
            !originals.has(identity) ||
            canonical(originals.get(identity)) !== encoded
          )
            this.store.run(
              "INSERT INTO diagnostics VALUES (?,?) ON CONFLICT(identity) DO UPDATE SET diagnostic_json=excluded.diagnostic_json",
              identity,
              encoded,
            );
        }
      }
    };
    if (this.store.db.isTransaction) work();
    else this.store.transaction(work);
  }
  collectDiagnostics(now = Date.now()): void {
    this.commitDiagnostics([], now);
  }
  sync(
    context: ServiceContext,
    params: RegistrySync,
  ): { generation: number; syncedAt: string } {
    const node = this.node(context.serviceNodeId);
    const nextBindings = new Map<string, ToolBinding[]>();
    const contactedAt = new Date().toISOString();
    this.contactTimes.set(node.serviceNodeId, contactedAt);
    try {
      const result = this.store.transaction(() => {
        validateShared("RegistrySync", params);
        requireThat(
          new Set(params.namespaces.map((ns) => ns.namespace)).size ===
            params.namespaces.length,
          "registry_invalid",
          "Duplicate namespace.",
        );
        const objects = new Objects(this.store),
          events = new Events(this.store);
        for (const contract of params.contracts)
          objects.register(contract, {
            kind: "service",
            serviceName: node.serviceName,
          });
        this.checkRequirements(params.requiredContracts);
        const mcpNames = new Map<string, string>();
        const mcpPrefix = params.mcpPrefix ?? node.serviceName.replaceAll('-', '_');
        requireThat(mcpPrefix !== 'hive' && mcpPrefix !== 'wiki', 'registry_invalid', 'Hive and Wiki MCP prefixes are reserved.');
        const otherMcpNames = new Set(this.nodes().filter(owner => owner.serviceName !== node.serviceName)
          .flatMap(owner => this.getRegistry(owner.serviceNodeId)?.namespaces.flatMap(namespace => namespace.tools.flatMap(tool => tool.discovery?.mcp?.name ? [tool.discovery.mcp.name] : [])) ?? []));
        for (const ns of params.namespaces) {
          requireThat(
            ns.namespace !== "hive",
            "registry_invalid",
            "The Hive namespace is reserved.",
          );
          const groups = new Set(
            (ns.discoveryGroups ?? []).map((group) => group.group),
          );
          requireThat(
            groups.size === (ns.discoveryGroups ?? []).length &&
              [...groups].every(
                (group) =>
                  group === ns.namespace ||
                  group.startsWith(ns.namespace + "/"),
              ),
            "registry_invalid",
            "Discovery groups must be unique children of their namespace.",
          );
          requireThat(
            new Set(ns.tools.map((tool) => tool.name)).size === ns.tools.length,
            "registry_invalid",
            "Duplicate tool name.",
          );
          requireThat(
            new Set(ns.tools.map((tool) => tool.interfaceVersion)).size <= 1,
            "registry_invalid",
            "A namespace exposes one interface version.",
          );
          const bindings: ToolBinding[] = [];
          for (const tool of ns.tools) {
            if (tool.discovery?.mcp?.name) {
              requireThat(
                tool.discovery.mcp.name.startsWith(mcpPrefix + '_') && !/^(hive|wiki)_/.test(tool.discovery.mcp.name)
                  && (!mcpNames.has(tool.discovery.mcp.name) || mcpNames.get(tool.discovery.mcp.name) === ns.namespace)
                  && !otherMcpNames.has(tool.discovery.mcp.name),
                'registry_invalid',
                'MCP names must be unique and use the service-declared MCP prefix. Hive and Wiki prefixes are reserved.',
              );
              mcpNames.set(tool.discovery.mcp.name, ns.namespace);
            }
            requireThat(
              tool.namespace === ns.namespace,
              "registry_invalid",
              "Tool namespace does not match its container.",
            );
            if (tool.discovery?.group)
              requireThat(
                groups.has(tool.discovery.group),
                "registry_invalid",
                "A Tool discovery group must be declared by its namespace.",
              );
            if (tool.nativeMethod !== undefined)
              requireThat(
                tool.nativeMethod === tool.name,
                "registry_invalid",
                "Native methods must preserve their exact names.",
              );
            // Registry admission checks the schema graph without compiling a provider
            // message validator. Debug message validation is owned by LiveRouting.
            admitSchema(tool.inputSchema);
            admitSchema(tool.outputSchema);
            bindings.push({
              qualifiedName: tool.namespace + "." + tool.name,
              definition: tool,
              definitionHash: toolDefinitionHash(tool),
            });
          }
          nextBindings.set(ns.namespace, bindings);
          requireThat(
            new Set(
              (ns.notifications ?? []).map(
                (notification) =>
                  notification.name + "@" + notification.version,
              ),
            ).size === (ns.notifications ?? []).length,
            "registry_invalid",
            "Duplicate transient notification version.",
          );
          for (const notification of ns.notifications ?? [])
            admitSchema(notification.payloadSchema);
          requireThat(
            new Set(ns.topics.map((topic) => topic.topic + "@" + topic.version))
              .size === ns.topics.length,
            "registry_invalid",
            "Duplicate topic version.",
          );
          for (const topic of ns.topics) {
            requireThat(
              topic.topic.startsWith(ns.namespace + "."),
              "registry_invalid",
              "Topic belongs to another namespace.",
            );
            requireThat(
              new Set(topic.eventKinds.map((event) => event.kind)).size ===
                topic.eventKinds.length,
              "registry_invalid",
              "Duplicate event kind.",
            );
            events.registerTopic(topic, "service:" + node.serviceName);
          }
          requireThat(
            new Set(
              ns.inventoryKinds.map((kind) => kind.kind + "@" + kind.version),
            ).size === ns.inventoryKinds.length,
            "registry_invalid",
            "Duplicate inventory kind version.",
          );
          for (const kind of ns.inventoryKinds) {
            this.store.validators.compile(kind.summarySchema);
            for (const path of kind.searchPointers ?? [])
              requireThat(
                scalarTypes(kind.summarySchema, path).has("string"),
                "registry_invalid",
                "Inventory search paths must select strings.",
              );
            if (kind.archivedPointer)
              requireThat(
                scalarTypes(kind.summarySchema, kind.archivedPointer).has(
                  "boolean",
                ),
                "registry_invalid",
                "Inventory archive paths must select booleans.",
              );
            if (kind.recencyPointer)
              requireThat(
                [...scalarTypes(kind.summarySchema, kind.recencyPointer)].some(
                  (type) => ["string", "number", "integer"].includes(type),
                ),
                "registry_invalid",
                "Inventory recency paths must select strings or numbers.",
              );
            const prior = this.store.get(
              "SELECT definition FROM inventory_schemas WHERE namespace=? AND kind=? AND version=?",
              ns.namespace,
              kind.kind,
              kind.version,
            );
            requireThat(
              !prior || prior["definition"] === canonical(kind),
              "registry_invalid",
              "Inventory schema version is immutable.",
            );
            this.store.run(
              "INSERT OR IGNORE INTO inventory_schemas VALUES (?,?,?,?)",
              ns.namespace,
              kind.kind,
              kind.version,
              canonical(kind),
            );
          }
        }
        const now = new Date().toISOString();
        this.contactTimes.set(node.serviceNodeId, now);
        this.store.run(
          "INSERT INTO registries VALUES (?,?) ON CONFLICT(node_id) DO UPDATE SET registry_json=excluded.registry_json",
          node.serviceNodeId,
          canonical(params),
        );
        this.save({
          ...node,
          synced: true,
          lastSuccessfulSyncAt: now,
          lastContactAt: now,
        });
        this.diagnostic({
          code: "registry_invalid",
          resource: { serviceNodeId: node.serviceNodeId },
          severity: "error",
          source: "hive",
          firstObservedAt: now,
          lastObservedAt: now,
          status: "resolved",
          message: "Registry is synchronized.",
        });
        return { generation: context.generation, syncedAt: now };
      });
      this.bindingCatalog.set(node.serviceNodeId, nextBindings);
      return result;
    } catch (error) {
      const now = new Date().toISOString();
      this.contactTimes.set(node.serviceNodeId, now);
      this.save({
        ...node,
        synced: false,
        ready: false,
        lastFailedSyncAt: now,
      });
      this.diagnostic({
        code: "registry_invalid",
        resource: { serviceNodeId: node.serviceNodeId },
        severity: "error",
        source: "hive",
        firstObservedAt: now,
        lastObservedAt: now,
        status: "current",
        message:
          "Registry candidate rejected; the previous catalog remains cached.",
      });
      if (error instanceof IvyError) throw error;
      return fail(
        "registry_invalid",
        "Registry candidate could not be applied.",
      );
    }
  }
  checkRequirements(requirements: ContractRequirement[], options: { allowUnregistered?: boolean } = {}): void {
    for (const required of requirements) {
      const registered = this.store.get("SELECT 1 FROM contracts WHERE key=? LIMIT 1", required.key);
      if (!registered && options.allowUnregistered) continue;
      requireThat(registered, "contract_not_found", "Required contract is not registered.");
      for (const version of required.writeVersions) this.store.contract(required.key, version);
      const stored = contractVersions(this.store.db, required);
      requireThat(
        stored.every((version) => required.readVersions.includes(version)),
        "contract_version_conflict",
        "Required Object and evidence versions are not all readable by this component.",
      );
    }
  }
  heartbeat(
    context: ServiceContext,
    params: { ready: boolean; diagnostics: Diagnostic[] },
  ): { generation: number; ready: boolean; observedAt: string } {
    const now = new Date().toISOString();
    this.contactTimes.set(context.serviceNodeId, now);
    return this.store.transaction(() => {
      const node = this.node(context.serviceNodeId);
      requireThat(
        !params.ready || node.synced,
        "service_not_ready",
        "Full registry sync is required before readiness.",
      );
      const source = "service:" + context.serviceNodeId,
        reported = new Set<string>(),
        updates: Diagnostic[] = [];
      for (const diagnostic of params.diagnostics) {
        const value = { ...diagnostic, source };
        updates.push(value);
        reported.add(
          hashJson({ code: value.code, resource: value.resource, source }),
        );
      }
      for (const row of this.store.all(
        "SELECT identity,diagnostic_json FROM diagnostics",
      )) {
        const retained = JSON.parse(
          String(row["diagnostic_json"]),
        ) as Diagnostic;
        if (
          retained.source === source &&
          retained.status === "current" &&
          !reported.has(String(row["identity"]))
        )
          updates.push({
            ...retained,
            lastObservedAt: now,
            status: "resolved",
          });
      }
      this.commitDiagnostics(updates);
      if (node.ready !== params.ready)
        this.save({ ...node, ready: params.ready, lastContactAt: now });
      return {
        generation: context.generation,
        ready: params.ready,
        observedAt: now,
      };
    });
  }
  getRegistry(serviceNodeId: string): RegistrySync | null {
    const row = this.store.get(
      "SELECT registry_json FROM registries WHERE node_id=?",
      serviceNodeId,
    );
    return row
      ? (JSON.parse(String(row["registry_json"])) as RegistrySync)
      : null;
  }
  namespace(nodeId: string, namespace: string): NamespaceDefinition {
    const ns = this.getRegistry(nodeId)?.namespaces.find(
      (ns) => ns.namespace === namespace,
    );
    requireThat(
      ns,
      "not_found",
      "Namespace is not in the selected provider catalog.",
    );
    return ns;
  }
  select(namespace: string, target: Target): ServiceNode {
    const explicitId =
      target.serviceNodeId ?? target.resourceRef?.serviceNodeId;
    requireThat(
      !target.serviceNodeId ||
        !target.resourceRef ||
        target.serviceNodeId === target.resourceRef.serviceNodeId,
      "target_conflict",
      "Resource and node selectors disagree.",
    );
    requireThat(
      !target.resourceRef || target.resourceRef.namespace === namespace,
      "target_conflict",
      "Resource and namespace selectors disagree.",
    );
    if (explicitId) {
      const node = this.node(explicitId);
      requireThat(
        (!target.hostId || node.hostId === target.hostId) &&
          (!target.serviceName || node.serviceName === target.serviceName),
        "target_conflict",
        "The selected resource has a different owner.",
      );
      this.namespace(node.serviceNodeId, namespace);
      return node;
    }
    const candidates = this.nodes().filter(
      (node) =>
        (!target.hostId || node.hostId === target.hostId) &&
        (!target.serviceName || node.serviceName === target.serviceName) &&
        this.getRegistry(node.serviceNodeId)?.namespaces.some(
          (ns) => ns.namespace === namespace,
        ),
    );
    const eligible = candidates.filter((node) => this.eligible(node));
    if (eligible.length > 1)
      throw new IvyError(
        "ambiguous_service_node",
        "Choose the owning Service Node.",
        "not_executed",
        eligible.map((node) => ({
          serviceNodeId: node.serviceNodeId,
          hostId: node.hostId,
          serviceName: node.serviceName,
        })),
      );
    requireThat(
      eligible.length === 1,
      "service_unavailable",
      "No eligible provider matches the target.",
    );
    return eligible[0]!;
  }
  snapshot(namespace: string, target: Target) {
    const node = this.select(namespace, target),
      ns = this.namespace(node.serviceNodeId, namespace);
    return {
      namespace,
      description: ns.description,
      guideMarkdown: ns.guideMarkdown,
      provider: { node, eligible: this.eligible(node) },
      toolCount: ns.tools.length,
      topicCount: ns.topics.length,
    };
  }
  /** Hash each admitted definition once per storage-worker lifetime. */
  bindings(serviceNodeId: string, ns: NamespaceDefinition): ToolBinding[] {
    let catalog = this.bindingCatalog.get(serviceNodeId);
    if (!catalog) {
      catalog = new Map(
        (this.getRegistry(serviceNodeId)?.namespaces ?? []).map((namespace) => [
          namespace.namespace,
          namespace.tools.map((definition) => ({
            qualifiedName: definition.namespace + "." + definition.name,
            definition,
            definitionHash: toolDefinitionHash(definition),
          })),
        ]),
      );
      this.bindingCatalog.set(serviceNodeId, catalog);
    }
    return catalog.get(ns.namespace) ?? [];
  }
  liveCatalog(serviceNodeId: string): LiveCatalog | null {
    const registry = this.getRegistry(serviceNodeId);
    if (!registry) return null;
    return {
      namespaces: registry.namespaces.map((namespace) => namespace.namespace),
      tools: registry.namespaces.flatMap((namespace) =>
        this.bindings(serviceNodeId, namespace),
      ),
      notifications: registry.namespaces.flatMap((namespace) =>
        (namespace.notifications ?? []).map((notification) => ({
          namespace: namespace.namespace,
          ...notification,
        })),
      ),
    };
  }
}
