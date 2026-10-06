import {
  mkdir,
  realpath,
  rename,
  chmod,
  copyFile,
  lstat,
  rm,
} from "node:fs/promises";
import { join, dirname, resolve, isAbsolute, relative, sep } from "node:path";
import { randomUUID } from "node:crypto";
import { spawnSync } from "node:child_process";
import { atomicJson, jsonFile, inside } from "./config.js";
import { requireThat } from "../../contracts/src/errors.js";
import { validateHost } from "../../contracts/src/host-validation.js";
import type { Host } from "../../contracts/src/generated.js";
import { requireStorageSpace } from "./storage-space.js";
import { hostStorageAreas, hostConfigurationPath } from "./layout.js";
import { mapConcurrent } from "./concurrency.js";

type SnapshotManifest = Host.SnapshotManifest;
export async function verifySnapshotMetadata(
  snapshot: Host.SourceSnapshot,
  config: Host.HostConfig,
): Promise<SnapshotManifest> {
  validateHost("SourceSnapshot", snapshot);
  requireThat(
    inside(config.stagingRoot, snapshot.sourceRoot) &&
      inside(config.stagingRoot, snapshot.manifestPath),
    "target_conflict",
    "Source snapshot is outside this host staging root.",
  );
  const manifest = await jsonFile<SnapshotManifest>(
    snapshot.manifestPath,
    8 * 1024 * 1024,
  );
  validateHost("SnapshotManifest", manifest);
  requireThat(
    manifest.schemaVersion === 1 &&
      manifest.snapshot.snapshotId === snapshot.snapshotId &&
      manifest.snapshot.sourceRoot === snapshot.sourceRoot,
    "artifact_changed",
    "Snapshot metadata changed.",
  );
  return manifest;
}
export async function verifySnapshot(
  snapshot: Host.SourceSnapshot,
  config: Host.HostConfig,
): Promise<void> {
  await verifySnapshotMetadata(snapshot, config);
}

export async function sourceBuildPlan(
  root: string,
  componentId: string,
): Promise<Host.BuildPlan> {
  requireThat(
    /^[a-z][a-z0-9-]*$/.test(componentId),
    "invalid_arguments",
    "Component ID is not a source component name.",
  );
  const plans: Host.BuildPlan[] = [];
  for (const owner of ["services", "ui"]) {
    try {
      const plan = await jsonFile<Host.BuildPlan>(
        join(root, owner, componentId, "deploy.json"),
      );
      validateHost("BuildPlan", plan);
      requireThat(
        plan.componentId === componentId,
        "invalid_arguments",
        "Source deploy plan does not match its component directory.",
      );
      plans.push(plan);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
  }
  requireThat(
    plans.length > 0,
    "not_found",
    "Source component has no deploy plan.",
  );
  requireThat(
    plans.length === 1,
    "target_conflict",
    "Source component has more than one deploy plan.",
  );
  return plans[0]!;
}

const sourceTopLevels = new Set([
  "packages",
  "services",
  "ui",
  "instructions",
  "tools",
  "specs",
  "docs",
  "tests",
  "acceptance",
]);
const sourceRootFiles = new Set([
  "package.json",
  "package-lock.json",
  "tsconfig.json",
  "tsconfig.web.json",
  "tsconfig.runtime.json",
  "tsconfig.test.json",
  "README.md",
  "LICENSE",
  ".npmrc",
  ".gitattributes",
  ".gitignore",
]);
function sourcePaths(root: string, gitExecutable: string): string[] {
  const git = spawnSync(
    gitExecutable,
    ["ls-files", "--cached", "--others", "--exclude-standard", "-z"],
    {
      cwd: root,
      encoding: "utf8",
      timeout: 10_000,
      windowsHide: true,
      maxBuffer: 64 * 1024 * 1024,
    },
  );
  requireThat(
    !git.error && git.status === 0,
    "invalid_arguments",
    "Source must be an accessible Git checkout.",
  );
  const paths = [...new Set(git.stdout.split("\0").filter(Boolean))]
    .filter(
      (path) =>
        sourceRootFiles.has(path) ||
        sourceTopLevels.has(path.split("/")[0] ?? ""),
    )
    .filter(
      (path) =>
        !/(^|\/)(node_modules|dist|\.local|config\.json|\.env(?:\..*)?)($|\/)/.test(
          path,
        ),
    )
    .sort();
  requireThat(
    paths.length <= 20_000,
    "limit_exceeded",
    "Source copy exceeds its file limit.",
  );
  return paths;
}

export async function captureSource(
  original: string,
  config: Host.HostConfig,
): Promise<Host.SourceSnapshot> {
  requireThat(
    config.executables["git"],
    "invalid_arguments",
    "Host preparation requires an explicit Git executable.",
  );
  requireThat(
    isAbsolute(original),
    "invalid_arguments",
    "Source must be an explicit absolute checkout path.",
  );
  const source = await realpath(original);
  requireThat(
    [
      ...hostStorageAreas(config).map((area) => area.path),
      config.artifactRoot,
      config.stagingRoot,
    ].every((root) => !inside(root, source) && !inside(source, root)) &&
      !inside(source, hostConfigurationPath(config)),
    "target_conflict",
    "Release source must stay separate from private service state, internal projects, configuration, artifacts and staging.",
  );
  const paths = sourcePaths(source, config.executables["git"]),
    snapshotId = randomUUID();
  requireThat(
    paths.includes("package-lock.json") && paths.includes("package.json"),
    "invalid_arguments",
    "A source candidate needs its package and dependency lock.",
  );
  const checkedDirectories = new Map<string, Promise<void>>();
  const files = (
    await mapConcurrent(paths, 8, async (path) => {
      const absolute = resolve(source, path),
        local = relative(source, absolute);
      const metadata = await lstat(absolute).catch((error) => {
        if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
        throw error;
      });
      if (!metadata) return null; // A tracked deletion is intentionally absent from this fixed snapshot.
      requireThat(
        !isAbsolute(local) &&
          local !== ".." &&
          !local.startsWith(".." + sep) &&
          metadata.isFile() &&
          !metadata.isSymbolicLink(),
        "invalid_arguments",
        "Source input must be a contained regular file.",
      );
      const directory = dirname(absolute);
      let checked = checkedDirectories.get(directory);
      if (!checked) {
        checked = realpath(directory).then((actual) => {
          requireThat(
            inside(source, actual),
            "invalid_arguments",
            "Source input leaves its checkout.",
          );
        });
        checkedDirectories.set(directory, checked);
      }
      await checked;
      requireThat(
        metadata.size <= 64 * 1024 * 1024,
        "limit_exceeded",
        "Source copy exceeds its per-file byte limit.",
      );
      return {
        path,
        absolute,
        mode: metadata.mode & 0o777,
        bytes: metadata.size,
      };
    })
  ).filter((file): file is NonNullable<typeof file> => file !== null);
  const totalBytes = files.reduce((sum, file) => sum + file.bytes, 0);
  requireThat(
    totalBytes <= 1024 * 1024 * 1024,
    "limit_exceeded",
    "Source copy exceeds its byte limit.",
  );
  await mkdir(config.stagingRoot, { recursive: true });
  await requireStorageSpace(
    config.stagingRoot,
    Math.max(64 * 1024 * 1024, totalBytes * 2),
  );
  const finalRoot = join(config.stagingRoot, "sources", snapshotId),
    incoming = join(config.stagingRoot, "incoming-" + randomUUID());
  try {
    const sourceRoot = join(incoming, "source");
    await mkdir(sourceRoot, { recursive: true });
    const directories = [
      ...new Set(files.map((file) => dirname(resolve(sourceRoot, file.path)))),
    ];
    await mapConcurrent(directories, 8, (directory) =>
      mkdir(directory, { recursive: true }),
    );
    await mapConcurrent(files, 8, async (file) => {
      const destination = resolve(sourceRoot, file.path);
      requireThat(
        inside(sourceRoot, destination),
        "invalid_arguments",
        "Source file path leaves its snapshot.",
      );
      await copyFile(file.absolute, destination);
      if (process.platform !== "win32") await chmod(destination, file.mode);
    });
    const snapshot: Host.SourceSnapshot = {
      snapshotId,
      sourceRoot: join(finalRoot, "source"),
      originalRoot: source,
      manifestPath: join(finalRoot, "snapshot.json"),
      capturedAt: new Date().toISOString(),
    };
    await atomicJson(join(incoming, "snapshot.json"), {
      schemaVersion: 1,
      snapshot,
    } satisfies SnapshotManifest);
    await mkdir(dirname(finalRoot), { recursive: true });
    requireThat(
      inside(config.stagingRoot, incoming) &&
        inside(config.stagingRoot, finalRoot),
      "invalid_arguments",
      "Snapshot promotion leaves its staging root.",
    );
    await rename(incoming, finalRoot);
    await verifySnapshotMetadata(snapshot, config);
    return snapshot;
  } catch (error) {
    await rm(incoming, { recursive: true, force: true });
    throw error;
  }
}
