// The compact authoring source for the normative generated JSON schema. No runtime code is emitted.
import { readFileSync, writeFileSync } from "node:fs";
import { reservedUiSlugs } from "../../packages/contracts/src/ui-route.ts";
const wire = "https://ivy.invalid/schemas/hive-wire.schema.json";
const ref = (name) => ({ $ref: `#/$defs/${name}` });
const shared = (name) => ({ $ref: `${wire}#/$defs/${name}` });
const str = (maxLength = 256) => ({ type: "string", minLength: 1, maxLength });
const nullable = (schema) => ({ anyOf: [schema, { type: "null" }] });
const uint = (minimum = 0, maximum = Number.MAX_SAFE_INTEGER) => ({
  type: "integer",
  minimum,
  maximum,
});
const array = (items, maxItems = 200) => ({ type: "array", items, maxItems });
const obj = (properties, required = Object.keys(properties), extra = {}) => ({
  type: "object",
  properties,
  required,
  additionalProperties: false,
  ...extra,
});
const described = (schema, description) => ({ ...schema, description });
const bool = { type: "boolean" };
const id = shared("Identifier"),
  hash = shared("Hash"),
  version = shared("ContractVersion");
const date = {
  type: "string",
  pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$",
};
const hostConfiguration = {
  $ref: "https://ivy.invalid/schemas/host.schema.json#/$defs/HostConfig",
};
const releaseManifest = {
  $ref: "https://ivy.invalid/schemas/host.schema.json#/$defs/ReleaseManifest",
};
const page = {
  limit: described(uint(1, 200), "Maximum items to return on this page."),
  cursor: described(
    str(8192),
    "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged.",
  ),
};
const target = {
  serviceNodeId: described(
    id,
    "Exact registered provider node to select when a service has multiple providers.",
  ),
  hostId: described(
    id,
    "Provider host selector; it must agree with any service node or resource selector.",
  ),
  serviceName: described(str(64), "Registered service selector."),
  resourceRef: described(
    shared("ResourceRef"),
    "Resource owner selector obtained from an earlier Hive result.",
  ),
};
const location = {
  objectId: described(
    id,
    "Stable Hive Object ID returned by Object operations.",
  ),
  path: described(
    str(16384),
    "Absolute Hive Object path; supply either path or objectId, not both.",
  ),
};
const mutationId = described(
  id,
  "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action.",
);
const operationId = described(
  id,
  "Stable caller-generated identity for routed work; retain it to reconcile an unknown outcome.",
);
const byLocation = (extra) =>
  obj({ ...location, ...extra }, [], {
    oneOf: [
      { required: ["objectId"], not: { required: ["path"] } },
      { required: ["path"], not: { required: ["objectId"] } },
    ],
  });
const paged = (item) =>
  obj({ items: array(item), nextCursor: nullable(str(8192)) });
const defs = {
  ObjectMetadata: obj({
    id,
    parentId: nullable(id),
    ownerObjectId: nullable(id),
    name: str(255),
    path: str(16384),
    position: uint(),
    icon: nullable(str(32)),
    contractKey: str(192),
    currentRevision: uint(1),
    contractVersion: version,
    archivedAt: nullable(date),
    effectivelyArchived: bool,
    createdAt: date,
    updatedAt: date,
  }),
  RevisionMetadata: obj({
    objectId: id,
    revision: uint(1),
    contractVersion: version,
    contentHash: hash,
    mediaType: str(128),
    byteLength: uint(0, 8388608),
    createdAt: date,
    references: shared("RevisionReferences"),
  }),
  ObjectRead: obj({
    object: ref("ObjectMetadata"),
    revision: ref("RevisionMetadata"),
    content: shared("Content"),
  }),
  ObjectWriteResult: obj({
    object: ref("ObjectMetadata"),
    revision: ref("RevisionMetadata"),
  }),
  QueryItem: obj({
    objectId: id,
    revision: uint(1),
    contractVersion: version,
    values: {
      type: "object",
      maxProperties: 40,
      additionalProperties: { type: ["null", "boolean", "number", "string"] },
    },
    document: ref("ObjectRead"),
  }, ["objectId", "revision", "contractVersion", "values"]),
  ServiceNode: obj({
    serviceNodeId: id,
    hostId: id,
    serviceName: str(64),
    instanceMode: { enum: ["singleton", "multiple"] },
    principalId: id,
    version: str(128),
    buildId: hash,
    hiveProtocol: { const: 1 },
    nativeVersion: nullable(str(256)),
    connected: bool,
    synced: bool,
    ready: bool,
    stale: bool,
    desiredEnabled: bool,
    lastContactAt: nullable(date),
    lastSuccessfulSyncAt: nullable(date),
    lastFailedSyncAt: nullable(date),
    lastObservationAt: nullable(date),
    diagnostic: nullable(
      obj({
        code: str(128),
        severity: { enum: ["info", "warning", "error"] },
        message: str(2048),
        observedAt: date,
      }),
    ),
  }),
  Host: obj({ hostId: id, serviceNodeIds: array(id), connected: bool }),
  Provider: obj({ node: ref("ServiceNode"), eligible: bool }),
  NodeContracts: obj({
    provider: ref("Provider"),
    hasCatalog: bool,
    capturedAt: nullable(date),
    items: array(shared("ContractRequirement")),
    nextCursor: nullable(str(8192)),
  }),
  ContractSummary: obj({
    key: str(192),
    version,
    description: str(512),
    owner: { oneOf: [obj({ kind: { const: "hive" } }), obj({ kind: { const: "agent" } }), obj({ kind: { const: "service" }, serviceName: str(64) })] },
    mediaType: str(128),
    hasJsonSchema: bool,
  }),
  ToolBinding: obj({
    qualifiedName: str(256),
    definition: shared("ToolDefinition"),
    definitionHash: described(
      hash,
      "Hash of the exact returned tool definition; pass it unchanged when invoking that definition.",
    ),
  }),
  NamespaceSnapshot: obj({
    namespace: str(64),
    description: str(8192),
    guideMarkdown: { type: "string", maxLength: 65536 },
    provider: ref("Provider"),
    toolCount: uint(0, 2000),
    topicCount: uint(0, 128),
  }),
  NamespaceSummary: obj({
    namespace: str(64),
    providers: array(ref("Provider")),
  }),
  DiscoveryEntry: obj(
    {
      kind: { enum: ["service", "group", "tool", "provider"] },
      mcpName: described(str(64), "Direct MCP tool when available; call it with its published schema."),
      name: str(256),
      description: str(320),
      serviceName: str(64),
      namespace: str(64),
      group: str(256),
      toolCount: uint(),
      providerCount: uint(),
      availableCount: uint(),
      serviceNodeId: id,
      hostId: id,
      available: bool,
      effect: { enum: ["read", "write", "unknown"] },
    },
    ["kind", "name", "description", "serviceName"],
  ),
  DiscoveryProvider: obj({ serviceNodeId: id, hostId: id, available: bool }),
  InventoryItem: obj({
    resourceRef: shared("ResourceRef"),
    schemaVersion: version,
    summary: shared("Json"),
    observedAt: date,
    stale: bool,
    snapshotRevision: uint(1),
  }),
  Event: obj({
    sequence: uint(1),
    topic: str(192),
    topicVersion: version,
    source: str(256),
    occurredAt: date,
    mutationId: id,
    payload: shared("Json"),
  }),
  EventPublication: obj({
    sequence: uint(1),
    topic: str(192),
    publishedAt: date,
  }),
  EventGap: obj({ prunedThroughSequence: uint(), resumeAfterSequence: uint() }),
  EventBatch: obj({
    items: array(ref("Event"), 100),
    throughSequence: uint(),
    hasMore: bool,
    gap: nullable(ref("EventGap")),
  }),
  FamilyRetentionStatus: obj({
    contractKey: str(192),
    policy: shared("RetentionPolicy"),
    policyHash: hash,
    previewRequired: bool,
    objectCount: uint(),
    revisionCount: uint(),
    byteLength: uint(),
    eligibleObjectCount: uint(),
    eligibleRevisionCount: uint(),
    protectedRevisionCount: uint(),
  }),
  RetentionStatus: obj({
    checkedAt: date,
    nextPassAt: date,
    lastSuccessAt: nullable(date),
    lastFailure: nullable(obj({ at: date, message: str(512) })),
    families: array(ref("FamilyRetentionStatus"), 1024),
    events: obj({
      count: uint(),
      byteLength: uint(),
      prunedThroughSequence: uint(),
    }),
    mutations: obj({
      count: uint(),
      byteLength: uint(),
      expiredBefore: uint(),
    }),
  }),
  UiMetadata: obj({
    uiId: str(128),
    slug: described({ ...str(64), pattern: "^[a-z][a-z0-9-]{0,63}$", not: { enum: reservedUiSlugs } }, "Unique top-level UI path segment; Hive endpoints are reserved. Omit to use /ui/{uiId}/."),
    priority: described({ type: "integer", minimum: -1000000, maximum: 1000000 }, "Navigation priority, highest first; defaults to 0. Equal priorities sort by uiId."),
    displayName: str(256),
    description: str(2048),
    iconKey: {
      enum: [
        "ui",
        "bot",
        "book-open",
        "notebook-text",
        "list-checks",
        "message-circle",
        "phone",
        "camera",
        "newspaper",
        "settings",
        "layout-dashboard",
        "chart-no-axes-combined",
        "presentation",
        "database",
        "funnel",
        "user-round",
        "clipboard-clock",
      ],
    },
  }, ["uiId", "displayName", "description", "iconKey"]),
  UiAsset: obj({
    path: str(1024),
    mediaType: str(128),
    contentHash: hash,
    byteLength: uint(0, 8 * 1024 * 1024),
  }),
  ServiceRequirement: obj({
    serviceName: str(64),
    namespace: str(64),
    interfaceVersion: version,
  }),
  NotificationFilter: obj(
    { namespace: str(64), name: str(192), version, serviceNodeId: id },
    ["namespace", "name", "version"],
  ),
  UiRequirements: obj({
    hiveProtocol: { const: 1 },
    contracts: array(shared("ContractRequirement"), 128),
    services: array(ref("ServiceRequirement"), 128),
  }),
  UiRelease: obj({
    releaseId: str(128),
    entryPath: str(1024),
    requirements: ref("UiRequirements"),
    assets: array(ref("UiAsset"), 2000),
  }),
  UI: obj({
    metadata: ref("UiMetadata"),
    currentReleaseId: nullable(str(128)),
    releases: array(ref("UiRelease"), 2000),
  }),
  UiReleaseSummary: obj({
    releaseId: str(128),
    entryPath: str(1024),
    assetCount: uint(0, 2000),
  }),
  UiIssue: obj({
    code: str(128),
    message: str(2048),
    resource: obj(
      {
        serviceNodeId: id,
        serviceName: str(64),
        namespace: str(64),
        qualifiedName: str(256),
        contractKey: str(192),
        assetPath: str(1024),
      },
      [],
    ),
  }),
  UiInspection: obj({
    uiId: str(128),
    currentReleaseId: nullable(str(128)),
    requestedReleaseId: nullable(str(128)),
    release: nullable(ref("UiReleaseSummary")),
    requirements: nullable(ref("UiRequirements")),
    checkedAt: date,
    status: {
      enum: [
        "ready",
        "unavailable",
        "incompatible",
        "invalid",
        "missing",
        "unselected",
      ],
    },
    issues: array(ref("UiIssue"), 32),
    issuesTruncated: bool,
  }),
  UiSummary: obj({
    metadata: ref("UiMetadata"),
    currentReleaseId: nullable(str(128)),
    releaseCount: uint(),
  }),
  UiDefinition: obj({
    metadata: ref("UiMetadata"),
    entryPath: str(1024),
    requirements: ref("UiRequirements"),
    dataContracts: array(shared("DataContract"), 128),
  }),
  UiPointerResult: obj({
    uiId: str(128),
    releaseId: str(128),
    previousReleaseId: nullable(str(128)),
  }),
  DeploymentSnapshot: obj({
    record: shared("DeploymentRecord"),
    serviceNodeId: id,
    reportedAt: date,
    stale: bool,
  }),
  HostConfigurationSummary: obj({
    hostId: id,
    objectId: id,
    revision: uint(1),
    contentHash: hash,
    updatedAt: date,
    byteLength: uint(1, 1048576),
  }),
  HostConfiguration: obj({
    hostId: id,
    objectId: id,
    revision: uint(1),
    contentHash: hash,
    updatedAt: date,
    byteLength: uint(1, 1048576),
    configuration: hostConfiguration,
  }),
  HostConfigurationEditor: obj({
    hostId: id,
    objectId: id,
    revision: uint(1),
    contentHash: hash,
    updatedAt: date,
    byteLength: uint(1, 1048576),
    configuration: shared("Json"),
  }),
  PackageUploadAuthorization: obj({
    componentId: described(
      { type: "string", pattern: "^[a-z][a-z0-9-]*$", maxLength: 128 },
      "Component ID from the prepared artifact manifest.",
    ),
    version: described(
      version,
      "Semantic package version from the prepared artifact manifest; published versions are immutable and must increase.",
    ),
    buildId: described(
      hash,
      "SHA-256 build identity from the prepared artifact manifest and build stamp.",
    ),
    archiveHash: described(
      hash,
      "SHA-256 hash of the exact gzip tar archive to upload.",
    ),
    bytes: described(
      uint(1, 268435456),
      "Exact archive size in bytes, from 1 through 268435456.",
    ),
    manifest: described(
      shared("Json"),
      "Exact validated release manifest of the prepared artifact; its componentId, version, and buildId must match these fields.",
    ),
  }),
  PackageCatalogEntry: obj({
    componentId: {
      type: "string",
      pattern: "^[a-z][a-z0-9-]*$",
      maxLength: 128,
    },
    version: str(128),
    buildId: hash,
    archiveHash: hash,
    bytes: uint(1, 268435456),
    manifest: releaseManifest,
    revision: uint(1),
    publishedAt: date,
    publisherPrincipalId: id,
  }),
  PackageCatalog: obj({
    schemaVersion: { const: 1 },
    revision: uint(),
    packages: array(ref("PackageCatalogEntry"), 4096),
  }),
  PackageCatalogSnapshot: obj({
    objectId: id,
    objectRevision: uint(1),
    catalog: ref("PackageCatalog"),
  }),
  PackageUploadResult: {
    oneOf: [
      obj({
        alreadyPublished: { const: true },
        entry: ref("PackageCatalogEntry"),
      }),
      obj({
        alreadyPublished: { const: false },
        uploadId: id,
        uploadToken: str(512),
        uploadPath: str(2048),
        expiresAt: date,
      }),
    ],
  },
  Status: obj({
    callerPrincipalId: id,
    version: str(128),
    buildId: hash,
    hiveProtocol: { const: 1 },
    runtimeEpoch: described(id, "Current Hive epoch for mutation IDs and services that explicitly require epoch-formatted operation IDs, including AgentManager and Secretary. Read again after a Hive restart; other services may use different ID formats."),
    ready: bool,
    serverTime: date,
    storageFormat: uint(1),
    publicBaseUrl: str(2048),
  }),
};
const operations = {};
function op(
  method,
  input,
  output,
  access = "client",
  mutation = false,
  discoverable = true,
) {
  const stem = method
    .split(".")
    .map((s) => s[0].toUpperCase() + s.slice(1))
    .join("");
  defs[`${stem}Params`] = input;
  defs[`${stem}Result`] = output;
  operations[method] = {
    input: `#/$defs/${stem}Params`,
    output: `#/$defs/${stem}Result`,
    access,
    mutation,
    ...(discoverable ? {} : { discoverable: false }),
  };
}
op("system.status", obj({}), ref("Status"));
op(
  "system.instructions",
  obj({
    view: { enum: ["instructions", "examples"], description: "instructions (default) reads the exact current handshake text; examples reads tested public tool workflows." },
    cursor: described(str(80), "Opaque continuation from the previous page. Omit to start at the beginning of the selected view."),
  }, []),
  obj({
    view: { enum: ["instructions", "examples"] },
    text: { type: "string", maxLength: 32768 },
    sha256: hash,
    totalBytes: uint(1, 32768),
    startByte: uint(0, 32768),
    endByte: uint(0, 32768),
    complete: described(bool, "True only when this response contains the entire selected text."),
    nextCursor: nullable(str(80)),
  }),
);
op(
  "system.toolSchema",
  obj({ name: described(str(128), "Exact public MCP tool name from tools/list, such as wiki_read.") }),
  obj({
    name: str(128),
    description: { type: "string", maxLength: 65536 },
    inputSchema: shared("Json"),
    outputSchema: nullable(shared("Json")),
    schemaHash: hash,
    complete: { const: true },
  }),
);
op(
  "system.inspectStorage",
  obj({
    contracts: array(
      obj({ key: str(192), readScope: shared("ContractReadScope") }, ["key"]),
      128,
    ),
  }),
  {
    $ref: "https://ivy.invalid/schemas/host.schema.json#/$defs/StorageInspection",
  },
);
op(
  "system.diagnostics",
  obj(
    {
      ...page,
      status: described(
        { enum: ["current", "stale", "unknown", "resolved", "unresolved"] },
        "Observation state to list; unresolved selects every state except resolved.",
      ),
    },
    [],
  ),
  paged(shared("Diagnostic")),
);
op(
  "contracts.list",
  obj({ ...page, key: str(192), allVersions: bool }, []),
  paged(shared("DataContract")),
  "client",
  false,
  false,
);
op(
  "contracts.summaries",
  obj({ ...page, key: str(192), allVersions: bool }, []),
  paged(ref("ContractSummary")),
);
op(
  "contracts.get",
  obj({ key: str(192), version }, ["key"]),
  shared("DataContract"),
);
op(
  "contracts.register",
  obj({ definition: shared("DataContract"), mutationId }),
  shared("DataContract"),
  "client",
  true,
);
op(
  "contracts.validate",
  obj({ key: str(192), contractVersion: version, content: shared("Content") }),
  obj({
    valid: { const: true },
    contentHash: hash,
    byteLength: uint(0, 8388608),
  }),
);
op("objects.stat", byLocation({}), ref("ObjectMetadata"));
op("objects.read", byLocation({ revision: uint(1) }), ref("ObjectRead"));
op(
  "objects.write",
  shared("ObjectWrite"),
  ref("ObjectWriteResult"),
  "client",
  true,
);
op(
  "objects.writeReceipt",
  obj({ mutationId: id, expectedRequestHash: hash }),
  nullable(ref("ObjectWriteResult")),
  "service",
  false,
  false,
);
op(
  "objects.history",
  obj({ objectId: id, ...page }, ["objectId"]),
  obj({
    items: array(ref("RevisionMetadata")),
    nextCursor: nullable(str(8192)),
    historyComplete: bool,
  }),
);
op(
  "objects.pruneRevisions",
  obj({ objectId: id, expectedRevision: uint(1), mutationId,
    maximumCount: uint(1, 1000000), maximumAgeDays: { type: "number", exclusiveMinimum: 0 }, maximumBytes: uint(1) }),
  obj({ deleted: uint(), protected: uint(), remainingBytes: uint() }),
  "service", true, false,
);
op(
  "objects.list",
  obj({ parentId: nullable(id), includeArchived: bool, ...page }, ["parentId"]),
  paged(ref("ObjectMetadata")),
);
op(
  "objects.tree",
  obj(
    {
      rootId: nullable(id),
      depth: uint(1, 8),
      limit: uint(1, 500),
      includeArchived: bool,
    },
    ["rootId"],
  ),
  obj({ items: array(ref("ObjectMetadata"), 500), truncated: bool }),
);
op("objects.query", shared("ObjectQuery"), paged(ref("QueryItem")));
op(
  "objects.search",
  obj(
    {
      text: str(1024),
      contractKey: str(192),
      rootId: id,
      includeArchived: bool,
      ...page,
    },
    ["text"],
  ),
  paged(ref("ObjectMetadata")),
);
op("retention.preview", obj({}), ref("RetentionStatus"));
op("retention.status", obj({}), ref("RetentionStatus"));
op(
  "objects.move",
  obj(
    {
      objectId: id,
      parentId: nullable(id),
      name: str(255),
      icon: nullable(str(32)),
      mutationId,
    },
    ["objectId", "parentId", "name", "mutationId"],
  ),
  ref("ObjectMetadata"),
  "client",
  true,
);
op(
  "objects.reorder",
  obj({ objectId: id, beforeObjectId: nullable(id), mutationId }),
  ref("ObjectMetadata"),
  "client",
  true,
);
op(
  "objects.archive",
  obj({ objectId: id, archived: bool, mutationId, expectedRevision: uint(1) }, ["objectId", "archived", "mutationId"]),
  ref("ObjectMetadata"),
  "client",
  true,
);
op(
  "objects.delete",
  obj({ objectId: id, expectedRevision: uint(1), mutationId }),
  obj({ deleted: bool }),
  "client",
  true,
  false,
);
op(
  "service.connect",
  shared("ServiceConnect"),
  obj({ serviceNodeId: id, generation: uint(1) }),
  "service",
);
op(
  "registry.sync",
  shared("RegistrySync"),
  obj({ generation: uint(1), syncedAt: date }),
  "service",
);
op(
  "service.heartbeat",
  shared("ServiceHeartbeat"),
  obj({ generation: uint(1), ready: bool, observedAt: date }),
  "service",
);
op("hosts.list", obj(page, []), paged(ref("Host")));
op(
  "hosts.report",
  {
    $ref: "https://ivy.invalid/schemas/host.schema.json#/$defs/ManagementSnapshot",
  },
  obj({ sequence: uint(1), reportedAt: date }),
  "service",
);
op(
  "hosts.observations",
  obj({ ...page, hostId: id }, []),
  paged({
    $ref: "https://ivy.invalid/schemas/host.schema.json#/$defs/ManagementObservation",
  }),
);
op(
  "hostConfigurations.list",
  obj({ ...page }, []),
  paged(ref("HostConfigurationSummary")),
);
op(
  "hostConfigurations.get",
  obj({ hostId: id, revision: uint(1) }, ["hostId"]),
  ref("HostConfiguration"),
);
op(
  "hostConfigurations.put",
  obj(
    {
      hostId: id,
      expectedRevision: uint(1),
      configuration: hostConfiguration,
      mutationId,
    },
    ["hostId", "configuration", "mutationId"],
  ),
  ref("HostConfiguration"),
  "client",
  true,
);
op(
  "hostConfigurations.edit",
  obj({ hostId: id, revision: uint(1) }, ["hostId"]),
  ref("HostConfigurationEditor"),
);
op(
  "hostConfigurations.save",
  obj(
    {
      hostId: id,
      expectedRevision: uint(1),
      configuration: shared("Json"),
      mutationId,
    },
    ["hostId", "configuration", "mutationId"],
  ),
  ref("HostConfigurationEditor"),
  "client",
  true,
);
op(
  "hostConfigurations.history",
  obj({ hostId: id, ...page }, ["hostId"]),
  obj({
    items: array(ref("RevisionMetadata")),
    nextCursor: nullable(str(8192)),
    historyComplete: bool,
  }),
);
op(
  "serviceNodes.list",
  obj({ ...page, hostId: id, serviceName: str(64) }, []),
  paged(ref("ServiceNode")),
);
op("serviceNodes.get", obj({ serviceNodeId: id }), ref("ServiceNode"));
op(
  "serviceNodes.contracts",
  obj({ serviceNodeId: id, key: str(192), ...page }, ["serviceNodeId"]),
  ref("NodeContracts"),
);
op("namespaces.list", obj(page, []), paged(ref("NamespaceSummary")));
op(
  "namespaces.get",
  obj({ namespace: str(64), ...target }, ["namespace"]),
  ref("NamespaceSnapshot"),
);
op(
  "tools.list",
  obj({ namespace: str(64), ...target, namePrefix: str(192), ...page }, [
    "namespace",
  ]),
  obj({
    provider: ref("Provider"),
    guideMarkdown: { type: "string", maxLength: 65536 },
    items: array(ref("ToolBinding")),
    nextCursor: nullable(str(8192)),
  }),
);
op(
  "topics.list",
  obj({ namespace: str(64), ...target, topicPrefix: str(192), ...page }, []),
  paged(obj({
    provider: nullable(ref("Provider")),
    namespace: str(64),
    definition: shared("TopicDefinition"),
  })),
);
op("tools.call", shared("ToolCall"), shared("Json"));
op(
  "discovery.list",
  obj(
    {
      serviceName: described(
        str(64),
        "Registered service to browse; a service-specific MCP tool supplies this selector.",
      ),
      group: described(
        str(256),
        "Discovery group returned by an earlier browse result.",
      ),
      query: described(str(512), "Capability keywords; all terms must match tool names, descriptions or keywords. Searches tools, not stored content."),
      serviceNodeId: described(
        id,
        "Exact provider node selected from a provider discovery result.",
      ),
      providers: described(
        bool,
        "With serviceName, list hosts that provide it. Omit query, group and serviceNodeId.",
      ),
      limit: described(
        uint(1, 20),
        "Maximum entries to return, from 1 through 20.",
      ),
      cursor: described(
        str(8192),
        "Opaque cursor from the preceding discovery page; omit for the first page and keep selectors unchanged.",
      ),
    },
    [],
  ),
  paged(ref("DiscoveryEntry")),
);
op(
  "discovery.instructions",
  obj({}),
  obj({ instructions: { type: "string", minLength: 1, maxLength: 32768 } }),
  "client",
  false,
  false,
);
op(
  "discovery.describe",
  obj(
    {
      serviceName: described(str(64), "Registered service from discovery."),
      tools: described(
        { ...array(str(256), 5), minItems: 1, uniqueItems: true },
        "Exact qualified tool names returned by discovery, at most five.",
      ),
      serviceNodeId: described(
        id,
        "Exact provider node required when the service is ambiguous.",
      ),
    },
    ["serviceName", "tools"],
  ),
  obj({
    serviceName: str(64),
    provider: nullable(ref("DiscoveryProvider")),
    guides: array(
      obj({
        namespace: str(64),
        guideMarkdown: { type: "string", maxLength: 65536 },
      }),
      5,
    ),
    items: array(ref("ToolBinding"), 5),
  }),
);
op(
  "discovery.call",
  obj(
    {
      serviceName: described(
        str(64),
        "Registered service from the matching describe result.",
      ),
      qualifiedName: described(
        str(256),
        "Exact qualified tool name from the describe result.",
      ),
      expectedDefinitionHash: described(
        hash,
        "Definition hash from the same describe result; invocation fails if the definition changed.",
      ),
      serviceNodeId: described(
        id,
        "Exact provider node from the describe result when present.",
      ),
      arguments: described(
        shared("Json"),
        "Arguments validated against the selected input schema returned by describe.",
      ),
      operationId,
      expectedCallerPrincipalId: described(
        id,
        "Optional caller identity fence obtained from authenticated status when required.",
      ),
    },
    ["serviceName", "qualifiedName", "expectedDefinitionHash", "arguments"],
  ),
  shared("Json"),
);
op(
  "inventory.list",
  obj(
    {
      ...page,
      ...target,
      namespace: str(64),
      kind: str(64),
      sort: { enum: ["native-id", "recency-desc"] },
      archived: bool,
      searchTerm: str(512),
    },
    [],
  ),
  paged(ref("InventoryItem")),
);
op(
  "inventory.get",
  obj({ resourceRef: shared("ResourceRef") }),
  ref("InventoryItem"),
);
op(
  "inventory.sync",
  shared("InventorySync"),
  obj({ snapshotRevision: uint(1), observedAt: date }),
  "service",
);
op(
  "events.publish",
  obj({
    topic: str(192),
    topicVersion: version,
    payload: shared("Json"),
    mutationId,
  }),
  ref("EventPublication"),
  "service",
  true,
);
op(
  "events.read",
  obj(
    {
      afterSequence: uint(),
      filter: shared("EventFilter"),
      limit: uint(1, 100),
    },
    ["afterSequence", "filter"],
  ),
  ref("EventBatch"),
);
op("events.head", obj({}), obj({ throughSequence: uint() }), "service");
op("events.subscribe", shared("EventSubscribe"), ref("EventBatch"), "service");
op(
  "events.ack",
  shared("EventAck"),
  obj({ acknowledgedSequence: uint() }),
  "service",
);
op(
  "events.unsubscribe",
  obj({ name: id, mutationId }),
  obj({ removed: bool }),
  "service",
  true,
);
op(
  "notifications.subscribe",
  obj({
    filters: array(ref("NotificationFilter"), 64),
    changes: described({ ...array({ ...str(512), pattern: "^(objects(?:/[a-zA-Z0-9._/-]+)?|services|inventory|uis|system)$" }, 64), uniqueItems: true },
      "Transient UI invalidations: objects or objects/<contract prefix>, services, inventory, uis and system. Reconnect rereads current state."),
  }, ["filters"]),
  obj({ subscribed: uint(0, 64), changes: uint(0, 64) }, ["subscribed"]),
  "client",
  false,
  false,
);
op(
  "packages.catalog",
  obj({ after: uint(), includeUis: bool }),
  ref("PackageCatalogSnapshot"),
);
op(
  "packages.authorizeUpload",
  ref("PackageUploadAuthorization"),
  ref("PackageUploadResult"),
  "client",
  true,
);
op("uis.list", obj(page, []), paged(ref("UI")));
op("uis.get", obj({ uiId: str(128) }), ref("UI"));
op("uis.catalog", obj(page, []), paged(ref("UiSummary")));
op(
  "uis.releases",
  obj({ uiId: str(128), ...page }, ["uiId"]),
  paged(ref("UiReleaseSummary")),
);
op(
  "uis.inspect",
  obj({ uiId: str(128), releaseId: str(128) }, ["uiId"]),
  ref("UiInspection"),
);
op(
  "uis.stageAsset",
  obj({
    uiId: str(128),
    releaseId: str(128),
    asset: ref("UiAsset"),
    base64: { type: "string", maxLength: 11_184_812 },
    mutationId,
  }),
  obj({ staged: bool }),
  "client",
  true,
  false,
);
op(
  "uis.deploy",
  obj({
    metadata: ref("UiMetadata"),
    release: ref("UiRelease"),
    expectedReleaseId: nullable(str(128)),
    mutationId,
  }),
  ref("UiPointerResult"),
  "client",
  true,
);
op(
  "uis.rollback",
  obj({
    uiId: str(128),
    releaseId: str(128),
    expectedReleaseId: str(128),
    mutationId,
  }),
  ref("UiPointerResult"),
  "client",
  true,
);
op(
  "deployments.list",
  obj({ ...page, hostId: id, instanceId: id }, []),
  paged(ref("DeploymentSnapshot")),
);
op(
  "deployments.get",
  obj({ deploymentId: id, hostId: id }),
  ref("DeploymentSnapshot"),
);
op(
  "deployments.report",
  obj({ records: array(shared("DeploymentRecord"), 200) }),
  obj({ reportedAt: date }),
  "service",
);
const wikiSearch = structuredClone(defs.ObjectsSearchParams);
delete wikiSearch.properties.contractKey;
const markdown = { type: 'string', maxLength: 1048576 };
op('wiki.search', wikiSearch, ref('ObjectsSearchResult'));
op('wiki.list', obj({ parentId: nullable(id), ...page, includeArchived: bool }, []), ref('ObjectsQueryResult'));
op('wiki.read', byLocation({ revision: uint(1) }), ref('ObjectRead'));
op('wiki.create', obj({ mutationId, title: str(255), parentId: nullable(id), markdown }, ['mutationId', 'title', 'markdown']), ref('ObjectWriteResult'), 'client', true);
op('wiki.update', obj({ mutationId, objectId: id, expectedRevision: uint(1), markdown }), ref('ObjectWriteResult'), 'client', true);
op('wiki.history', structuredClone(defs.ObjectsHistoryParams), ref('ObjectsHistoryResult'));
op('wiki.move', structuredClone(defs.ObjectsMoveParams), ref('ObjectsMoveResult'), 'client', true);
op('wiki.archive', structuredClone(defs.ObjectsArchiveParams), ref('ObjectsArchiveResult'), 'client', true);

const schema = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://ivy.invalid/schemas/hive-operations.schema.json",
  title: "Ivy canonical operation parameters and results",
  $defs: defs,
};
for (const [path, value] of [
  ["specs/schemas/hive-operations.schema.json", schema],
  ["specs/schemas/operations.json", { schemaVersion: 1, operations }],
]) {
  const content = JSON.stringify(value, null, 2) + "\n";
  if (process.argv.includes("--check")) {
    if (readFileSync(path, "utf8") !== content)
      throw new Error(`Stale generated contract: ${path}`);
  } else writeFileSync(path, content);
}
