import type { Host } from "../../contracts/src/generated.js";

type Settings = Host.Instance["settings"];
type Context = {
  ivyRoot: string;
  configPath: string;
  join: (...parts: string[]) => string;
};

/** Fill omitted fields only. Explicit nulls, arrays and operator values are authoritative. */
export function withDefaults(
  defaults: Settings,
  configured: Settings,
): Settings {
  const result = structuredClone(defaults);
  for (const [key, value] of Object.entries(configured)) {
    const previous = result[key];
    const selected =
      previous &&
      typeof previous === "object" &&
      !Array.isArray(previous) &&
      value &&
      typeof value === "object" &&
      !Array.isArray(value)
        ? withDefaults(previous, value)
        : structuredClone(value);
    Object.defineProperty(result, key, {
      value: selected,
      enumerable: true,
      configurable: true,
      writable: true,
    });
  }
  return result;
}

/** Portable service policy; account identities, credentials and devices are installation inputs. */
export function defaultServiceSettings(
  instance: Host.Instance,
  context: Context,
): Settings {
  const { ivyRoot, configPath, join } = context;
  let defaults: Settings;
  switch (instance.componentId) {
    case "host-executor":
    case "service-manager":
      defaults = { hostConfigPath: configPath };
      break;
    case "hive":
      defaults = {
        listenHost: "127.0.0.1",
        listenPort: 39081,
        backup: {
          enabled: true,
          directory: join(ivyRoot, "backups", "hive"),
          intervalHours: 24,
          retain: 7,
        },
      };
      break;
    case "agent-manager": {
      const claude =
        (instance.settings["appServer"] as { mode?: string } | undefined)
          ?.mode === "claude-adapter";
      defaults = {
        patcherEnabled: true,
        capabilities: [
          {
            key: claude ? "general-claude" : "general-codex",
            label: claude ? "General Claude work" : "General Codex work",
          },
        ],
        limits: {
          maxOperations: 100000,
          maxJournalBytes: 536870912,
          maxPendingInputs: 64,
          maxNotificationBytes: 16777216,
        },
      };
      break;
    }
    case "task-board":
      defaults = {
        rootObjectId: null,
        scheduler: { enabled: true, intervalMs: 2000, pageSize: 50 },
        phoneTarget: null,
      };
      break;
    case "chat-bridge":
      defaults = { pollMs: 1000, language: "en" };
      break;
    case "secretary":
      defaults = {
        sources: [],
        pollMs: 2000,
        recordsPerTick: 2,
        policy: {
          minimumMainUrgency: "normal",
          researchMaxMinutes: 5,
          silentTime: null,
          urgentBypass: false,
          voiceEscalation: null,
          whatsappQuestionsOnly: true,
        },
      };
      break;
    case "data-collector":
      defaults = { rootName: "DataCollector", maximumMemoryMb: 512 };
      break;
    case "dashboards":
      defaults = { rootObjectId: null };
      break;
    case "phone-bridge":
      defaults = {
        native: { executable: "dist/native/phone/Ivy.PhoneRuntime.exe" },
        binding: {
          address: "127.0.0.1",
          port: 5070,
          transport: "udp",
          incomingRingSeconds: 60,
          outgoingOfferMode: "auto",
        },
        codecs: {
          packetMs: 20,
          preferences: ["EVS", "G722", "PCMA", "PCMU"],
          receiveReorderMs: 20,
        },
        audio: {
          captureBufferMs: 20,
          queueMs: 80,
          renderLatencyMs: 20,
          sourceMode: "system_excluding_runtime",
        },
        policy: { incoming: [], recipients: [] },
        registration: null,
        credentialsPath: null,
        incomingPrincipalId: null,
        ringSeconds: 60,
        pollMs: 1000,
      };
      break;
    default:
      return structuredClone(instance.settings);
  }
  return withDefaults(defaults, instance.settings);
}
