import {
  hashJson,
  requireThat,
  toolDefinitionHash,
  validateAgent,
} from "../../../packages/sdk/src/node.js";
import type { Agent, Wire } from "../../../packages/sdk/src/node.js";

/** Native discovery belongs to the selected Codex host, not Hive's MCP router. */
export function discoverNativeTools(
  catalog: Agent.Catalog,
  definitions: Wire.ToolDefinition[],
  input: Agent.NativeDiscoveryInput,
  platform: NodeJS.Platform = process.platform,
): Agent.NativeDiscovery {
  validateAgent("NativeDiscoveryInput", input);
  requireThat(
    !(input.query !== undefined && input.method !== undefined),
    "invalid_arguments",
    "Choose capability keywords or one exact native method.",
  );
  const words =
    input.query?.toLocaleLowerCase("en").split(/\s+/).filter(Boolean) ?? [];
  const selected = definitions.filter((definition) =>
    input.method !== undefined
      ? definition.nativeMethod === input.method
      : words.every((word) =>
          [
            definition.name,
            definition.description,
            definition.discovery?.summary,
            definition.discovery?.group,
          ]
            .join(" ")
            .toLocaleLowerCase("en")
            .includes(word),
        ),
  );
  requireThat(
    input.method === undefined || selected.length === 1,
    "native_method_unsupported",
    "The method is absent from this host’s installed native catalog.",
  );
  return {
    nativeVersion: catalog.version,
    catalogHash: hashJson(catalog),
    items: selected.map((definition) => ({
      method: definition.nativeMethod!,
      description: definition.discovery?.summary ?? definition.description,
      readOnlyHint: definition.annotations?.readOnlyHint === true,
      expectedDefinitionHash: toolDefinitionHash(definition),
      ...(input.method === undefined
        ? {}
        : {
            inputSchema:
              definition.inputSchema as Agent.Catalog["clientRequests"][number]["inputSchema"],
            outputSchema:
              definition.outputSchema as Agent.Catalog["clientRequests"][number]["outputSchema"],
            ...(platform === "win32" && definition.nativeMethod === "command/exec"
              ? { runtimeConstraints: [{
                  condition: "Windows sandbox is active",
                  unsupportedParameters: ["outputBytesCap"],
                  guidance: "Omit a custom outputBytesCap when executing in the Windows sandbox. The native method schema remains unchanged.",
                }] }
              : {}),
          }),
    })),
  };
}
