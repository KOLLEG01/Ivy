import { readFileSync, writeFileSync } from "node:fs";

// Normative contracts only. Native templates are derived from the retained public schemas.
const schemaId = "https://ivy.invalid/schemas/chat.schema.json";
const ref = (name) => ({ $ref: "#/$defs/" + name });
const wire = (name) => ({
  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/" + name,
});
const object = (properties) => ({
  type: "object",
  properties,
  required: Object.keys(properties),
  additionalProperties: false,
});
const nullable = (value) => ({ anyOf: [value, { type: "null" }] });
const string = (maxLength, minLength = 0) => ({
  type: "string",
  minLength,
  maxLength,
});
const integer = (minimum = 0, maximum = Number.MAX_SAFE_INTEGER) => ({
  type: "integer",
  minimum,
  maximum,
});
const array = (items, maxItems) => ({ type: "array", items, maxItems });
const id = wire("Identifier"),
  hash = wire("Hash"),
  resource = wire("ResourceRef");
const timestamp = {
  ...string(24, 24),
  pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$",
};
const agent = (name) => ({
  $ref: "https://ivy.invalid/schemas/agent.schema.json#/$defs/" + name,
});
const defs = {};
Object.assign(defs, {
  NativePlan: agent("Plan"),
  NativePlanDraft: agent("PlanDraft"),
  // Reassembled binary evidence has a transport envelope; the shared SDK validates the entire
  // native payload against its exact selected schema before every save and reconstruction.
  NativeRequest: agent("InvocationDraft"),
  ObjectPin: object({ objectId: id, revision: integer(1) }),
  Channel: object({
    // Saved transport identities are evidence, including outputs admitted before a reconfiguration.
    adapter: id,
    accountId: id,
    channelId: id,
  }),
  ChannelConfiguration: object({
    channel: ref("Channel"),
    displayName: string(256, 1),
  }),
  Definition: object({
    workspaceId: id,
    principalId: id,
    rootObjectId: nullable(id),
    project: resource,
    nativePlan: ref("NativePlan"),
    channels: { ...array(ref("WhatsAppChannelConfiguration"), 1), minItems: 1 },
  }),
  Settings: object({
    definition: ref("Definition"),
    pollMs: integer(1000, 60000),
  }),
  ExpectedBridge: object({
    principalId: id,
    callerPrincipalId: id,
    workspaceId: id,
    definitionHash: hash,
  }),
  Artifact: object({
    object: ref("ObjectPin"),
    contentHash: hash,
    byteLength: integer(1, 1048576),
    mediaType: string(128, 1),
    label: string(256, 1),
  }),
  Image: object({
    object: ref("ObjectPin"),
    contentHash: hash,
    byteLength: integer(1, 2097152),
    mediaType: { enum: ["image/png", "image/jpeg", "image/webp"] },
    label: string(256, 1),
  }),
  Payload: object({ text: string(65536), images: array(ref("Image"), 2) }),
  Error: object({
    code: id,
    outcome: { enum: ["not_executed", "unknown"] },
    detail: string(2048, 1),
  }),
  InputIdentity: object({
    channel: ref("Channel"),
    senderPrincipalId: id,
    messageId: id,
  }),
});
defs.WhatsAppChannelConfiguration = object({
  channel: object({ adapter: { const: "whatsapp" }, accountId: id, channelId: id }),
  displayName: string(256, 1),
});
// Frozen definitions on retained Main records describe their original admission.
defs.SavedDefinition = structuredClone(defs.Definition);
defs.SavedDefinition.properties.channels = { ...array(ref("ChannelConfiguration"), 32), minItems: 1 };
// Protected runtime transport configuration.
defs.Settings.properties.whatsappConfigPath = string(4096, 1);
defs.Settings.properties.language = { enum: ["en", "de"] };
defs.Payload.properties.turnOptions = {
  type: "object",
  additionalProperties: false,
  properties: {
    model: nullable(string(256, 1)),
    effort: nullable({
      enum: [
        "none",
        "minimal",
        "low",
        "medium",
        "high",
        "xhigh",
        "max",
        "ultra",
      ],
    }),
    serviceTier: nullable({ enum: ["fast", "flex"] }),
  },
};
const action = (name, properties) =>
  object({
    action: { const: name },
    operationId: id,
    expectedBridge: ref("ExpectedBridge"),
    ...properties,
  });
Object.assign(defs, {
  CreateMainRequest: action("createMain", {
    expectedMainRevision: nullable(integer(1)),
    reason: string(4096, 1),
  }),
  SendRequest: action("send", {
    channel: ref("Channel"),
    messageId: id,
    expectedBinding: nullable(ref("ObjectPin")),
    payload: ref("Payload"),
  }),
  CancelRequest: action("cancel", {
    inputId: id,
    expectedRevision: integer(1),
    reason: string(4096, 1),
  }),
  ReceiveRequest: action("receive", {
    channel: ref("Channel"),
    afterSequence: integer(),
    limit: integer(1, 8),
  }),
  AcknowledgeRequest: action("acknowledge", {
    offerId: id,
    replyIds: { ...array(id, 8), minItems: 1, uniqueItems: true },
  }),
  OperationQuery: object({
    operationId: id,
    expectedBridge: ref("ExpectedBridge"),
  }),
  WorkspaceQuery: object({}),
  HistoryQuery: object({
    expectedBridge: ref("ExpectedBridge"),
    channel: ref("Channel"),
    afterSequence: integer(),
    limit: integer(1, 8),
  }),
  InputQuery: object({ expectedBridge: ref("ExpectedBridge"), inputId: id }),
  NotifyRequest: object({
    action: { const: "notify" },
    operationId: id,
    source: ref("ObjectPin"),
    text: string(16384, 1),
  }),
  NoticeQuery: object({
    operationId: id,
  }),
});
defs.AdmittedNotifyRequest = action("notify", {
  channel: ref("Channel"), source: ref("ObjectPin"), text: string(16384, 1),
});
// Explicitly carry a previous configuration's Main into this definition.
defs.CreateMainRequest.properties.resumeMain = ref("ObjectPin");
defs.ActionRequest = {
  oneOf: [
    "CreateMain",
    "Send",
    "Cancel",
    "Receive",
    "Acknowledge",
    "Notify",
  ].map((name) => ref(name + "Request")),
};
defs.AdmittedActionRequest = structuredClone(defs.ActionRequest);
defs.AdmittedActionRequest.oneOf = defs.AdmittedActionRequest.oneOf.map(value =>
  value.$ref.endsWith("/NotifyRequest") ? ref("AdmittedNotifyRequest") : value);
Object.assign(defs, {
  Binding: object({
    schemaVersion: { const: 1 },
    workspaceId: id,
    definitionHash: hash,
    project: resource,
    primary: resource,
    nativePlan: ref("NativePlan"),
    createdAt: timestamp,
    operationId: id,
    predecessor: nullable(ref("ObjectPin")),
  }),
  Ticket: object({
    inputId: id,
    identity: ref("InputIdentity"),
    requestHash: hash,
    binding: ref("ObjectPin"),
    sequence: integer(1),
    admittedAt: timestamp,
  }),
  Conversation: object({
    schemaVersion: { const: 1 },
    workspaceId: id,
    definitionHash: hash,
    createdAt: timestamp,
  }),
  ResultPublication: object({
    inputId: id,
    collection: ref("ObjectPin"),
    result: ref("ObjectPin"),
    firstSequence: integer(1),
    partCount: integer(1, 65536),
    publishedParts: integer(0, 65536),
    createdAt: timestamp,
  }),
  Main: object({
    schemaVersion: { const: 1 },
    definition: ref("SavedDefinition"),
    definitionHash: hash,
    conversation: ref("ObjectPin"),
    binding: nullable(ref("ObjectPin")),
    pendingAction: nullable(ref("ObjectPin")),
    publication: nullable(ref("ResultPublication")),
    queue: array(ref("Ticket"), 64),
    nextSequence: integer(1),
    updatedAt: timestamp,
  }),
  NativeCall: object({
    schemaVersion: { const: 1 },
    operationId: id,
    serviceNodeId: id,
    callerPrincipalId: id,
    request: ref("ObjectPin"),
    nativeVersion: id,
    catalogSourceHash: hash,
    method: {
      enum: ["thread/start", "thread/resume", "turn/start", "turn/interrupt"],
    },
    predecessor: nullable(ref("ObjectPin")),
    preparedEpoch: nullable(id),
    requestHash: hash,
    expectedDefinitionHash: hash,
    createdAt: timestamp,
    state: {
      enum: [
        "prepared",
        "accepted",
        "dispatched",
        "succeeded",
        "failed",
        "outcome_unknown",
      ],
    },
    epoch: nullable(id),
    evidence: nullable(ref("ObjectPin")),
    code: nullable(id),
    updatedAt: timestamp,
  }),
  Input: object({
    schemaVersion: { const: 1 },
    workspaceId: id,
    definitionHash: hash,
    identity: ref("InputIdentity"),
    operationId: id,
    requestHash: hash,
    binding: ref("ObjectPin"),
    sequence: integer(1),
    payload: ref("Payload"),
    state: {
      enum: [
        "queued",
        "preparing",
        "delivering",
        "running",
        "waiting_input",
        "collecting",
        "completed",
        "failed",
        "cancel_requested",
        "cancelled",
        "outcome_unknown",
      ],
    },
    nativeCalls: object({
      resume: nullable(ref("ObjectPin")),
      turn: nullable(ref("ObjectPin")),
      interrupt: nullable(ref("ObjectPin")),
    }),
    turnId: nullable(id),
    epoch: nullable(id),
    result: nullable(ref("ObjectPin")),
    error: nullable(ref("Error")),
    cancellation: nullable(
      object({
        operationId: id,
        callerPrincipalId: id,
        reason: string(4096, 1),
        requestedAt: timestamp,
      }),
    ),
    admittedAt: timestamp,
    updatedAt: timestamp,
    finishedAt: nullable(timestamp),
  }),
  Evidence: object({
    schemaVersion: { const: 1 },
    format: {
      enum: [
        "native-request/canonical-json",
        "agent-operation/canonical-json",
        "agent-read-observation/canonical-json",
      ],
    },
    contentHash: hash,
    byteLength: integer(1, 16777216),
    chunks: { ...array(ref("Artifact"), 16), minItems: 1 },
  }),
  Collection: object({
    schemaVersion: { const: 1 },
    input: ref("ObjectPin"),
    binding: ref("ObjectPin"),
    turnCall: ref("ObjectPin"),
    turnId: id,
    epoch: id,
    attempt: integer(1),
    previousAttempt: nullable(ref("ObjectPin")),
    reads: array(ref("ObjectPin"), 1024),
    nativeBytes: integer(0, 67108864),
    state: { enum: ["collecting", "ready", "complete", "failed"] },
    result: nullable(ref("ObjectPin")),
    error: nullable(ref("Error")),
    createdAt: timestamp,
    updatedAt: timestamp,
  }),
  RetryCollectionRequest: object({
    inputId: id,
    expectedCollection: ref("ObjectPin"),
    expectedBridge: ref("ExpectedBridge"),
    operationId: id,
    reason: string(4096, 1),
  }),
  Result: object({
    schemaVersion: { const: 1 },
    inputId: id,
    binding: ref("ObjectPin"),
    turnId: id,
    nativeState: { enum: ["completed", "failed", "interrupted"] },
    evidence: { ...array(ref("ObjectPin"), 1024), minItems: 1 },
    textParts: array(ref("Artifact"), 64),
    createdAt: timestamp,
  }),
  ReplyOrigin: {
    oneOf: [
      object({
        kind: { const: "native" },
        inputId: id,
        binding: ref("ObjectPin"),
        result: ref("ObjectPin"),
      }),
      object({
        kind: { const: "notice" },
        operation: ref("ObjectPin"),
        claim: ref("ObjectPin"),
        producerPrincipalId: id,
        source: ref("ObjectPin"),
      }),
    ],
  },
  Reply: object({
    schemaVersion: { const: 1 },
    workspaceId: id,
    definitionHash: hash,
    channel: ref("Channel"),
    origin: ref("ReplyOrigin"),
    sequence: integer(1),
    partIndex: integer(0, 65535),
    text: string(16384),
    artifacts: array(ref("Artifact"), 32),
    createdAt: timestamp,
    immutableHash: hash,
    state: { enum: ["pending", "outcome_unknown", "confirmed"] },
    firstOfferedAt: nullable(timestamp),
    confirmedAt: nullable(timestamp),
  }),
  Offer: object({
    schemaVersion: { const: 1 },
    operationId: id,
    callerPrincipalId: id,
    channel: ref("Channel"),
    afterSequence: integer(),
    throughSequence: integer(),
    replies: array(ref("ObjectPin"), 8),
    createdAt: timestamp,
  }),
  Acknowledgement: object({
    schemaVersion: { const: 1 },
    operationId: id,
    callerPrincipalId: id,
    offer: ref("ObjectPin"),
    replyIds: { ...array(id, 8), minItems: 1, uniqueItems: true },
    createdAt: timestamp,
  }),
  ObjectView: object({ object: ref("ObjectPin"), data: wire("Json") }),
});
// Views name their exact content instead of allowing an arbitrary JSON provider response.
for (const name of [
  "Main",
  "Binding",
  "Input",
  "Reply",
  "Offer",
  "Acknowledgement",
])
  defs[name + "View"] = object({ object: ref("ObjectPin"), data: ref(name) });
delete defs.ObjectView;
Object.assign(defs, {
  ActionOutcome: {
    oneOf: [
      object({
        action: { const: "createMain" },
        operationId: id,
        binding: ref("BindingView"),
      }),
      object({
        action: { const: "send" },
        operationId: id,
        input: ref("InputView"),
      }),
      object({
        action: { const: "cancel" },
        operationId: id,
        input: ref("InputView"),
      }),
      object({
        action: { const: "receive" },
        operationId: id,
        offer: ref("OfferView"),
        replies: array(ref("ReplyView"), 8),
      }),
      object({
        action: { const: "acknowledge" },
        operationId: id,
        acknowledgement: ref("AcknowledgementView"),
      }),
      object({
        action: { const: "notify" },
        operationId: id,
        reply: ref("ReplyView"),
      }),
    ],
  },
  Operation: object({
    schemaVersion: { const: 1 },
    workspaceId: id,
    definitionHash: hash,
    callerPrincipalId: id,
    operationId: id,
    request: ref("AdmittedActionRequest"),
    requestHash: hash,
    phase: {
      enum: ["accepted", "applying", "succeeded", "failed", "outcome_unknown"],
    },
    nativeCall: nullable(ref("ObjectPin")),
    outcome: nullable(ref("ActionOutcome")),
    error: nullable(ref("Error")),
    createdAt: timestamp,
    updatedAt: timestamp,
  }),
  WorkspaceInfo: object({
    expectedBridge: ref("ExpectedBridge"),
    project: resource,
    main: nullable(ref("MainView")),
    binding: nullable(ref("BindingView")),
    channels: array(ref("ChannelConfiguration"), 32),
    nativeOwnerReady: { type: "boolean" },
    observedAt: timestamp,
  }),
  HistoryPage: object({
    entries: array({ oneOf: [ref("InputView"), ref("ReplyView")] }, 8),
    throughSequence: integer(),
    hasMore: { type: "boolean" },
  }),
});
const when = (key, values, properties) => ({
  if: { properties: { [key]: { enum: values } } },
  then: { properties },
});
defs.Input.allOf = [
  when("state", ["queued"], {
    turnId: { type: "null" },
    epoch: { type: "null" },
    result: { type: "null" },
    error: { type: "null" },
  }),
  when(
    "state",
    [
      "queued",
      "preparing",
      "delivering",
      "running",
      "waiting_input",
      "collecting",
      "cancel_requested",
      "outcome_unknown",
    ],
    { finishedAt: { type: "null" } },
  ),
  when("state", ["running", "waiting_input", "collecting", "completed"], {
    turnId: id,
    epoch: id,
  }),
  when("state", ["completed"], {
    result: ref("ObjectPin"),
    finishedAt: timestamp,
    error: { type: "null" },
  }),
  when("state", ["failed", "cancelled"], { finishedAt: timestamp }),
  when("state", ["failed"], { error: ref("Error") }),
  when("state", ["cancel_requested", "cancelled"], {
    cancellation: defs.Input.properties.cancellation.anyOf[0],
  }),
  when("state", ["outcome_unknown"], {
    error: object({ ...defs.Error.properties, outcome: { const: "unknown" } }),
  }),
];
defs.NativeCall.allOf = [
  when("state", ["prepared"], {
    epoch: { type: "null" },
    evidence: { type: "null" },
    code: { type: "null" },
  }),
  when("state", ["accepted"], {
    epoch: { type: "null" },
    evidence: ref("ObjectPin"),
    code: { type: "null" },
  }),
  when("state", ["dispatched", "succeeded"], {
    epoch: id,
    evidence: ref("ObjectPin"),
    code: { type: "null" },
  }),
  when("state", ["failed"], { evidence: ref("ObjectPin"), code: id }),
  when("state", ["outcome_unknown"], { code: id }),
];
defs.Collection.allOf = [
  when("state", ["collecting", "ready"], {
    result: { type: "null" },
    error: { type: "null" },
  }),
  when("state", ["ready", "complete"], {
    reads: { minItems: 3 },
    nativeBytes: integer(1, 67108864),
  }),
  when("state", ["complete"], {
    result: ref("ObjectPin"),
    error: { type: "null" },
  }),
  when("state", ["failed"], { result: { type: "null" }, error: ref("Error") }),
  when("attempt", [1], { previousAttempt: { type: "null" } }),
  {
    if: { properties: { attempt: { minimum: 2 } } },
    then: { properties: { previousAttempt: ref("ObjectPin") } },
  },
];
defs.Reply.allOf = [
  when("state", ["pending"], {
    firstOfferedAt: { type: "null" },
    confirmedAt: { type: "null" },
  }),
  when("state", ["outcome_unknown"], {
    firstOfferedAt: timestamp,
    confirmedAt: { type: "null" },
  }),
  when("state", ["confirmed"], {
    firstOfferedAt: timestamp,
    confirmedAt: timestamp,
  }),
];
defs.Operation.allOf = [
  when("phase", ["succeeded"], {
    outcome: ref("ActionOutcome"),
    error: { type: "null" },
  }),
  when("phase", ["accepted", "applying"], {
    outcome: { type: "null" },
    error: { type: "null" },
  }),
  when("phase", ["outcome_unknown"], {
    error: object({ ...defs.Error.properties, outcome: { const: "unknown" } }),
  }),
  when("phase", ["failed", "outcome_unknown"], {
    outcome: { type: "null" },
    error: ref("Error"),
  }),
];
defs.Collection.properties.retryAuthorization = nullable(
  object({
    callerPrincipalId: id,
    operationId: id,
    reason: string(4096, 1),
    failed: ref("ObjectPin"),
  }),
);
const schema = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: schemaId,
  title:
    "ChatBridge WhatsApp Main, original native delivery and acknowledged outbox",
  $defs: defs,
};
const operations = Object.fromEntries(
  defs.ActionRequest.oneOf.map((value) => {
    const definition = value.$ref.slice(8),
      name = defs[definition].properties.action.const;
    return [
      name,
      {
        input: "#/$defs/" + definition,
        output: "#/$defs/Operation",
        mutation: true,
      },
    ];
  }),
);
for (const [name, input, output] of [
  ["operation", "OperationQuery", "Operation"],
  ["workspace", "WorkspaceQuery", "WorkspaceInfo"],
  ["history", "HistoryQuery", "HistoryPage"],
  ["input", "InputQuery", "InputView"],
  ["notice", "NoticeQuery", "ReplyView"],
])
  operations[name] = {
    input: "#/$defs/" + input,
    output: "#/$defs/" + output,
    mutation: false,
  };
operations.retryResult = {
  input: "#/$defs/RetryCollectionRequest",
  output: "#/$defs/Collection",
  mutation: true,
};
const contracts = [
  "Conversation",
  "Main",
  "Binding",
  "NativeCall",
  "Input",
  "Evidence",
  "Collection",
  "Result",
  "Reply",
  "Offer",
  "Acknowledgement",
  "Operation",
].map((name) => ({
  key:
    "chat-bridge/" +
    name.replace(/[A-Z]/g, (c, i) => (i ? "-" : "") + c.toLowerCase()),
  version: "1.1.0",
  definition: "#/$defs/" + name,
}));
const mediaContracts = [
  [
    "image-png",
    "image/png",
    "PNG image, at most 2 MiB. Exact magic, declared length and complete hash must agree before native dispatch.",
  ],
  [
    "image-jpeg",
    "image/jpeg",
    "JPEG image, at most 2 MiB. Exact magic, declared length and complete hash must agree before native dispatch.",
  ],
  [
    "image-webp",
    "image/webp",
    "WebP image, at most 2 MiB. RIFF/WEBP header, complete RIFF length, declared length and complete hash must agree before native dispatch.",
  ],
  [
    "evidence-chunk",
    "application/octet-stream",
    "At most 1 MiB exact canonical UTF-8 JSON bytes per chunk. Ordered manifests pin revisions, hashes and byte lengths. The reconstructed document is at most 16 MiB and must validate against its exact declared schema and original owner, caller and request identity. Bytes alone are never native proof or dispatch authority.",
  ],
  [
    "result-text",
    "text/plain",
    "At most 1 MiB UTF-8 text per immutable part. Ordered Result parts retain the complete text, at most 64 MiB in total. Text is rendered inert. No result is silently truncated.",
  ],
].map(([name, mediaType, detail]) => ({
  key: "chat-bridge/" + name,
  version: "1.0.0",
  owner: { kind: "service", serviceName: "chat-bridge" },
  mediaType,
  specMarkdown: "# ChatBridge " + name + " 1.0.0\n\n" + detail,
}));
for (const [path, data] of [
  ["specs/schemas/chat.schema.json", schema],
  [
    "specs/schemas/chat.operations.json",
    {
      schemaVersion: 1,
      namespace: "chat",
      schema: schemaId,
      contracts,
      mediaContracts,
      operations,
    },
  ],
]) {
  const content = JSON.stringify(data, null, 2) + "\n";
  if (process.argv.includes("--check")) {
    if (readFileSync(path, "utf8") !== content)
      throw new Error("Stale ChatBridge contract: " + path);
  } else writeFileSync(path, content);
}
