import {
  requireThat
} from "./chunk-BO4WKKA7.js";

// packages/host-runtime/src/layout.ts
import { homedir } from "node:os";
import { join, resolve, isAbsolute, posix, win32, dirname, basename } from "node:path";
import { realpath } from "node:fs/promises";
function accountHome(environment = process.env, platform = process.platform) {
  const paths = platform === "win32" ? win32 : posix;
  const home = environment[platform === "win32" ? "USERPROFILE" : "HOME"] || homedir();
  requireThat(paths.isAbsolute(home), "invalid_arguments", "The executing account must have an absolute user home.");
  return paths.normalize(home);
}
function defaultHostConfigPath(environment = process.env, platform = process.platform) {
  return (platform === "win32" ? win32 : posix).join(accountHome(environment, platform), ".ivy", "config.json");
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
      requireThat(parent !== ancestor, "target_conflict", "Storage path has no existing ancestor.");
      suffix.push(basename(ancestor));
      ancestor = parent;
    }
  }
}
function servicePaths(config, instance) {
  const selected = typeof instance === "string" ? config.instances.find((value) => value.instanceId === instance) : instance;
  requireThat(selected, "not_found", "Unknown service instance for path resolution.");
  const base = config.servicesRoot ? join(config.servicesRoot, selected.componentId) : join(config.runtimeRoot, "instances", selected.instanceId);
  return selected.paths ?? { data: join(base, "data"), work: join(base, "work"), logs: join(base, "logs") };
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
    ...modern ? { ivyRoot, servicesRoot: input.servicesRoot ?? paths.join(ivyRoot, "services"), configPath: input.configPath ?? sourcePath } : {}
  };
  config.instances = input.instances.map((instance) => instance.componentId === "agent-manager" ? {
    ...instance,
    settings: {
      ...instance.settings,
      ...instance.settings["codexHome"] === void 0 && instance.settings["nativeHome"] === void 0 ? { codexHome: paths.join(home, ".codex") } : {},
      appServer: instance.settings["appServer"] ?? { mode: "external-proxy" },
      projectRoot: instance.settings["projectRoot"] ?? paths.join(home, "projects"),
      internalProjectRoot: instance.settings["internalProjectRoot"] ?? paths.join(home, ".ivy", "codex-projects"),
      skillsRoot: instance.settings["skillsRoot"] ?? paths.join(home, ".agents", "skills")
    }
  } : instance);
  for (const value of [config.runtimeRoot, config.artifactRoot, config.stagingRoot, config.ivyRoot, config.servicesRoot, config.configPath])
    requireThat(value === void 0 || paths.isAbsolute(value), "invalid_arguments", "Host directories and configuration path must be absolute.");
  return config;
}
function hostStorageAreas(config) {
  const areas = [{ key: "state", path: config.runtimeRoot }];
  const contains = (root, path) => {
    const local = (process.platform === "win32" ? win32 : posix).relative(root, path);
    return !local.startsWith("..") && !isAbsolute(local);
  };
  for (const instance of [...config.instances].sort((a, b) => a.instanceId.localeCompare(b.instanceId))) {
    for (const [kind, path] of Object.entries(servicePaths(config, instance)))
      if (!areas.some((area) => contains(area.path, path))) areas.push({ key: "services/" + instance.instanceId + "/" + kind, path });
    if (instance.componentId === "agent-manager") {
      const configured = instance.settings["internalProjectRoot"];
      const path = typeof configured === "string" ? configured : join(accountHome(), ".ivy", "codex-projects");
      if (!areas.some((area) => contains(area.path, path))) areas.push({ key: "projects/" + instance.instanceId, path });
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
//# sourceMappingURL=chunk-NYH76QJX.js.map
