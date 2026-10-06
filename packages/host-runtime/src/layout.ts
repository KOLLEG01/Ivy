import { homedir } from "node:os";
import {
  join,
  resolve,
  isAbsolute,
  posix,
  win32,
  dirname,
  basename,
} from "node:path";
import { realpath } from "node:fs/promises";
import { requireThat } from "../../contracts/src/errors.js";
import type { Host } from "../../contracts/src/generated.js";
import { defaultServiceSettings } from "./default-settings.js";

export function accountHome(
  environment: NodeJS.ProcessEnv = process.env,
  platform: NodeJS.Platform = process.platform,
): string {
  const paths = platform === "win32" ? win32 : posix;
  const home =
    environment[platform === "win32" ? "USERPROFILE" : "HOME"] || homedir();
  requireThat(
    paths.isAbsolute(home),
    "invalid_arguments",
    "The executing account must have an absolute user home.",
  );
  return paths.normalize(home);
}
export function defaultHostConfigPath(
  environment: NodeJS.ProcessEnv = process.env,
  platform: NodeJS.Platform = process.platform,
): string {
  return (platform === "win32" ? win32 : posix).join(
    accountHome(environment, platform),
    ".ivy",
    "config.json",
  );
}
export function hostConfigurationPath(config: Host.HostConfig): string {
  return config.configPath ?? join(config.runtimeRoot, "host.json");
}
export async function resolvedFuturePath(path: string): Promise<string> {
  let ancestor = resolve(path);
  const suffix: string[] = [];
  while (true) {
    try {
      return join(await realpath(ancestor), ...suffix.reverse());
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
      const parent = dirname(ancestor);
      requireThat(
        parent !== ancestor,
        "target_conflict",
        "Storage path has no existing ancestor.",
      );
      suffix.push(basename(ancestor));
      ancestor = parent;
    }
  }
}
export function servicePaths(
  config: Host.HostConfig,
  instance: Host.Instance | string,
) {
  const selected =
    typeof instance === "string"
      ? config.instances.find((value) => value.instanceId === instance)
      : instance;
  requireThat(
    selected,
    "not_found",
    "Unknown service instance for path resolution.",
  );
  const base = config.servicesRoot
    ? join(config.servicesRoot, selected.componentId)
    : join(config.runtimeRoot, "instances", selected.instanceId);
  return (
    selected.paths ?? {
      data: join(base, "data"),
      work: join(base, "work"),
      logs: join(base, "logs"),
    }
  );
}
export function agentManagerAccountSettings(
  settings: Host.Instance["settings"],
  home = accountHome(),
  joinPath: (...parts: string[]) => string = join,
): Host.Instance["settings"] {
  return {
    ...settings,
    projectRoot: settings["projectRoot"] ?? joinPath(home, "projects"),
    internalProjectRoot:
      settings["internalProjectRoot"] ??
      joinPath(home, ".ivy", "codex-projects"),
    ...((settings["appServer"] as { mode?: string } | undefined)?.mode ===
    "claude-adapter"
      ? {}
      : {
          skillsRoot:
            settings["skillsRoot"] ??
            joinPath(
              String(
                settings["codexHome"] ??
                  settings["nativeHome"] ??
                  joinPath(home, ".codex"),
              ),
              "skills",
            ),
        }),
  };
}
/** Resolve once at the host boundary. Explicit old roots retain the original layout. */
export function resolveHostConfiguration(
  input: Host.HostConfigInput,
  sourcePath: string,
  environment: NodeJS.ProcessEnv = process.env,
  platform: NodeJS.Platform = process.platform,
): Host.HostConfig {
  const paths = platform === "win32" ? win32 : posix;
  const home = accountHome(environment, platform);
  const modern =
    input.ivyRoot !== undefined ||
    input.servicesRoot !== undefined ||
    input.runtimeRoot === undefined ||
    paths.normalize(sourcePath) ===
      defaultHostConfigPath(environment, platform);
  const ivyRoot = input.ivyRoot ?? paths.join(home, ".ivy");
  const config: Host.HostConfig = {
    ...input,
    runtimeRoot: input.runtimeRoot ?? paths.join(ivyRoot, "runtime"),
    artifactRoot: input.artifactRoot ?? paths.join(ivyRoot, "artifacts"),
    stagingRoot: input.stagingRoot ?? paths.join(ivyRoot, "staging"),
    ...(modern
      ? {
          ivyRoot,
          servicesRoot: input.servicesRoot ?? paths.join(ivyRoot, "services"),
          configPath: input.configPath ?? sourcePath,
        }
      : {}),
  };
  config.packageUpdates ??= { intervalSeconds: 60 };
  config.configurationUpdates ??= { intervalSeconds: 60 };
  config.instances = input.instances.map((instance) => {
    let settings = defaultServiceSettings(instance, {
      ivyRoot,
      configPath: sourcePath,
      join: paths.join,
    });
    if (instance.componentId === "agent-manager") {
      const claude =
        (settings["appServer"] as { mode?: string } | undefined)?.mode ===
        "claude-adapter";
      const dataRoot =
        instance.paths?.data ??
        (config.servicesRoot
          ? paths.join(config.servicesRoot, instance.componentId, "data")
          : paths.join(
              config.runtimeRoot,
              "instances",
              instance.instanceId,
              "data",
            ));
      const nativeHome = claude
        ? paths.join(dataRoot, "codex-home")
        : paths.join(ivyRoot, "codex", instance.instanceId);
      settings = agentManagerAccountSettings(
        {
          ...settings,
          ...(settings["codexHome"] === undefined &&
          settings["nativeHome"] === undefined
            ? { codexHome: nativeHome }
            : {}),
          internalProjectRoot:
            settings["internalProjectRoot"] ??
            paths.join(ivyRoot, "codex-projects"),
          appServer: settings["appServer"] ?? {
            mode: platform === "win32" ? "external-proxy" : "owned-stdio",
          },
        },
        home,
        paths.join,
      );
    }
    return {
      ...instance,
      settings,
      ...(instance.componentId === "service-manager" &&
      instance.engine === "process" &&
      platform === "win32" &&
      !instance.process
        ? { process: { allowWindowsBreakaway: true } }
        : {}),
    };
  });
  for (const value of [
    config.runtimeRoot,
    config.artifactRoot,
    config.stagingRoot,
    config.ivyRoot,
    config.servicesRoot,
    config.configPath,
  ])
    requireThat(
      value === undefined || paths.isAbsolute(value),
      "invalid_arguments",
      "Host directories and configuration path must be absolute.",
    );
  return config;
}

/** Permanent storage outside runtime is captured separately, never treated as staging/cache. */
export function hostStorageAreas(
  config: Host.HostConfig,
): { key: string; path: string }[] {
  const areas = [{ key: "state", path: config.runtimeRoot }];
  const contains = (root: string, path: string) => {
    const local = (process.platform === "win32" ? win32 : posix).relative(
      root,
      path,
    );
    return !local.startsWith("..") && !isAbsolute(local);
  };
  for (const instance of [...config.instances].sort((a, b) =>
    a.instanceId.localeCompare(b.instanceId),
  )) {
    for (const [kind, path] of Object.entries(servicePaths(config, instance)))
      if (!areas.some((area) => contains(area.path, path)))
        areas.push({
          key: "services/" + instance.instanceId + "/" + kind,
          path,
        });
    if (instance.componentId === "agent-manager") {
      const configured = instance.settings["internalProjectRoot"];
      const path =
        typeof configured === "string"
          ? configured
          : join(accountHome(), ".ivy", "codex-projects");
      if (!areas.some((area) => contains(area.path, path)))
        areas.push({ key: "projects/" + instance.instanceId, path });
    }
  }
  return areas;
}
export function storagePath(config: Host.HostConfig, path: string): string {
  const area = hostStorageAreas(config).find((area) => {
    const local = (process.platform === "win32" ? win32 : posix).relative(
      area.path,
      path,
    );
    return !local.startsWith("..") && !isAbsolute(local);
  });
  requireThat(area, "target_conflict", "Path is not in captured host storage.");
  return (
    area.key +
    "/" +
    (process.platform === "win32" ? win32 : posix)
      .relative(area.path, path)
      .split(/[\\/]/)
      .join("/")
  );
}
export function storageFile(
  config: Host.HostConfig,
  key: string,
  areas = hostStorageAreas(config),
): string {
  const area = areas.find((area) => key.startsWith(area.key + "/"));
  requireThat(area, "target_conflict", "Unknown host storage area.");
  const local = key.slice(area.key.length + 1);
  requireThat(
    local.length > 0 &&
      !local.split(/[\\/]/).includes("..") &&
      !isAbsolute(local),
    "target_conflict",
    "Storage member leaves its area.",
  );
  return join(area.path, local);
}
