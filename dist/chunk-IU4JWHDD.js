import {
  hashJson
} from "./chunk-CE4KJT7S.js";
import {
  canonical,
  fail,
  requireThat
} from "./chunk-BO4WKKA7.js";

// packages/contracts/src/schema.ts
import { Ajv2020 } from "ajv/dist/2020.js";
import { RE2JS } from "re2js";
var keywords = /* @__PURE__ */ new Set([
  "$schema",
  "$id",
  "$defs",
  "$ref",
  "title",
  "description",
  "default",
  "examples",
  "deprecated",
  "readOnly",
  "writeOnly",
  "format",
  "type",
  "properties",
  "required",
  "additionalProperties",
  "minProperties",
  "maxProperties",
  "items",
  "minItems",
  "maxItems",
  "uniqueItems",
  "enum",
  "const",
  "anyOf",
  "oneOf",
  "allOf",
  "if",
  "then",
  "else",
  "not",
  "minimum",
  "maximum",
  "exclusiveMinimum",
  "exclusiveMaximum",
  "multipleOf",
  "minLength",
  "maxLength",
  "pattern"
]);
function pointerParts(pointer) {
  requireThat(pointer.startsWith("/") && !/~(?:[^01]|$)/.test(pointer), "invalid_arguments", "Invalid JSON pointer.");
  return pointer.slice(1).split("/").map((part) => part.replaceAll("~1", "/").replaceAll("~0", "~"));
}
function resolve(root, reference) {
  requireThat(reference.startsWith("#/"), "registry_invalid", "Only local JSON pointer schema references are supported.");
  let value = root;
  for (const key of pointerParts(reference.slice(1))) {
    requireThat(value !== null && typeof value === "object" && Object.hasOwn(value, key), "registry_invalid", "Unresolved schema reference.");
    value = value[key];
  }
  requireThat(typeof value === "boolean" || value !== null && typeof value === "object" && !Array.isArray(value), "registry_invalid", "Reference must address a schema.");
  return value;
}
function admitSchema(schema) {
  canonical(schema, 256 * 1024);
  let visited = 0;
  const inspect = (node, depth, trail, consumption, refs) => {
    requireThat(++visited <= 8192 && depth <= 64, "limit_exceeded", "Schema exceeds the validation complexity limit.");
    if (typeof node === "boolean") return;
    requireThat(node !== null && typeof node === "object" && !Array.isArray(node), "registry_invalid", "Expected a JSON schema.");
    for (const key of Object.keys(node)) requireThat(keywords.has(key), "registry_invalid", `Unsupported schema keyword: ${key.slice(0, 80)}`);
    requireThat(!trail.has(node), "registry_invalid", "Schema has a non-consuming recursive reference.");
    const nextTrail = new Set(trail).add(node);
    if (node["$ref"] !== void 0) {
      requireThat(typeof node["$ref"] === "string", "registry_invalid", "Invalid schema reference.");
      const target = resolve(schema, node["$ref"]);
      const prior = refs.get(target);
      if (prior !== void 0) requireThat(consumption > prior, "registry_invalid", "Schema recursion must consume a value level.");
      else inspect(target, depth + 1, nextTrail, consumption, new Map(refs).set(target, consumption));
    }
    if (node["pattern"] !== void 0) {
      requireThat(typeof node["pattern"] === "string" && node["pattern"].length <= 1024, "registry_invalid", "Schema pattern exceeds its limit.");
      try {
        RE2JS.compile(node["pattern"]);
      } catch {
        fail("registry_invalid", "Schema pattern is outside the supported RE2 syntax.");
      }
    }
    for (const key of ["anyOf", "oneOf", "allOf"]) if (node[key] !== void 0) {
      requireThat(Array.isArray(node[key]) && node[key].length > 0 && node[key].length <= 32, "registry_invalid", "Schema alternatives exceed their limit.");
      for (const child of node[key]) inspect(child, depth + 1, nextTrail, consumption, refs);
    }
    for (const key of ["if", "then", "else", "not"]) if (node[key] !== void 0) inspect(node[key], depth + 1, nextTrail, consumption, refs);
    for (const key of ["properties", "$defs"]) if (node[key] !== void 0) {
      const map = node[key];
      requireThat(map !== null && typeof map === "object" && !Array.isArray(map), "registry_invalid", "Invalid schema property map.");
      requireThat(Object.keys(map).length <= 1024 && !Object.hasOwn(map, "__proto__"), "registry_invalid", "Unsupported schema property map.");
      for (const child of Object.values(map)) inspect(child, depth + 1, /* @__PURE__ */ new Set(), consumption + (key === "properties" ? 1 : 0), refs);
    }
    for (const key of ["items", "additionalProperties"]) if (node[key] !== void 0) inspect(node[key], depth + 1, /* @__PURE__ */ new Set(), consumption + 1, refs);
  };
  inspect(schema, 0, /* @__PURE__ */ new Set(), 0, /* @__PURE__ */ new Map([[schema, 0]]));
}
var re2Engine = Object.assign((pattern, _flags) => {
  const expression = RE2JS.compile(pattern);
  return { test: (input) => expression.matcher(input).find(), toString: () => pattern };
}, { code: "ivyRE2" });
var SchemaValidators = class {
  cache = /* @__PURE__ */ new Map();
  immutable = /* @__PURE__ */ new WeakMap();
  anonymousCompiler = null;
  compilations = 0;
  compile(schema) {
    const frozen = typeof schema === "object" && Object.isFrozen(schema);
    if (frozen) {
      const validator2 = this.immutable.get(schema);
      if (validator2) return validator2;
    }
    const identity = hashJson(schema);
    const existing = this.cache.get(identity);
    if (existing) {
      if (frozen) this.immutable.set(schema, existing);
      return existing;
    }
    admitSchema(schema);
    if (this.compilations === 256) {
      this.cache.clear();
      this.anonymousCompiler = null;
      this.compilations = 0;
    }
    this.compilations++;
    const options = {
      strict: false,
      allErrors: false,
      ownProperties: true,
      inlineRefs: false,
      validateFormats: false,
      loopRequired: 64,
      loopEnum: 64,
      code: { regExp: re2Engine }
    };
    const containsId = (value) => value !== null && typeof value === "object" && (Object.hasOwn(value, "$id") || Object.values(value).some(containsId));
    const isolated = containsId(schema);
    const ajv = isolated ? new Ajv2020(options) : this.anonymousCompiler ??= new Ajv2020({ ...options, addUsedSchema: false });
    let validator;
    try {
      validator = ajv.compile(schema);
    } catch {
      fail("registry_invalid", "Invalid or unsupported JSON schema.");
    } finally {
      if (!isolated && typeof schema === "object") ajv.removeSchema(schema);
    }
    this.cache.set(identity, validator);
    if (frozen) this.immutable.set(schema, validator);
    return validator;
  }
  validate(schema, value, maximumBytes = 1024 * 1024) {
    canonical(value, maximumBytes);
    requireThat(this.compile(schema)(value), "invalid_arguments", "Content does not satisfy its exact contract.");
  }
};

export {
  pointerParts,
  admitSchema,
  SchemaValidators
};
//# sourceMappingURL=chunk-IU4JWHDD.js.map
