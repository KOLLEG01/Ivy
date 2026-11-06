import { computed, watch } from "vue";
import { useRemote } from "@ivy/ui";
import type { Agent, RpcClient } from "../../sdk/src/client.js";
import {
  modelsFrom,
  nativeModels,
  nativePermissionProfiles,
  optionalTool,
} from "./native.js";
import { backendRead, nativeAccountRead, nativeMethod, nativeBackendKey } from './native-backends.js';
import { accountExecutionProfile, readAccountExecutionDefaults } from './execution-defaults.js';
export { effectiveNativeSettings, nativeConfiguredHome } from "./native-effective-settings.js";

/** Each capability loads independently and retries after transient failures. */
export function useNativeSettings(
  client: RpcClient,
  node: () => string,
  cwd: () => string,
  backend: () => Agent.BackendIdentity | undefined = () => undefined,
  creationDefaults = true,
) {
  const services = () => [node() ? "services/agent-manager/" + node() : "services/agent-manager"];
  const models = useRemote(
    async (signal) => {
      if (!node()) return { data: [] };
      const result = await nativeAccountRead(client, { serviceNodeId: node(), backend: backend() }, owner =>
        nativeModels(client, owner.serviceNodeId, signal, owner.backend?.backendId), signal);
      if (!modelsFrom(result).length) throw new Error('The native server returned no available models.');
      return result;
    },
    0,
    services,
  );
  const modes = useRemote(
    async (signal) =>
      node() &&
      (await optionalTool(client, node(), nativeMethod(backend(), "collaborationMode/list")))
        ? backendRead(client, { serviceNodeId: node(), backend: backend() }, "collaborationMode/list", {}, signal)
        : null,
    0,
    services,
  );
  const permissions = useRemote(
    async (signal) =>
      node() &&
      (await optionalTool(client, node(), nativeMethod(backend(), "permissionProfile/list")))
        ? nativeAccountRead(client, { serviceNodeId: node(), backend: backend() }, owner =>
            nativePermissionProfiles(client, owner.serviceNodeId, cwd() || undefined, signal, owner.backend?.backendId), signal)
        : null,
    0,
    services,
  );
  const config = useRemote(
    async (signal) =>
      node() && cwd() && (await optionalTool(client, node(), nativeMethod(backend(), "config/read")))
        ? backendRead(
            client,
            { serviceNodeId: node(), backend: backend() },
            "config/read",
            { ...(cwd() ? { cwd: cwd() } : {}), includeLayers: false },
            signal,
          )
        : null,
    0,
    services,
  );
  const defaults = useRemote(signal => creationDefaults ? readAccountExecutionDefaults(client, signal) : Promise.resolve(null), 0, ['objects/agent/account-execution-defaults']);
  const profile = computed(() => accountExecutionProfile(defaults.value.value?.document, backend()));
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
  watch(() => [node(), nativeBackendKey(backend())].join('\u0000'), () => {
    for (const capability of [models, modes, permissions, config]) capability.value.value = null;
    void models.refresh(); void modes.refresh(); void permissions.refresh(); void config.refresh();
  });
  return { models, modes, permissions, config, defaults, profile, choices, refresh };
}
