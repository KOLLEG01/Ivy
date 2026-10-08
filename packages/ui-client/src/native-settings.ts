import { computed, watch } from "vue";
import { useRemote } from "@ivy/ui";
import type { Agent, RpcClient } from "../../sdk/src/client.js";
import {
  modelsFrom,
  nativeModels,
  nativePermissionProfiles,
  nativeRead,
  optionalTool,
} from "./native.js";
export { effectiveNativeSettings, nativeConfiguredHome } from "./native-effective-settings.js";

export const executionDefaultsPath = "/ivy-agent-execution-defaults";
export const emptyExecutionDefaults = (): Agent.ExecutionDefaults => ({
  model: null,
  mode: null,
  effort: null,
  permission: null,
});
export async function readExecutionDefaults(client: RpcClient) {
  const object = await client
    .request("objects.stat", { path: executionDefaultsPath })
    .catch((cause) => {
      if ((cause as { code?: string }).code === "not_found") return null;
      throw cause;
    });
  if (!object || object.effectivelyArchived) return null;
  const value = await client.request("objects.read", {
    objectId: object.id,
    revision: object.currentRevision,
  });
  if (
    value.object.contractKey !== "agent/execution-defaults" ||
    value.revision.contractVersion !== "1.0.0" ||
    value.content.encoding !== "json"
  )
    throw new Error(
      "Agent execution defaults have a different document contract.",
    );
  return {
    object,
    document: value.content.value as unknown as Agent.ExecutionDefaultsDocument,
  };
}

/** Each capability loads independently and retries after transient failures. */
export function useNativeSettings(
  client: RpcClient,
  node: () => string,
  cwd: () => string,
) {
  const services = () => [node() ? "services/agent-manager/" + node() : "services/agent-manager"];
  const models = useRemote(
    async (signal) => {
      if (!node()) return { data: [] };
      const result = await nativeModels(client, node(), signal);
      if (!modelsFrom(result).length) throw new Error('The native server returned no available models.');
      return result;
    },
    0,
    services,
  );
  const modes = useRemote(
    async (signal) =>
      node() &&
      (await optionalTool(client, node(), "codex.collaborationMode/list"))
        ? nativeRead(client, node(), "codex.collaborationMode/list", {}, signal)
        : null,
    0,
    services,
  );
  const permissions = useRemote(
    async (signal) =>
      node() &&
      (await optionalTool(client, node(), "codex.permissionProfile/list"))
        ? nativePermissionProfiles(client, node(), cwd() || undefined, signal)
        : null,
    0,
    services,
  );
  const config = useRemote(
    async (signal) =>
      node() && cwd() && (await optionalTool(client, node(), "codex.config/read"))
        ? nativeRead(
            client,
            node(),
            "codex.config/read",
            { ...(cwd() ? { cwd: cwd() } : {}), includeLayers: false },
            signal,
          )
        : null,
    0,
    services,
  );
  const defaults = useRemote(() => readExecutionDefaults(client), 0, [
    "objects/agent/execution-defaults",
  ]);
  watch(cwd, () => {
    void config.refresh();
    void permissions.refresh();
  });
  const choices = computed(() => modelsFrom(models.value.value));
  const refresh = () => {
    void models.refresh();
    void modes.refresh();
    void permissions.refresh();
    void config.refresh();
    void defaults.refresh();
  };
  return { models, modes, permissions, config, defaults, choices, refresh };
}
