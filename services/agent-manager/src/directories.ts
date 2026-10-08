import { readdir, realpath } from "node:fs/promises";
import { dirname, isAbsolute, join } from "node:path";
import { IvyError, requireThat } from "../../../packages/sdk/src/node.js";
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
  const { path, entries } = await (async () => {
    try {
      const path = await realpath(input.path);
      return { path, entries: await readdir(path, { withFileTypes: true }) };
    } catch (cause) {
      const code = (cause as NodeJS.ErrnoException).code;
      const message = code === 'EACCES' || code === 'EPERM'
        ? 'Access to this folder is denied'
        : code === 'ENOENT' ? 'This folder does not exist'
          : code === 'ENOTDIR' ? 'This path is not a folder' : null;
      if (message) throw new IvyError(code!, `${message}: ${input.path}.`, 'not_executed');
      throw cause;
    }
  })();
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
