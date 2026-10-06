import { lstat, realpath, rm } from "node:fs/promises";
import { join, parse, resolve } from "node:path";
import {
  atomicJson,
  jsonFile,
  resolveCodexHome,
} from "../../../packages/sdk/src/host.js";
import { requireThat } from "../../../packages/sdk/src/node.js";
import type { Agent } from "../../../packages/sdk/src/node.js";

interface ResetMarker {
  schemaVersion: 1;
  resetId: string;
  runtimeEpoch: string;
  completedAt: string;
}

const resetPaths = [
  "sessions",
  "archived_sessions",
  "history.jsonl",
  "state_5.sqlite",
  "state_5.sqlite-wal",
  "state_5.sqlite-shm",
  "shell_snapshots",
  "tmp",
] as const;

const validMarker = (value: ResetMarker): void => {
  requireThat(
    value?.schemaVersion === 1 &&
      typeof value.resetId === "string" &&
      value.resetId.length > 0 &&
      value.resetId.length <= 128 &&
      typeof value.runtimeEpoch === "string" &&
      value.runtimeEpoch.length > 0 &&
      value.runtimeEpoch.length <= 128 &&
      typeof value.completedAt === "string" &&
      new Date(value.completedAt).toISOString() === value.completedAt,
    "invalid_arguments",
    "AgentManager runtime reset marker is invalid.",
  );
};

/** Apply the service-owned part of a host reset before any native process can start. */
export async function applyAgentRuntimeReset(
  dataRoot: string,
  settings: Agent.Settings,
): Promise<void> {
  const markerPath = join(dataRoot, "runtime-reset.json"),
    appliedPath = join(dataRoot, "runtime-reset-applied.json");
  const marker = await jsonFile<ResetMarker>(markerPath).catch(
    (error: NodeJS.ErrnoException) => {
      if (error.code === "ENOENT") return null;
      throw error;
    },
  );
  if (!marker) return;
  validMarker(marker);
  const applied = await jsonFile<ResetMarker>(appliedPath).catch(() => null);
  if (
    applied?.schemaVersion === 1 &&
    applied.resetId === marker.resetId &&
    applied.runtimeEpoch === marker.runtimeEpoch
  )
    return;
  requireThat(
    settings.codexHome !== undefined || settings.nativeHome !== undefined,
    "unsupported_storage",
    "Runtime reset requires an explicitly configured Codex home.",
  );
  const configured = resolve(resolveCodexHome(settings).path),
    root = parse(configured).root;
  requireThat(
    configured !== root,
    "target_conflict",
    "Codex home cannot be a filesystem root.",
  );
  const existing = await lstat(configured).catch(
    (error: NodeJS.ErrnoException) => {
      if (error.code === "ENOENT") return null;
      throw error;
    },
  );
  if (existing) {
    requireThat(
      existing.isDirectory() && !existing.isSymbolicLink(),
      "target_conflict",
      "Configured Codex home must be a real directory during runtime reset.",
    );
    const actual = await realpath(configured);
    requireThat(
      actual !== parse(actual).root,
      "target_conflict",
      "Resolved Codex home cannot be a filesystem root.",
    );
    for (const path of resetPaths)
      await rm(join(actual, path), { recursive: true, force: true });
  }
  await atomicJson(appliedPath, {
    ...marker,
    appliedAt: new Date().toISOString(),
  });
}
