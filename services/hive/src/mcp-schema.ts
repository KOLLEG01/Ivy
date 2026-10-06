import { hashJson } from "../../../packages/contracts/src/canonical.js";
import { requireThat } from "../../../packages/contracts/src/errors.js";

type Schema = Record<string, unknown>;
const pointerToken = (value: string) =>
  value.replaceAll("~", "~0").replaceAll("/", "~1");

/** Visit schema positions only; defaults, examples and literal data are not schemas. */
function visitSchemas(value: unknown, visit: (schema: Schema) => void): void {
  if (!value || typeof value !== "object" || Array.isArray(value)) return;
  const schema = value as Schema;
  visit(schema);
  for (const key of [
    "properties",
    "$defs",
    "patternProperties",
    "dependentSchemas",
  ])
    if (schema[key] && typeof schema[key] === "object")
      for (const child of Object.values(schema[key]))
        visitSchemas(child, visit);
  for (const key of ["allOf", "anyOf", "oneOf", "prefixItems"])
    if (Array.isArray(schema[key]))
      for (const child of schema[key]) visitSchemas(child, visit);
  for (const key of [
    "items",
    "additionalProperties",
    "unevaluatedProperties",
    "not",
    "if",
    "then",
    "else",
    "contains",
    "propertyNames",
  ])
    visitSchemas(schema[key], visit);
}

/** Share definitions within one tool schema while retaining each provider's constraints. */
export class McpSchemaBundle {
  private definitions: Schema = Object.create(null) as Schema;
  private names = new Map<string, string>();

  embed(value: unknown, path: string): unknown {
    if (!value || typeof value !== "object") return value;
    const source = value as Schema;
    const definitions = Object.entries((source["$defs"] ?? {}) as Schema);
    const references = new Map<string, string>();
    const added: [string, unknown][] = [];
    for (const [key, definition] of definitions) {
      // Include the complete referenced graph, including cycles. Identical local
      // names alone do not imply identical definitions on different providers.
      const dependencies: Schema = Object.create(null) as Schema;
      let outsideDefinitions = false;
      const collect = (schema: Schema) => {
        const ref = schema["$ref"];
        if (typeof ref !== "string" || !ref.startsWith("#")) return;
        if (!ref.startsWith("#/$defs/")) outsideDefinitions = true;
        if (Object.hasOwn(dependencies, ref)) return;
        let target: unknown = source;
        for (const token of ref === "#" ? [] : ref.slice(2).split("/")) {
          const key = token.replaceAll("~1", "/").replaceAll("~0", "~");
          requireThat(
            target && typeof target === "object" && Object.hasOwn(target, key),
            "registry_invalid",
            "MCP schema reference has no local target.",
          );
          target = (target as Schema)[key];
        }
        dependencies[ref] = target;
        visitSchemas(target, collect);
      };
      visitSchemas(definition, collect);
      const identity = hashJson({
        definition,
        dependencies,
        ...(outsideDefinitions ? { path } : {}),
      });
      let name = this.names.get(identity);
      if (name === undefined) {
        name = key;
        for (let suffix = 2; Object.hasOwn(this.definitions, name); suffix++)
          name = key + "_" + suffix;
        this.names.set(identity, name);
        this.definitions[name] = true;
        added.push([name, definition]);
      }
      references.set(pointerToken(key), pointerToken(name));
    }
    const rewrite = (value: unknown) => {
      const result = structuredClone(value);
      visitSchemas(result, (schema) => {
        delete schema["$id"];
        delete schema["$schema"];
        const ref = schema["$ref"];
        if (typeof ref !== "string" || !ref.startsWith("#")) return;
        const match = /^#\/\$defs\/([^/]+)(.*)$/.exec(ref);
        const name = match && references.get(match[1]!);
        schema["$ref"] =
          name !== null && name !== undefined
            ? "#/$defs/" + name + match![2]
            : "#" + path + ref.slice(1);
      });
      return result;
    };
    for (const [name, definition] of added)
      this.definitions[name] = rewrite(definition);
    const root = { ...source };
    delete root["$defs"];
    return rewrite(root);
  }

  finish(schema: Schema): Schema {
    return Object.keys(this.definitions).length
      ? { ...schema, $defs: this.definitions }
      : schema;
  }
}
