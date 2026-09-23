import {
  validateHost
} from "./chunk-JNUJKCR7.js";
import {
  IvyError,
  canonical,
  compareVersions,
  requireThat
} from "./chunk-BO4WKKA7.js";

// packages/host-runtime/src/config.ts
import { readFile, stat, mkdir, open, unlink } from "node:fs/promises";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { randomUUID } from "node:crypto";

// packages/host-runtime/src/atomic-file.ts
import { rename } from "node:fs/promises";
import { setTimeout as delay } from "node:timers/promises";
async function replaceFile(temporary, destination) {
  const end = Date.now() + 2e3;
  let attempts = 0;
  while (true) {
    try {
      await rename(temporary, destination);
      return;
    } catch (error) {
      if (process.platform !== "win32" || !["EPERM", "EACCES", "EBUSY"].includes(error.code ?? "") || Date.now() >= end) throw error;
      await delay(Math.min(100, 10 * 2 ** Math.min(attempts++, 4), end - Date.now()));
    }
  }
}

// packages/host-runtime/src/config.ts
async function jsonFile(path, limit = 1024 * 1024) {
  const metadata = await stat(path);
  requireThat(metadata.isFile() && metadata.size <= limit, "invalid_arguments", "Configuration must be a bounded regular file.");
  return JSON.parse(await readFile(path, "utf8"));
}
async function atomicJson(path, value) {
  await mkdir(dirname(path), { recursive: true });
  const temporary = path + "." + randomUUID() + ".tmp";
  const file = await open(temporary, "wx", 384);
  try {
    await file.writeFile(canonical(value) + "\n");
    await file.sync();
  } finally {
    await file.close();
  }
  try {
    await replaceFile(temporary, path);
  } catch (error) {
    await unlink(temporary).catch(() => void 0);
    throw error;
  }
  if (process.platform !== "win32") {
    const directory = await open(dirname(path), "r");
    try {
      await directory.sync();
    } finally {
      await directory.close();
    }
  }
}
function inside(root, path) {
  const local = relative(resolve(root), resolve(path));
  return local !== ".." && !local.startsWith("..\\") && !local.startsWith("../") && !isAbsolute(local);
}
async function instanceConfig(path) {
  const config = await jsonFile(path);
  validateHost("InstanceConfig", config);
  requireThat(isAbsolute(config.dataRoot) && isAbsolute(config.artifactRoot) && !inside(config.artifactRoot, config.dataRoot) && !inside(config.dataRoot, config.artifactRoot), "invalid_arguments", "Instance data and artifact roots must be separate absolute paths.");
  requireThat(process.versions.node.startsWith("24.") && Number(process.versions.node.split(".")[1]) >= 18, "unsupported_runtime", "This distribution requires pinned Node 24.18 or newer in the 24.x line.");
  const build = await jsonFile(join(config.artifactRoot, "dist", "build-info.json"));
  requireThat(build.buildId === config.buildId && build.version === config.version, "build_mismatch", "Configuration identity does not match the prepared executable artifact.");
  await mkdir(config.dataRoot, { recursive: true });
  for (const root of [config.workRoot, config.logsRoot]) if (root) {
    requireThat(isAbsolute(root) && !inside(config.artifactRoot, root) && !inside(root, config.artifactRoot), "invalid_arguments", "Service work/logs must stay outside executable artifacts.");
    await mkdir(root, { recursive: true });
  }
  return config;
}
function configurationPath() {
  const args = process.argv.slice(2), position = args.indexOf("--config");
  const path = position >= 0 ? args[position + 1] : process.env["IVY_INSTANCE_CONFIG"];
  requireThat(path && isAbsolute(path), "invalid_arguments", "Use --config ABSOLUTE_PATH or IVY_INSTANCE_CONFIG.");
  return path;
}

// packages/host-runtime/src/artifact.ts
import { createHash } from "node:crypto";
import { lstat, realpath, open as open2, readdir } from "node:fs/promises";
import { join as join2, relative as relative2 } from "node:path";

// packages/host-runtime/src/package-requirements.ts
function nodeVersion(value) {
  const normalized = value.startsWith("v") ? value.slice(1) : value;
  requireThat(
    /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(normalized),
    "unsupported_runtime",
    "Node runtime version is not an exact numeric version."
  );
  return normalized;
}
function runtimeRequirementsSatisfied(requirements, runtime = { node: process.version, os: process.platform, arch: process.arch }) {
  const match = /^>=(\d+\.\d+\.\d+) <(\d+\.\d+\.\d+)$/.exec(requirements.node);
  if (!match) throw new IvyError("invalid_arguments", "Package Node requirement is invalid.");
  const current = nodeVersion(runtime.node);
  return compareVersions(current, match[1]) >= 0 && compareVersions(current, match[2]) < 0 && (!requirements.os || requirements.os.includes(runtime.os)) && (!requirements.arch || requirements.arch.includes(runtime.arch));
}
function requireRuntimeRequirements(requirements) {
  requireThat(
    runtimeRequirementsSatisfied(requirements),
    "unsupported_runtime",
    `Package does not support ${process.platform}/${process.arch} on ${process.version}.`
  );
}

// packages/host-runtime/src/artifact.ts
async function hashFile(path, buffer) {
  const hash = createHash("sha256"), file = await open2(path, "r");
  try {
    for (; ; ) {
      const { bytesRead } = await file.read(buffer, 0, buffer.length, null);
      if (bytesRead === 0) break;
      hash.update(buffer.subarray(0, bytesRead));
    }
    return "sha256:" + hash.digest("hex");
  } finally {
    await file.close();
  }
}
var fileHash = (path) => hashFile(path, Buffer.allocUnsafe(4 * 1024 * 1024));
function releaseManifest(input) {
  const value = input;
  const manifest = {
    schemaVersion: value.schemaVersion,
    componentId: value.componentId,
    kind: value.kind,
    version: value.version,
    buildId: value.buildId,
    connectsToHive: value.connectsToHive,
    requirements: value.requirements,
    ...value.app === void 0 ? {} : { app: value.app },
    ...value.entrypoint === void 0 ? {} : { entrypoint: value.entrypoint },
    ...value.readiness === void 0 ? {} : { readiness: value.readiness },
    ...value.shutdown === void 0 ? {} : { shutdown: value.shutdown },
    ...value.restart === void 0 ? {} : { restart: value.restart },
    ...value.storage === void 0 ? {} : { storage: value.storage },
    ...value.runtimeReset === void 0 ? {} : { runtimeReset: value.runtimeReset }
  };
  validateHost("ReleaseManifest", manifest);
  return manifest;
}
async function verifyLaunchCandidate(candidate, config, entrypoint) {
  validateHost("Candidate", candidate);
  requireThat(inside(config.artifactRoot, await realpath(candidate.artifactRoot)), "target_conflict", "Launch leaves its artifact root.");
  for (const selected of ["dist/build-info.json", ...entrypoint?.startsWith("dist/") ? [entrypoint] : []]) {
    const local = join2(candidate.artifactRoot, selected), metadata = await lstat(local);
    requireThat(metadata.isFile() && !metadata.isSymbolicLink() && inside(candidate.artifactRoot, await realpath(local)), "artifact_changed", "Launch entry or stamp is unavailable.");
  }
  const info = await jsonFile(join2(candidate.artifactRoot, "dist/build-info.json"));
  requireThat(info.buildId === candidate.buildId, "build_mismatch", "Launch build differs from its prepared identity.");
}

export {
  jsonFile,
  atomicJson,
  inside,
  instanceConfig,
  configurationPath,
  requireRuntimeRequirements,
  fileHash,
  releaseManifest,
  verifyLaunchCandidate
};
//# sourceMappingURL=chunk-JROR37UL.js.map
