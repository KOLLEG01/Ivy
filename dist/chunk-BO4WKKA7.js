// packages/contracts/src/limits.ts
var managementFrameBytes = 33554432;
var jsonObjectContentBytes = 1048576;
var textObjectContentBytes = 1048576;
var binaryObjectContentBytes = 8388608;
var connectionInFlightRequests = 64;
var mutationReceiptJournalBytes = 256 * 1024 * 1024;
var mcpDiscoveryResultBytes = 512 * 1024;

// packages/contracts/src/errors.ts
var statuses = {
  unauthenticated: 401,
  forbidden: 403,
  not_found: 404,
  contract_not_found: 404,
  revision_conflict: 409,
  mutation_conflict: 409,
  contract_version_conflict: 409,
  revision_pruned: 410,
  mutation_expired: 410,
  operation_expired: 410,
  contract_definition_conflict: 409,
  contract_owner_mismatch: 409,
  stale_generation: 409,
  target_conflict: 409,
  ambiguous_service_node: 409,
  tool_definition_changed: 409,
  release_conflict: 409,
  subscription_filter_conflict: 409,
  content_too_large: 413,
  frame_too_large: 413,
  result_too_large: 413,
  limit_exceeded: 429,
  capacity_exceeded: 429,
  service_unavailable: 503,
  service_not_ready: 503,
  storage_unavailable: 503,
  deadline_exceeded: 504,
  outcome_unknown: 504,
  internal_error: 500,
  provider_contract_error: 502
};
var IvyError = class _IvyError extends Error {
  constructor(code, message, outcome = "not_executed", details) {
    super(message);
    this.code = code;
    this.outcome = outcome;
    this.details = details;
    this.name = "IvyError";
  }
  code;
  outcome;
  details;
  get httpStatus() {
    return statuses[this.code] ?? 400;
  }
  toWire() {
    return {
      code: this.code === "invalid_frame" ? -32600 : this.code === "invalid_arguments" ? -32602 : -32e3,
      message: this.message.slice(0, 2048),
      data: {
        code: this.code,
        outcome: this.outcome,
        ...this.details === void 0 ? {} : { details: this.details }
      }
    };
  }
  static from(error) {
    return error instanceof _IvyError ? error : new _IvyError(
      "internal_error",
      "The operation failed internally.",
      "unknown"
    );
  }
};
function fail(code, message, outcome = "not_executed") {
  throw new IvyError(code, message, outcome);
}
function requireThat(value, code, message) {
  if (!value) fail(code, message);
}

// packages/contracts/src/canonical-json.ts
var encoder = new TextEncoder();
var utf8ByteLength = globalThis.Buffer?.byteLength ?? ((text) => encoder.encode(text).byteLength);
function encodeJson(value, maximumBytes = managementFrameBytes) {
  const text = JSON.stringify(value);
  if (text === void 0) fail("invalid_arguments", "Expected a JSON message.");
  if (text.length > maximumBytes || utf8ByteLength(text) > maximumBytes) fail("content_too_large", "JSON exceeds the configured byte limit.");
  return text;
}
function canonical(value, maximumBytes = managementFrameBytes) {
  const ancestors = /* @__PURE__ */ new Set();
  let nodes = 0;
  let bytes = 0;
  const account = (text) => {
    bytes += /^[\x00-\x7f]*$/.test(text) ? text.length : encoder.encode(text).byteLength;
    if (bytes > maximumBytes) fail("content_too_large", "JSON exceeds the configured byte limit.");
    return text;
  };
  const visit = (item, depth) => {
    if (++nodes > 4e5 || depth > 128) fail("limit_exceeded", "JSON structure is too complex.");
    if (item === null) return account("null");
    if (typeof item === "boolean") return account(String(item));
    if (typeof item === "string") return account(JSON.stringify(item));
    if (typeof item === "number") {
      if (!Number.isFinite(item) || Number.isInteger(item) && !Number.isSafeInteger(item)) {
        fail("invalid_arguments", "Numbers must be finite and integers must be safe.");
      }
      return account(JSON.stringify(item));
    }
    if (typeof item !== "object") fail("invalid_arguments", "Value is outside the JSON domain.");
    if (ancestors.has(item)) fail("invalid_arguments", "Cyclic JSON value.");
    ancestors.add(item);
    try {
      if (Array.isArray(item)) {
        if (Object.getOwnPropertySymbols(item).length || Object.keys(item).length !== item.length) {
          fail("invalid_arguments", "Arrays must be dense and have no extra properties.");
        }
        const parts = [];
        for (let i = 0; i < item.length; i++) {
          const descriptor = Object.getOwnPropertyDescriptor(item, String(i));
          if (!descriptor || !("value" in descriptor)) fail("invalid_arguments", "JSON cannot contain accessors.");
          parts.push(visit(descriptor.value, depth + 1));
        }
        account("[" + ",".repeat(Math.max(0, parts.length - 1)) + "]");
        return "[" + parts.join(",") + "]";
      }
      const prototype = Object.getPrototypeOf(item);
      if (prototype !== Object.prototype && prototype !== null) fail("invalid_arguments", "JSON objects must be plain objects.");
      if (Object.getOwnPropertySymbols(item).length) fail("invalid_arguments", "JSON cannot contain symbol keys.");
      const keys = Object.getOwnPropertyNames(item).sort();
      const pairs = keys.map((key) => {
        const descriptor = Object.getOwnPropertyDescriptor(item, key);
        if (!descriptor.enumerable || !("value" in descriptor)) fail("invalid_arguments", "JSON properties must be enumerable values.");
        return account(JSON.stringify(key) + ":") + visit(descriptor.value, depth + 1);
      });
      account("{" + ",".repeat(Math.max(0, pairs.length - 1)) + "}");
      return "{" + pairs.join(",") + "}";
    } finally {
      ancestors.delete(item);
    }
  };
  return visit(value, 0);
}
function compareVersions(a, b) {
  const parse = (value) => {
    if (value.length > 128 || !/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(value)) {
      fail("invalid_arguments", "Expected an exact numeric contract version.");
    }
    return value.split(".").map(BigInt);
  };
  const aa = parse(a), bb = parse(b);
  for (let i = 0; i < 3; i++) if (aa[i] !== bb[i]) return aa[i] < bb[i] ? -1 : 1;
  return 0;
}

export {
  managementFrameBytes,
  jsonObjectContentBytes,
  textObjectContentBytes,
  binaryObjectContentBytes,
  connectionInFlightRequests,
  mcpDiscoveryResultBytes,
  IvyError,
  fail,
  requireThat,
  encodeJson,
  canonical,
  compareVersions
};
//# sourceMappingURL=chunk-BO4WKKA7.js.map
