import { readdir, realpath } from "node:fs/promises";
import { dirname, isAbsolute, join } from "node:path";
import { requireThat } from "../../../packages/sdk/src/node.js";
import type { Agent } from "../../../packages/sdk/src/node.js";

/** Host folder browsing is independent of the selected native agent's filesystem tools. */
export async function listDirectories(
  input: Agent.DirectoryListInput,
): Promise<Agent.DirectoryListResult> {
  requireThat(
    isAbsolute(input.path),
    "invalid_arguments",
    "Choose an absolute directory on this host.",
  );
  const path = await realpath(input.path);
  const entries = await readdir(path, { withFileTypes: true });
  const parent = dirname(path);
  return {
    path,
    parent: parent === path ? null : parent,
    directories: entries
      .filter((entry) => entry.isDirectory())
      .map((entry) => ({ name: entry.name, path: join(path, entry.name) }))
      .sort((a, b) => a.name.localeCompare(b.name)),
  };
}
