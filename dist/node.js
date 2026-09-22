import {
  ServiceClient,
  ServiceConnection,
  consumeEvents,
  toolDefinitionHash
} from "./chunk-JUSTUNMN.js";
import {
  hive_operations_schema_default,
  operationSchema,
  validateInput,
  validateOutput,
  validateShared,
  validateTransport
} from "./chunk-TYPU3LZR.js";
import {
  wireSchema
} from "./chunk-C4DMFRYX.js";
import {
  BoundToolClient,
  BrowserNotifications,
  HiveClient,
  agent_schema_default,
  baseUrl,
  browserClient,
  browserNotifications,
  callBound,
  deriveOperationId,
  discover,
  nativeServiceTools,
  nativeVersions,
  newOperationId,
  operationId,
  parseOperationId,
  responseValue,
  runtimeEpoch,
  scopedOperationId,
  serviceTools
} from "./chunk-YZRDUBD3.js";
import {
  SchemaValidators,
  admitSchema,
  hostSchema,
  host_schema_default
} from "./chunk-XNKMI6K4.js";
import {
  digest,
  hashJson,
  hive_wire_schema_default
} from "./chunk-ZEQOMSP4.js";
import {
  IvyError,
  binaryObjectContentBytes,
  canonical,
  compareVersions,
  connectionInFlightRequests,
  encodeJson,
  jsonObjectContentBytes,
  managementFrameBytes,
  mcpDiscoveryResultBytes,
  requireThat,
  textObjectContentBytes
} from "./chunk-62CEFMQW.js";

// packages/contracts/src/local-bundle.ts
function bundleLocalSchema(document, name) {
  const definitions = /* @__PURE__ */ Object.create(null);
  const visit = (value) => {
    if (Array.isArray(value)) {
      for (const child of value) visit(child);
      return;
    }
    if (!value || typeof value !== "object") return;
    for (const [key, child] of Object.entries(value)) {
      if (key === "$ref") {
        requireThat(typeof child === "string" && /^#\/\$defs\/[^/]+(?:\/.*)?$/.test(child), "registry_invalid", "Only local definition references may be bundled.");
        include(child.split("/")[2].replaceAll("~1", "/").replaceAll("~0", "~"));
      } else if (["properties", "$defs"].includes(key) && child && typeof child === "object") {
        for (const schema of Object.values(child)) visit(schema);
      } else if (["items", "additionalProperties", "allOf", "anyOf", "oneOf", "if", "then", "else", "not"].includes(key)) visit(child);
    }
  };
  const include = (key) => {
    if (Object.hasOwn(definitions, key)) return;
    requireThat(Object.hasOwn(document.$defs, key), "registry_invalid", "Missing bundled schema definition.");
    definitions[key] = structuredClone(document.$defs[key]);
    visit(definitions[key]);
  };
  include(name);
  return { ...document.$schema ? { $schema: document.$schema } : {}, $ref: "#/$defs/" + name.replaceAll("~", "~0").replaceAll("/", "~1"), $defs: definitions };
}

// packages/contracts/src/agent-validation.ts
import { Ajv2020 } from "ajv/dist/2020.js";
var agentSchema = agent_schema_default;
var ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [agent_schema_default, wireSchema]) ajv.addSchema(schema);
function validateAgent(name, value) {
  const validator = ajv.getSchema(`${String(agentSchema["$id"])}#/$defs/${name}`);
  requireThat(validator && validator(value), "invalid_arguments", "Value does not match its native projection/AgentManager contract.");
}
function validateAgentFrame(value, kind = "reply") {
  if (kind === "request-id") {
    requireThat(typeof value === "string" && value.length > 0 && value.length <= 256 || typeof value === "number" && Number.isSafeInteger(value), "native_invalid_frame", "Native request ID is invalid.");
    return;
  }
  requireThat(value !== null && typeof value === "object" && !Array.isArray(value), "native_invalid_frame", "Native reply must be an object.");
  const reply = value;
  const result = Object.hasOwn(reply, "result"), error = Object.hasOwn(reply, "error");
  requireThat(result !== error, "native_invalid_frame", "Native reply requires exactly one result or error.");
  if (error) {
    const detail = reply["error"];
    requireThat(detail !== null && typeof detail === "object" && !Array.isArray(detail), "native_invalid_frame", "Native error must be an object.");
    const item = detail;
    requireThat(Number.isSafeInteger(item["code"]) && typeof item["message"] === "string", "native_invalid_frame", "Native error envelope is incomplete.");
  }
}

// packages/contracts/src/native-contract.ts
var readOnlyNativeMethods = /* @__PURE__ */ new Set([
  "account/rateLimits/read",
  "collaborationMode/list",
  "model/list",
  "project/list",
  "server/diagnostics",
  "thread/items/list",
  "thread/list",
  "thread/loaded/list",
  "thread/read",
  "thread/turns/list",
  "userVerification/status"
]);
function isReadOnlyNativeMethod(method) {
  return readOnlyNativeMethods.has(method);
}
function nativeToolDefinition(catalog, method, catalogHash = hashJson(catalog)) {
  const readOnly = isReadOnlyNativeMethod(method.method);
  return {
    namespace: "codex",
    name: method.method,
    interfaceVersion: catalog.version,
    description: "Installed public Codex " + catalog.version + " method " + method.method + ". Supply its unchanged native arguments." + (readOnly ? " This read-only call does not require an operationId." : " Supply an outer stable operationId."),
    inputSchema: method.inputSchema,
    outputSchema: method.outputSchema,
    ...readOnly ? { annotations: { readOnlyHint: true, idempotentHint: true } } : {},
    nativeMethod: method.method,
    nativeSchemaIdentity: catalogHash
  };
}
var freeze = (value) => {
  if (value && typeof value === "object" && !Object.isFrozen(value)) {
    for (const child of Object.values(value)) freeze(child);
    Object.freeze(value);
  }
};
var NativeContract = class {
  catalog;
  catalogHash;
  methods = /* @__PURE__ */ new Map();
  definitionHashes = /* @__PURE__ */ new Map();
  validators = new SchemaValidators();
  schemas = /* @__PURE__ */ new Map();
  constructor(catalog) {
    validateAgent("Catalog", catalog);
    this.catalog = structuredClone(catalog);
    freeze(this.catalog);
    this.catalogHash = hashJson(this.catalog);
    for (const method of this.catalog.clientRequests) {
      requireThat(!this.methods.has(method.method), "native_catalog_invalid", "The selected native catalog contains a duplicate method.");
      const definition = nativeToolDefinition(this.catalog, method, this.catalogHash);
      freeze(definition);
      this.methods.set(method.method, definition);
    }
  }
  definition(method) {
    const value = this.methods.get(method);
    requireThat(value, "native_method_unsupported", "The method is absent from the exact selected native catalog.");
    return value;
  }
  definitions() {
    return new Map(this.methods);
  }
  /** Retained definitions are deeply frozen; only their immutable local hashes are cached. */
  definitionHash(method) {
    const existing = this.definitionHashes.get(method);
    if (existing) return existing;
    const value = toolDefinitionHash(this.definition(method));
    this.definitionHashes.set(method, value);
    return value;
  }
  /** Check a discovery snapshot against the retained contract; never adopt a replacement. */
  verifyDefinition(method, definition, definitionHash) {
    validateShared("ToolDefinition", definition);
    requireThat(
      definitionHash === toolDefinitionHash(definition) && definitionHash === this.definitionHash(method),
      "native_definition_changed",
      "The selected provider definition differs from the retained native contract."
    );
  }
  /** Close only this adapter's outer arguments. Nested extension objects retain native semantics. */
  inputSchema(method, reserved2 = []) {
    const names = [...new Set(reserved2)].sort(), key = canonical({ method, reserved: names });
    const cached = this.schemas.get(key);
    if (cached !== void 0) return cached;
    const input = structuredClone(this.definition(method).inputSchema);
    let root = input;
    if (typeof input === "object" && input["$ref"] !== void 0) {
      const defs = input["$defs"], reference = input["$ref"];
      requireThat(
        typeof reference === "string" && /^#\/\$defs\/[A-Za-z0-9]+$/.test(reference) && defs?.[reference.slice(8)] !== void 0,
        "native_catalog_invalid",
        "Native arguments must use their exact finite local root definition."
      );
      root = defs[reference.slice(8)];
    }
    if (typeof root === "object" && root["type"] === "object" && root["properties"] && typeof root["properties"] === "object") {
      const properties2 = root["properties"];
      for (const name of names) delete properties2[name];
      root["required"] = (root["required"] ?? []).filter((name) => !names.includes(name));
      root["additionalProperties"] = false;
    } else {
      requireThat(names.length === 0, "native_template_invalid", "Only native object arguments can reserve workflow fields.");
      const visited = /* @__PURE__ */ new Set();
      const closeAlternative = (value) => {
        if (typeof value !== "object" || visited.has(value)) return;
        visited.add(value);
        if (value["$ref"] !== void 0) {
          const reference = value["$ref"], defs = typeof input === "object" ? input["$defs"] : void 0;
          requireThat(
            typeof reference === "string" && /^#\/\$defs\/[A-Za-z0-9]+$/.test(reference) && defs?.[reference.slice(8)] !== void 0,
            "native_catalog_invalid",
            "Native argument alternatives must use finite local definitions."
          );
          closeAlternative(defs[reference.slice(8)]);
        }
        if (value["type"] === "object" && value["properties"] && typeof value["properties"] === "object") value["additionalProperties"] = false;
        for (const key2 of ["anyOf", "oneOf"]) if (Array.isArray(value[key2])) for (const child of value[key2]) closeAlternative(child);
      };
      closeAlternative(root);
    }
    const strip = (value) => {
      if (!value || typeof value !== "object") return;
      for (const name of ["description", "title", "$id", "$schema"]) delete value[name];
      const node = value;
      for (const name of ["properties", "$defs"]) if (node[name] && typeof node[name] === "object") for (const child of Object.values(node[name])) strip(child);
      for (const name of ["anyOf", "oneOf", "allOf"]) if (Array.isArray(node[name])) for (const child of node[name]) strip(child);
      for (const name of ["items", "additionalProperties", "if", "then", "else", "not"]) strip(node[name]);
    };
    strip(input);
    freeze(input);
    this.schemas.set(key, input);
    return input;
  }
  validateInput(method, params, maximumBytes2 = 1024 * 1024) {
    this.validators.validate(this.inputSchema(method), params, maximumBytes2);
  }
  validateTemplate(method, params, reserved2, maximumBytes2 = 1024 * 1024) {
    this.validators.validate(this.inputSchema(method, reserved2), params, maximumBytes2);
  }
  validateResult(method, result, maximumBytes2 = 1024 * 1024) {
    this.validators.validate(this.definition(method).outputSchema, result, maximumBytes2);
  }
};

// packages/sdk/src/native-evidence.ts
function nativeInstant(value, code = "native_evidence_mismatch") {
  const at = Date.parse(value);
  requireThat(Number.isFinite(at) && new Date(at).toISOString() === value, code, "Native evidence needs its original valid timestamp.");
  return at;
}
function validateNativeStatus(value, target, code = "native_owner_mismatch") {
  validateAgent("Status", value);
  const status = value;
  requireThat(
    status.serviceNodeId === target.serviceNodeId && (target.hostId === void 0 || status.hostId === target.hostId) && status.state === "ready" && status.epoch && status.nativeVersion === target.nativeVersion && status.nativeExecutableHash === target.nativeExecutableHash && status.catalogHash === target.catalogHash,
    code,
    "Native readiness requires the exact selected owner, executable and catalog."
  );
  nativeInstant(status.observedAt, code);
}
function validateNativeOperation(value, expected, code = "native_evidence_mismatch") {
  validateAgent("Operation", value);
  const observed = value;
  requireThat(
    observed.operationId === expected.operationId && observed.callerPrincipalId === expected.callerPrincipalId && observed.serviceNodeId === expected.serviceNodeId && observed.nativeVersion === expected.nativeVersion && observed.nativeExecutableHash === expected.nativeExecutableHash && observed.method === expected.method && canonical(observed.params) === canonical(expected.params) && observed.requestHash === hashJson({ method: expected.method, params: expected.params }),
    code,
    "Native evidence must identify the exact original caller, owner, executable, operation and request."
  );
  requireThat(nativeInstant(observed.updatedAt, code) >= nativeInstant(observed.createdAt, code), code, "An original operation cannot finish before it was created.");
}
function validateNativeRead(value, expected, code = "native_evidence_mismatch") {
  validateAgent("ReadObservation", value);
  const observed = value;
  requireThat(
    observed.serviceNodeId === expected.serviceNodeId && observed.callerPrincipalId === expected.callerPrincipalId && observed.nativeVersion === expected.nativeVersion && observed.nativeExecutableHash === expected.nativeExecutableHash && observed.catalogHash === expected.catalogHash && observed.epoch === expected.epoch && observed.method === expected.method && canonical(observed.params) === canonical(expected.params) && observed.requestHash === hashJson({ method: expected.method, params: expected.params }),
    code,
    "Native reads must identify the exact original owner, caller, catalog, epoch and request."
  );
  nativeInstant(observed.observedAt, code);
}
function validateNativeProgress(previous, next, code = "native_evidence_regressed") {
  validateAgent("Operation", previous);
  validateAgent("Operation", next);
  const identity = (value) => ({
    operationId: value.operationId,
    callerPrincipalId: value.callerPrincipalId,
    serviceNodeId: value.serviceNodeId,
    nativeVersion: value.nativeVersion,
    nativeExecutableHash: value.nativeExecutableHash,
    method: value.method,
    params: value.params,
    requestHash: value.requestHash,
    createdAt: value.createdAt
  });
  const rank = { accepted: 0, dispatched: 1, outcome_unknown: 2, succeeded: 3, failed: 3 };
  requireThat(
    canonical(identity(previous)) === canonical(identity(next)) && nativeInstant(next.updatedAt, code) >= nativeInstant(previous.updatedAt, code) && rank[next.phase] >= rank[previous.phase] && (previous.epoch === null || previous.epoch === next.epoch) && (previous.requestId === null || previous.requestId === next.requestId) && (!nativeOperationTerminal(previous) || canonical(previous) === canonical(next)),
    code,
    "The original operation cannot change identity, regress or replace its terminal receipt."
  );
}
function nativeOperationTerminal(value) {
  return value.phase === "succeeded" || value.phase === "failed";
}

// packages/sdk/src/native.ts
async function readNativeContract(client, target, options) {
  const expected = structuredClone(target);
  const management = async (name) => {
    options?.signal?.throwIfAborted();
    return serviceTools(client, expected.serviceNodeId, [{ namespace: "agent", interfaceVersion: "1.0.0" }]).call("agent." + name, {}, void 0, options);
  };
  const status = async () => {
    const value2 = await management("status");
    validateNativeStatus(value2, expected);
    return value2;
  };
  const before = await status(), value = await management("catalog");
  validateAgent("Catalog", value);
  const contract = new NativeContract(value);
  requireThat(
    contract.catalogHash === expected.catalogHash && contract.catalog.version === expected.nativeVersion && contract.catalog.nativeExecutableHash === expected.nativeExecutableHash,
    "native_catalog_mismatch",
    "The provider catalog differs from the retained native version, executable or catalog hash."
  );
  const after = await status();
  requireThat(before.epoch === after.epoch, "native_epoch_changed", "The native owner restarted during contract discovery.");
  return { target: expected, epoch: before.epoch, contract };
}

// packages/sdk/src/native-observations.ts
var record = (value) => {
  requireThat(value !== null && typeof value === "object" && !Array.isArray(value), "native_observation_shape_unsupported", "Expected the selected native object shape.");
  return value;
};
function resolve(root, value) {
  let node = record(value);
  const seen = /* @__PURE__ */ new Set();
  while (node["$ref"] !== void 0) {
    requireThat(
      !seen.has(node) && seen.size < 64 && typeof node["$ref"] === "string" && /^#\/\$defs\/[A-Za-z0-9]+$/.test(node["$ref"]),
      "native_observation_shape_unsupported",
      "Native observation shape must resolve within its original finite schema."
    );
    seen.add(node);
    node = record(record(root["$defs"])[node["$ref"].slice(8)]);
  }
  return node;
}
function properties(root, value) {
  const node = resolve(root, value);
  requireThat(node["type"] === "object", "native_observation_shape_unsupported", "Expected the selected native object schema.");
  return record(node["properties"]);
}
var layouts = /* @__PURE__ */ new WeakMap();
var layout = (contract) => {
  let value = layouts.get(contract);
  if (!value) {
    value = {};
    layouts.set(contract, value);
  }
  return value;
};
function nativeThreadState(contract, result) {
  const selected = layout(contract);
  if (selected.directInputFlag === void 0) {
    const schema = record(contract.definition("thread/read").outputSchema), thread2 = properties(schema, properties(schema, schema)["thread"]);
    selected.directInputFlag = Object.hasOwn(thread2, "canAcceptDirectInput");
  }
  const thread = record(record(result)["thread"]), state = record(thread["status"])["type"];
  requireThat(typeof state === "string", "native_observation_shape_unsupported", "Native thread status requires its actual discriminator.");
  return { thread: structuredClone(thread), state, canStartTurn: ["idle", "systemError"].includes(state) && (!selected.directInputFlag || thread["canAcceptDirectInput"] === true) };
}
function nativeTurnItems(contract, result, turnId) {
  requireThat(typeof turnId === "string" && turnId.length > 0, "native_turn_mismatch", "Native item interpretation requires the original turn ID.");
  const selected = layout(contract);
  if (!selected.items) {
    const schema = record(contract.definition("thread/items/list").outputSchema), data2 = resolve(schema, properties(schema, schema)["data"]);
    requireThat(data2["type"] === "array", "native_observation_shape_unsupported", "Native item pages require the selected data array.");
    const item = resolve(schema, data2["items"]);
    if (item["type"] === "object") {
      const fields = properties(schema, item), required = item["required"];
      requireThat(
        Object.hasOwn(fields, "item") && Object.hasOwn(fields, "turnId") && Array.isArray(required) && required.includes("item") && required.includes("turnId"),
        "native_observation_shape_unsupported",
        "Unknown native item envelope."
      );
      selected.items = "turn-wrapper";
    } else {
      const variants = item["oneOf"] ?? item["anyOf"];
      requireThat(Array.isArray(variants) && variants.length > 0 && variants.every((value) => {
        const shape = resolve(schema, value), fields = properties(schema, shape), required = shape["required"];
        return Object.hasOwn(fields, "type") && Object.hasOwn(fields, "id") && Array.isArray(required) && required.includes("type") && required.includes("id");
      }), "native_observation_shape_unsupported", "Unknown native item union.");
      selected.items = "direct";
    }
  }
  const data = record(result)["data"];
  requireThat(Array.isArray(data), "native_observation_shape_unsupported", "Native page lacks its complete data array.");
  return data.map((value) => {
    const entry = record(value);
    if (selected.items === "direct") return structuredClone(entry);
    requireThat(entry["turnId"] === turnId, "native_turn_mismatch", "Native item belongs to another turn.");
    return structuredClone(record(entry["item"]));
  });
}

// packages/sdk/src/native-owner.ts
var contracts = /* @__PURE__ */ new WeakMap();
var NativeOwner = class {
  constructor(client, caller, target, options = {}) {
    this.client = client;
    this.caller = caller;
    this.options = options;
    this.target = Object.freeze(structuredClone(target));
  }
  client;
  caller;
  options;
  target;
  checkSignal() {
    this.options.signal?.throwIfAborted();
  }
  async selectedContract() {
    this.checkSignal();
    const key = hashJson(this.target);
    let cache = contracts.get(this.client);
    if (!cache) {
      cache = /* @__PURE__ */ new Map();
      contracts.set(this.client, cache);
    }
    let pending = cache.get(key);
    if (!pending) {
      pending = (async () => {
        const hostId = this.target.hostId ?? (await this.status()).hostId;
        return (await readNativeContract(this.client, { ...this.target, hostId })).contract;
      })();
      if (cache.size >= 16) cache.delete(cache.keys().next().value);
      cache.set(key, pending);
      const selected = cache;
      void pending.catch(() => {
        if (selected.get(key) === pending) selected.delete(key);
      });
    }
    const contract = await pending;
    this.checkSignal();
    return contract;
  }
  async binding(method, expectedDefinitionHash) {
    this.checkSignal();
    const binding = await nativeServiceTools(this.client, this.target.serviceNodeId).binding("codex." + method, expectedDefinitionHash);
    this.checkSignal();
    return binding;
  }
  management(name, args, operationId2) {
    this.checkSignal();
    return serviceTools(this.client, this.target.serviceNodeId, [{ namespace: "agent", interfaceVersion: "1.0.0" }]).call("agent." + name, args, operationId2, { ...this.options, expectedCallerPrincipalId: this.caller });
  }
  async status() {
    const value = await this.management("status", {});
    validateNativeStatus(value, this.target);
    return value;
  }
  async frameLimits() {
    const value = await this.management("frameLimits", {});
    validateAgent("FrameLimits", value);
    const limits = value;
    requireThat(
      limits.serviceNodeId === this.target.serviceNodeId && limits.nativeVersion === this.target.nativeVersion && limits.nativeExecutableHash === this.target.nativeExecutableHash && limits.catalogHash === this.target.catalogHash && limits.epoch,
      "native_owner_mismatch",
      "Frame limits must come from the exact selected native owner."
    );
    return limits;
  }
  checkOperation(value, call) {
    validateNativeOperation(value, { ...this.target, callerPrincipalId: this.caller, ...call });
  }
  async operation(call) {
    const original = structuredClone(call);
    try {
      const value = await this.management("operation", { operationId: original.operationId });
      this.checkOperation(value, original);
      return value;
    } catch (error) {
      if (!(error instanceof IvyError && error.code === "not_found")) throw error;
      validateAgent("OperationAbsence", error.details);
      const absence = error.details;
      requireThat(
        absence.serviceNodeId === this.target.serviceNodeId && absence.operationId === original.operationId,
        "native_absence_mismatch",
        "Only exact owner-produced absence can identify an unobserved original operation."
      );
      return absence;
    }
  }
  async dispatch(call, guard) {
    const original = structuredClone(call);
    const binding = await this.binding(original.method, original.definitionHash);
    const invoke = await serviceTools(this.client, this.target.serviceNodeId, [{ namespace: "agent", interfaceVersion: "1.0.0" }]).binding("agent.invoke");
    await guard();
    this.checkSignal();
    try {
      return await callBound(
        this.client,
        invoke,
        {
          operationId: original.operationId,
          nativeVersion: this.target.nativeVersion,
          method: original.method,
          params: original.params,
          expectedDefinitionHash: binding.definitionHash
        },
        original.operationId,
        { ...this.options, expectedCallerPrincipalId: this.caller }
      );
    } catch (error) {
      if (!(error instanceof IvyError) || error.outcome === "not_executed") throw error;
    }
  }
  checkRead(value, method, params, epoch, allowError = false) {
    validateNativeRead(value, { ...this.target, callerPrincipalId: this.caller, epoch, method, params });
    requireThat(allowError || "result" in value.reply, "native_read_failed", "The native read returned its original error.");
  }
  /** Interactive calls never pass through the durable Hive operation store. */
  async interact(call, beforeSend = async () => {
  }) {
    const binding = await this.binding(call.method, call.definitionHash);
    const invoke = await serviceTools(this.client, this.target.serviceNodeId, [{ namespace: "agent", interfaceVersion: "1.0.0" }]).binding("agent.interact");
    await beforeSend();
    this.checkSignal();
    const value = await callBound(
      this.client,
      invoke,
      {
        operationId: call.operationId,
        nativeVersion: this.target.nativeVersion,
        method: call.method,
        params: call.params,
        expectedDefinitionHash: binding.definitionHash
      },
      call.operationId,
      { ...this.options, expectedCallerPrincipalId: this.caller }
    );
    validateAgent("Interaction", value);
    requireThat(value.operationId === call.operationId, "native_observation_invalid", "Interaction reply identity differs.");
    return value;
  }
  async interaction(operationId2) {
    const value = await this.management("interaction", { operationId: operationId2 });
    validateAgent("Interaction", value);
    requireThat(value.operationId === operationId2, "native_observation_invalid", "Interaction lookup identity differs.");
    return value;
  }
  async read(method, params, epoch, allowError = false) {
    const original = structuredClone(params), value = await this.management("read", { nativeVersion: this.target.nativeVersion, method, params: original });
    this.checkRead(value, method, original, epoch ?? value.epoch, allowError);
    return value;
  }
  async threadState(observation, epoch) {
    this.checkRead(observation, "thread/read", observation.params, epoch);
    const contract = await this.selectedContract();
    requireThat("result" in observation.reply, "native_read_failed", "Thread state needs a successful original read.");
    return nativeThreadState(contract, observation.reply.result);
  }
};

// packages/sdk/src/native-operation.ts
async function reconcileNativeOperation(options) {
  const call = structuredClone(options.call), { journal, owner } = options, previous = journal.previous ? structuredClone(journal.previous) : null;
  if (previous) {
    owner.checkOperation(previous, call);
    if (nativeOperationTerminal(previous)) return { kind: "observed", value: journal.current, observation: previous };
  }
  let observed = await owner.operation(call);
  if ("kind" in observed) {
    requireThat(
      !previous && !journal.wasObserved,
      options.absenceConflictCode ?? "native_absence_conflict",
      "Owner absence cannot erase a previously observed operation or authorize replay."
    );
    if (!options.onAbsent) return { kind: "absent", value: journal.current, absence: observed };
    const submitted = await options.onAbsent(structuredClone(call), observed);
    observed = submitted ?? await owner.operation(call);
    if ("kind" in observed) return { kind: "absent", value: journal.current, absence: observed };
  }
  owner.checkOperation(observed, call);
  if (previous) validateNativeProgress(previous, observed);
  const value = previous && previous.updatedAt === observed.updatedAt && previous.phase === observed.phase ? journal.current : await journal.retain(structuredClone(observed));
  return { kind: "observed", value, observation: observed };
}

// packages/sdk/src/assessment-policy.ts
var assessmentToolConfiguration = Object.freeze({
  "features.shell_tool": false,
  "features.apps": false,
  "features.multi_agent": false,
  web_search: "disabled"
});
function restrictAssessmentParameters(original) {
  const value = structuredClone(original);
  for (const params of [value.threadStart, value.threadResume, value.turnStart]) {
    requireThat(!("sandbox" in params) && !("sandboxPolicy" in params), "assessment_policy_invalid", "Assessment uses the named permissions profile without legacy sandbox fields.");
    Object.assign(params, { permissions: ":danger-full-access", approvalPolicy: "never" });
  }
  for (const params of [value.threadStart, value.threadResume]) {
    const config = params["config"];
    requireThat(config === void 0 || config !== null && typeof config === "object" && !Array.isArray(config), "assessment_policy_invalid", "Assessment config must be an object.");
    params["config"] = { ...config, ...assessmentToolConfiguration };
  }
  Object.assign(value.threadStart, { environments: [], selectedCapabilityRoots: [], dynamicTools: [] });
  value.turnStart["environments"] = [];
  return value;
}
function assertAssessmentPolicy(params) {
  requireThat(canonical(params) === canonical(restrictAssessmentParameters(params)), "assessment_policy_changed", "Original assessment parameters lack the configured restrictions; do not dispatch them.");
}

// packages/sdk/src/receipt-archive.ts
var ReceiptArchive = class {
  lookup;
  insert;
  usage;
  constructor(db) {
    db.exec(`CREATE TABLE IF NOT EXISTS receipt_archive(
      kind TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL,
      PRIMARY KEY(kind,key)
    ) STRICT;
      CREATE TABLE IF NOT EXISTS receipt_archive_usage(id INTEGER PRIMARY KEY CHECK(id=1),records INTEGER NOT NULL,bytes INTEGER NOT NULL) STRICT;
      INSERT OR IGNORE INTO receipt_archive_usage VALUES(1,0,0);
      CREATE TRIGGER IF NOT EXISTS receipt_archive_insert AFTER INSERT ON receipt_archive BEGIN
        UPDATE receipt_archive_usage SET records=records+1,bytes=bytes+length(CAST(NEW.value AS BLOB)) WHERE id=1;
      END;`);
    this.lookup = db.prepare("SELECT value FROM receipt_archive WHERE kind=? AND key=?");
    this.insert = db.prepare("INSERT INTO receipt_archive VALUES (?,?,?)");
    this.usage = db.prepare("SELECT records,bytes FROM receipt_archive_usage WHERE id=1");
  }
  read(kind, key) {
    const row = this.lookup.get(kind, key);
    return row ? String(row["value"]) : null;
  }
  retain(kind, key, value) {
    const prior = this.read(kind, key);
    requireThat(prior === null || prior === value, "receipt_archive_conflict", "An original receipt cannot be replaced.");
    if (prior === null) this.insert.run(kind, key, value);
  }
  status() {
    const row = this.usage.get();
    return { records: Number(row["records"]), bytes: Number(row["bytes"]) };
  }
};

// packages/sdk/src/connection-owner.ts
var owners = /* @__PURE__ */ new WeakMap();
async function connectionOwner(client, serviceNodeId) {
  if (!(client instanceof ServiceConnection) || client.serviceNodeId !== serviceNodeId) return client.request("serviceNodes.get", { serviceNodeId });
  client.signal.throwIfAborted();
  let pending = owners.get(client);
  if (!pending) {
    pending = client.request("serviceNodes.get", { serviceNodeId });
    owners.set(client, pending);
    void pending.then((node2) => {
      if (!node2.connected || !node2.synced) owners.delete(client);
    }, () => owners.delete(client));
  }
  const node = await pending;
  client.signal.throwIfAborted();
  return structuredClone(node);
}

// packages/sdk/src/native-parameters.ts
var maximumBytes = 1048576;
var validators = new SchemaValidators();
var reservedNames = (reserved2) => [...new Set(reserved2)].sort();
function header(contract, method, reserved2) {
  return {
    schemaVersion: 1,
    nativeVersion: contract.catalog.version,
    nativeExecutableHash: contract.catalog.nativeExecutableHash,
    catalogHash: contract.catalogHash,
    method,
    reservedFields: reservedNames(reserved2)
  };
}
function parametersSchema(contract, method, reserved2 = []) {
  const input = contract.inputSchema(method, reserved2), identity = header(contract, method, reserved2);
  const { $defs, ...root } = typeof input === "object" ? input : {}, params = typeof input === "boolean" ? input : root;
  const jsonSchema = {
    type: "object",
    properties: { ...Object.fromEntries(Object.entries(identity).map(([key, value]) => [key, { const: value }])), params },
    required: [...Object.keys(identity), "params"],
    additionalProperties: false,
    ...$defs === void 0 ? {} : { $defs }
  };
  validators.compile(jsonSchema);
  return jsonSchema;
}
async function saveNativeParameters(_client, contract, input, options) {
  const { method } = input, reserved2 = reservedNames(input.reserved ?? []), params = structuredClone(input.params);
  const value = { ...header(contract, method, reserved2), params };
  canonical(value, maximumBytes);
  contract.validateTemplate(method, params, reserved2, maximumBytes);
  parametersSchema(contract, method, reserved2);
  validateAgent("ParametersRef", value);
  return value;
}
async function readNativeParameters(_client, contract, input, options) {
  const { method } = input, value = structuredClone(input.ref), reserved2 = reservedNames(input.reserved ?? []), expected = header(contract, method, reserved2);
  validateAgent("ParametersRef", value);
  canonical(value, maximumBytes);
  parametersSchema(contract, method, reserved2);
  requireThat(value.nativeVersion === expected.nativeVersion && value.nativeExecutableHash === expected.nativeExecutableHash && value.catalogHash === expected.catalogHash && value.method === method && canonical(value.reservedFields) === canonical(reserved2), "native_parameters_contract_mismatch", "Native parameters belong to another selected catalog, method or template.");
  contract.validateTemplate(method, value.params, reserved2, maximumBytes);
  return structuredClone(value.params);
}

// packages/sdk/src/native-plan.ts
var methods = { threadStart: "thread/start", threadResume: "thread/resume", turnStart: "turn/start", turnInterrupt: "turn/interrupt" };
var keys = ["threadStart", "threadResume", "turnStart"];
var reserved = (key, kind) => key === "threadStart" ? [] : key === "threadResume" ? ["threadId", "history", "path"] : kind === "chat" ? ["threadId", "input", "clientUserMessageId"] : ["threadId"];
function validateNativeInvocation(contract, value, maximumBytes2 = 1048576) {
  validateAgent("InvocationDraft", value);
  const request = value;
  requireThat(
    request.nativeVersion === contract.catalog.version && request.catalogSourceHash === contract.catalog.sourceHash,
    "native_request_catalog_mismatch",
    "Native invocation requires its exact retained catalog."
  );
  contract.validateInput(request.method, request.params, maximumBytes2);
}
function validateNativePlanIdentity(contract, plan) {
  requireThat(
    plan.nativeVersion === contract.catalog.version && plan.catalogSourceHash === contract.catalog.sourceHash,
    "native_plan_catalog_mismatch",
    "The plan must retain its selected native catalog source and version."
  );
  for (const [key, method] of Object.entries(methods)) requireThat(
    plan.definitions[key] === contract.definitionHash(method),
    "native_plan_definition_mismatch",
    "The plan must retain every original native method definition."
  );
}
function validateNativePlanDraft(contract, draft, kind) {
  validateAgent("PlanDraft", draft);
  const value = draft;
  canonical(value, 1048576);
  validateNativePlanIdentity(contract, value);
  if (value.location) for (const [index, params] of [value.threadStart, value.threadResume, value.turnStart].entries())
    requireThat(
      (index !== 0 || params["cwd"] === value.location.cwd) && (params["cwd"] == null || params["cwd"] === value.location.cwd),
      "native_plan_location_mismatch",
      "Native parameters must retain the concrete selected project cwd."
    );
  for (const key of keys) contract.validateTemplate(methods[key], value[key], reserved(key, kind));
}
async function saveNativePlan(client, contract, input, options) {
  const draft = structuredClone(input.draft);
  validateNativePlanDraft(contract, draft, input.kind);
  const refs = {};
  for (const key of keys) refs[key] = await saveNativeParameters(client, contract, {
    method: methods[key],
    params: draft[key],
    reserved: reserved(key, input.kind),
    parentId: input.parentId,
    mutationId: "native-plan-" + hashJson({ identity: input.mutationId, key }).slice(7)
  }, options);
  const plan = { ...draft, ...refs };
  validateAgent("Plan", plan);
  return plan;
}
async function readNativePlan(client, contract, input, options) {
  const plan = structuredClone(input.plan);
  validateAgent("Plan", plan);
  validateNativePlanIdentity(contract, plan);
  const params = {};
  const reads = await Promise.allSettled(keys.map(async (key) => readNativeParameters(client, contract, {
    method: methods[key],
    reserved: reserved(key, input.kind),
    parentId: input.parentId,
    ref: plan[key]
  }, options)));
  for (const [index, key] of keys.entries()) {
    const read = reads[index];
    if (read.status === "rejected") throw read.reason;
    params[key] = read.value;
  }
  const draft = { ...plan, ...params };
  validateNativePlanDraft(contract, draft, input.kind);
  return draft;
}

// specs/schemas/automation.schema.json
var automation_schema_default = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://ivy.invalid/schemas/automation.schema.json",
  title: "Ivy SDK automation example contracts",
  $defs: {
    Date: {
      type: "string",
      minLength: 10,
      maxLength: 10,
      pattern: "^[0-9]{4}-[0-9]{2}-[0-9]{2}$"
    },
    LocalTime: {
      type: "string",
      minLength: 5,
      maxLength: 5,
      pattern: "^([01][0-9]|2[0-3]):[0-5][0-9]$"
    },
    Timestamp: {
      type: "string",
      minLength: 20,
      maxLength: 24,
      pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
    },
    Definition: {
      type: "object",
      properties: {
        scheduleId: {
          type: "string",
          minLength: 1,
          maxLength: 80,
          pattern: "^[a-z][a-z0-9-]*$"
        },
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        rootObjectId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        timeZone: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        firstDate: {
          $ref: "#/$defs/Date"
        },
        localTime: {
          $ref: "#/$defs/LocalTime"
        },
        catchUp: {
          enum: [
            "all",
            "latest"
          ]
        },
        subscriptionName: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        inputSource: {
          type: "string",
          minLength: 9,
          maxLength: 256,
          pattern: "^service:.+$"
        },
        inputTopic: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        initialSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        }
      },
      required: [
        "scheduleId",
        "principalId",
        "rootObjectId",
        "timeZone",
        "firstDate",
        "localTime",
        "catchUp",
        "subscriptionName",
        "inputSource",
        "inputTopic",
        "initialSequence"
      ],
      additionalProperties: false
    },
    Settings: {
      type: "object",
      properties: {
        definition: {
          $ref: "#/$defs/Definition"
        },
        maximumPeriodsPerTick: {
          type: "integer",
          minimum: 1,
          maximum: 10
        },
        pollMs: {
          type: "integer",
          minimum: 1e3,
          maximum: 6e4
        }
      },
      required: [
        "definition",
        "maximumPeriodsPerTick",
        "pollMs"
      ],
      additionalProperties: false
    },
    Pending: {
      type: "object",
      properties: {
        periodDate: {
          $ref: "#/$defs/Date"
        },
        advanceToDate: {
          $ref: "#/$defs/Date"
        },
        skippedFromDate: {
          anyOf: [
            {
              $ref: "#/$defs/Date"
            },
            {
              type: "null"
            }
          ]
        },
        skippedThroughDate: {
          anyOf: [
            {
              $ref: "#/$defs/Date"
            },
            {
              type: "null"
            }
          ]
        },
        snapshot: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-operations.schema.json#/$defs/Status"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "periodDate",
        "advanceToDate",
        "skippedFromDate",
        "skippedThroughDate",
        "snapshot"
      ],
      additionalProperties: false
    },
    Checkpoint: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        definition: {
          $ref: "#/$defs/Definition"
        },
        definitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        nextDate: {
          $ref: "#/$defs/Date"
        },
        pending: {
          anyOf: [
            {
              $ref: "#/$defs/Pending"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "schemaVersion",
        "definition",
        "definitionHash",
        "nextDate",
        "pending"
      ],
      additionalProperties: false
    },
    Period: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        definitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        periodDate: {
          $ref: "#/$defs/Date"
        },
        timeZone: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        localTime: {
          $ref: "#/$defs/LocalTime"
        },
        skippedFromDate: {
          anyOf: [
            {
              $ref: "#/$defs/Date"
            },
            {
              type: "null"
            }
          ]
        },
        skippedThroughDate: {
          anyOf: [
            {
              $ref: "#/$defs/Date"
            },
            {
              type: "null"
            }
          ]
        },
        snapshot: {
          $ref: "https://ivy.invalid/schemas/hive-operations.schema.json#/$defs/Status"
        }
      },
      required: [
        "schemaVersion",
        "definitionHash",
        "periodDate",
        "timeZone",
        "localTime",
        "skippedFromDate",
        "skippedThroughDate",
        "snapshot"
      ],
      additionalProperties: false
    },
    EventReceipt: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        definitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        sequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        topic: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        topicVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        },
        source: {
          type: "string",
          minLength: 9,
          maxLength: 256,
          pattern: "^service:.+$"
        },
        occurredAt: {
          $ref: "#/$defs/Timestamp"
        },
        sourceMutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        payloadHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "schemaVersion",
        "definitionHash",
        "sequence",
        "topic",
        "topicVersion",
        "source",
        "occurredAt",
        "sourceMutationId",
        "payloadHash"
      ],
      additionalProperties: false
    }
  }
};

// packages/contracts/src/automation-validation.ts
import { Ajv2020 as Ajv20202 } from "ajv/dist/2020.js";
var automationSchema = automation_schema_default;
var ajv2 = new Ajv20202({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [automation_schema_default, hive_operations_schema_default, host_schema_default, hive_wire_schema_default]) ajv2.addSchema(schema);
function validateAutomation(name, value) {
  canonical(value, managementFrameBytes);
  const validator = ajv2.getSchema(`${String(automationSchema["$id"])}#/$defs/${name}`);
  requireThat(validator && validator(value), "invalid_arguments", "Value does not match its automation example contract.");
}

// specs/schemas/chat.schema.json
var chat_schema_default = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://ivy.invalid/schemas/chat.schema.json",
  title: "ChatBridge WebChat, shared Main, original native delivery and acknowledged outbox",
  $defs: {
    NativePlan: {
      $ref: "https://ivy.invalid/schemas/agent.schema.json#/$defs/Plan"
    },
    NativePlanDraft: {
      $ref: "https://ivy.invalid/schemas/agent.schema.json#/$defs/PlanDraft"
    },
    NativeRequest: {
      $ref: "https://ivy.invalid/schemas/agent.schema.json#/$defs/InvocationDraft"
    },
    ObjectPin: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [
        "objectId",
        "revision"
      ],
      additionalProperties: false
    },
    Channel: {
      type: "object",
      properties: {
        adapter: {
          enum: [
            "webchat",
            "whatsapp"
          ]
        },
        accountId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        channelId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "adapter",
        "accountId",
        "channelId"
      ],
      additionalProperties: false
    },
    ChannelConfiguration: {
      type: "object",
      properties: {
        channel: {
          $ref: "#/$defs/Channel"
        },
        displayName: {
          type: "string",
          minLength: 1,
          maxLength: 256
        }
      },
      required: [
        "channel",
        "displayName"
      ],
      additionalProperties: false
    },
    Definition: {
      type: "object",
      properties: {
        workspaceId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        rootObjectId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        project: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
        },
        nativePlan: {
          $ref: "#/$defs/NativePlan"
        },
        channels: {
          type: "array",
          items: {
            $ref: "#/$defs/ChannelConfiguration"
          },
          maxItems: 32,
          minItems: 1
        }
      },
      required: [
        "workspaceId",
        "principalId",
        "rootObjectId",
        "project",
        "nativePlan",
        "channels"
      ],
      additionalProperties: false
    },
    Settings: {
      type: "object",
      properties: {
        definition: {
          $ref: "#/$defs/Definition"
        },
        pollMs: {
          type: "integer",
          minimum: 1e3,
          maximum: 6e4
        },
        whatsappConfigPath: {
          type: "string",
          minLength: 1,
          maxLength: 4096
        },
        language: {
          enum: [
            "en",
            "de"
          ]
        }
      },
      required: [
        "definition",
        "pollMs"
      ],
      additionalProperties: false
    },
    ExpectedBridge: {
      type: "object",
      properties: {
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        workspaceId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        definitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "principalId",
        "callerPrincipalId",
        "workspaceId",
        "definitionHash"
      ],
      additionalProperties: false
    },
    Artifact: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectPin"
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        byteLength: {
          type: "integer",
          minimum: 1,
          maximum: 1048576
        },
        mediaType: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        label: {
          type: "string",
          minLength: 1,
          maxLength: 256
        }
      },
      required: [
        "object",
        "contentHash",
        "byteLength",
        "mediaType",
        "label"
      ],
      additionalProperties: false
    },
    Image: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectPin"
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        byteLength: {
          type: "integer",
          minimum: 1,
          maximum: 2097152
        },
        mediaType: {
          enum: [
            "image/png",
            "image/jpeg",
            "image/webp"
          ]
        },
        label: {
          type: "string",
          minLength: 1,
          maxLength: 256
        }
      },
      required: [
        "object",
        "contentHash",
        "byteLength",
        "mediaType",
        "label"
      ],
      additionalProperties: false
    },
    Payload: {
      type: "object",
      properties: {
        text: {
          type: "string",
          minLength: 0,
          maxLength: 65536
        },
        images: {
          type: "array",
          items: {
            $ref: "#/$defs/Image"
          },
          maxItems: 2
        },
        turnOptions: {
          type: "object",
          additionalProperties: false,
          properties: {
            model: {
              anyOf: [
                {
                  type: "string",
                  minLength: 1,
                  maxLength: 256
                },
                {
                  type: "null"
                }
              ]
            },
            effort: {
              anyOf: [
                {
                  enum: [
                    "none",
                    "minimal",
                    "low",
                    "medium",
                    "high",
                    "xhigh",
                    "max",
                    "ultra"
                  ]
                },
                {
                  type: "null"
                }
              ]
            },
            serviceTier: {
              anyOf: [
                {
                  enum: [
                    "fast",
                    "flex"
                  ]
                },
                {
                  type: "null"
                }
              ]
            }
          }
        }
      },
      required: [
        "text",
        "images"
      ],
      additionalProperties: false
    },
    Error: {
      type: "object",
      properties: {
        code: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        outcome: {
          enum: [
            "not_executed",
            "unknown"
          ]
        },
        detail: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        }
      },
      required: [
        "code",
        "outcome",
        "detail"
      ],
      additionalProperties: false
    },
    InputIdentity: {
      type: "object",
      properties: {
        channel: {
          $ref: "#/$defs/Channel"
        },
        senderPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        messageId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "channel",
        "senderPrincipalId",
        "messageId"
      ],
      additionalProperties: false
    },
    CreateMainRequest: {
      type: "object",
      properties: {
        action: {
          const: "createMain"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedBridge: {
          $ref: "#/$defs/ExpectedBridge"
        },
        expectedMainRevision: {
          anyOf: [
            {
              type: "integer",
              minimum: 1,
              maximum: 9007199254740991
            },
            {
              type: "null"
            }
          ]
        },
        reason: {
          type: "string",
          minLength: 1,
          maxLength: 4096
        }
      },
      required: [
        "action",
        "operationId",
        "expectedBridge",
        "expectedMainRevision",
        "reason"
      ],
      additionalProperties: false
    },
    SendRequest: {
      type: "object",
      properties: {
        action: {
          const: "send"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedBridge: {
          $ref: "#/$defs/ExpectedBridge"
        },
        channel: {
          $ref: "#/$defs/Channel"
        },
        messageId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedBinding: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        payload: {
          $ref: "#/$defs/Payload"
        }
      },
      required: [
        "action",
        "operationId",
        "expectedBridge",
        "channel",
        "messageId",
        "expectedBinding",
        "payload"
      ],
      additionalProperties: false
    },
    CancelRequest: {
      type: "object",
      properties: {
        action: {
          const: "cancel"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedBridge: {
          $ref: "#/$defs/ExpectedBridge"
        },
        inputId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        reason: {
          type: "string",
          minLength: 1,
          maxLength: 4096
        }
      },
      required: [
        "action",
        "operationId",
        "expectedBridge",
        "inputId",
        "expectedRevision",
        "reason"
      ],
      additionalProperties: false
    },
    ReceiveRequest: {
      type: "object",
      properties: {
        action: {
          const: "receive"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedBridge: {
          $ref: "#/$defs/ExpectedBridge"
        },
        channel: {
          $ref: "#/$defs/Channel"
        },
        afterSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 8
        }
      },
      required: [
        "action",
        "operationId",
        "expectedBridge",
        "channel",
        "afterSequence",
        "limit"
      ],
      additionalProperties: false
    },
    AcknowledgeRequest: {
      type: "object",
      properties: {
        action: {
          const: "acknowledge"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedBridge: {
          $ref: "#/$defs/ExpectedBridge"
        },
        offerId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        replyIds: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
          },
          maxItems: 8,
          minItems: 1,
          uniqueItems: true
        }
      },
      required: [
        "action",
        "operationId",
        "expectedBridge",
        "offerId",
        "replyIds"
      ],
      additionalProperties: false
    },
    OperationQuery: {
      type: "object",
      properties: {
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedBridge: {
          $ref: "#/$defs/ExpectedBridge"
        }
      },
      required: [
        "operationId",
        "expectedBridge"
      ],
      additionalProperties: false
    },
    WorkspaceQuery: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    HistoryQuery: {
      type: "object",
      properties: {
        expectedBridge: {
          $ref: "#/$defs/ExpectedBridge"
        },
        channel: {
          $ref: "#/$defs/Channel"
        },
        afterSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 8
        }
      },
      required: [
        "expectedBridge",
        "channel",
        "afterSequence",
        "limit"
      ],
      additionalProperties: false
    },
    InputQuery: {
      type: "object",
      properties: {
        expectedBridge: {
          $ref: "#/$defs/ExpectedBridge"
        },
        inputId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "expectedBridge",
        "inputId"
      ],
      additionalProperties: false
    },
    NotifyRequest: {
      type: "object",
      properties: {
        action: {
          const: "notify"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedBridge: {
          $ref: "#/$defs/ExpectedBridge"
        },
        channel: {
          $ref: "#/$defs/Channel"
        },
        source: {
          $ref: "#/$defs/ObjectPin"
        },
        text: {
          type: "string",
          minLength: 1,
          maxLength: 16384
        }
      },
      required: [
        "action",
        "operationId",
        "expectedBridge",
        "channel",
        "source",
        "text"
      ],
      additionalProperties: false
    },
    NoticeQuery: {
      type: "object",
      properties: {
        expectedBridge: {
          $ref: "#/$defs/ExpectedBridge"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "expectedBridge",
        "operationId"
      ],
      additionalProperties: false
    },
    ActionRequest: {
      oneOf: [
        {
          $ref: "#/$defs/CreateMainRequest"
        },
        {
          $ref: "#/$defs/SendRequest"
        },
        {
          $ref: "#/$defs/CancelRequest"
        },
        {
          $ref: "#/$defs/ReceiveRequest"
        },
        {
          $ref: "#/$defs/AcknowledgeRequest"
        },
        {
          $ref: "#/$defs/NotifyRequest"
        }
      ]
    },
    Binding: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        workspaceId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        definitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        project: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
        },
        primary: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
        },
        nativePlan: {
          $ref: "#/$defs/NativePlan"
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        predecessor: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "schemaVersion",
        "workspaceId",
        "definitionHash",
        "project",
        "primary",
        "nativePlan",
        "createdAt",
        "operationId",
        "predecessor"
      ],
      additionalProperties: false
    },
    Ticket: {
      type: "object",
      properties: {
        inputId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        identity: {
          $ref: "#/$defs/InputIdentity"
        },
        requestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        binding: {
          $ref: "#/$defs/ObjectPin"
        },
        sequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        admittedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "inputId",
        "identity",
        "requestHash",
        "binding",
        "sequence",
        "admittedAt"
      ],
      additionalProperties: false
    },
    Conversation: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        workspaceId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        definitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "workspaceId",
        "definitionHash",
        "createdAt"
      ],
      additionalProperties: false
    },
    ResultPublication: {
      type: "object",
      properties: {
        inputId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        collection: {
          $ref: "#/$defs/ObjectPin"
        },
        result: {
          $ref: "#/$defs/ObjectPin"
        },
        firstSequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        partCount: {
          type: "integer",
          minimum: 1,
          maximum: 65536
        },
        publishedParts: {
          type: "integer",
          minimum: 0,
          maximum: 65536
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "inputId",
        "collection",
        "result",
        "firstSequence",
        "partCount",
        "publishedParts",
        "createdAt"
      ],
      additionalProperties: false
    },
    Main: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        definition: {
          $ref: "#/$defs/Definition"
        },
        definitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        conversation: {
          $ref: "#/$defs/ObjectPin"
        },
        binding: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        pendingAction: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        publication: {
          anyOf: [
            {
              $ref: "#/$defs/ResultPublication"
            },
            {
              type: "null"
            }
          ]
        },
        queue: {
          type: "array",
          items: {
            $ref: "#/$defs/Ticket"
          },
          maxItems: 64
        },
        nextSequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "definition",
        "definitionHash",
        "conversation",
        "binding",
        "pendingAction",
        "publication",
        "queue",
        "nextSequence",
        "updatedAt"
      ],
      additionalProperties: false
    },
    NativeCall: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        request: {
          $ref: "#/$defs/ObjectPin"
        },
        nativeVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        catalogSourceHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        method: {
          enum: [
            "thread/start",
            "thread/resume",
            "turn/start",
            "turn/interrupt"
          ]
        },
        predecessor: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        preparedEpoch: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        requestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        expectedDefinitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        state: {
          enum: [
            "prepared",
            "accepted",
            "dispatched",
            "succeeded",
            "failed",
            "outcome_unknown"
          ]
        },
        epoch: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        evidence: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        code: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "operationId",
        "serviceNodeId",
        "callerPrincipalId",
        "request",
        "nativeVersion",
        "catalogSourceHash",
        "method",
        "predecessor",
        "preparedEpoch",
        "requestHash",
        "expectedDefinitionHash",
        "createdAt",
        "state",
        "epoch",
        "evidence",
        "code",
        "updatedAt"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              state: {
                enum: [
                  "prepared"
                ]
              }
            }
          },
          then: {
            properties: {
              epoch: {
                type: "null"
              },
              evidence: {
                type: "null"
              },
              code: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "accepted"
                ]
              }
            }
          },
          then: {
            properties: {
              epoch: {
                type: "null"
              },
              evidence: {
                $ref: "#/$defs/ObjectPin"
              },
              code: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "dispatched",
                  "succeeded"
                ]
              }
            }
          },
          then: {
            properties: {
              epoch: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
              },
              evidence: {
                $ref: "#/$defs/ObjectPin"
              },
              code: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "failed"
                ]
              }
            }
          },
          then: {
            properties: {
              evidence: {
                $ref: "#/$defs/ObjectPin"
              },
              code: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "outcome_unknown"
                ]
              }
            }
          },
          then: {
            properties: {
              code: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
              }
            }
          }
        }
      ]
    },
    Input: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        workspaceId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        definitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        identity: {
          $ref: "#/$defs/InputIdentity"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        requestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        binding: {
          $ref: "#/$defs/ObjectPin"
        },
        sequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        payload: {
          $ref: "#/$defs/Payload"
        },
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
            "outcome_unknown"
          ]
        },
        nativeCalls: {
          type: "object",
          properties: {
            resume: {
              anyOf: [
                {
                  $ref: "#/$defs/ObjectPin"
                },
                {
                  type: "null"
                }
              ]
            },
            turn: {
              anyOf: [
                {
                  $ref: "#/$defs/ObjectPin"
                },
                {
                  type: "null"
                }
              ]
            },
            interrupt: {
              anyOf: [
                {
                  $ref: "#/$defs/ObjectPin"
                },
                {
                  type: "null"
                }
              ]
            }
          },
          required: [
            "resume",
            "turn",
            "interrupt"
          ],
          additionalProperties: false
        },
        turnId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        epoch: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        result: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        error: {
          anyOf: [
            {
              $ref: "#/$defs/Error"
            },
            {
              type: "null"
            }
          ]
        },
        cancellation: {
          anyOf: [
            {
              type: "object",
              properties: {
                operationId: {
                  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                },
                callerPrincipalId: {
                  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                },
                reason: {
                  type: "string",
                  minLength: 1,
                  maxLength: 4096
                },
                requestedAt: {
                  type: "string",
                  minLength: 24,
                  maxLength: 24,
                  pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
                }
              },
              required: [
                "operationId",
                "callerPrincipalId",
                "reason",
                "requestedAt"
              ],
              additionalProperties: false
            },
            {
              type: "null"
            }
          ]
        },
        admittedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        finishedAt: {
          anyOf: [
            {
              type: "string",
              minLength: 24,
              maxLength: 24,
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "schemaVersion",
        "workspaceId",
        "definitionHash",
        "identity",
        "operationId",
        "requestHash",
        "binding",
        "sequence",
        "payload",
        "state",
        "nativeCalls",
        "turnId",
        "epoch",
        "result",
        "error",
        "cancellation",
        "admittedAt",
        "updatedAt",
        "finishedAt"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              state: {
                enum: [
                  "queued"
                ]
              }
            }
          },
          then: {
            properties: {
              turnId: {
                type: "null"
              },
              epoch: {
                type: "null"
              },
              result: {
                type: "null"
              },
              error: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "queued",
                  "preparing",
                  "delivering",
                  "running",
                  "waiting_input",
                  "collecting",
                  "cancel_requested",
                  "outcome_unknown"
                ]
              }
            }
          },
          then: {
            properties: {
              finishedAt: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "running",
                  "waiting_input",
                  "collecting",
                  "completed"
                ]
              }
            }
          },
          then: {
            properties: {
              turnId: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
              },
              epoch: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "completed"
                ]
              }
            }
          },
          then: {
            properties: {
              result: {
                $ref: "#/$defs/ObjectPin"
              },
              finishedAt: {
                type: "string",
                minLength: 24,
                maxLength: 24,
                pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
              },
              error: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "failed",
                  "cancelled"
                ]
              }
            }
          },
          then: {
            properties: {
              finishedAt: {
                type: "string",
                minLength: 24,
                maxLength: 24,
                pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "failed"
                ]
              }
            }
          },
          then: {
            properties: {
              error: {
                $ref: "#/$defs/Error"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "cancel_requested",
                  "cancelled"
                ]
              }
            }
          },
          then: {
            properties: {
              cancellation: {
                type: "object",
                properties: {
                  operationId: {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                  },
                  callerPrincipalId: {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                  },
                  reason: {
                    type: "string",
                    minLength: 1,
                    maxLength: 4096
                  },
                  requestedAt: {
                    type: "string",
                    minLength: 24,
                    maxLength: 24,
                    pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
                  }
                },
                required: [
                  "operationId",
                  "callerPrincipalId",
                  "reason",
                  "requestedAt"
                ],
                additionalProperties: false
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "outcome_unknown"
                ]
              }
            }
          },
          then: {
            properties: {
              error: {
                type: "object",
                properties: {
                  code: {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                  },
                  outcome: {
                    const: "unknown"
                  },
                  detail: {
                    type: "string",
                    minLength: 1,
                    maxLength: 2048
                  }
                },
                required: [
                  "code",
                  "outcome",
                  "detail"
                ],
                additionalProperties: false
              }
            }
          }
        }
      ]
    },
    Evidence: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        format: {
          enum: [
            "native-request/canonical-json",
            "agent-operation/canonical-json",
            "agent-read-observation/canonical-json"
          ]
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        byteLength: {
          type: "integer",
          minimum: 1,
          maximum: 16777216
        },
        chunks: {
          type: "array",
          items: {
            $ref: "#/$defs/Artifact"
          },
          maxItems: 16,
          minItems: 1
        }
      },
      required: [
        "schemaVersion",
        "format",
        "contentHash",
        "byteLength",
        "chunks"
      ],
      additionalProperties: false
    },
    Collection: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        input: {
          $ref: "#/$defs/ObjectPin"
        },
        binding: {
          $ref: "#/$defs/ObjectPin"
        },
        turnCall: {
          $ref: "#/$defs/ObjectPin"
        },
        turnId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        epoch: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        attempt: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        previousAttempt: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        reads: {
          type: "array",
          items: {
            $ref: "#/$defs/ObjectPin"
          },
          maxItems: 1024
        },
        nativeBytes: {
          type: "integer",
          minimum: 0,
          maximum: 67108864
        },
        state: {
          enum: [
            "collecting",
            "ready",
            "complete",
            "failed"
          ]
        },
        result: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        error: {
          anyOf: [
            {
              $ref: "#/$defs/Error"
            },
            {
              type: "null"
            }
          ]
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        retryAuthorization: {
          anyOf: [
            {
              type: "object",
              properties: {
                callerPrincipalId: {
                  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                },
                operationId: {
                  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                },
                reason: {
                  type: "string",
                  minLength: 1,
                  maxLength: 4096
                },
                failed: {
                  $ref: "#/$defs/ObjectPin"
                }
              },
              required: [
                "callerPrincipalId",
                "operationId",
                "reason",
                "failed"
              ],
              additionalProperties: false
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "schemaVersion",
        "input",
        "binding",
        "turnCall",
        "turnId",
        "epoch",
        "attempt",
        "previousAttempt",
        "reads",
        "nativeBytes",
        "state",
        "result",
        "error",
        "createdAt",
        "updatedAt"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              state: {
                enum: [
                  "collecting",
                  "ready"
                ]
              }
            }
          },
          then: {
            properties: {
              result: {
                type: "null"
              },
              error: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "ready",
                  "complete"
                ]
              }
            }
          },
          then: {
            properties: {
              reads: {
                minItems: 3
              },
              nativeBytes: {
                type: "integer",
                minimum: 1,
                maximum: 67108864
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "complete"
                ]
              }
            }
          },
          then: {
            properties: {
              result: {
                $ref: "#/$defs/ObjectPin"
              },
              error: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "failed"
                ]
              }
            }
          },
          then: {
            properties: {
              result: {
                type: "null"
              },
              error: {
                $ref: "#/$defs/Error"
              }
            }
          }
        },
        {
          if: {
            properties: {
              attempt: {
                enum: [
                  1
                ]
              }
            }
          },
          then: {
            properties: {
              previousAttempt: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              attempt: {
                minimum: 2
              }
            }
          },
          then: {
            properties: {
              previousAttempt: {
                $ref: "#/$defs/ObjectPin"
              }
            }
          }
        }
      ]
    },
    RetryCollectionRequest: {
      type: "object",
      properties: {
        inputId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedCollection: {
          $ref: "#/$defs/ObjectPin"
        },
        expectedBridge: {
          $ref: "#/$defs/ExpectedBridge"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        reason: {
          type: "string",
          minLength: 1,
          maxLength: 4096
        }
      },
      required: [
        "inputId",
        "expectedCollection",
        "expectedBridge",
        "operationId",
        "reason"
      ],
      additionalProperties: false
    },
    Result: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        inputId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        binding: {
          $ref: "#/$defs/ObjectPin"
        },
        turnId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        nativeState: {
          enum: [
            "completed",
            "failed",
            "interrupted"
          ]
        },
        evidence: {
          type: "array",
          items: {
            $ref: "#/$defs/ObjectPin"
          },
          maxItems: 1024,
          minItems: 1
        },
        textParts: {
          type: "array",
          items: {
            $ref: "#/$defs/Artifact"
          },
          maxItems: 64
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "inputId",
        "binding",
        "turnId",
        "nativeState",
        "evidence",
        "textParts",
        "createdAt"
      ],
      additionalProperties: false
    },
    ReplyOrigin: {
      oneOf: [
        {
          type: "object",
          properties: {
            kind: {
              const: "native"
            },
            inputId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            binding: {
              $ref: "#/$defs/ObjectPin"
            },
            result: {
              $ref: "#/$defs/ObjectPin"
            }
          },
          required: [
            "kind",
            "inputId",
            "binding",
            "result"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            kind: {
              const: "notice"
            },
            operation: {
              $ref: "#/$defs/ObjectPin"
            },
            claim: {
              $ref: "#/$defs/ObjectPin"
            },
            producerPrincipalId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            source: {
              $ref: "#/$defs/ObjectPin"
            }
          },
          required: [
            "kind",
            "operation",
            "claim",
            "producerPrincipalId",
            "source"
          ],
          additionalProperties: false
        }
      ]
    },
    Reply: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        workspaceId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        definitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        channel: {
          $ref: "#/$defs/Channel"
        },
        origin: {
          $ref: "#/$defs/ReplyOrigin"
        },
        sequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        partIndex: {
          type: "integer",
          minimum: 0,
          maximum: 65535
        },
        text: {
          type: "string",
          minLength: 0,
          maxLength: 16384
        },
        artifacts: {
          type: "array",
          items: {
            $ref: "#/$defs/Artifact"
          },
          maxItems: 32
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        immutableHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        state: {
          enum: [
            "pending",
            "outcome_unknown",
            "confirmed"
          ]
        },
        firstOfferedAt: {
          anyOf: [
            {
              type: "string",
              minLength: 24,
              maxLength: 24,
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
            },
            {
              type: "null"
            }
          ]
        },
        confirmedAt: {
          anyOf: [
            {
              type: "string",
              minLength: 24,
              maxLength: 24,
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "schemaVersion",
        "workspaceId",
        "definitionHash",
        "channel",
        "origin",
        "sequence",
        "partIndex",
        "text",
        "artifacts",
        "createdAt",
        "immutableHash",
        "state",
        "firstOfferedAt",
        "confirmedAt"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              state: {
                enum: [
                  "pending"
                ]
              }
            }
          },
          then: {
            properties: {
              firstOfferedAt: {
                type: "null"
              },
              confirmedAt: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "outcome_unknown"
                ]
              }
            }
          },
          then: {
            properties: {
              firstOfferedAt: {
                type: "string",
                minLength: 24,
                maxLength: 24,
                pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
              },
              confirmedAt: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "confirmed"
                ]
              }
            }
          },
          then: {
            properties: {
              firstOfferedAt: {
                type: "string",
                minLength: 24,
                maxLength: 24,
                pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
              },
              confirmedAt: {
                type: "string",
                minLength: 24,
                maxLength: 24,
                pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
              }
            }
          }
        }
      ]
    },
    Offer: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        channel: {
          $ref: "#/$defs/Channel"
        },
        afterSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        throughSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        replies: {
          type: "array",
          items: {
            $ref: "#/$defs/ObjectPin"
          },
          maxItems: 8
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "operationId",
        "callerPrincipalId",
        "channel",
        "afterSequence",
        "throughSequence",
        "replies",
        "createdAt"
      ],
      additionalProperties: false
    },
    Acknowledgement: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        offer: {
          $ref: "#/$defs/ObjectPin"
        },
        replyIds: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
          },
          maxItems: 8,
          minItems: 1,
          uniqueItems: true
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "operationId",
        "callerPrincipalId",
        "offer",
        "replyIds",
        "createdAt"
      ],
      additionalProperties: false
    },
    MainView: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectPin"
        },
        data: {
          $ref: "#/$defs/Main"
        }
      },
      required: [
        "object",
        "data"
      ],
      additionalProperties: false
    },
    BindingView: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectPin"
        },
        data: {
          $ref: "#/$defs/Binding"
        }
      },
      required: [
        "object",
        "data"
      ],
      additionalProperties: false
    },
    InputView: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectPin"
        },
        data: {
          $ref: "#/$defs/Input"
        }
      },
      required: [
        "object",
        "data"
      ],
      additionalProperties: false
    },
    ReplyView: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectPin"
        },
        data: {
          $ref: "#/$defs/Reply"
        }
      },
      required: [
        "object",
        "data"
      ],
      additionalProperties: false
    },
    OfferView: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectPin"
        },
        data: {
          $ref: "#/$defs/Offer"
        }
      },
      required: [
        "object",
        "data"
      ],
      additionalProperties: false
    },
    AcknowledgementView: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectPin"
        },
        data: {
          $ref: "#/$defs/Acknowledgement"
        }
      },
      required: [
        "object",
        "data"
      ],
      additionalProperties: false
    },
    ActionOutcome: {
      oneOf: [
        {
          type: "object",
          properties: {
            action: {
              const: "createMain"
            },
            operationId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            binding: {
              $ref: "#/$defs/BindingView"
            }
          },
          required: [
            "action",
            "operationId",
            "binding"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            action: {
              const: "send"
            },
            operationId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            input: {
              $ref: "#/$defs/InputView"
            }
          },
          required: [
            "action",
            "operationId",
            "input"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            action: {
              const: "cancel"
            },
            operationId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            input: {
              $ref: "#/$defs/InputView"
            }
          },
          required: [
            "action",
            "operationId",
            "input"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            action: {
              const: "receive"
            },
            operationId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            offer: {
              $ref: "#/$defs/OfferView"
            },
            replies: {
              type: "array",
              items: {
                $ref: "#/$defs/ReplyView"
              },
              maxItems: 8
            }
          },
          required: [
            "action",
            "operationId",
            "offer",
            "replies"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            action: {
              const: "acknowledge"
            },
            operationId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            acknowledgement: {
              $ref: "#/$defs/AcknowledgementView"
            }
          },
          required: [
            "action",
            "operationId",
            "acknowledgement"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            action: {
              const: "notify"
            },
            operationId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            reply: {
              $ref: "#/$defs/ReplyView"
            }
          },
          required: [
            "action",
            "operationId",
            "reply"
          ],
          additionalProperties: false
        }
      ]
    },
    Operation: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        workspaceId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        definitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        request: {
          $ref: "#/$defs/ActionRequest"
        },
        requestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        phase: {
          enum: [
            "accepted",
            "applying",
            "succeeded",
            "failed",
            "outcome_unknown"
          ]
        },
        nativeCall: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        outcome: {
          anyOf: [
            {
              $ref: "#/$defs/ActionOutcome"
            },
            {
              type: "null"
            }
          ]
        },
        error: {
          anyOf: [
            {
              $ref: "#/$defs/Error"
            },
            {
              type: "null"
            }
          ]
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "workspaceId",
        "definitionHash",
        "callerPrincipalId",
        "operationId",
        "request",
        "requestHash",
        "phase",
        "nativeCall",
        "outcome",
        "error",
        "createdAt",
        "updatedAt"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              phase: {
                enum: [
                  "succeeded"
                ]
              }
            }
          },
          then: {
            properties: {
              outcome: {
                $ref: "#/$defs/ActionOutcome"
              },
              error: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                enum: [
                  "accepted",
                  "applying"
                ]
              }
            }
          },
          then: {
            properties: {
              outcome: {
                type: "null"
              },
              error: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                enum: [
                  "outcome_unknown"
                ]
              }
            }
          },
          then: {
            properties: {
              error: {
                type: "object",
                properties: {
                  code: {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                  },
                  outcome: {
                    const: "unknown"
                  },
                  detail: {
                    type: "string",
                    minLength: 1,
                    maxLength: 2048
                  }
                },
                required: [
                  "code",
                  "outcome",
                  "detail"
                ],
                additionalProperties: false
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                enum: [
                  "failed",
                  "outcome_unknown"
                ]
              }
            }
          },
          then: {
            properties: {
              outcome: {
                type: "null"
              },
              error: {
                $ref: "#/$defs/Error"
              }
            }
          }
        }
      ]
    },
    WorkspaceInfo: {
      type: "object",
      properties: {
        expectedBridge: {
          $ref: "#/$defs/ExpectedBridge"
        },
        project: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
        },
        main: {
          anyOf: [
            {
              $ref: "#/$defs/MainView"
            },
            {
              type: "null"
            }
          ]
        },
        binding: {
          anyOf: [
            {
              $ref: "#/$defs/BindingView"
            },
            {
              type: "null"
            }
          ]
        },
        channels: {
          type: "array",
          items: {
            $ref: "#/$defs/ChannelConfiguration"
          },
          maxItems: 32
        },
        nativeOwnerReady: {
          type: "boolean"
        },
        observedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "expectedBridge",
        "project",
        "main",
        "binding",
        "channels",
        "nativeOwnerReady",
        "observedAt"
      ],
      additionalProperties: false
    },
    HistoryPage: {
      type: "object",
      properties: {
        entries: {
          type: "array",
          items: {
            oneOf: [
              {
                $ref: "#/$defs/InputView"
              },
              {
                $ref: "#/$defs/ReplyView"
              }
            ]
          },
          maxItems: 8
        },
        throughSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        hasMore: {
          type: "boolean"
        }
      },
      required: [
        "entries",
        "throughSequence",
        "hasMore"
      ],
      additionalProperties: false
    }
  }
};

// packages/contracts/src/chat-validation.ts
import { Ajv2020 as Ajv20203 } from "ajv/dist/2020.js";
var chatSchema = chat_schema_default;
var ajv3 = new Ajv20203({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [chat_schema_default, agent_schema_default, hive_wire_schema_default]) ajv3.addSchema(schema);
function validateChat(name, value) {
  canonical(value, managementFrameBytes);
  const validator = ajv3.getSchema(`${String(chatSchema["$id"])}#/$defs/${name}`);
  requireThat(validator && validator(value), "invalid_arguments", "Value does not match its ChatBridge contract.");
}

// specs/schemas/secretary.schema.json
var secretary_schema_default = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://ivy.invalid/schemas/secretary.schema.json",
  $defs: {
    Json: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
    },
    Pin: {
      type: "object",
      properties: {
        objectId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        revision: {
          type: "integer",
          minimum: 1
        }
      },
      required: [
        "objectId",
        "revision"
      ],
      additionalProperties: false
    },
    Scope: {
      type: "object",
      properties: {
        secretaryId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        rootObjectId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        }
      },
      required: [
        "secretaryId",
        "rootObjectId"
      ],
      additionalProperties: false
    },
    Identity: {
      type: "object",
      properties: {
        serviceNodeId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        principalId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        scope: {
          $ref: "#/$defs/Scope"
        }
      },
      required: [
        "serviceNodeId",
        "hostId",
        "principalId",
        "scope"
      ],
      additionalProperties: false
    },
    Settings: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        identity: {
          $ref: "#/$defs/Identity"
        },
        pollMs: {
          type: "integer",
          minimum: 100,
          maximum: 6e4
        },
        recordsPerTick: {
          type: "integer",
          minimum: 1,
          maximum: 100
        }
      },
      required: [
        "schemaVersion",
        "identity",
        "pollMs",
        "recordsPerTick"
      ],
      additionalProperties: false
    },
    ContactWindow: {
      type: "object",
      properties: {
        timeZone: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        start: {
          type: "string",
          pattern: "^\\d{2}:\\d{2}$"
        },
        end: {
          type: "string",
          pattern: "^\\d{2}:\\d{2}$"
        }
      },
      required: [
        "timeZone",
        "start",
        "end"
      ],
      additionalProperties: false
    },
    ContactRule: {
      type: "object",
      properties: {
        enabled: {
          type: "boolean"
        },
        minimumUrgency: {
          enum: [
            "normal",
            "high",
            "critical"
          ]
        },
        window: {
          anyOf: [
            {
              $ref: "#/$defs/ContactWindow"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "enabled",
        "minimumUrgency",
        "window"
      ],
      additionalProperties: false
    },
    VoiceRule: {
      type: "object",
      properties: {
        enabled: {
          type: "boolean"
        },
        minimumUrgency: {
          const: "critical"
        },
        window: {
          anyOf: [
            {
              $ref: "#/$defs/ContactWindow"
            },
            {
              type: "null"
            }
          ]
        },
        immediateOnly: {
          type: "boolean"
        }
      },
      required: [
        "enabled",
        "minimumUrgency",
        "window",
        "immediateOnly"
      ],
      additionalProperties: false
    },
    GlobalRules: {
      type: "object",
      properties: {
        main: {
          $ref: "#/$defs/ContactRule"
        },
        voice: {
          $ref: "#/$defs/VoiceRule"
        },
        researchMaxMinutes: {
          type: "integer",
          minimum: 0,
          maximum: 60
        }
      },
      required: [
        "main",
        "voice",
        "researchMaxMinutes"
      ],
      additionalProperties: false
    },
    RuleOverrides: {
      type: "object",
      properties: {
        main: {
          type: "object",
          properties: {
            enabled: {
              type: "boolean"
            },
            minimumUrgency: {
              enum: [
                "normal",
                "high",
                "critical"
              ]
            },
            window: {
              anyOf: [
                {
                  $ref: "#/$defs/ContactWindow"
                },
                {
                  type: "null"
                }
              ]
            }
          },
          required: [],
          additionalProperties: false
        },
        voice: {
          type: "object",
          properties: {
            enabled: {
              type: "boolean"
            },
            minimumUrgency: {
              const: "critical"
            },
            window: {
              anyOf: [
                {
                  $ref: "#/$defs/ContactWindow"
                },
                {
                  type: "null"
                }
              ]
            },
            immediateOnly: {
              type: "boolean"
            }
          },
          required: [],
          additionalProperties: false
        },
        researchMaxMinutes: {
          type: "integer",
          minimum: 0,
          maximum: 60
        }
      },
      required: [],
      additionalProperties: false
    },
    ExecutionTarget: {
      type: "object",
      properties: {
        serviceNodeId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        threadCwd: {
          type: "string",
          minLength: 1,
          maxLength: 16384
        },
        model: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "null"
            }
          ]
        },
        effort: {
          enum: [
            "low",
            "medium",
            "high",
            "xhigh"
          ]
        },
        permissions: {
          type: "string",
          minLength: 1,
          maxLength: 256
        }
      },
      required: [
        "serviceNodeId",
        "threadCwd",
        "model",
        "effort",
        "permissions"
      ],
      additionalProperties: false
    },
    SecretaryConfiguration: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        rules: {
          $ref: "#/$defs/GlobalRules"
        },
        execution: {
          anyOf: [
            {
              $ref: "#/$defs/ExecutionTarget"
            },
            {
              type: "null"
            }
          ]
        },
        updatedAt: {
          type: "string",
          format: "date-time"
        }
      },
      required: [
        "schemaVersion",
        "rules",
        "execution",
        "updatedAt"
      ],
      additionalProperties: false
    },
    ScheduleTrigger: {
      type: "object",
      properties: {
        kind: {
          const: "schedule"
        },
        timeZone: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        cadence: {
          enum: [
            "interval",
            "daily",
            "weekly"
          ]
        },
        intervalMinutes: {
          anyOf: [
            {
              type: "integer",
              minimum: 1,
              maximum: 10080
            },
            {
              type: "null"
            }
          ]
        },
        localTime: {
          anyOf: [
            {
              type: "string",
              pattern: "^\\d{2}:\\d{2}$"
            },
            {
              type: "null"
            }
          ]
        },
        weekdays: {
          type: "array",
          items: {
            type: "integer",
            minimum: 0,
            maximum: 6
          },
          maxItems: 7
        }
      },
      required: [
        "kind",
        "timeZone",
        "cadence",
        "intervalMinutes",
        "localTime",
        "weekdays"
      ],
      additionalProperties: false
    },
    EventTrigger: {
      type: "object",
      properties: {
        kind: {
          const: "event"
        },
        topic: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        topicVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        },
        sourceServiceNodeId: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "null"
            }
          ]
        },
        eventKind: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "kind",
        "topic",
        "topicVersion",
        "sourceServiceNodeId",
        "eventKind"
      ],
      additionalProperties: false
    },
    AssignmentTrigger: {
      oneOf: [
        {
          $ref: "#/$defs/ScheduleTrigger"
        },
        {
          $ref: "#/$defs/EventTrigger"
        }
      ]
    },
    Assignment: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        assignmentId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        name: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        description: {
          type: "string",
          minLength: 0,
          maxLength: 2048
        },
        enabled: {
          type: "boolean"
        },
        trigger: {
          $ref: "#/$defs/AssignmentTrigger"
        },
        prompt: {
          type: "string",
          minLength: 1,
          maxLength: 131072
        },
        rules: {
          $ref: "#/$defs/RuleOverrides"
        },
        createdAt: {
          type: "string",
          format: "date-time"
        },
        updatedAt: {
          type: "string",
          format: "date-time"
        }
      },
      required: [
        "schemaVersion",
        "assignmentId",
        "name",
        "description",
        "enabled",
        "trigger",
        "prompt",
        "rules",
        "createdAt",
        "updatedAt"
      ],
      additionalProperties: false
    },
    ExecutionTrigger: {
      type: "object",
      properties: {
        kind: {
          enum: [
            "schedule",
            "event"
          ]
        },
        key: {
          type: "string",
          minLength: 1,
          maxLength: 1024
        },
        occurredAt: {
          type: "string",
          format: "date-time"
        },
        payload: {
          $ref: "#/$defs/Json"
        }
      },
      required: [
        "kind",
        "key",
        "occurredAt",
        "payload"
      ],
      additionalProperties: false
    },
    NativeTarget: {
      type: "object",
      properties: {
        serviceNodeId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        nativeVersion: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        nativeExecutableHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        catalogHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "serviceNodeId",
        "hostId",
        "nativeVersion",
        "nativeExecutableHash",
        "catalogHash"
      ],
      additionalProperties: false
    },
    NativeOperations: {
      type: "object",
      properties: {
        threadStart: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "null"
            }
          ]
        },
        turnStart: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "null"
            }
          ]
        },
        archive: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "null"
            }
          ]
        },
        delete: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "threadStart",
        "turnStart",
        "archive",
        "delete"
      ],
      additionalProperties: false
    },
    Execution: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        executionId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        assignment: {
          $ref: "#/$defs/Pin"
        },
        assignmentSnapshot: {
          $ref: "#/$defs/Assignment"
        },
        trigger: {
          $ref: "#/$defs/ExecutionTrigger"
        },
        effectiveRules: {
          $ref: "#/$defs/GlobalRules"
        },
        executionTarget: {
          $ref: "#/$defs/ExecutionTarget"
        },
        phase: {
          enum: [
            "queued",
            "starting",
            "running",
            "archiving",
            "archived",
            "deleting",
            "deleted",
            "failed"
          ]
        },
        serviceNodeId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        nativeTarget: {
          anyOf: [
            {
              $ref: "#/$defs/NativeTarget"
            },
            {
              type: "null"
            }
          ]
        },
        threadId: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "null"
            }
          ]
        },
        turnId: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "null"
            }
          ]
        },
        nativeOperations: {
          $ref: "#/$defs/NativeOperations"
        },
        result: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 262144
            },
            {
              type: "null"
            }
          ]
        },
        errorCode: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        createdAt: {
          type: "string",
          format: "date-time"
        },
        startedAt: {
          anyOf: [
            {
              type: "string",
              format: "date-time"
            },
            {
              type: "null"
            }
          ]
        },
        completedAt: {
          anyOf: [
            {
              type: "string",
              format: "date-time"
            },
            {
              type: "null"
            }
          ]
        },
        archivedAt: {
          anyOf: [
            {
              type: "string",
              format: "date-time"
            },
            {
              type: "null"
            }
          ]
        },
        deleteAfter: {
          anyOf: [
            {
              type: "string",
              format: "date-time"
            },
            {
              type: "null"
            }
          ]
        },
        deletedAt: {
          anyOf: [
            {
              type: "string",
              format: "date-time"
            },
            {
              type: "null"
            }
          ]
        },
        updatedAt: {
          type: "string",
          format: "date-time"
        }
      },
      required: [
        "schemaVersion",
        "executionId",
        "assignment",
        "assignmentSnapshot",
        "trigger",
        "effectiveRules",
        "executionTarget",
        "phase",
        "serviceNodeId",
        "nativeTarget",
        "threadId",
        "turnId",
        "nativeOperations",
        "result",
        "errorCode",
        "createdAt",
        "startedAt",
        "completedAt",
        "archivedAt",
        "deleteAfter",
        "deletedAt",
        "updatedAt"
      ],
      additionalProperties: false
    },
    Workspace: {
      type: "object",
      properties: {
        scope: {
          $ref: "#/$defs/Scope"
        },
        configuration: {
          anyOf: [
            {
              $ref: "#/$defs/Pin"
            },
            {
              type: "null"
            }
          ]
        },
        observedAt: {
          type: "string",
          format: "date-time"
        }
      },
      required: [
        "scope",
        "configuration",
        "observedAt"
      ],
      additionalProperties: false
    },
    WorkspaceRequest: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    SaveConfigurationRequest: {
      type: "object",
      properties: {
        operationId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        configuration: {
          $ref: "#/$defs/Pin"
        },
        value: {
          $ref: "#/$defs/SecretaryConfiguration"
        }
      },
      required: [
        "operationId",
        "configuration",
        "value"
      ],
      additionalProperties: false
    },
    SaveAssignmentRequest: {
      type: "object",
      properties: {
        operationId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        assignment: {
          anyOf: [
            {
              $ref: "#/$defs/Pin"
            },
            {
              type: "null"
            }
          ]
        },
        value: {
          $ref: "#/$defs/Assignment"
        }
      },
      required: [
        "operationId",
        "assignment",
        "value"
      ],
      additionalProperties: false
    }
  }
};

// packages/contracts/src/secretary-validation.ts
import { Ajv2020 as Ajv20204 } from "ajv/dist/2020.js";
var secretarySchema = secretary_schema_default;
var ajv4 = new Ajv20204({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [secretary_schema_default, hive_wire_schema_default]) ajv4.addSchema(schema);
function validateSecretary(name, value) {
  canonical(value, managementFrameBytes);
  const validator = ajv4.getSchema(`${String(secretarySchema["$id"])}#/$defs/${name}`);
  requireThat(validator && validator(value), "invalid_arguments", "Value does not match its Secretary contract.");
}

// specs/schemas/symphony.schema.json
var symphony_schema_default = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://ivy.invalid/schemas/symphony.schema.json",
  title: "Symphony durable workflow, action and native-intent contracts",
  $defs: {
    NativePlan: {
      $ref: "https://ivy.invalid/schemas/agent.schema.json#/$defs/Plan"
    },
    NativePlanDraft: {
      $ref: "https://ivy.invalid/schemas/agent.schema.json#/$defs/PlanDraft"
    },
    NativeRequest: {
      type: "object",
      properties: {
        nativeVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        catalogSourceHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        method: {
          enum: [
            "thread/start",
            "thread/resume",
            "turn/start",
            "turn/interrupt"
          ]
        },
        params: {
          $ref: "https://ivy.invalid/schemas/agent.schema.json#/$defs/ParametersRef"
        }
      },
      required: [
        "nativeVersion",
        "catalogSourceHash",
        "method",
        "params"
      ],
      additionalProperties: false
    },
    ObjectPin: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [
        "objectId",
        "revision"
      ],
      additionalProperties: false
    },
    Target: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "hostId",
        "serviceNodeId"
      ],
      additionalProperties: false
    },
    Capability: {
      type: "object",
      properties: {
        key: {
          type: "string",
          minLength: 1,
          maxLength: 64,
          pattern: "^[a-z0-9]+(?:-[a-z0-9]+)*$"
        },
        label: {
          type: "string",
          minLength: 1,
          maxLength: 128
        }
      },
      required: [
        "key",
        "label"
      ],
      additionalProperties: false
    },
    ExecutionRequirement: {
      oneOf: [
        {
          type: "object",
          properties: {
            kind: {
              const: "host"
            },
            hostId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            }
          },
          required: [
            "kind",
            "hostId"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            kind: {
              const: "capability"
            },
            capabilityKey: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            }
          },
          required: [
            "kind",
            "capabilityKey"
          ],
          additionalProperties: false
        }
      ]
    },
    NativeOptions: {
      type: "object",
      properties: {
        model: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "null"
            }
          ]
        },
        reasoningEffort: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 64
            },
            {
              type: "null"
            }
          ]
        },
        serviceTier: {
          anyOf: [
            {
              type: "string",
              enum: [
                "fast",
                "flex"
              ]
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "model",
        "reasoningEffort",
        "serviceTier"
      ],
      additionalProperties: false
    },
    WorkspaceRequirement: {
      oneOf: [
        {
          type: "object",
          properties: {
            kind: {
              const: "task_workspace"
            }
          },
          required: [
            "kind"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            kind: {
              const: "existing_project"
            },
            projectId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            path: {
              anyOf: [
                {
                  type: "string",
                  minLength: 1,
                  maxLength: 2048
                },
                {
                  type: "null"
                }
              ]
            },
            useWorktree: {
              type: "boolean"
            }
          },
          required: [
            "kind",
            "projectId",
            "path",
            "useWorktree"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            kind: {
              const: "repository_path"
            },
            repositoryUrl: {
              type: "string",
              minLength: 1,
              maxLength: 4096,
              pattern: "^https?://[^@/]+(?:/|$)"
            },
            folderName: {
              type: "string",
              minLength: 1,
              maxLength: 128,
              pattern: "^[^./\\\\][^/\\\\]*$"
            }
          },
          required: [
            "kind",
            "repositoryUrl",
            "folderName"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            kind: {
              const: "new_project_path"
            },
            folderName: {
              type: "string",
              minLength: 1,
              maxLength: 128,
              pattern: "^[^./\\\\][^/\\\\]*$"
            }
          },
          required: [
            "kind",
            "folderName"
          ],
          additionalProperties: false
        }
      ]
    },
    WorkspaceResolution: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        canonicalCwd: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        taskRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        bootstrapPath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        intendedPath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        sourcePath: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 2048
            },
            {
              type: "null"
            }
          ]
        },
        project: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
            },
            {
              type: "null"
            }
          ]
        },
        useWorktree: {
          type: "boolean"
        },
        repository: {
          anyOf: [
            {
              type: "object",
              properties: {
                name: {
                  type: "string",
                  minLength: 1,
                  maxLength: 512
                },
                branch: {
                  anyOf: [
                    {
                      type: "string",
                      minLength: 1,
                      maxLength: 512
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                commit: {
                  anyOf: [
                    {
                      type: "string",
                      minLength: 1,
                      maxLength: 128
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                dirty: {
                  type: "boolean"
                },
                originName: {
                  anyOf: [
                    {
                      type: "string",
                      minLength: 1,
                      maxLength: 128
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                originUrl: {
                  anyOf: [
                    {
                      type: "string",
                      minLength: 1,
                      maxLength: 4096
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                originState: {
                  enum: [
                    "confirmed",
                    "local_only",
                    "no_origin",
                    "unknown"
                  ]
                },
                remoteRef: {
                  anyOf: [
                    {
                      type: "string",
                      minLength: 1,
                      maxLength: 1024
                    },
                    {
                      type: "null"
                    }
                  ]
                },
                observedAt: {
                  type: "string",
                  minLength: 24,
                  maxLength: 24,
                  pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
                },
                limitation: {
                  anyOf: [
                    {
                      type: "string",
                      minLength: 1,
                      maxLength: 4096
                    },
                    {
                      type: "null"
                    }
                  ]
                }
              },
              required: [
                "name",
                "branch",
                "commit",
                "dirty",
                "originName",
                "originUrl",
                "originState",
                "remoteRef",
                "observedAt",
                "limitation"
              ],
              additionalProperties: false
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "hostId",
        "serviceNodeId",
        "canonicalCwd",
        "taskRoot",
        "bootstrapPath",
        "intendedPath",
        "sourcePath",
        "project",
        "useWorktree",
        "repository"
      ],
      additionalProperties: false
    },
    Actor: {
      type: "object",
      properties: {
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        generation: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        source: {
          enum: [
            "user",
            "worker",
            "scheduler",
            "recovery",
            "native"
          ]
        }
      },
      required: [
        "principalId",
        "serviceNodeId",
        "generation",
        "source"
      ],
      additionalProperties: false
    },
    Artifact: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectPin"
        },
        label: {
          type: "string",
          minLength: 1,
          maxLength: 512
        },
        mediaType: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        byteLength: {
          type: "integer",
          minimum: 0,
          maximum: 8388608
        }
      },
      required: [
        "object",
        "label",
        "mediaType",
        "contentHash"
      ],
      additionalProperties: false
    },
    CodeReference: {
      type: "object",
      properties: {
        kind: {
          enum: [
            "repository",
            "commit",
            "pull_request",
            "issue",
            "file"
          ]
        },
        url: {
          type: "string",
          minLength: 1,
          maxLength: 4096,
          pattern: "^https?://"
        },
        label: {
          type: "string",
          minLength: 1,
          maxLength: 512
        },
        commit: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "kind",
        "url",
        "label",
        "commit"
      ],
      additionalProperties: false
    },
    Check: {
      type: "object",
      properties: {
        name: {
          type: "string",
          minLength: 1,
          maxLength: 512
        },
        status: {
          enum: [
            "passed",
            "failed",
            "not_run",
            "blocked"
          ]
        },
        detail: {
          type: "string",
          minLength: 0,
          maxLength: 16384
        },
        evidence: {
          type: "array",
          items: {
            $ref: "#/$defs/Artifact"
          },
          maxItems: 32
        }
      },
      required: [
        "name",
        "status",
        "detail",
        "evidence"
      ],
      additionalProperties: false
    },
    RepositoryResult: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        project: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
        },
        repositoryName: {
          type: "string",
          minLength: 1,
          maxLength: 512
        },
        branch: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 512
            },
            {
              type: "null"
            }
          ]
        },
        commit: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        originName: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        originUrl: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 4096
            },
            {
              type: "null"
            }
          ]
        },
        originState: {
          enum: [
            "confirmed",
            "local_only",
            "no_origin",
            "unknown"
          ]
        },
        remoteRef: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 512
            },
            {
              type: "null"
            }
          ]
        },
        observedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        limitation: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 16384
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "hostId",
        "serviceNodeId",
        "project",
        "repositoryName",
        "branch",
        "commit",
        "originName",
        "originUrl",
        "originState",
        "remoteRef",
        "observedAt",
        "limitation"
      ],
      additionalProperties: false
    },
    ResultContent: {
      type: "object",
      properties: {
        summary: {
          type: "string",
          minLength: 1,
          maxLength: 1048576
        },
        artifacts: {
          type: "array",
          items: {
            $ref: "#/$defs/Artifact"
          },
          maxItems: 256
        },
        checks: {
          type: "array",
          items: {
            $ref: "#/$defs/Check"
          },
          maxItems: 256
        },
        codeReferences: {
          type: "array",
          items: {
            $ref: "#/$defs/CodeReference"
          },
          maxItems: 256
        },
        repositoryResult: {
          anyOf: [
            {
              $ref: "#/$defs/RepositoryResult"
            },
            {
              type: "null"
            }
          ]
        },
        limitations: {
          type: "array",
          items: {
            type: "string",
            minLength: 1,
            maxLength: 16384
          },
          maxItems: 128
        }
      },
      required: [
        "summary",
        "artifacts",
        "checks",
        "codeReferences",
        "repositoryResult",
        "limitations"
      ],
      additionalProperties: false
    },
    Waiting: {
      type: "object",
      properties: {
        reason: {
          type: "string",
          enum: [
            "user",
            "time",
            "dependency",
            "host",
            "capability",
            "workspace",
            "external_outcome",
            "publication"
          ]
        },
        detail: {
          type: "string",
          minLength: 1,
          maxLength: 16384
        },
        since: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "reason",
        "detail",
        "since"
      ],
      additionalProperties: false
    },
    ChatTarget: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedBridge: {
          $ref: "https://ivy.invalid/schemas/chat.schema.json#/$defs/ExpectedBridge"
        },
        channel: {
          $ref: "https://ivy.invalid/schemas/chat.schema.json#/$defs/Channel"
        }
      },
      required: [
        "serviceNodeId",
        "expectedBridge",
        "channel"
      ],
      additionalProperties: false
    },
    PhoneTarget: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        recipientId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "serviceNodeId",
        "recipientId"
      ],
      additionalProperties: false
    },
    TaskFields: {
      type: "object",
      properties: {
        title: {
          type: "string",
          minLength: 1,
          maxLength: 512
        },
        description: {
          type: "string",
          minLength: 0,
          maxLength: 1048576
        },
        acceptanceCriteria: {
          type: "array",
          items: {
            type: "string",
            minLength: 1,
            maxLength: 16384
          },
          maxItems: 256
        },
        category: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        control: {
          type: "string",
          enum: [
            "user",
            "agent"
          ]
        },
        priority: {
          type: "integer",
          minimum: 0,
          maximum: 4
        },
        executionRequirement: {
          anyOf: [
            {
              $ref: "#/$defs/ExecutionRequirement"
            },
            {
              type: "null"
            }
          ]
        },
        nativeOptions: {
          $ref: "#/$defs/NativeOptions"
        },
        workspaceRequirement: {
          $ref: "#/$defs/WorkspaceRequirement"
        },
        dependencies: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
          },
          maxItems: 256,
          uniqueItems: true
        },
        userContact: {
          type: "string",
          enum: [
            "ticket",
            "chat",
            "phone"
          ],
          default: "ticket"
        },
        nextReviewAt: {
          anyOf: [
            {
              type: "string",
              minLength: 24,
              maxLength: 24,
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
            },
            {
              type: "null"
            }
          ]
        },
        dueAt: {
          anyOf: [
            {
              type: "string",
              minLength: 24,
              maxLength: 24,
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "title",
        "description",
        "acceptanceCriteria",
        "category",
        "control",
        "priority",
        "executionRequirement",
        "workspaceRequirement",
        "dependencies",
        "userContact",
        "nextReviewAt",
        "dueAt"
      ],
      additionalProperties: false
    },
    TaskAttachment: {
      type: "object",
      properties: {
        attachmentId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        object: {
          $ref: "#/$defs/ObjectPin"
        },
        filename: {
          type: "string",
          minLength: 1,
          maxLength: 512
        },
        mediaType: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        byteLength: {
          type: "integer",
          minimum: 0,
          maximum: 8388608
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        uploader: {
          $ref: "#/$defs/Actor"
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "attachmentId",
        "object",
        "filename",
        "mediaType",
        "byteLength",
        "contentHash",
        "uploader",
        "createdAt"
      ],
      additionalProperties: false
    },
    ApprovalRequest: {
      type: "object",
      properties: {
        requestId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        type: {
          const: "approval"
        },
        title: {
          type: "string",
          minLength: 1,
          maxLength: 512
        },
        detail: {
          type: "string",
          minLength: 0,
          maxLength: 16384
        }
      },
      required: [
        "requestId",
        "type",
        "title",
        "detail"
      ],
      additionalProperties: false
    },
    ApprovalResponse: {
      type: "object",
      properties: {
        requestId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        type: {
          const: "approval"
        },
        decision: {
          enum: [
            "approved",
            "rejected"
          ]
        },
        detail: {
          type: "string",
          minLength: 0,
          maxLength: 16384
        }
      },
      required: [
        "requestId",
        "type",
        "decision",
        "detail"
      ],
      additionalProperties: false
    },
    CommentRequest: {
      oneOf: [
        {
          $ref: "#/$defs/ApprovalRequest"
        }
      ]
    },
    CommentResponse: {
      oneOf: [
        {
          $ref: "#/$defs/ApprovalResponse"
        }
      ]
    },
    Comment: {
      type: "object",
      properties: {
        commentId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        sequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        author: {
          $ref: "#/$defs/Actor"
        },
        authorKind: {
          enum: [
            "user",
            "agent",
            "system"
          ]
        },
        body: {
          type: "string",
          minLength: 1,
          maxLength: 65536
        },
        requests: {
          type: "array",
          items: {
            $ref: "#/$defs/CommentRequest"
          },
          maxItems: 32
        },
        responses: {
          type: "array",
          items: {
            $ref: "#/$defs/CommentResponse"
          },
          maxItems: 32
        },
        delivery: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        attachments: {
          type: "array",
          items: {
            $ref: "#/$defs/TaskAttachment"
          },
          maxItems: 32
        },
        run: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        turnId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        replyTo: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "commentId",
        "sequence",
        "author",
        "authorKind",
        "body",
        "requests",
        "responses",
        "delivery",
        "attachments",
        "run",
        "turnId",
        "replyTo",
        "createdAt",
        "operationId"
      ],
      additionalProperties: false
    },
    CommentDelivery: {
      type: "object",
      properties: {
        commentId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        state: {
          enum: [
            "queued",
            "delivering",
            "delivered",
            "outcome_unknown"
          ]
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        code: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "commentId",
        "state",
        "operationId",
        "updatedAt",
        "code"
      ],
      additionalProperties: false
    },
    PhoneDeliveryRequest: {
      type: "object",
      properties: {
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        recipientId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        route: {
          const: "voice"
        },
        voicePrompt: {
          type: "string",
          minLength: 1,
          maxLength: 4096
        }
      },
      required: [
        "operationId",
        "recipientId",
        "route",
        "voicePrompt"
      ],
      additionalProperties: false
    },
    Delivery: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        commentId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        publication: {
          $ref: "#/$defs/ObjectPin"
        },
        requestedRoute: {
          enum: [
            "chat",
            "phone"
          ]
        },
        activeRoute: {
          enum: [
            "chat",
            "phone"
          ]
        },
        state: {
          enum: [
            "pending",
            "dispatching",
            "confirmed",
            "outcome_unknown"
          ]
        },
        chatTarget: {
          anyOf: [
            {
              $ref: "#/$defs/ChatTarget"
            },
            {
              type: "null"
            }
          ]
        },
        phoneTarget: {
          anyOf: [
            {
              $ref: "#/$defs/PhoneTarget"
            },
            {
              type: "null"
            }
          ]
        },
        operationId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        chatRequest: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/chat.schema.json#/$defs/NotifyRequest"
            },
            {
              type: "null"
            }
          ]
        },
        phoneRequest: {
          anyOf: [
            {
              $ref: "#/$defs/PhoneDeliveryRequest"
            },
            {
              type: "null"
            }
          ]
        },
        phoneCallId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        deliveryEvidence: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        code: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "taskId",
        "commentId",
        "publication",
        "requestedRoute",
        "activeRoute",
        "state",
        "chatTarget",
        "phoneTarget",
        "operationId",
        "chatRequest",
        "phoneRequest",
        "phoneCallId",
        "deliveryEvidence",
        "code",
        "createdAt",
        "updatedAt"
      ],
      additionalProperties: false
    },
    TaskKeySequence: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        nextValue: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "nextValue",
        "updatedAt"
      ],
      additionalProperties: false
    },
    TaskKeyAllocation: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskKey: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          pattern: "^TASK-[0-9]{4,}$"
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "operationId",
        "taskKey",
        "createdAt"
      ],
      additionalProperties: false
    },
    CommentReadState: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        readAgentCommentCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "principalId",
        "taskId",
        "readAgentCommentCount",
        "updatedAt"
      ],
      additionalProperties: false
    },
    TaskFieldsInput: {
      type: "object",
      properties: {
        title: {
          type: "string",
          minLength: 1,
          maxLength: 512
        },
        description: {
          type: "string",
          minLength: 0,
          maxLength: 1048576
        },
        acceptanceCriteria: {
          type: "array",
          items: {
            type: "string",
            minLength: 1,
            maxLength: 16384
          },
          maxItems: 256
        },
        category: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        control: {
          type: "string",
          enum: [
            "user",
            "agent"
          ]
        },
        priority: {
          type: "integer",
          minimum: 0,
          maximum: 4
        },
        executionRequirement: {
          anyOf: [
            {
              $ref: "#/$defs/ExecutionRequirement"
            },
            {
              type: "null"
            }
          ]
        },
        nativeOptions: {
          $ref: "#/$defs/NativeOptions"
        },
        workspaceRequirement: {
          $ref: "#/$defs/WorkspaceRequirement"
        },
        dependencies: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
          },
          maxItems: 256,
          uniqueItems: true
        },
        userContact: {
          type: "string",
          enum: [
            "ticket",
            "chat",
            "phone"
          ],
          default: "ticket"
        },
        nextReviewAt: {
          anyOf: [
            {
              type: "string",
              minLength: 24,
              maxLength: 24,
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
            },
            {
              type: "null"
            }
          ]
        },
        dueAt: {
          anyOf: [
            {
              type: "string",
              minLength: 24,
              maxLength: 24,
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "title",
        "description",
        "acceptanceCriteria",
        "category",
        "control",
        "priority",
        "executionRequirement",
        "workspaceRequirement",
        "dependencies",
        "nextReviewAt",
        "dueAt"
      ],
      additionalProperties: false
    },
    ExpectedWorkspace: {
      type: "object",
      properties: {
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        rootObjectId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "principalId",
        "rootObjectId",
        "callerPrincipalId"
      ],
      additionalProperties: false
    },
    SavePlanRequest: {
      type: "object",
      properties: {
        action: {
          const: "savePlan"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        plan: {
          $ref: "#/$defs/NativePlan"
        },
        draftHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "action",
        "operationId",
        "plan",
        "draftHash"
      ],
      additionalProperties: false
    },
    SavePlanInput: {
      type: "object",
      properties: {
        action: {
          const: "savePlan"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        plan: {
          $ref: "#/$defs/NativePlanDraft"
        }
      },
      required: [
        "action",
        "operationId",
        "plan"
      ],
      additionalProperties: false
    },
    CreateRequest: {
      type: "object",
      properties: {
        action: {
          const: "create"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        fields: {
          $ref: "#/$defs/TaskFieldsInput"
        }
      },
      required: [
        "action",
        "operationId",
        "fields"
      ],
      additionalProperties: false
    },
    EditRequest: {
      type: "object",
      properties: {
        action: {
          const: "edit"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        fields: {
          $ref: "#/$defs/TaskFieldsInput"
        }
      },
      required: [
        "action",
        "operationId",
        "taskId",
        "expectedRevision",
        "fields"
      ],
      additionalProperties: false
    },
    TransitionRequest: {
      type: "object",
      properties: {
        action: {
          const: "transition"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        workflowState: {
          enum: [
            "todo",
            "in_progress",
            "waiting",
            "done",
            "cancelled"
          ]
        },
        detail: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 16384
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "action",
        "operationId",
        "taskId",
        "expectedRevision",
        "workflowState",
        "detail"
      ],
      additionalProperties: false
    },
    StartRequest: {
      type: "object",
      properties: {
        action: {
          const: "start"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        target: {
          $ref: "#/$defs/Target"
        },
        intent: {
          $ref: "#/$defs/ObjectPin"
        },
        workspace: {
          $ref: "#/$defs/WorkspaceResolution"
        },
        commentIds: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
          },
          maxItems: 256
        }
      },
      required: [
        "action",
        "operationId",
        "taskId",
        "expectedRevision",
        "target",
        "intent",
        "workspace",
        "commentIds"
      ],
      additionalProperties: false
    },
    DeferRequest: {
      type: "object",
      properties: {
        action: {
          const: "defer"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        reason: {
          enum: [
            "user",
            "time",
            "dependency",
            "host",
            "capability",
            "workspace"
          ]
        },
        detail: {
          type: "string",
          minLength: 1,
          maxLength: 16384
        },
        nextReviewAt: {
          anyOf: [
            {
              type: "string",
              minLength: 24,
              maxLength: 24,
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "action",
        "operationId",
        "taskId",
        "expectedRevision",
        "reason",
        "detail",
        "nextReviewAt"
      ],
      additionalProperties: false
    },
    ReassignRequest: {
      type: "object",
      properties: {
        action: {
          const: "reassign"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        executionRequirement: {
          anyOf: [
            {
              $ref: "#/$defs/ExecutionRequirement"
            },
            {
              type: "null"
            }
          ]
        },
        workspaceRequirement: {
          $ref: "#/$defs/WorkspaceRequirement"
        },
        reason: {
          type: "string",
          minLength: 1,
          maxLength: 16384
        },
        previousRun: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        acknowledgeUnresolved: {
          type: "boolean"
        }
      },
      required: [
        "action",
        "operationId",
        "taskId",
        "expectedRevision",
        "executionRequirement",
        "workspaceRequirement",
        "reason",
        "previousRun",
        "acknowledgeUnresolved"
      ],
      additionalProperties: false
    },
    CancelRequest: {
      type: "object",
      properties: {
        action: {
          const: "cancel"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        reason: {
          type: "string",
          minLength: 1,
          maxLength: 16384
        }
      },
      required: [
        "action",
        "operationId",
        "taskId",
        "expectedRevision",
        "reason"
      ],
      additionalProperties: false
    },
    ReviewRequest: {
      type: "object",
      properties: {
        action: {
          const: "review"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        result: {
          $ref: "#/$defs/ObjectPin"
        },
        acceptance: {
          type: "string",
          minLength: 1,
          maxLength: 65536
        }
      },
      required: [
        "action",
        "operationId",
        "taskId",
        "expectedRevision",
        "result",
        "acceptance"
      ],
      additionalProperties: false
    },
    ContinueRequest: {
      type: "object",
      properties: {
        action: {
          const: "continue"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        feedback: {
          type: "string",
          minLength: 1,
          maxLength: 65536
        },
        target: {
          $ref: "#/$defs/Target"
        },
        intent: {
          $ref: "#/$defs/ObjectPin"
        },
        workspace: {
          $ref: "#/$defs/WorkspaceResolution"
        },
        commentIds: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
          },
          maxItems: 256
        }
      },
      required: [
        "action",
        "operationId",
        "taskId",
        "expectedRevision",
        "feedback",
        "target",
        "intent",
        "workspace",
        "commentIds"
      ],
      additionalProperties: false
    },
    SaveResultRequest: {
      type: "object",
      properties: {
        action: {
          const: "saveResult"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        run: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        content: {
          $ref: "#/$defs/ResultContent"
        },
        kind: {
          enum: [
            "intermediate",
            "final"
          ]
        }
      },
      required: [
        "action",
        "operationId",
        "taskId",
        "expectedRevision",
        "run",
        "content",
        "kind"
      ],
      additionalProperties: false
    },
    CommentActionRequest: {
      type: "object",
      properties: {
        action: {
          const: "comment"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        commentId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        body: {
          type: "string",
          minLength: 1,
          maxLength: 65536
        },
        requests: {
          type: "array",
          items: {
            $ref: "#/$defs/CommentRequest"
          },
          maxItems: 32
        },
        responses: {
          type: "array",
          items: {
            $ref: "#/$defs/CommentResponse"
          },
          maxItems: 32
        },
        attachments: {
          type: "array",
          items: {
            $ref: "#/$defs/TaskAttachment"
          },
          maxItems: 32
        },
        replyTo: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "action",
        "operationId",
        "taskId",
        "expectedRevision",
        "commentId",
        "body",
        "requests",
        "responses",
        "attachments",
        "replyTo"
      ],
      additionalProperties: false
    },
    UploadAttachmentRequest: {
      type: "object",
      properties: {
        action: {
          const: "uploadAttachment"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        expectedWorkspace: {
          $ref: "#/$defs/ExpectedWorkspace"
        },
        attachmentId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        filename: {
          type: "string",
          minLength: 1,
          maxLength: 512
        },
        mediaType: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        bytesBase64: {
          type: "string",
          minLength: 1,
          maxLength: 11184812
        }
      },
      required: [
        "action",
        "operationId",
        "taskId",
        "expectedRevision",
        "attachmentId",
        "filename",
        "mediaType",
        "bytesBase64"
      ],
      additionalProperties: false
    },
    OperationQuery: {
      type: "object",
      properties: {
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "operationId"
      ],
      additionalProperties: false
    },
    CategoriesQuery: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    CategoriesResult: {
      type: "object",
      properties: {
        categories: {
          type: "array",
          items: {
            type: "string",
            minLength: 1,
            maxLength: 128
          },
          maxItems: 256
        }
      },
      required: [
        "categories"
      ],
      additionalProperties: false
    },
    TaskQuery: {
      type: "object",
      properties: {
        task: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "object",
              properties: {
                taskKey: {
                  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                }
              },
              required: [
                "taskKey"
              ],
              additionalProperties: false
            }
          ]
        }
      },
      required: [
        "task"
      ],
      additionalProperties: false
    },
    MarkCommentsReadRequest: {
      type: "object",
      properties: {
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedTaskRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [
        "operationId",
        "taskId",
        "expectedTaskRevision"
      ],
      additionalProperties: false
    },
    MarkCommentsReadResult: {
      type: "object",
      properties: {
        task: {
          $ref: "#/$defs/ObjectPin"
        },
        readState: {
          $ref: "#/$defs/ObjectPin"
        },
        unreadAgentComments: {
          const: 0
        }
      },
      required: [
        "task",
        "readState",
        "unreadAgentComments"
      ],
      additionalProperties: false
    },
    ExecutionCondition: {
      type: "object",
      properties: {
        state: {
          enum: [
            "idle",
            "queued",
            "allocating",
            "starting",
            "active",
            "needs_user",
            "deferred",
            "blocked_dependency",
            "blocked_environment",
            "recovering",
            "outcome_unknown",
            "publishing"
          ]
        },
        detail: {
          type: "string",
          minLength: 0,
          maxLength: 2048
        }
      },
      required: [
        "state",
        "detail"
      ],
      additionalProperties: false
    },
    TaskView: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectPin"
        },
        task: {
          $ref: "#/$defs/Task"
        },
        executionCondition: {
          $ref: "#/$defs/ExecutionCondition"
        },
        unreadAgentComments: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        }
      },
      required: [
        "object",
        "task",
        "executionCondition",
        "unreadAgentComments"
      ],
      additionalProperties: false
    },
    ExecutionRequest: {
      oneOf: [
        {
          $ref: "#/$defs/StartRequest"
        },
        {
          $ref: "#/$defs/ContinueRequest"
        }
      ]
    },
    ActionRequest: {
      oneOf: [
        {
          $ref: "#/$defs/SavePlanRequest"
        },
        {
          $ref: "#/$defs/CreateRequest"
        },
        {
          $ref: "#/$defs/EditRequest"
        },
        {
          $ref: "#/$defs/TransitionRequest"
        },
        {
          $ref: "#/$defs/DeferRequest"
        },
        {
          $ref: "#/$defs/ReassignRequest"
        },
        {
          $ref: "#/$defs/CancelRequest"
        },
        {
          $ref: "#/$defs/ReviewRequest"
        },
        {
          $ref: "#/$defs/SaveResultRequest"
        },
        {
          $ref: "#/$defs/CommentActionRequest"
        },
        {
          $ref: "#/$defs/UploadAttachmentRequest"
        }
      ]
    },
    ActionInput: {
      oneOf: [
        {
          $ref: "#/$defs/SavePlanInput"
        },
        {
          $ref: "#/$defs/CreateRequest"
        },
        {
          $ref: "#/$defs/EditRequest"
        },
        {
          $ref: "#/$defs/TransitionRequest"
        },
        {
          $ref: "#/$defs/DeferRequest"
        },
        {
          $ref: "#/$defs/ReassignRequest"
        },
        {
          $ref: "#/$defs/CancelRequest"
        },
        {
          $ref: "#/$defs/ReviewRequest"
        },
        {
          $ref: "#/$defs/SaveResultRequest"
        },
        {
          $ref: "#/$defs/CommentActionRequest"
        },
        {
          $ref: "#/$defs/UploadAttachmentRequest"
        }
      ]
    },
    Claim: {
      type: "object",
      properties: {
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        attempt: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        owner: {
          $ref: "#/$defs/Actor"
        },
        request: {
          $ref: "#/$defs/ExecutionRequest"
        },
        requestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        target: {
          $ref: "#/$defs/Target"
        },
        workspace: {
          $ref: "#/$defs/WorkspaceResolution"
        },
        primaryAtStart: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
            },
            {
              type: "null"
            }
          ]
        },
        claimedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        phase: {
          enum: [
            "claimed",
            "run_created",
            "starting",
            "running",
            "waiting_input",
            "publishing_result",
            "cancel_requested",
            "outcome_unknown"
          ]
        },
        run: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "operationId",
        "callerPrincipalId",
        "attempt",
        "owner",
        "request",
        "requestHash",
        "target",
        "workspace",
        "primaryAtStart",
        "claimedAt",
        "phase",
        "run"
      ],
      additionalProperties: false
    },
    Task: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        taskKey: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          pattern: "^TASK-[0-9]{4,}$"
        },
        fields: {
          $ref: "#/$defs/TaskFields"
        },
        workflowState: {
          type: "string",
          enum: [
            "backlog",
            "todo",
            "in_progress",
            "waiting",
            "review",
            "done",
            "cancelled"
          ]
        },
        waiting: {
          anyOf: [
            {
              $ref: "#/$defs/Waiting"
            },
            {
              type: "null"
            }
          ]
        },
        publication: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        primaryResourceRef: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
            },
            {
              type: "null"
            }
          ]
        },
        claim: {
          anyOf: [
            {
              $ref: "#/$defs/Claim"
            },
            {
              type: "null"
            }
          ]
        },
        attemptCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        lastRun: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        lastHistory: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        latestResult: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        acceptedReview: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        comments: {
          type: "array",
          items: {
            $ref: "#/$defs/Comment"
          },
          maxItems: 512
        },
        commentDeliveries: {
          type: "array",
          items: {
            $ref: "#/$defs/CommentDelivery"
          },
          maxItems: 512
        },
        agentCommentCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        workRevision: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        attachments: {
          type: "array",
          items: {
            $ref: "#/$defs/TaskAttachment"
          },
          maxItems: 256
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "taskKey",
        "fields",
        "workflowState",
        "waiting",
        "publication",
        "primaryResourceRef",
        "claim",
        "attemptCount",
        "lastRun",
        "lastHistory",
        "latestResult",
        "acceptedReview",
        "comments",
        "commentDeliveries",
        "agentCommentCount",
        "workRevision",
        "attachments",
        "createdAt",
        "updatedAt"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              workflowState: {
                const: "waiting"
              }
            }
          },
          then: {
            properties: {
              waiting: {
                $ref: "#/$defs/Waiting"
              }
            }
          }
        },
        {
          if: {
            properties: {
              workflowState: {
                enum: [
                  "backlog",
                  "in_progress",
                  "review",
                  "done",
                  "cancelled"
                ]
              }
            }
          },
          then: {
            properties: {
              waiting: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              workflowState: {
                const: "in_progress"
              },
              fields: {
                properties: {
                  control: {
                    const: "agent"
                  }
                }
              }
            }
          },
          then: {
            properties: {
              claim: {
                $ref: "#/$defs/Claim"
              }
            }
          }
        },
        {
          if: {
            properties: {
              workflowState: {
                enum: [
                  "backlog",
                  "todo",
                  "review",
                  "done",
                  "cancelled"
                ]
              }
            }
          },
          then: {
            properties: {
              claim: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              workflowState: {
                const: "review"
              }
            }
          },
          then: {
            properties: {
              latestResult: {
                $ref: "#/$defs/ObjectPin"
              }
            }
          }
        },
        {
          if: {
            properties: {
              workflowState: {
                const: "done"
              },
              fields: {
                properties: {
                  control: {
                    const: "agent"
                  }
                }
              }
            }
          },
          then: {
            properties: {
              latestResult: {
                $ref: "#/$defs/ObjectPin"
              },
              acceptedReview: {
                $ref: "#/$defs/ObjectPin"
              }
            }
          }
        }
      ]
    },
    NativeCallOrigin: {
      oneOf: [
        {
          type: "object",
          properties: {
            kind: {
              const: "initial"
            }
          },
          required: [
            "kind"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            kind: {
              const: "reattach"
            },
            predecessor: {
              $ref: "#/$defs/ObjectPin"
            },
            preparedEpoch: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            }
          },
          required: [
            "kind",
            "predecessor",
            "preparedEpoch"
          ],
          additionalProperties: false
        }
      ]
    },
    NativeCall: {
      type: "object",
      properties: {
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        request: {
          $ref: "#/$defs/NativeRequest"
        },
        expectedDefinitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        origin: {
          $ref: "#/$defs/NativeCallOrigin"
        },
        state: {
          enum: [
            "prepared",
            "observed",
            "outcome_unknown"
          ]
        },
        preparedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        observedAt: {
          anyOf: [
            {
              type: "string",
              minLength: 24,
              maxLength: 24,
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
            },
            {
              type: "null"
            }
          ]
        },
        ownerPhase: {
          anyOf: [
            {
              enum: [
                "accepted",
                "dispatched",
                "succeeded",
                "failed",
                "outcome_unknown"
              ]
            },
            {
              type: "null"
            }
          ]
        },
        epoch: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        evidence: {
          anyOf: [
            {
              $ref: "#/$defs/Artifact"
            },
            {
              type: "null"
            }
          ]
        },
        code: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "operationId",
        "request",
        "expectedDefinitionHash",
        "state",
        "preparedAt",
        "observedAt",
        "ownerPhase",
        "epoch",
        "evidence",
        "code"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              state: {
                const: "prepared"
              }
            }
          },
          then: {
            properties: {
              observedAt: {
                type: "null"
              },
              ownerPhase: {
                type: "null"
              },
              epoch: {
                type: "null"
              },
              evidence: {
                type: "null"
              },
              code: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                const: "observed"
              }
            }
          },
          then: {
            properties: {
              observedAt: {
                type: "string",
                minLength: 24,
                maxLength: 24,
                pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
              },
              ownerPhase: {
                enum: [
                  "accepted",
                  "dispatched",
                  "succeeded",
                  "failed",
                  "outcome_unknown"
                ]
              },
              evidence: {
                $ref: "#/$defs/Artifact"
              }
            }
          }
        }
      ]
    },
    Run: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        attempt: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        originalRequest: {
          $ref: "#/$defs/ExecutionRequest"
        },
        requestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        target: {
          $ref: "#/$defs/Target"
        },
        workspace: {
          $ref: "#/$defs/WorkspaceResolution"
        },
        plan: {
          $ref: "#/$defs/NativePlanDraft"
        },
        owner: {
          $ref: "#/$defs/Actor"
        },
        phase: {
          type: "string",
          enum: [
            "starting",
            "running",
            "waiting_input",
            "publishing_result",
            "completed",
            "failed",
            "cancel_requested",
            "cancelled",
            "outcome_unknown"
          ]
        },
        primaryResourceRef: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
            },
            {
              type: "null"
            }
          ]
        },
        turnId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        nativeEpoch: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        calls: {
          type: "object",
          properties: {
            thread: {
              anyOf: [
                {
                  $ref: "#/$defs/ObjectPin"
                },
                {
                  type: "null"
                }
              ]
            },
            turn: {
              anyOf: [
                {
                  $ref: "#/$defs/ObjectPin"
                },
                {
                  type: "null"
                }
              ]
            },
            interrupt: {
              anyOf: [
                {
                  $ref: "#/$defs/ObjectPin"
                },
                {
                  type: "null"
                }
              ]
            }
          },
          required: [
            "thread",
            "turn",
            "interrupt"
          ],
          additionalProperties: false
        },
        notificationCursor: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        result: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        cancellation: {
          anyOf: [
            {
              type: "object",
              properties: {
                operationId: {
                  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                },
                reason: {
                  type: "string",
                  minLength: 1,
                  maxLength: 16384
                },
                requestedAt: {
                  type: "string",
                  minLength: 24,
                  maxLength: 24,
                  pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
                },
                actor: {
                  $ref: "#/$defs/Actor"
                }
              },
              required: [
                "operationId",
                "reason",
                "requestedAt",
                "actor"
              ],
              additionalProperties: false
            },
            {
              type: "null"
            }
          ]
        },
        externalOutcome: {
          type: "object",
          properties: {
            state: {
              enum: [
                "not_started",
                "active",
                "succeeded",
                "failed",
                "cancelled",
                "unknown"
              ]
            },
            observedAt: {
              anyOf: [
                {
                  type: "string",
                  minLength: 24,
                  maxLength: 24,
                  pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
                },
                {
                  type: "null"
                }
              ]
            },
            evidence: {
              anyOf: [
                {
                  $ref: "#/$defs/Artifact"
                },
                {
                  type: "null"
                }
              ]
            },
            code: {
              anyOf: [
                {
                  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                },
                {
                  type: "null"
                }
              ]
            }
          },
          required: [
            "state",
            "observedAt",
            "evidence",
            "code"
          ],
          additionalProperties: false
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        finishedAt: {
          anyOf: [
            {
              type: "string",
              minLength: 24,
              maxLength: 24,
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "schemaVersion",
        "taskId",
        "attempt",
        "operationId",
        "callerPrincipalId",
        "originalRequest",
        "requestHash",
        "target",
        "workspace",
        "plan",
        "owner",
        "phase",
        "primaryResourceRef",
        "turnId",
        "nativeEpoch",
        "calls",
        "notificationCursor",
        "result",
        "cancellation",
        "externalOutcome",
        "createdAt",
        "updatedAt",
        "finishedAt"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              phase: {
                enum: [
                  "running",
                  "waiting_input",
                  "publishing_result",
                  "completed"
                ]
              }
            }
          },
          then: {
            properties: {
              primaryResourceRef: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
              },
              turnId: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
              },
              nativeEpoch: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                const: "completed"
              }
            }
          },
          then: {
            properties: {
              result: {
                $ref: "#/$defs/ObjectPin"
              },
              finishedAt: {
                type: "string",
                minLength: 24,
                maxLength: 24,
                pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                const: "cancel_requested"
              }
            }
          },
          then: {
            properties: {
              cancellation: {
                type: "object",
                properties: {
                  operationId: {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                  },
                  reason: {
                    type: "string",
                    minLength: 1,
                    maxLength: 16384
                  },
                  requestedAt: {
                    type: "string",
                    minLength: 24,
                    maxLength: 24,
                    pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
                  },
                  actor: {
                    $ref: "#/$defs/Actor"
                  }
                },
                required: [
                  "operationId",
                  "reason",
                  "requestedAt",
                  "actor"
                ],
                additionalProperties: false
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                const: "outcome_unknown"
              }
            }
          },
          then: {
            properties: {
              externalOutcome: {
                properties: {
                  state: {
                    const: "unknown"
                  }
                }
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                const: "cancelled"
              }
            }
          },
          then: {
            properties: {
              cancellation: {
                type: "object",
                properties: {
                  operationId: {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                  },
                  reason: {
                    type: "string",
                    minLength: 1,
                    maxLength: 16384
                  },
                  requestedAt: {
                    type: "string",
                    minLength: 24,
                    maxLength: 24,
                    pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
                  },
                  actor: {
                    $ref: "#/$defs/Actor"
                  }
                },
                required: [
                  "operationId",
                  "reason",
                  "requestedAt",
                  "actor"
                ],
                additionalProperties: false
              },
              finishedAt: {
                type: "string",
                minLength: 24,
                maxLength: 24,
                pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
              },
              externalOutcome: {
                properties: {
                  state: {
                    const: "cancelled"
                  }
                }
              }
            }
          }
        }
      ]
    },
    Result: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        run: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        kind: {
          enum: [
            "intermediate",
            "final"
          ]
        },
        content: {
          $ref: "#/$defs/ResultContent"
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        actor: {
          $ref: "#/$defs/Actor"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "schemaVersion",
        "taskId",
        "run",
        "kind",
        "content",
        "createdAt",
        "actor",
        "operationId"
      ],
      additionalProperties: false
    },
    Review: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        result: {
          $ref: "#/$defs/ObjectPin"
        },
        acceptance: {
          type: "string",
          minLength: 1,
          maxLength: 65536
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        actor: {
          $ref: "#/$defs/Actor"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "schemaVersion",
        "taskId",
        "result",
        "acceptance",
        "createdAt",
        "actor",
        "operationId"
      ],
      additionalProperties: false
    },
    History: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        actor: {
          $ref: "#/$defs/Actor"
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        kind: {
          enum: [
            "created",
            "edited",
            "ready",
            "started",
            "deferred",
            "reassigned",
            "cancel_requested",
            "cancelled",
            "reviewed",
            "continued",
            "result_saved",
            "commented",
            "recovered",
            "outcome_unknown"
          ]
        },
        detail: {
          type: "string",
          minLength: 0,
          maxLength: 65536
        },
        previousTask: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        previousHistory: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        fromTarget: {
          anyOf: [
            {
              $ref: "#/$defs/Target"
            },
            {
              type: "null"
            }
          ]
        },
        toTarget: {
          anyOf: [
            {
              $ref: "#/$defs/Target"
            },
            {
              type: "null"
            }
          ]
        },
        priorRun: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        result: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        review: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "schemaVersion",
        "taskId",
        "operationId",
        "actor",
        "createdAt",
        "kind",
        "detail",
        "previousTask",
        "previousHistory",
        "fromTarget",
        "toTarget",
        "priorRun",
        "result",
        "review"
      ],
      additionalProperties: false
    },
    ActionOutcome: {
      type: "object",
      properties: {
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        task: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        plan: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        run: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        history: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        result: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        review: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        attachment: {
          anyOf: [
            {
              $ref: "#/$defs/TaskAttachment"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "operationId",
        "task",
        "plan",
        "run",
        "history",
        "result",
        "review",
        "attachment"
      ],
      additionalProperties: false
    },
    NativeUpdateRequest: {
      type: "object",
      properties: {
        action: {
          const: "nativeUpdate"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        taskId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        run: {
          $ref: "#/$defs/ObjectPin"
        },
        change: {
          oneOf: [
            {
              type: "object",
              properties: {
                kind: {
                  const: "attachCall"
                },
                slot: {
                  enum: [
                    "thread",
                    "turn",
                    "interrupt"
                  ]
                },
                call: {
                  $ref: "#/$defs/ObjectPin"
                }
              },
              required: [
                "kind",
                "slot",
                "call"
              ],
              additionalProperties: false
            },
            {
              type: "object",
              properties: {
                kind: {
                  const: "reattachCall"
                },
                call: {
                  $ref: "#/$defs/ObjectPin"
                }
              },
              required: [
                "kind",
                "call"
              ],
              additionalProperties: false
            },
            {
              type: "object",
              properties: {
                kind: {
                  const: "observeCall"
                },
                slot: {
                  enum: [
                    "thread",
                    "turn",
                    "interrupt"
                  ]
                },
                call: {
                  $ref: "#/$defs/ObjectPin"
                }
              },
              required: [
                "kind",
                "slot",
                "call"
              ],
              additionalProperties: false
            },
            {
              type: "object",
              properties: {
                kind: {
                  const: "snapshot"
                },
                evidence: {
                  $ref: "#/$defs/Artifact"
                },
                nativeOperationId: {
                  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                },
                notificationCursor: {
                  type: "integer",
                  minimum: 0,
                  maximum: 9007199254740991
                }
              },
              required: [
                "kind",
                "evidence",
                "nativeOperationId",
                "notificationCursor"
              ],
              additionalProperties: false
            },
            {
              type: "object",
              properties: {
                kind: {
                  const: "readSnapshot"
                },
                snapshot: {
                  $ref: "#/$defs/Artifact"
                },
                notificationCursor: {
                  type: "integer",
                  minimum: 0,
                  maximum: 9007199254740991
                }
              },
              required: [
                "kind",
                "snapshot",
                "notificationCursor"
              ],
              additionalProperties: false
            },
            {
              type: "object",
              properties: {
                kind: {
                  const: "transcript"
                },
                evidence: {
                  $ref: "#/$defs/Artifact"
                },
                notificationCursor: {
                  type: "integer",
                  minimum: 0,
                  maximum: 9007199254740991
                }
              },
              required: [
                "kind",
                "evidence",
                "notificationCursor"
              ],
              additionalProperties: false
            },
            {
              type: "object",
              properties: {
                kind: {
                  const: "unavailable"
                },
                reason: {
                  enum: [
                    "host",
                    "external_outcome"
                  ]
                },
                code: {
                  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                },
                detail: {
                  type: "string",
                  minLength: 1,
                  maxLength: 16384
                }
              },
              required: [
                "kind",
                "reason",
                "code",
                "detail"
              ],
              additionalProperties: false
            },
            {
              type: "object",
              properties: {
                kind: {
                  const: "cancelUnstarted"
                }
              },
              required: [
                "kind"
              ],
              additionalProperties: false
            },
            {
              type: "object",
              properties: {
                kind: {
                  const: "retireUnstarted"
                }
              },
              required: [
                "kind"
              ],
              additionalProperties: false
            }
          ]
        }
      },
      required: [
        "action",
        "operationId",
        "taskId",
        "expectedRevision",
        "run",
        "change"
      ],
      additionalProperties: false
    },
    DurableRequest: {
      oneOf: [
        {
          $ref: "#/$defs/ActionRequest"
        },
        {
          $ref: "#/$defs/SchedulerRequest"
        },
        {
          $ref: "#/$defs/NativeUpdateRequest"
        }
      ]
    },
    PreparedWrite: {
      type: "object",
      properties: {
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        contractKey: {
          enum: [
            "symphony/task",
            "symphony/run",
            "symphony/result",
            "symphony/review",
            "symphony/history",
            "symphony/delivery",
            "symphony/native-plan",
            "symphony/native-call"
          ]
        },
        contractVersion: {
          enum: [
            "1.0.0",
            "1.1.0"
          ]
        },
        payload: {
          $ref: "#/$defs/Artifact"
        },
        destination: {
          oneOf: [
            {
              type: "object",
              properties: {
                create: {
                  type: "object",
                  properties: {
                    parentId: {
                      anyOf: [
                        {
                          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                        },
                        {
                          type: "null"
                        }
                      ]
                    },
                    name: {
                      type: "string",
                      minLength: 1,
                      maxLength: 256
                    }
                  },
                  required: [
                    "parentId",
                    "name"
                  ],
                  additionalProperties: false
                }
              },
              required: [
                "create"
              ],
              additionalProperties: false
            },
            {
              type: "object",
              properties: {
                objectId: {
                  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                },
                expectedRevision: {
                  type: "integer",
                  minimum: 1,
                  maximum: 9007199254740991
                }
              },
              required: [
                "objectId",
                "expectedRevision"
              ],
              additionalProperties: false
            }
          ]
        },
        outcome: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "mutationId",
        "contractKey",
        "contractVersion",
        "payload",
        "destination",
        "outcome"
      ],
      additionalProperties: false
    },
    Operation: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        request: {
          $ref: "#/$defs/DurableRequest"
        },
        requestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        actor: {
          $ref: "#/$defs/Actor"
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        phase: {
          type: "string",
          enum: [
            "accepted",
            "preparing",
            "publishing",
            "succeeded",
            "failed",
            "needs_attention"
          ]
        },
        writes: {
          type: "array",
          items: {
            $ref: "#/$defs/PreparedWrite"
          },
          maxItems: 16
        },
        outcome: {
          anyOf: [
            {
              $ref: "#/$defs/ActionOutcome"
            },
            {
              type: "null"
            }
          ]
        },
        error: {
          anyOf: [
            {
              type: "object",
              properties: {
                code: {
                  $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                },
                message: {
                  type: "string",
                  minLength: 1,
                  maxLength: 16384
                },
                execution: {
                  enum: [
                    "not_executed",
                    "completed",
                    "unknown"
                  ]
                }
              },
              required: [
                "code",
                "message",
                "execution"
              ],
              additionalProperties: false
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "schemaVersion",
        "operationId",
        "callerPrincipalId",
        "request",
        "requestHash",
        "actor",
        "createdAt",
        "updatedAt",
        "phase",
        "writes",
        "outcome",
        "error"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              phase: {
                const: "succeeded"
              }
            }
          },
          then: {
            properties: {
              outcome: {
                $ref: "#/$defs/ActionOutcome"
              },
              error: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                const: "failed"
              }
            }
          },
          then: {
            properties: {
              outcome: {
                type: "null"
              },
              error: {
                type: "object",
                properties: {
                  code: {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                  },
                  message: {
                    type: "string",
                    minLength: 1,
                    maxLength: 16384
                  },
                  execution: {
                    enum: [
                      "not_executed",
                      "completed",
                      "unknown"
                    ]
                  }
                },
                required: [
                  "code",
                  "message",
                  "execution"
                ],
                additionalProperties: false
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                const: "needs_attention"
              }
            }
          },
          then: {
            properties: {
              error: {
                type: "object",
                properties: {
                  code: {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                  },
                  message: {
                    type: "string",
                    minLength: 1,
                    maxLength: 16384
                  },
                  execution: {
                    enum: [
                      "not_executed",
                      "completed",
                      "unknown"
                    ]
                  }
                },
                required: [
                  "code",
                  "message",
                  "execution"
                ],
                additionalProperties: false
              }
            }
          }
        },
        {
          if: {
            properties: {
              request: {
                properties: {
                  action: {
                    const: "nativeUpdate"
                  }
                }
              }
            }
          },
          then: {
            properties: {
              actor: {
                properties: {
                  source: {
                    const: "native"
                  }
                }
              }
            }
          }
        }
      ]
    },
    Settings: {
      type: "object",
      properties: {
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        rootObjectId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        scheduler: {
          type: "object",
          properties: {
            enabled: {
              type: "boolean"
            },
            intervalMs: {
              type: "integer",
              minimum: 1e3,
              maximum: 36e5
            },
            pageSize: {
              type: "integer",
              minimum: 1,
              maximum: 50
            }
          },
          required: [
            "enabled",
            "intervalMs",
            "pageSize"
          ],
          additionalProperties: false
        },
        chatTarget: {
          anyOf: [
            {
              $ref: "#/$defs/ChatTarget"
            },
            {
              type: "null"
            }
          ]
        },
        phoneTarget: {
          anyOf: [
            {
              $ref: "#/$defs/PhoneTarget"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "principalId",
        "rootObjectId",
        "scheduler",
        "chatTarget",
        "phoneTarget"
      ],
      additionalProperties: false
    },
    WorkspaceQuery: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    WorkspaceInfo: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        generation: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        rootObjectId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        role: {
          enum: [
            "user",
            "worker",
            "observer"
          ]
        },
        recovered: {
          type: "boolean"
        },
        observedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        scheduler: {
          type: "object",
          properties: {
            enabled: {
              type: "boolean"
            },
            intervalMs: {
              type: "integer",
              minimum: 1e3,
              maximum: 36e5
            },
            pageSize: {
              type: "integer",
              minimum: 1,
              maximum: 50
            }
          },
          required: [
            "enabled",
            "intervalMs",
            "pageSize"
          ],
          additionalProperties: false
        },
        chatTarget: {
          anyOf: [
            {
              $ref: "#/$defs/ChatTarget"
            },
            {
              type: "null"
            }
          ]
        },
        phoneTarget: {
          anyOf: [
            {
              $ref: "#/$defs/PhoneTarget"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "serviceNodeId",
        "generation",
        "principalId",
        "rootObjectId",
        "callerPrincipalId",
        "role",
        "recovered",
        "observedAt",
        "scheduler",
        "chatTarget",
        "phoneTarget"
      ],
      additionalProperties: false
    },
    Coordination: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        activeOperation: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "principalId",
        "activeOperation",
        "updatedAt"
      ],
      additionalProperties: false
    },
    SchedulerRequest: {
      oneOf: [
        {
          $ref: "#/$defs/StartRequest"
        },
        {
          $ref: "#/$defs/ContinueRequest"
        },
        {
          $ref: "#/$defs/TransitionRequest"
        },
        {
          $ref: "#/$defs/DeferRequest"
        }
      ]
    },
    ScanCursor: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        lane: {
          enum: [
            "operations",
            "runs",
            "tasks",
            "deliveries"
          ]
        },
        queryHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        cursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "principalId",
        "serviceNodeId",
        "lane",
        "queryHash",
        "cursor",
        "updatedAt"
      ],
      additionalProperties: false
    },
    SchedulerAttempt: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        task: {
          $ref: "#/$defs/ObjectPin"
        },
        sequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        request: {
          $ref: "#/$defs/SchedulerRequest"
        },
        requestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        previous: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        previousOperation: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        preparedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "principalId",
        "task",
        "sequence",
        "request",
        "requestHash",
        "previous",
        "previousOperation",
        "preparedAt"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              sequence: {
                const: 1
              }
            }
          },
          then: {
            properties: {
              previous: {
                type: "null"
              },
              previousOperation: {
                type: "null"
              }
            }
          },
          else: {
            properties: {
              previous: {
                $ref: "#/$defs/ObjectPin"
              },
              previousOperation: {
                $ref: "#/$defs/ObjectPin"
              }
            }
          }
        }
      ]
    },
    NativeSignals: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        run: {
          $ref: "#/$defs/ObjectPin"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        nativeVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        epoch: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        threadId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        turnId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        observedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        inputsTruncated: {
          type: "boolean"
        },
        inputs: {
          type: "array",
          items: {
            type: "object",
            properties: {
              identity: {
                $ref: "https://ivy.invalid/schemas/agent.schema.json#/$defs/InputIdentity"
              },
              method: {
                type: "string",
                minLength: 1,
                maxLength: 192
              },
              turnId: {
                anyOf: [
                  {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
                  },
                  {
                    type: "null"
                  }
                ]
              },
              state: {
                enum: [
                  "pending",
                  "answering"
                ]
              },
              observedAt: {
                type: "string",
                minLength: 24,
                maxLength: 24,
                pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
              }
            },
            required: [
              "identity",
              "method",
              "turnId",
              "state",
              "observedAt"
            ],
            additionalProperties: false
          },
          maxItems: 128
        },
        notificationCursor: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        notificationGap: {
          type: "boolean"
        },
        notificationsPending: {
          type: "boolean"
        },
        activity: {
          type: "array",
          items: {
            type: "object",
            properties: {
              epoch: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
              },
              sequence: {
                type: "integer",
                minimum: 1,
                maximum: 9007199254740991
              },
              method: {
                type: "string",
                minLength: 1,
                maxLength: 192
              },
              observedAt: {
                type: "string",
                minLength: 24,
                maxLength: 24,
                pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
              }
            },
            required: [
              "epoch",
              "sequence",
              "method",
              "observedAt"
            ],
            additionalProperties: false
          },
          maxItems: 100
        }
      },
      required: [
        "schemaVersion",
        "run",
        "serviceNodeId",
        "nativeVersion",
        "epoch",
        "threadId",
        "turnId",
        "observedAt",
        "inputsTruncated",
        "inputs",
        "notificationCursor",
        "notificationGap",
        "notificationsPending",
        "activity"
      ],
      additionalProperties: false
    },
    NativeEvidenceChunk: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectPin"
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        byteLength: {
          type: "integer",
          minimum: 1,
          maximum: 1048576
        }
      },
      required: [
        "object",
        "contentHash",
        "byteLength"
      ],
      additionalProperties: false
    },
    NativeEvidence: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        format: {
          const: "agent-operation/canonical-json"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        nativeVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        nativeExecutableHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        method: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        requestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        phase: {
          enum: [
            "accepted",
            "dispatched",
            "succeeded",
            "failed",
            "outcome_unknown"
          ]
        },
        epoch: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        byteLength: {
          type: "integer",
          minimum: 1,
          maximum: 16777216
        },
        chunks: {
          type: "array",
          items: {
            $ref: "#/$defs/NativeEvidenceChunk"
          },
          maxItems: 16,
          minItems: 1
        }
      },
      required: [
        "schemaVersion",
        "format",
        "operationId",
        "callerPrincipalId",
        "serviceNodeId",
        "nativeVersion",
        "nativeExecutableHash",
        "method",
        "requestHash",
        "phase",
        "epoch",
        "contentHash",
        "byteLength",
        "chunks"
      ],
      additionalProperties: false
    },
    NativeReadEvidence: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        format: {
          const: "agent-read-observation/canonical-json"
        },
        observationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        nativeVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        nativeExecutableHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        catalogHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        method: {
          enum: [
            "thread/read",
            "thread/turns/list",
            "thread/items/list"
          ]
        },
        requestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        epoch: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        byteLength: {
          type: "integer",
          minimum: 1,
          maximum: 16777216
        },
        chunks: {
          type: "array",
          items: {
            $ref: "#/$defs/NativeEvidenceChunk"
          },
          maxItems: 16,
          minItems: 1
        }
      },
      required: [
        "schemaVersion",
        "format",
        "observationId",
        "callerPrincipalId",
        "serviceNodeId",
        "nativeVersion",
        "nativeExecutableHash",
        "catalogHash",
        "method",
        "requestHash",
        "epoch",
        "contentHash",
        "byteLength",
        "chunks"
      ],
      additionalProperties: false
    },
    NativeTurnSearch: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        run: {
          $ref: "#/$defs/ObjectPin"
        },
        nativeVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        epoch: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        threadId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        turnId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        phase: {
          enum: [
            "searching",
            "finished"
          ]
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        },
        pageCount: {
          type: "integer",
          minimum: 1,
          maximum: 1024
        },
        visitedCursorHashes: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
          },
          maxItems: 1024,
          minItems: 1,
          uniqueItems: true
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "run",
        "nativeVersion",
        "serviceNodeId",
        "epoch",
        "threadId",
        "turnId",
        "phase",
        "nextCursor",
        "pageCount",
        "visitedCursorHashes",
        "createdAt",
        "updatedAt"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              phase: {
                const: "searching"
              }
            }
          },
          then: {
            properties: {
              nextCursor: {
                type: "string",
                minLength: 1,
                maxLength: 8192
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                const: "finished"
              }
            }
          },
          then: {
            properties: {
              nextCursor: {
                type: "null"
              }
            }
          }
        }
      ]
    },
    ReadEvidenceRef: {
      type: "object",
      properties: {
        artifact: {
          $ref: "#/$defs/Artifact"
        },
        observationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        epoch: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "artifact",
        "observationId",
        "epoch"
      ],
      additionalProperties: false
    },
    NativeTurnSnapshot: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        run: {
          $ref: "#/$defs/ObjectPin"
        },
        nativeVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        epoch: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        threadId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        turnId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        turnCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        },
        metadata: {
          $ref: "#/$defs/ReadEvidenceRef"
        },
        turn: {
          $ref: "#/$defs/ReadEvidenceRef"
        },
        status: {
          enum: [
            "inProgress",
            "completed",
            "failed",
            "interrupted"
          ]
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "run",
        "nativeVersion",
        "serviceNodeId",
        "epoch",
        "threadId",
        "turnId",
        "turnCursor",
        "metadata",
        "turn",
        "status",
        "createdAt"
      ],
      additionalProperties: false
    },
    NativeResultPage: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        runId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        collectionId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        previous: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        index: {
          type: "integer",
          minimum: 1,
          maximum: 1024
        },
        cursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        },
        evidence: {
          $ref: "#/$defs/ReadEvidenceRef"
        },
        itemCount: {
          type: "integer",
          minimum: 0,
          maximum: 100
        },
        nativeBytes: {
          type: "integer",
          minimum: 1,
          maximum: 16777216
        }
      },
      required: [
        "schemaVersion",
        "runId",
        "collectionId",
        "previous",
        "index",
        "cursor",
        "nextCursor",
        "evidence",
        "itemCount",
        "nativeBytes"
      ],
      additionalProperties: false
    },
    NativeTranscript: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        collectionId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        run: {
          $ref: "#/$defs/ObjectPin"
        },
        snapshot: {
          $ref: "#/$defs/Artifact"
        },
        phase: {
          enum: [
            "collecting",
            "complete"
          ]
        },
        head: {
          anyOf: [
            {
              $ref: "#/$defs/ObjectPin"
            },
            {
              type: "null"
            }
          ]
        },
        pageCount: {
          type: "integer",
          minimum: 0,
          maximum: 1024
        },
        nativeBytes: {
          type: "integer",
          minimum: 0,
          maximum: 1073741824
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        },
        verification: {
          anyOf: [
            {
              $ref: "#/$defs/ReadEvidenceRef"
            },
            {
              type: "null"
            }
          ]
        },
        verificationCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        },
        updatedAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "collectionId",
        "run",
        "snapshot",
        "phase",
        "head",
        "pageCount",
        "nativeBytes",
        "nextCursor",
        "verification",
        "verificationCursor",
        "createdAt",
        "updatedAt"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              phase: {
                const: "complete"
              }
            }
          },
          then: {
            properties: {
              head: {
                $ref: "#/$defs/ObjectPin"
              },
              pageCount: {
                type: "integer",
                minimum: 1,
                maximum: 1024
              },
              nextCursor: {
                type: "null"
              },
              verification: {
                $ref: "#/$defs/ReadEvidenceRef"
              }
            }
          }
        }
      ]
    },
    NativeFullTurnTranscript: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        collectionId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        run: {
          $ref: "#/$defs/ObjectPin"
        },
        snapshot: {
          $ref: "#/$defs/Artifact"
        },
        unsupportedItems: {
          $ref: "#/$defs/ReadEvidenceRef"
        },
        fullTurn: {
          $ref: "#/$defs/ReadEvidenceRef"
        },
        fullTurnCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        },
        fullTurnBytes: {
          type: "integer",
          minimum: 1,
          maximum: 16777216
        },
        verification: {
          $ref: "#/$defs/ReadEvidenceRef"
        },
        verificationCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        },
        createdAt: {
          type: "string",
          minLength: 24,
          maxLength: 24,
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"
        }
      },
      required: [
        "schemaVersion",
        "collectionId",
        "run",
        "snapshot",
        "unsupportedItems",
        "fullTurn",
        "fullTurnCursor",
        "fullTurnBytes",
        "verification",
        "verificationCursor",
        "createdAt"
      ],
      additionalProperties: false
    }
  }
};

// packages/contracts/src/symphony-validation.ts
import { Ajv2020 as Ajv20205 } from "ajv/dist/2020.js";
var symphonySchema = symphony_schema_default;
var ajv5 = new Ajv20205({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [symphony_schema_default, chat_schema_default, agent_schema_default, hive_wire_schema_default]) ajv5.addSchema(schema);
function validateSymphony(name, value) {
  canonical(value, managementFrameBytes);
  const validator = ajv5.getSchema(`${String(symphonySchema["$id"])}#/$defs/${name}`);
  requireThat(validator && validator(value), "invalid_arguments", "Value does not match its Symphony workflow contract.");
}

// packages/contracts/src/checked-native-contract.ts
import { readFileSync, existsSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
var catalogs = /* @__PURE__ */ new Map();
function nativeCatalogPath(root, version, platform = process.platform, architecture = process.arch) {
  requireThat(nativeVersions.includes(version) && /^[a-z0-9]+$/.test(platform) && /^[a-z0-9]+$/.test(architecture), "native_version_unsupported", "Supported native platform and version required.");
  const directory = join(root, "specs", "native", "codex-" + version), selected = join(directory, "catalog." + platform + "-" + architecture + ".json");
  return existsSync(selected) ? selected : join(directory, "catalog.json");
}
function checkedNativeContract(version, identity) {
  requireThat(nativeVersions.includes(version), "native_version_unsupported", "A supported native catalog version is required.");
  const key = version + ":" + JSON.stringify(identity ?? null);
  const existing = catalogs.get(key);
  if (existing) return existing;
  if (identity !== void 0) {
    requireThat(
      Object.keys(identity).length > 0 && Object.keys(identity).every((key2) => ["sourceHash", "catalogHash", "nativeExecutableHash"].includes(key2)) && Object.values(identity).every((value2) => typeof value2 === "string" && /^sha256:[a-f0-9]{64}$/.test(value2)),
      "native_catalog_mismatch",
      "An explicit native catalog identity must contain exact hashes."
    );
    const directory = fileURLToPath(new URL("../../../specs/native/codex-" + version + "/", import.meta.url));
    let files;
    try {
      files = readdirSync(directory).filter((name) => /^catalog(?:\.[a-z0-9]+-[a-z0-9]+)?\.json$/.test(name));
    } catch {
      requireThat(false, "native_version_unsupported", "The requested native catalog is not retained in this distribution.");
    }
    for (const file of files) {
      const contract2 = new NativeContract(JSON.parse(readFileSync(join(directory, file), "utf8")));
      if (contract2.catalog.version !== version || identity.sourceHash !== void 0 && contract2.catalog.sourceHash !== identity.sourceHash || identity.catalogHash !== void 0 && contract2.catalogHash !== identity.catalogHash || identity.nativeExecutableHash !== void 0 && contract2.catalog.nativeExecutableHash !== identity.nativeExecutableHash) continue;
      const selected = Object.freeze({ catalog: contract2.catalog, catalogHash: contract2.catalogHash, definitions: contract2.definitions(), contract: contract2 });
      catalogs.set(key, selected);
      return selected;
    }
    requireThat(false, "native_catalog_mismatch", "No retained native platform build matches the explicitly selected catalog identity.");
  }
  let source;
  try {
    const selected = new URL("../../../specs/native/codex-" + version + "/catalog." + process.platform + "-" + process.arch + ".json", import.meta.url);
    source = readFileSync(existsSync(selected) ? selected : new URL("../../../specs/native/codex-" + version + "/catalog.json", import.meta.url), "utf8");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    requireThat(false, "native_version_unsupported", "The requested native catalog is not retained in this distribution.");
  }
  const contract = new NativeContract(JSON.parse(source));
  requireThat(contract.catalog.version === version, "native_catalog_mismatch", "The retained catalog identifies another native version.");
  const value = Object.freeze({ catalog: contract.catalog, catalogHash: contract.catalogHash, definitions: contract.definitions(), contract });
  catalogs.set(key, value);
  return value;
}

// packages/contracts/src/bundle.ts
function bundledSchema(reference, document = String(hostSchema["$id"])) {
  const sources = { Wire: wireSchema, Operation: operationSchema, Host: hostSchema, Agent: agentSchema, Symphony: symphonySchema, Automation: automationSchema, Chat: chatSchema, Secretary: secretarySchema };
  const documents = new Map(Object.entries(sources).map(([prefix, schema]) => [String(schema["$id"]), { prefix, schema }]));
  const definitions = /* @__PURE__ */ Object.create(null);
  const resolve2 = (reference2, base) => {
    const url = new URL(reference2, base), document2 = url.origin + url.pathname;
    const source = documents.get(document2)?.schema;
    requireThat(source && /^#\/\$defs\/[A-Za-z0-9]+$/.test(url.hash), "internal_error", "Unknown canonical schema reference.");
    const name = url.hash.slice("#/$defs/".length);
    const value = source["$defs"][name];
    requireThat(value, "internal_error", "Missing canonical schema definition.");
    return { value, document: document2, name };
  };
  const clone = (value, base) => {
    if (Array.isArray(value)) return value.map((item) => clone(item, base));
    if (value === null || typeof value !== "object") return value;
    const result = {};
    for (const [key, item] of Object.entries(value)) {
      if (key === "$id" || key === "$schema") continue;
      if (key !== "$ref") {
        result[key] = clone(item, base);
        continue;
      }
      const resolved = resolve2(String(item), base);
      const name = documents.get(resolved.document).prefix + resolved.name;
      if (!Object.hasOwn(definitions, name)) {
        definitions[name] = true;
        definitions[name] = clone(resolved.value, resolved.document);
      }
      result[key] = "#/$defs/" + name;
    }
    return result;
  };
  let root = resolve2(reference, document);
  while (typeof root.value["$ref"] === "string") root = resolve2(root.value["$ref"], root.document);
  return { ...clone(root.value, root.document), $defs: definitions };
}

// packages/contracts/src/agent-bundle.ts
function bundledSchema2(reference, document = String(agentSchema["$id"])) {
  const sources = { Agent: agentSchema, Wire: wireSchema };
  const documents = new Map(Object.entries(sources).map(([prefix, schema]) => [String(schema["$id"]), { prefix, schema }]));
  const definitions = /* @__PURE__ */ Object.create(null);
  const resolve2 = (ref, base) => {
    const url = new URL(ref, base), documentId = url.origin + url.pathname, source = documents.get(documentId)?.schema;
    requireThat(source && /^#\/\$defs\/[A-Za-z0-9]+$/.test(url.hash), "internal_error", "Unknown native schema reference.");
    const name = url.hash.slice("#/$defs/".length), value = source["$defs"][name];
    requireThat(value, "internal_error", "Missing native schema definition.");
    return { value, document: documentId, name };
  };
  const clone = (value, base) => {
    if (Array.isArray(value)) return value.map((item) => clone(item, base));
    if (value === null || typeof value !== "object") return value;
    const result = {};
    for (const [key, item] of Object.entries(value)) {
      if (key === "$id" || key === "$schema") continue;
      if (key !== "$ref") {
        result[key] = clone(item, base);
        continue;
      }
      const resolved = resolve2(String(item), base), name = documents.get(resolved.document).prefix + resolved.name;
      if (!Object.hasOwn(definitions, name)) {
        definitions[name] = true;
        definitions[name] = clone(resolved.value, resolved.document);
      }
      result[key] = "#/$defs/" + name;
    }
    return result;
  };
  let root = resolve2(reference, document);
  while (typeof root.value["$ref"] === "string") root = resolve2(root.value["$ref"], root.document);
  return { ...clone(root.value, root.document), $defs: definitions };
}
export {
  BoundToolClient,
  BrowserNotifications,
  HiveClient,
  IvyError,
  NativeContract,
  NativeOwner,
  ReceiptArchive,
  SchemaValidators,
  ServiceClient,
  ServiceConnection,
  admitSchema,
  assertAssessmentPolicy,
  assessmentToolConfiguration,
  baseUrl,
  binaryObjectContentBytes,
  browserClient,
  browserNotifications,
  bundledSchema2 as bundleAgentSchema,
  bundleLocalSchema,
  bundledSchema,
  callBound,
  canonical,
  checkedNativeContract,
  compareVersions,
  connectionInFlightRequests,
  connectionOwner,
  consumeEvents,
  deriveOperationId,
  digest,
  discover,
  encodeJson,
  hashJson,
  isReadOnlyNativeMethod,
  jsonObjectContentBytes,
  managementFrameBytes,
  mcpDiscoveryResultBytes,
  nativeCatalogPath,
  nativeInstant,
  nativeOperationTerminal,
  nativeServiceTools,
  nativeThreadState,
  nativeToolDefinition,
  nativeTurnItems,
  nativeVersions,
  newOperationId,
  operationId,
  parseOperationId,
  readNativeContract,
  readNativeParameters,
  readNativePlan,
  reconcileNativeOperation,
  requireThat,
  responseValue,
  restrictAssessmentParameters,
  runtimeEpoch,
  saveNativeParameters,
  saveNativePlan,
  scopedOperationId,
  serviceTools,
  textObjectContentBytes,
  toolDefinitionHash,
  validateAgent,
  validateAgentFrame,
  validateAutomation,
  validateChat,
  validateInput,
  validateNativeInvocation,
  validateNativeOperation,
  validateNativePlanDraft,
  validateNativePlanIdentity,
  validateNativeProgress,
  validateNativeRead,
  validateNativeStatus,
  validateOutput,
  validateSecretary,
  validateShared,
  validateSymphony,
  validateTransport
};
//# sourceMappingURL=node.js.map
