import {
  requireThat
} from "./chunk-GPVLKTRL.js";

// packages/host-runtime/src/layout.ts
import { homedir } from "node:os";
import {
  join,
  resolve,
  isAbsolute,
  posix,
  win32,
  dirname,
  basename
} from "node:path";
import { realpath } from "node:fs/promises";

// packages/host-runtime/src/default-settings.ts
function withDefaults(defaults, configured) {
  const result = structuredClone(defaults);
  for (const [key, value] of Object.entries(configured)) {
    const previous = result[key];
    const selected = previous && typeof previous === "object" && !Array.isArray(previous) && value && typeof value === "object" && !Array.isArray(value) ? withDefaults(previous, value) : structuredClone(value);
    Object.defineProperty(result, key, {
      value: selected,
      enumerable: true,
      configurable: true,
      writable: true
    });
  }
  return result;
}
function defaultServiceSettings(instance, context) {
  const { ivyRoot, configPath, join: join2 } = context;
  let defaults;
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
          directory: join2(ivyRoot, "backups", "hive"),
          intervalHours: 24,
          retain: 7
        }
      };
      break;
    case "agent-manager": {
      const claude = instance.settings["appServer"]?.mode === "claude-adapter";
      defaults = {
        patcherEnabled: true,
        capabilities: [
          {
            key: claude ? "general-claude" : "general-codex",
            label: claude ? "General Claude work" : "General Codex work"
          }
        ],
        limits: {
          maxOperations: 1e5,
          maxJournalBytes: 536870912,
          maxPendingInputs: 64,
          maxNotificationBytes: 16777216
        }
      };
      break;
    }
    case "task-board":
      defaults = {
        rootObjectId: null,
        scheduler: { enabled: true, intervalMs: 2e3, pageSize: 50, stallWarningMs: 3e5 },
        phoneTarget: null
      };
      break;
    case "chat-bridge":
      defaults = { pollMs: 1e3, language: "en" };
      break;
    case "secretary":
      defaults = {
        sources: [],
        pollMs: 2e3,
        recordsPerTick: 2,
        policy: {
          minimumMainUrgency: "normal",
          researchMaxMinutes: 5,
          silentTime: null,
          urgentBypass: false,
          voiceEscalation: null,
          whatsappQuestionsOnly: true
        }
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
          outgoingOfferMode: "auto"
        },
        codecs: {
          packetMs: 20,
          preferences: ["EVS", "G722", "PCMA", "PCMU"],
          receiveReorderMs: 20
        },
        audio: {
          captureBufferMs: 20,
          queueMs: 80,
          renderLatencyMs: 20,
          sourceMode: "system_excluding_runtime"
        },
        policy: { incoming: [], recipients: [] },
        registration: null,
        credentialsPath: null,
        incomingPrincipalId: null,
        ringSeconds: 60,
        pollMs: 1e3
      };
      break;
    default:
      return structuredClone(instance.settings);
  }
  return withDefaults(defaults, instance.settings);
}

// packages/host-runtime/src/layout.ts
function accountHome(environment = process.env, platform = process.platform) {
  const paths = platform === "win32" ? win32 : posix;
  const home = environment[platform === "win32" ? "USERPROFILE" : "HOME"] || homedir();
  requireThat(
    paths.isAbsolute(home),
    "invalid_arguments",
    "The executing account must have an absolute user home."
  );
  return paths.normalize(home);
}
function defaultHostConfigPath(environment = process.env, platform = process.platform) {
  return (platform === "win32" ? win32 : posix).join(
    accountHome(environment, platform),
    ".ivy",
    "config.json"
  );
}
async function resolvedFuturePath(path) {
  let ancestor = resolve(path);
  const suffix = [];
  while (true) {
    try {
      return join(await realpath(ancestor), ...suffix.reverse());
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
      const parent = dirname(ancestor);
      requireThat(
        parent !== ancestor,
        "target_conflict",
        "Storage path has no existing ancestor."
      );
      suffix.push(basename(ancestor));
      ancestor = parent;
    }
  }
}
function servicePaths(config, instance) {
  const selected = typeof instance === "string" ? config.instances.find((value) => value.instanceId === instance) : instance;
  requireThat(
    selected,
    "not_found",
    "Unknown service instance for path resolution."
  );
  const base = config.servicesRoot ? join(config.servicesRoot, selected.componentId) : join(config.runtimeRoot, "instances", selected.instanceId);
  return selected.paths ?? {
    data: join(base, "data"),
    work: join(base, "work"),
    logs: join(base, "logs")
  };
}
function agentManagerAccountSettings(settings, home = accountHome(), joinPath = join) {
  return {
    ...settings,
    projectRoot: settings["projectRoot"] ?? joinPath(home, "projects"),
    internalProjectRoot: settings["internalProjectRoot"] ?? joinPath(home, ".ivy", "codex-projects"),
    ...settings["appServer"]?.mode === "claude-adapter" ? {} : {
      skillsRoot: settings["skillsRoot"] ?? joinPath(
        String(
          settings["codexHome"] ?? settings["nativeHome"] ?? joinPath(home, ".codex")
        ),
        "skills"
      )
    }
  };
}
function resolveHostConfiguration(input, sourcePath, environment = process.env, platform = process.platform) {
  const paths = platform === "win32" ? win32 : posix;
  const home = accountHome(environment, platform);
  const modern = input.ivyRoot !== void 0 || input.servicesRoot !== void 0 || input.runtimeRoot === void 0 || paths.normalize(sourcePath) === defaultHostConfigPath(environment, platform);
  const ivyRoot = input.ivyRoot ?? paths.join(home, ".ivy");
  const config = {
    ...input,
    runtimeRoot: input.runtimeRoot ?? paths.join(ivyRoot, "runtime"),
    artifactRoot: input.artifactRoot ?? paths.join(ivyRoot, "artifacts"),
    stagingRoot: input.stagingRoot ?? paths.join(ivyRoot, "staging"),
    ...modern ? {
      ivyRoot,
      servicesRoot: input.servicesRoot ?? paths.join(ivyRoot, "services"),
      configPath: input.configPath ?? sourcePath
    } : {}
  };
  config.packageUpdates ??= { intervalSeconds: 60 };
  config.configurationUpdates ??= { intervalSeconds: 60 };
  config.instances = input.instances.map((instance) => {
    let settings = defaultServiceSettings(instance, {
      ivyRoot,
      configPath: sourcePath,
      join: paths.join
    });
    if (instance.componentId === "agent-manager") {
      const claude = settings["appServer"]?.mode === "claude-adapter";
      const dataRoot = instance.paths?.data ?? (config.servicesRoot ? paths.join(config.servicesRoot, instance.componentId, "data") : paths.join(
        config.runtimeRoot,
        "instances",
        instance.instanceId,
        "data"
      ));
      const nativeHome = claude ? paths.join(dataRoot, "codex-home") : paths.join(ivyRoot, "codex", instance.instanceId);
      settings = agentManagerAccountSettings(
        {
          ...settings,
          ...settings["codexHome"] === void 0 && settings["nativeHome"] === void 0 ? { codexHome: nativeHome } : {},
          internalProjectRoot: settings["internalProjectRoot"] ?? paths.join(ivyRoot, "codex-projects"),
          appServer: settings["appServer"] ?? {
            mode: platform === "win32" ? "external-proxy" : "owned-stdio"
          }
        },
        home,
        paths.join
      );
    }
    return {
      ...instance,
      settings,
      ...instance.componentId === "service-manager" && instance.engine === "process" && platform === "win32" && !instance.process ? { process: { allowWindowsBreakaway: true } } : {}
    };
  });
  for (const value of [
    config.runtimeRoot,
    config.artifactRoot,
    config.stagingRoot,
    config.ivyRoot,
    config.servicesRoot,
    config.configPath
  ])
    requireThat(
      value === void 0 || paths.isAbsolute(value),
      "invalid_arguments",
      "Host directories and configuration path must be absolute."
    );
  return config;
}
function hostStorageAreas(config) {
  const areas = [{ key: "state", path: config.runtimeRoot }];
  const contains = (root, path) => {
    const local = (process.platform === "win32" ? win32 : posix).relative(
      root,
      path
    );
    return !local.startsWith("..") && !isAbsolute(local);
  };
  for (const instance of [...config.instances].sort(
    (a, b) => a.instanceId.localeCompare(b.instanceId)
  )) {
    for (const [kind, path] of Object.entries(servicePaths(config, instance)))
      if (!areas.some((area) => contains(area.path, path)))
        areas.push({
          key: "services/" + instance.instanceId + "/" + kind,
          path
        });
    if (instance.componentId === "agent-manager") {
      const configured = instance.settings["internalProjectRoot"];
      const path = typeof configured === "string" ? configured : join(accountHome(), ".ivy", "codex-projects");
      if (!areas.some((area) => contains(area.path, path)))
        areas.push({ key: "projects/" + instance.instanceId, path });
    }
  }
  return areas;
}

export {
  accountHome,
  resolvedFuturePath,
  servicePaths,
  resolveHostConfiguration,
  hostStorageAreas
};
//# sourceMappingURL=chunk-ICDQHEWQ.js.map
