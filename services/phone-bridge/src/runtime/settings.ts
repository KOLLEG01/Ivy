import { isAbsolute } from "node:path";
import { jsonFile } from "../../../../packages/host-runtime/src/config.js";
import { requireThat } from "../../../../packages/sdk/src/node.js";
import { PhoneAdmission } from "./admission.js";
import type { PhonePolicyDefinition } from "./admission.js";
import type { PhoneFlowSettings } from "./flow.js";
import { validatePhoneService } from "./registry.js";
import { resolveCodexHome } from "../../../../packages/host-runtime/src/codex-home.js";

export interface PhoneSettings extends PhoneFlowSettings {
  native: { executable: string; executableHash: string };
  binding: Record<string, unknown>;
  codecs: Record<string, unknown>;
  registration: Record<string, unknown> | null;
  credentialsPath: string | null;
  policy: PhonePolicyDefinition;
  pollMs: number;
  incomingPrincipalId: string | null;
}
export function phoneSettings(value: unknown): PhoneSettings {
  validatePhoneService("PhoneServiceSettings", value);
  const settings = value as PhoneSettings;
  new PhoneAdmission(settings.policy);
  requireThat(
    !settings.policy.challengedIncoming?.length ||
      (typeof settings.accessCodePath === "string" &&
        isAbsolute(settings.accessCodePath)),
    "invalid_arguments",
    "Challenged incoming access requires an absolute protected code settings path.",
  );
  const voice =
    (settings.incomingRoute ?? "voice") === "voice" ||
    (settings.outgoingRoute ?? "windows") === "voice";
  requireThat(
    !voice || settings.codexVoice !== null,
    "invalid_arguments",
    "Codex Voice routing requires its configured CLI runtime.",
  );
  if (settings.codexVoice) {
    requireThat(settings.codexVoice.playbackPrebufferMs === undefined ||
      settings.codexVoice.playbackPrebufferMs <= (settings.codexVoice.queueMs ?? 80),
      "invalid_arguments", "Voice playout prebuffer cannot exceed its audio queue.");
    resolveCodexHome(settings.codexVoice);
    requireThat(
      isAbsolute(settings.codexVoice.nativeExecutable) &&
        isAbsolute(settings.codexVoice.cwd),
      "invalid_arguments",
      "Codex Voice executable and working directory must be absolute.",
    );
  }
  requireThat(
    ((settings.incomingRoute ?? "voice") !== "windows" &&
      (settings.outgoingRoute ?? "windows") !== "windows" &&
      !settings.speech) ||
      settings.audio,
    "invalid_arguments",
    "Windows call routing requires explicit Windows audio settings.",
  );
  requireThat(
    settings.incomingInitialPrompt === undefined ||
      Buffer.byteLength(settings.incomingInitialPrompt) <= 32000,
    "invalid_arguments",
    "Initial Voice prompt exceeds the native session text bound.",
  );
  requireThat(
    settings.credentialsPath === null || isAbsolute(settings.credentialsPath),
    "invalid_arguments",
    "Phone credentials need an explicit absolute protected file path.",
  );
  requireThat(
    settings.registration === null || settings.credentialsPath !== null,
    "invalid_arguments",
    "SIP registration requires its protected credential file.",
  );
  requireThat(
    settings.incomingPrincipalId === null ||
      settings.policy.incoming.length > 0 ||
      (settings.policy.challengedIncoming?.length ?? 0) > 0,
    "invalid_arguments",
    "Automatic incoming acceptance requires a configured incoming policy.",
  );
  requireThat(
    new Set(
      [
        ...settings.policy.incoming,
        ...(settings.policy.challengedIncoming ?? []),
      ].map((item) => item.peerAddress),
    ).size <= 32,
    "invalid_arguments",
    "Native incoming peer inventory exceeds its bound.",
  );
  return structuredClone(settings);
}
export async function phoneConfigurationSection(
  path: string,
  section: "sip" | "access" | "speech",
): Promise<unknown> {
  requireThat(
    isAbsolute(path),
    "invalid_arguments",
    "Phone configuration requires an absolute protected path.",
  );
  const value = await jsonFile<Record<string, unknown>>(path, 128 * 1024);
  requireThat(
    value &&
      value["schemaVersion"] === 1 &&
      value[section] &&
      typeof value[section] === "object" &&
      !Array.isArray(value[section]),
    "invalid_arguments",
    "Phone service configuration is invalid.",
  );
  return value[section];
}
/** Only the protected native pipe receives the values; public tools cannot supply or read them. */
export async function phoneCredentials(
  path: string | null,
): Promise<{ username: string | null; password: string | null }> {
  if (path === null) return { username: null, password: null };
  const value = (await phoneConfigurationSection(path, "sip")) as {
    username: string | null;
    password: string | null;
  };
  validatePhoneService("PhoneCredentials", value);
  return value;
}
