import { readFileSync, writeFileSync } from "node:fs";
import { nativeVersions } from "./checked-native-versions.mjs";
const ref = (name) => ({ $ref: "#/$defs/" + name });
const wire = (name) => ({
  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/" + name,
});
const text = (maxLength = 1024) => ({ type: "string", maxLength });
const id = { ...text(256), minLength: 1 },
  method = { ...text(192), minLength: 1 };
const integer = (minimum = 0, maximum = Number.MAX_SAFE_INTEGER) => ({
  type: "integer",
  minimum,
  maximum,
});
const bool = { type: "boolean" },
  time = { ...text(64), minLength: 1 },
  hash = wire("Hash");
const object = (properties, required = Object.keys(properties)) => ({
  type: "object",
  properties,
  required,
  additionalProperties: false,
});
const array = (items, maxItems = 256) => ({ type: "array", items, maxItems });
const nullable = (value) => ({ anyOf: [value, { type: "null" }] });
const schema = {
  anyOf: [bool, { type: "object", additionalProperties: wire("Json") }],
};
const nativeMethod = object({
  method,
  paramsRequired: bool,
  inputSchema: schema,
  outputSchema: schema,
  responseType: id,
  inputSource: { enum: ["native-json", "native-typescript-and-rust"] },
  outputSource: { enum: ["native-json", "native-typescript-and-rust"] },
});
const notificationMethod = object({
  method,
  paramsRequired: bool,
  inputSchema: schema,
  paramsAbsent: bool,
  inputSource: { enum: ["native-json", "native-typescript-and-json-types"] },
});
const definitions = {
  Catalog: object({
    schemaVersion: { const: 1 },
    provider: { const: "codex" },
    version: id,
    experimental: { const: true },
    sourceHash: hash,
    nativeExecutableHash: hash,
    clientRequests: array(nativeMethod),
    serverRequests: array(nativeMethod, 64),
    serverNotifications: array(notificationMethod),
    clientNotifications: array(notificationMethod, 64),
    derivedLegacyTypes: array(id, 64),
  }),
  ParametersRef: object({
    schemaVersion: { const: 1 },
    nativeVersion: id,
    nativeExecutableHash: hash,
    catalogHash: hash,
    method,
    reservedFields: array(id, 64),
    params: wire("Json"),
  }),
  PlanDefinitions: object({
    threadStart: hash,
    threadResume: hash,
    turnStart: hash,
    turnInterrupt: hash,
  }),
  PlanLocation: object({
    serviceNodeId: id,
    hostId: id,
    kind: { enum: ["existing", "normal", "internal"] },
    cwd: { ...text(2048), minLength: 1 },
    projectId: id,
  }),
  Plan: object(
    {
      nativeVersion: id,
      catalogSourceHash: hash,
      definitions: ref("PlanDefinitions"),
      location: ref("PlanLocation"),
      threadStart: ref("ParametersRef"),
      threadResume: ref("ParametersRef"),
      turnStart: ref("ParametersRef"),
    },
    [
      "nativeVersion",
      "catalogSourceHash",
      "definitions",
      "threadStart",
      "threadResume",
      "turnStart",
    ],
  ),
  // Transport-only draft. Admission MUST validate all templates with the selected NativeContract
  // before any workflow write; durable plans contain their bounded validated parameter envelopes directly.
  PlanDraft: object(
    {
      nativeVersion: id,
      catalogSourceHash: hash,
      definitions: ref("PlanDefinitions"),
      location: ref("PlanLocation"),
      threadStart: { type: "object", additionalProperties: wire("Json") },
      threadResume: { type: "object", additionalProperties: wire("Json") },
      turnStart: { type: "object", additionalProperties: wire("Json") },
    },
    [
      "nativeVersion",
      "catalogSourceHash",
      "definitions",
      "threadStart",
      "threadResume",
      "turnStart",
    ],
  ),
  InvocationDraft: object({
    nativeVersion: id,
    catalogSourceHash: hash,
    method: {
      enum: [
        "thread/start",
        "thread/resume",
        "turn/start",
        "turn/interrupt",
        "turn/steer",
      ],
    },
    params: { type: "object", additionalProperties: wire("Json") },
  }),
  WindowsShell: object({
    executable: { ...text(2048), minLength: 1 },
    executableHash: hash,
  }),
  WindowsShellStatus: object({
    executable: { ...text(2048), minLength: 1 },
    executableHash: hash,
    version: id,
    packageIdentity: { const: "unpackaged" },
  }),
  InstructionsDocument: object({
    schemaVersion: { const: 1 },
    hostId: nullable(id),
    enabled: nullable(bool),
    includeLocal: bool,
    text: text(16384),
  }),
  InstructionsStatus: object({
    state: { enum: ["disabled", "pending", "applied", "conflict"] },
    home: nullable(text(2048)),
    desiredVersion: nullable(hash),
    appliedVersion: nullable(hash),
    observedAt: time,
    code: nullable(id),
    bytes: integer(0, 32768),
  }),
  ManagedOutputStatus: object({
    state: { enum: ["disabled", "pending", "applied", "conflict"] },
    target: nullable(text(2048)),
    desiredVersion: nullable(hash),
    appliedVersion: nullable(hash),
    observedAt: time,
    code: nullable(id),
    bytes: integer(0, 4 * 1024 * 1024),
  }),
  McpServer: object({
    name: { ...text(64), minLength: 1, pattern: "^[a-z][a-z0-9_-]{0,63}$" },
    url: { ...text(2048), minLength: 1 },
    enabled: bool,
    authentication: { enum: ["none", "agent-manager"] },
    startupTimeoutSeconds: integer(1, 300),
    toolTimeoutSeconds: integer(1, 3600),
  }),
  McpDocument: object({
    schemaVersion: { const: 1 },
    hostId: nullable(id),
    enabled: nullable(bool),
    servers: array(ref("McpServer"), 32),
  }),
  SkillFile: object({
    path: { ...text(512), minLength: 1 },
    content: text(262144),
  }),
  SkillsDocument: object({
    schemaVersion: { const: 1 },
    hostId: nullable(id),
    enabled: nullable(bool),
    files: array(ref("SkillFile"), 256),
  }),
  EnvironmentDefaults: object({
    instructions: ref("InstructionsDocument"),
    mcp: ref("McpDocument"),
    skills: ref("SkillsDocument"),
  }),
  EnvironmentDefaultsInput: object({}),
  EnvironmentStatus: object({
    mcp: ref("ManagedOutputStatus"),
    skills: ref("ManagedOutputStatus"),
  }),
  ExecutionCapability: object({
    key: { ...text(64), minLength: 1, pattern: "^[a-z0-9]+(?:-[a-z0-9]+)*$" },
    label: { ...text(128), minLength: 1 },
  }),
  CapabilityProfile: object({
    serviceNodeId: wire("Identifier"),
    hostId: wire("Identifier"),
    revision: integer(1),
    capabilities: array(ref("ExecutionCapability"), 64),
    updatedAt: time,
    operationId: nullable(wire("Identifier")),
  }),
  CapabilityProfileInput: object({}),
  ConfigureCapabilitiesInput: object({
    operationId: wire("Identifier"),
    expectedRevision: integer(1),
    capabilities: array(ref("ExecutionCapability"), 64),
  }),
  Settings: object(
    {
      nativeExecutable: { ...text(2048), minLength: 1 },
      nativeVersion: id,
      nativeExecutableHash: hash,
      nativeHome: { ...text(2048), minLength: 1 },
      codexHome: { ...text(2048), minLength: 1 },
      skillsRoot: { ...text(2048), minLength: 1 },
      patcherEnabled: bool,
      projectRoot: { ...text(2048), minLength: 1 },
      internalProjectRoot: { ...text(2048), minLength: 1 },
      appServer: {
        oneOf: [
          object({ mode: { const: "owned-stdio" } }),
          object(
            {
              mode: { const: "external-proxy" },
              socketPath: { ...text(2048), minLength: 1 },
              expectedCodexHome: { ...text(2048), minLength: 1 },
            },
            ["mode"],
          ),
          object(
            {
              mode: { const: "claude-adapter" },
              adapterRoot: { ...text(2048), minLength: 1 },
              nodeExecutableHash: hash,
              claudeExecutable: { ...text(2048), minLength: 1 },
              claudeExecutableHash: hash,
              defaultModel: { ...text(256), minLength: 1 },
            },
            [
              "mode",
              "adapterRoot",
              "nodeExecutableHash",
              "claudeExecutable",
              "claudeExecutableHash",
              "defaultModel",
            ],
          ),
        ],
      },
      windowsShell: ref("WindowsShell"),
      capabilities: array(ref("ExecutionCapability"), 64),
      limits: object({
        maxOperations: integer(100, 1000000),
        maxJournalBytes: integer(1048576, 17179869184),
        maxPendingInputs: integer(1, 128),
        maxNotificationBytes: integer(1048576, 268435456),
      }),
    },
    ["nativeExecutable", "nativeVersion", "nativeExecutableHash", "limits"],
  ),
  RequestId: { anyOf: [{ type: "string" }, integer(-Number.MAX_SAFE_INTEGER)] },
  NativeError: object(
    { code: { type: "integer" }, message: text(65536), data: wire("Json") },
    ["code", "message"],
  ),
  Reply: {
    oneOf: [
      object({ result: wire("Json") }),
      object({ error: ref("NativeError") }),
    ],
  },
  Operation: object({
    schemaVersion: { const: 1 },
    operationId: wire("Identifier"),
    callerPrincipalId: wire("Identifier"),
    serviceNodeId: wire("Identifier"),
    nativeVersion: id,
    nativeExecutableHash: hash,
    method,
    params: wire("Json"),
    requestHash: hash,
    phase: {
      enum: [
        "accepted",
        "dispatched",
        "succeeded",
        "failed",
        "outcome_unknown",
      ],
    },
    createdAt: time,
    updatedAt: time,
    epoch: nullable(id),
    requestId: nullable(ref("RequestId")),
    reply: nullable(ref("Reply")),
    code: nullable(id),
  }),
  InputIdentity: object({
    serviceNodeId: wire("Identifier"),
    epoch: id,
    requestId: ref("RequestId"),
  }),
  InputNotification: object({
    identity: ref("InputIdentity"),
    threadId: nullable(id),
  }),
  PendingInput: object({
    identity: ref("InputIdentity"),
    method,
    params: wire("Json"),
    observedAt: time,
    updatedAt: time,
    threadId: nullable(id),
    turnId: nullable(id),
    state: {
      enum: ["pending", "answering", "answered", "expired", "outcome_unknown"],
    },
    answerOperationId: nullable(wire("Identifier")),
    answerCallerPrincipalId: nullable(wire("Identifier")),
    reply: nullable(ref("Reply")),
    code: nullable(id),
  }),
  Notification: object({
    sequence: integer(1),
    serviceNodeId: wire("Identifier"),
    epoch: id,
    nativeVersion: id,
    method,
    params: wire("Json"),
    observedAt: time,
  }),
  Status: object(
    {
      serviceNodeId: wire("Identifier"),
      hostId: wire("Identifier"),
      nativeVersion: id,
      serverType: { enum: ['codex', 'claude'] },
      nativeExecutableHash: hash,
      catalogHash: hash,
      epoch: nullable(id),
      processStops: array(object({ epoch: id, observedAt: time })),
      pid: nullable(integer(1)),
      state: {
        enum: [
          "starting",
          "initializing",
          "ready",
          "stopping",
          "stopped",
          "failed",
        ],
      },
      observedAt: time,
      code: nullable(id),
      initialized: nullable(wire("Json")),
      pendingInputs: integer(0, 128),
      operations: object({
        retained: integer(),
        maximum: integer(100, 1000000),
        bytes: integer(),
        maximumBytes: integer(1048576, 17179869184),
      }),
      observedMethods: array(
        object({
          method,
          lastSucceededAt: nullable(time),
          lastFailedAt: nullable(time),
          lastCode: nullable(id),
        }),
      ),
      windowsShell: nullable(ref("WindowsShellStatus")),
      instructions: ref("InstructionsStatus"),
      environment: ref("EnvironmentStatus"),
      capabilities: ref("CapabilityProfile"),
      connection: object({
        mode: { enum: ["owned-stdio", "external-proxy"] },
        ownsServer: bool,
        ownsHome: bool,
        actualHome: text(2048),
        actualVersion: id,
        socketPath: nullable(text(2048)),
        serverPid: nullable(integer(1, 2147483647)),
        lifecycle: {
          enum: [
            "started",
            "alreadyRunning",
            "explicit-endpoint",
            "owned-stdio",
          ],
        },
        mcpIdentity: { enum: ["shared-user-home", "instance-environment"] },
      }),
    },
    [
      "serviceNodeId",
      "hostId",
      "nativeVersion",
      "nativeExecutableHash",
      "catalogHash",
      "epoch",
      "pid",
      "state",
      "observedAt",
      "code",
      "initialized",
      "pendingInputs",
      "operations",
      "observedMethods",
    ],
  ),
  ThreadSummary: object({
    nativeId: id,
    owner: { enum: ["this-native-connection", "historical-unattached"] },
    epoch: nullable(id),
    preview: text(2048),
    cwd: nullable(text(2048)),
    name: nullable(text(512)),
    status: wire("Json"),
    source: wire("Json"),
    updatedAt: wire("Json"),
    projectId: nullable(id),
    recencyAt: nullable(integer()),
    canAcceptDirectInput: nullable(bool),
    threadSource: nullable({ type: "string", maxLength: 256 }),
    archived: bool,
    ephemeral: bool,
  }),
  ProjectSummary: object(
    {
      nativeId: id,
      source: { const: "native" },
      name: text(256),
      paths: array(text(2048), 128),
      kind: { enum: ["existing", "normal", "internal", "task"] },
      position: integer(),
      recencyAt: nullable(integer()),
    },
    ["nativeId", "source", "name", "paths"],
  ),
  StatusInput: object({}),
  NativeDiscoveryInput: object({ query: text(512), method }, []),
  NativeDiscovery: object({
    nativeVersion: id,
    catalogHash: hash,
    items: array(
      object(
        {
          method,
          description: text(2048),
          readOnlyHint: bool,
          expectedDefinitionHash: hash,
          inputSchema: schema,
          outputSchema: schema,
          runtimeConstraints: array(
            object({
              condition: text(256),
              unsupportedParameters: array(text(128), 32),
              guidance: text(1024),
            }),
            32,
          ),
        },
        ["method", "description", "readOnlyHint", "expectedDefinitionHash"],
      ),
    ),
  }),
  FrameLimitsInput: object({}),
  StageFileInput: object({
    name: { ...text(255), minLength: 1 },
    dataBase64: { type: "string", maxLength: 11184812 },
  }),
  StagedFile: object({
    path: { ...text(4096), minLength: 1 },
    name: { ...text(128), minLength: 1 },
    byteLength: integer(0, 8388608),
    contentHash: hash,
  }),
  FrameLimits: object({
    serviceNodeId: wire("Identifier"),
    nativeVersion: id,
    nativeExecutableHash: hash,
    catalogHash: hash,
    epoch: nullable(id),
    requestFrameBytes: { const: 6291456 },
    receivedFrameBytes: { const: 25165824 },
    answerFrameBytes: { const: 4194304 },
    managementFrameBytes: { const: 33554432 },
  }),
  OperationInput: object({ operationId: wire("Identifier") }),
  Interaction: object({
    operationId: wire("Identifier"),
    epoch: id,
    reply: ref("Reply"),
  }),
  PreventInput: object({
    operationId: {
      ...wire("Identifier"),
      description:
        "Caller-generated <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status, use current Unix milliseconds and a unique 1-128 character nonce without a colon. Valid for 24 hours and at most 60 seconds in the future; a Hive restart changes the epoch. Keep this exact ID and check agent_manager_read after an uncertain result.",
    },
    nativeVersion: { enum: nativeVersions },
    method,
    params: wire("Json"),
    expectedDefinitionHash: hash,
  }),
  OperationAbsence: object({
    kind: { const: "agent_operation_absent" },
    operationId: wire("Identifier"),
    serviceNodeId: wire("Identifier"),
    epoch: nullable(id),
  }),
  PendingInputQuery: object(
    {
      includeExpired: bool,
      limit: integer(1, 128),
      identity: ref("InputIdentity"),
    },
    [],
  ),
  PendingInputPage: object({
    epoch: nullable(id),
    items: array(ref("PendingInput"), 128),
    truncated: bool,
  }),
  InputDefinitionQuery: object({ identity: ref("InputIdentity") }),
  InputDefinition: object({
    identity: ref("InputIdentity"),
    method,
    nativeVersion: id,
    catalogHash: hash,
    responseSchema: schema,
  }),
  AnswerInput: object({
    operationId: wire("Identifier"),
    identity: ref("InputIdentity"),
    reply: ref("Reply"),
  }),
  NotificationQuery: object(
    { afterSequence: integer(), limit: integer(1, 100) },
    ["afterSequence"],
  ),
  NotificationPage: object({
    epoch: nullable(id),
    firstAvailableSequence: integer(1),
    throughSequence: integer(),
    gap: bool,
    hasMore: bool,
    items: array(ref("Notification"), 100),
  }),
  ProjectsInput: object({}),
  ExecutionDefaults: object({
    model: nullable(text(256)), mode: nullable(text(64)), effort: nullable(text(64)), permission: nullable(text(256)),
  }),
  ExecutionDefaultsDocument: object({
    schemaVersion: { const: 1 },
    codex: ref('ExecutionDefaults'),
    claude: ref('ExecutionDefaults'),
  }),
  DirectoryListInput: object({ path: { ...text(2048), minLength: 1 } }),
  DirectoryListResult: object({
    path: text(2048),
    parent: nullable(text(2048)),
    directories: array(object({ name: text(256), path: text(2048) }), 10000),
  }),
  ProjectSelection: {
    oneOf: [
      object({ kind: { const: "projectless" }, key: id }),
      object({
        kind: { const: "existing" },
        cwd: { ...text(2048), minLength: 1 },
      }),
      object({
        kind: { enum: ["normal", "internal"] },
        key: id,
        name: { ...text(256), minLength: 1 },
      }),
      object({
        kind: { const: "task" },
        key: { type: "string", pattern: "^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$" },
        name: { ...text(256), minLength: 1 },
      }),
    ],
  },
  ProjectResolveInput: object(
    { selection: ref("ProjectSelection"), expectedProjectId: id },
    ["selection"],
  ),
  ProjectLocation: {
    oneOf: [
      object({
        project: ref("ProjectSummary"),
        cwd: { ...text(2048), minLength: 1 },
        kind: { enum: ["existing", "normal", "internal", "task"] },
      }),
      object({
        project: { type: "null" },
        cwd: { ...text(2048), minLength: 1 },
        kind: { const: "projectless" },
      }),
    ],
  },
  ProjectsResult: object(
    {
      source: { const: "native" },
      observedAt: time,
      projects: array(ref("ProjectSummary")),
      defaults: object({
        projectRoot: text(2048),
        internalProjectRoot: text(2048),
      }),
    },
    ["source", "observedAt", "projects"],
  ),
  WorkspaceRequirement: {
    oneOf: [
      object({ kind: { const: "task_workspace" } }),
      object({
        kind: { const: "directory_path" },
        path: { ...text(2048), minLength: 1 },
      }),
      object({
        kind: { const: "existing_project" },
        projectId: id,
        path: nullable(text(2048)),
        useWorktree: bool,
      }),
      object({
        kind: { const: "repository_path" },
        repositoryUrl: { ...text(4096), minLength: 1 },
        folderName: { ...text(128), minLength: 1 },
      }),
      object({
        kind: { const: "new_project_path" },
        folderName: { ...text(128), minLength: 1 },
      }),
    ],
  },
  WorkspaceResolveInput: object(
    {
      taskKey: { ...id, pattern: "^TASK-[0-9]{4,}$" },
      requirement: ref("WorkspaceRequirement"),
      prepare: bool,
      verifyOrigin: bool,
    },
    ["taskKey", "requirement", "prepare"],
  ),
  RepositoryObservation: object({
    name: text(512),
    branch: nullable(text(512)),
    commit: nullable(text(128)),
    dirty: bool,
    originName: nullable(text(128)),
    originUrl: nullable(text(4096)),
    originState: { enum: ["confirmed", "local_only", "no_origin", "unknown"] },
    remoteRef: nullable(text(1024)),
    observedAt: time,
    limitation: nullable(text(4096)),
  }),
  WorkspaceResolution: object(
    {
      hostId: wire("Identifier"),
      serviceNodeId: wire("Identifier"),
      canonicalCwd: { ...text(2048), minLength: 1 },
      taskRoot: { ...text(2048), minLength: 1 },
      bootstrapPath: { ...text(2048), minLength: 1 },
      intendedPath: { ...text(2048), minLength: 1 },
      sourcePath: nullable(text(2048)),
      project: nullable(wire("ResourceRef")),
      nativeProjectId: id,
      useWorktree: bool,
      repository: nullable(ref("RepositoryObservation")),
    },
    [
      "hostId",
      "serviceNodeId",
      "canonicalCwd",
      "taskRoot",
      "bootstrapPath",
      "intendedPath",
      "sourcePath",
      "project",
      "useWorktree",
      "repository",
    ],
  ),
};
definitions.Operation.allOf = [
  {
    if: {
      properties: {
        phase: { enum: ["accepted", "dispatched", "outcome_unknown"] },
      },
    },
    then: { properties: { reply: { type: "null" } } },
  },
  {
    if: { properties: { phase: { const: "succeeded" } } },
    then: {
      properties: {
        reply: object({ result: wire("Json") }),
        code: { type: "null" },
      },
    },
  },
  {
    if: { properties: { phase: { const: "failed" } } },
    then: {
      properties: {
        reply: nullable(object({ error: ref("NativeError") })),
        code: id,
      },
    },
  },
  {
    if: { properties: { phase: { const: "outcome_unknown" } } },
    then: { properties: { code: id, epoch: id, requestId: ref("RequestId") } },
  },
  {
    if: { properties: { phase: { const: "accepted" } } },
    then: {
      properties: { epoch: { type: "null" }, requestId: { type: "null" } },
    },
  },
  {
    if: { properties: { phase: { enum: ["dispatched", "succeeded"] } } },
    then: { properties: { epoch: id, requestId: ref("RequestId") } },
  },
];
definitions.PendingInput.allOf = [
  {
    if: { properties: { state: { const: "pending" } } },
    then: {
      properties: {
        answerOperationId: { type: "null" },
        answerCallerPrincipalId: { type: "null" },
        reply: { type: "null" },
        code: { type: "null" },
      },
    },
  },
  {
    if: { properties: { state: { enum: ["answering", "answered"] } } },
    then: {
      properties: {
        answerOperationId: wire("Identifier"),
        answerCallerPrincipalId: wire("Identifier"),
        reply: ref("Reply"),
      },
    },
  },
];
// The observation adapter is deliberately closed. Its native argument shapes come from
// the checked exports, without hand-maintained copies or a generic method escape hatch.
const readVariants = [];
for (const version of nativeVersions) {
  const catalog = JSON.parse(
    readFileSync(`specs/native/codex-${version}/catalog.json`, "utf8"),
  );
  if (catalog.version !== version)
    throw new Error("Native catalog folder and version must agree.");
  for (const name of [
    "thread/read",
    "thread/turns/list",
    "thread/items/list",
  ]) {
    const input = catalog.clientRequests.find(
      (value) => value.method === name,
    )?.inputSchema;
    if (!input?.$ref?.startsWith("#/$defs/") || !input.$defs)
      throw new Error(`Missing checked native read schema: ${version} ${name}`);
    const prefix =
      "Read" +
      version.replaceAll(".", "") +
      name
        .split("/")
        .map((part) => part[0].toUpperCase() + part.slice(1))
        .join("");
    const lift = (value) => {
      if (Array.isArray(value)) return value.map(lift);
      if (!value || typeof value !== "object") return value;
      const lifted = Object.fromEntries(
        Object.entries(value).map(([key, child]) => {
          if (key !== "$ref") return [key, lift(child)];
          if (!child.startsWith("#/$defs/") || !input.$defs[child.slice(8)])
            throw new Error("Unresolved native read reference.");
          return [key, "#/$defs/" + prefix + child.slice(8)];
        }),
      );
      // Open native objects still carry JSON values in Ivy's read envelopes.
      if (
        value.type === "object" &&
        (value.additionalProperties === undefined ||
          value.additionalProperties === true)
      )
        lifted.additionalProperties = wire("Json");
      return lifted;
    };
    for (const [key, value] of Object.entries(input.$defs))
      definitions[prefix + key] = lift(value);
    const root = definitions[prefix + input.$ref.slice(8)];
    if (root.type !== "object")
      throw new Error("Native read arguments must be an object.");
    root.additionalProperties = false;
    readVariants.push({
      nativeVersion: { const: version },
      method: { const: name },
      params: ref(prefix + input.$ref.slice(8)),
    });
  }
}
definitions.ReadInput = {
  oneOf: readVariants.map((properties) => object(properties)),
};
definitions.ReadObservation = {
  oneOf: readVariants.map((properties) =>
    object({
      schemaVersion: { const: 1 },
      observationId: wire("Identifier"),
      callerPrincipalId: wire("Identifier"),
      serviceNodeId: wire("Identifier"),
      nativeExecutableHash: hash,
      catalogHash: hash,
      epoch: id,
      requestId: ref("RequestId"),
      observedAt: time,
      requestHash: hash,
      ...properties,
      reply: ref("Reply"),
    }),
  ),
};
const document = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://ivy.invalid/schemas/agent.schema.json",
  title:
    "Native Codex projection and AgentManager ownership, observations and outcome journal",
  $defs: definitions,
};
const file = "specs/schemas/agent.schema.json",
  content = JSON.stringify(document, null, 2) + "\n";
if (process.argv.includes("--check")) {
  if (readFileSync(file, "utf8") !== content)
    throw new Error("Stale AgentManager contract.");
} else writeFileSync(file, content);
