import type { Agent } from "../../sdk/src/client.js";
import { modelsFrom, record, text } from "./native.js";

export interface EffectiveSettings {
  model: string;
  mode: string;
  effort: string;
  permission: string;
}
export function nativeConfiguredHome(status: Agent.Status | null | undefined): string {
  return status?.connection?.actualHome || text(record(status?.initialized).codexHome) || status?.instructions?.home || '';
}
export function effectiveNativeSettings(
  models: unknown,
  configuration: unknown,
  current: unknown,
  defaults?: Partial<Agent.ExecutionDefaults> | null,
): EffectiveSettings {
  const config = record(record(configuration).config),
    thread = record(current),
    collaboration = record(thread.collaborationMode);
  const advertised = modelsFrom(models),
    fallback = advertised.find((value) => value.isDefault);
  const model =
    text(thread.model) ||
    defaults?.model ||
    text(config.model) ||
    fallback?.model ||
    "";
  const choice = advertised.find((value) => value.model === model);
  const effort =
    text(thread.reasoningEffort ?? thread.effort) ||
    defaults?.effort ||
    (!text(config.model) || config.model === model
      ? text(config.model_reasoning_effort)
      : "") ||
    choice?.defaultEffort ||
    "";
  const profile =
    thread.activePermissionProfile ??
    thread.permissionProfile ??
    thread.permissions;
  const sandbox =
    text(thread.sandboxMode) ||
    text(record(thread.sandbox ?? thread.sandboxPolicy).type) ||
    text(config.sandbox_mode);
  const builtIn: Record<string, string> = {
    "read-only": ":read-only",
    readOnly: ":read-only",
    "workspace-write": ":workspace",
    workspaceWrite: ":workspace",
    "danger-full-access": ":danger-full-access",
    dangerFullAccess: ":danger-full-access",
  };
  return {
    model,
    effort: choice?.efforts.length === 0 ? "" : effort,
    mode:
      text(collaboration.mode) ||
      defaults?.mode ||
      text(record(config.collaboration_mode).mode) ||
      "default",
    permission:
      text(profile) ||
      text(record(profile).id) ||
      defaults?.permission ||
      text(config.default_permissions) ||
      builtIn[sandbox] ||
      "",
  };
}
