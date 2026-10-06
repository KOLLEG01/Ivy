import type { Tool } from "@modelcontextprotocol/server";
import { requireThat } from "../../../packages/contracts/src/errors.js";
import { SchemaValidators } from "../../../packages/contracts/src/schema.js";
import { validateInput } from "../../../packages/contracts/src/core-validation.js";
import { hashJson } from "../../../packages/contracts/src/canonical.js";
import { managementFrameBytes } from "../../../packages/contracts/src/limits.js";
import type {
  Operation,
  Wire,
} from "../../../packages/contracts/src/generated.js";
import type { McpBinding } from "./discovery.js";
import { fallbackToolTitle, toolPresentation } from "./mcp-presentation.js";
import { McpSchemaBundle } from "./mcp-schema.js";

type VariantSelector = { property: "view"; value: string };

/**
 * Public MCP tools intentionally combine closely related operations. A selector is
 * only needed when two of those operations otherwise accept the same input. Keep
 * it in the MCP facade so service and storage contracts stay transport agnostic.
 */
const variantSelectors: Record<string, VariantSelector> = {
  "hive.objects.stat": { property: "view", value: "metadata" },
  "hive.objects.history": { property: "view", value: "history" },
  "hive.wiki.history": { property: "view", value: "history" },
  "hive.hosts.observations": { property: "view", value: "observations" },
  "hive.hostConfigurations.history": { property: "view", value: "history" },
  "hive.retention.preview": { property: "view", value: "preview" },
  "hive.serviceNodes.contracts": { property: "view", value: "contracts" },
  "hive.uis.releases": { property: "view", value: "releases" },
  "agent.capabilities": { property: "view", value: "capabilities" },
  "agent.inputDefinition": { property: "view", value: "definition" },
  "chat.notice": { property: "view", value: "notice" },
  "phone.probeLoopback": { property: "view", value: "loopback" },
  "phone.audioSetup": { property: "view", value: "audio" },
  "phone.codecTest": { property: "view", value: "codecs" },
  "phone.inventory": { property: "view", value: "inventory" },
  "phone.logs": { property: "view", value: "logs" },
  "phone.reconcileArchive": { property: "view", value: "archive" },
};

function variantSelector(binding: McpBinding): VariantSelector | undefined {
  return variantSelectors[binding.qualifiedName];
}

function emptyObjectSchema(schema: Record<string, unknown>): boolean {
  return (
    schema["type"] === "object" &&
    schema["additionalProperties"] === false &&
    !Object.keys((schema["properties"] ?? {}) as object).length &&
    !Object.keys((schema["patternProperties"] ?? {}) as object).length &&
    !Object.keys((schema["required"] ?? []) as object).length &&
    Object.keys(schema).every((key) =>
      [
        "type",
        "additionalProperties",
        "properties",
        "patternProperties",
        "required",
        "$schema",
        "$id",
        "$defs",
        "title",
        "description",
      ].includes(key),
    )
  );
}

/** Null-only and empty-object results acknowledge completion without returning data. */
export function hasStructuredOutput(bindings: McpBinding[]): boolean {
  return bindings.some(({ definition }) => {
    const schema = definition.outputSchema;
    return (
      typeof schema !== "object" ||
      (schema["type"] !== "null" &&
        schema["const"] !== null &&
        !emptyObjectSchema(schema))
    );
  });
}

function outerOperation(binding: McpBinding): boolean {
  const schema = binding.definition.inputSchema;
  const properties =
    typeof schema === "object"
      ? (schema["properties"] as Record<string, unknown> | undefined)
      : undefined;
  return (
    binding.definition.annotations?.readOnlyHint !== true &&
    !properties?.["operationId"]
  );
}

/** Preserve the reference as a constraint and expose its object fields to clients that stop at $ref. */
function visibleRootReference(schema: Record<string, unknown>): Record<string, unknown> {
  const reference = schema["$ref"];
  const match = typeof reference === "string" && /^#\/\$defs\/([^/]+)$/.exec(reference);
  const target = match && ((schema["$defs"] ?? {}) as Record<string, unknown>)[match[1]!.replaceAll("~1", "/").replaceAll("~0", "~")];
  if (!target || typeof target !== "object" || (target as Record<string, unknown>)["type"] !== "object") return schema;
  const definition = target as Record<string, unknown>;
  if (!definition["properties"] || typeof definition["properties"] !== "object") return schema;
  const { $ref: _reference, ...rest } = schema;
  return {
    ...rest,
    type: "object",
    properties: definition["properties"],
    ...(definition["required"] ? { required: definition["required"] } : {}),
    allOf: [...(Array.isArray(schema["allOf"]) ? schema["allOf"] : []), { $ref: reference }],
  };
}

function serviceInput(
  bindings: McpBinding[],
  bundle: McpSchemaBundle,
  path = "",
): Record<string, unknown> {
  const binding = bindings[0]!;
  const selector = variantSelector(binding);
  return {
    type: "object",
    properties: {
      serviceNodeId: {
        type: "string",
        enum: bindings.map((value) => value.provider!.serviceNodeId),
        description: bindings
          .map(
            ({ provider }) =>
              `${provider!.serviceNodeId}: host ${provider!.hostId}; ${provider!.available ? "available" : "unavailable, retain this owner for existing work"}.`,
          )
          .join(" "),
      },
      input: bundle.embed(
        visibleRootReference(binding.definition.inputSchema as Record<string, unknown>),
        path + "/properties/input",
      ),
      ...(selector ? { [selector.property]: { const: selector.value } } : {}),
      ...(outerOperation(binding)
        ? {
            operationId: {
              type: "string",
              minLength: 1,
              maxLength: 256,
              description:
                binding.serviceName === "agent-manager"
                  ? "Caller-generated <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. Valid for 24 hours and at most 60 seconds in the future; a Hive restart changes the epoch. Retain this exact ID and check agent_manager_read after an uncertain result."
                  : "Stable identity for this action. Retain it and reconcile the original outcome after a lost reply; follow the owning service's ID format.",
            },
          }
        : {}),
    },
    required: [
      "serviceNodeId",
      "input",
      ...(selector ? [selector.property] : []),
      ...(outerOperation(binding) ? ["operationId"] : []),
    ],
    additionalProperties: false,
  };
}

function coreInput(
  binding: McpBinding,
  bundle: McpSchemaBundle,
  path: string,
): Record<string, unknown> {
  const schema = bundle.embed(visibleRootReference(binding.definition.inputSchema as Record<string, unknown>), path) as Record<
    string,
    unknown
  >;
  const selector = variantSelector(binding);
  if (!selector)
    return emptyObjectSchema(schema)
      ? { type: "object", additionalProperties: false }
      : schema;
  requireThat(
    schema["type"] === "object" &&
      schema["properties"] &&
      typeof schema["properties"] === "object",
    "registry_invalid",
    "A selected core MCP variant needs an object input schema.",
  );
  return {
    ...schema,
    properties: {
      ...(schema["properties"] as Record<string, unknown>),
      [selector.property]: { const: selector.value },
    },
    required: [
      ...new Set([
        ...(Array.isArray(schema["required"])
          ? (schema["required"] as string[])
          : []),
        selector.property,
      ]),
    ],
  };
}

/** Put discoverable fields at the root while the original alternatives still validate every call. */
function visibleAlternatives(branches: Record<string, unknown>[]): Record<string, unknown> {
  const keys = new Set(branches.flatMap(branch => Object.keys((branch["properties"] ?? {}) as object)));
  const properties: Record<string, unknown> = {};
  for (const key of keys) {
    const choices = [...new Map(branches.flatMap(branch => {
      const value = ((branch["properties"] ?? {}) as Record<string, unknown>)[key];
      return value === undefined ? [] : [[hashJson(value), value] as const];
    })).values()];
    properties[key] = choices.length === 1 ? choices[0] : { anyOf: choices };
  }
  return {
    type: "object",
    properties,
    allOf: [{ oneOf: branches }],
  };
}

export function directDescriptors(bindings: McpBinding[]): Tool[] {
  const groups = new Map<string, McpBinding[]>();
  for (const binding of bindings) {
    const name = binding.definition.discovery!.mcp!.name!;
    const group = groups.get(name) ?? [];
    group.push(binding);
    groups.set(name, group);
  }
  return [...groups].map(([name, group]) => {
    const first = group[0]!,
      core = first.provider === null;
    requireThat(
      group.every(
        (value) =>
          value.serviceName === first.serviceName &&
          (value.provider === null) === core,
      ),
      "registry_invalid",
      "Conflicting MCP tool owners.",
    );
    const variants = new Map<string, McpBinding[]>();
    for (const binding of group) {
      const identity =
        hashJson(binding.definition.inputSchema) +
        ":" +
        outerOperation(binding) +
        ":" +
        hashJson(variantSelector(binding) ?? null);
      const matches = variants.get(identity) ?? [];
      matches.push(binding);
      variants.set(identity, matches);
    }
    const inputs = [...variants.values()];
    const inputBundle = new McpSchemaBundle();
    const inputSchema = inputBundle.finish(
      core
        ? inputs.length === 1
          ? coreInput(inputs[0]![0]!, inputBundle, "")
          : visibleAlternatives(inputs.map((bindings, index) =>
                coreInput(bindings[0]!, inputBundle, "/allOf/0/oneOf/" + index),
              ))
        : inputs.length === 1
          ? serviceInput(inputs[0]!, inputBundle)
          : visibleAlternatives(inputs.map((bindings, index) =>
                serviceInput(bindings, inputBundle, "/allOf/0/oneOf/" + index),
              )),
    );
    requireThat(
      typeof inputSchema === "object",
      "registry_invalid",
      "Direct MCP tools need an object input schema.",
    );
    const outputs = [
      ...new Map(
        group.map((binding) => [
          hashJson(binding.definition.outputSchema),
          binding.definition.outputSchema,
        ]),
      ).values(),
    ];
    const outputBundle = new McpSchemaBundle();
    const visibleOutput = (schema: unknown) => schema && typeof schema === "object" && !Array.isArray(schema)
      ? visibleRootReference(schema as Record<string, unknown>) : schema;
    const resultSchema =
      outputs.length === 1
        ? outputBundle.embed(visibleOutput(outputs[0]), "/properties/result")
        : {
            anyOf: outputs.map((schema, index) =>
              outputBundle.embed(visibleOutput(schema), "/properties/result/anyOf/" + index),
            ),
          };
    const presentation = toolPresentation(first.serviceName, name);
    const declaredTitles = [
      ...new Set(
        group
          .map((binding) => binding.definition.annotations?.title)
          .filter(Boolean),
      ),
    ];
    return {
      name,
      _meta: { "ivy/serviceName": first.serviceName },
      title:
        declaredTitles.length === 1
          ? declaredTitles[0]!
          : (presentation?.title ?? fallbackToolTitle(name)),
      description:
        [
          ...new Set(group.map((binding) =>
            binding.definition.discovery?.summary ??
            presentation?.description ?? binding.definition.description,
          )),
          ...group.flatMap((binding) => {
            const selector = variantSelector(binding);
            return selector && !presentation
              ? [
                  `Set ${selector.property}=${JSON.stringify(selector.value)} for ${binding.qualifiedName}.`,
                ]
              : [];
          }),
        ].join("\n\n"),
      inputSchema: inputSchema as Tool["inputSchema"],
      ...(hasStructuredOutput(group)
        ? {
            outputSchema: outputBundle.finish({
              type: "object",
              properties: { result: resultSchema },
              required: ["result"],
              additionalProperties: false,
            }),
          }
        : {}),
      annotations: {
        readOnlyHint: group.every(
          (binding) => binding.definition.annotations?.readOnlyHint === true,
        ),
        destructiveHint: group.some(
          (binding) =>
            binding.definition.annotations?.destructiveHint ??
            presentation?.destructiveHint ??
            binding.definition.annotations?.readOnlyHint !== true,
        ),
        idempotentHint: group.every(
          (binding) =>
            binding.definition.annotations?.idempotentHint ??
            binding.definition.annotations?.readOnlyHint === true,
        ),
        openWorldHint: group.some(
          (binding) =>
            binding.definition.annotations?.openWorldHint ??
            presentation?.openWorldHint ??
            !core,
        ),
      },
    };
  });
}

const validators = new SchemaValidators();
function bindingInput(
  binding: McpBinding,
  args: Record<string, unknown>,
): unknown {
  if (binding.provider) return args["input"];
  const selector = variantSelector(binding);
  return selector
    ? Object.fromEntries(
        Object.entries(args).filter(([key]) => key !== selector.property),
      )
    : args;
}

export function directCall(
  bindings: McpBinding[],
  args: Record<string, unknown>,
): Operation.DiscoveryCallParams {
  const candidates = bindings.filter(
    (value) =>
      value.provider === null ||
      value.provider.serviceNodeId === args["serviceNodeId"],
  );
  const matches = candidates.filter((value) => {
    try {
      if (value.provider || variantSelector(value))
        validators.validate(
          directDescriptors([value])[0]!.inputSchema,
          args,
          managementFrameBytes,
        );
      if (!value.provider)
        validateInput(value.definition.name, bindingInput(value, args));
      return true;
    } catch {
      return false;
    }
  });
  const binding = matches.length === 1 ? matches[0] : undefined;
  requireThat(
    binding,
    "invalid_arguments",
    "Select one valid view or action variant and the exact owning serviceNodeId from this tool’s schema.",
  );
  const input = bindingInput(binding, args);
  const operationId = binding.provider
    ? (args["operationId"] ??
      (input as Record<string, unknown>)?.["operationId"])
    : undefined;
  return {
    serviceName: binding.serviceName,
    qualifiedName: binding.qualifiedName,
    expectedDefinitionHash: binding.definitionHash,
    ...(binding.provider
      ? { serviceNodeId: binding.provider.serviceNodeId }
      : {}),
    ...(typeof operationId === "string" ? { operationId } : {}),
    arguments: input as Wire.Json,
  };
}
