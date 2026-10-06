import { mkdir, readdir, rm, stat, utimes, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { digest, requireThat } from '../../../packages/sdk/src/node.js';
import type { Agent } from '../../../packages/sdk/src/node.js';

const maximumBytes = 8 * 1024 * 1024, retentionMs = 30 * 24 * 60 * 60 * 1000;

/** One path segment that is valid on Windows and POSIX hosts. */
export function stagedName(name: string): string {
  const value = (name.split(/[\\/]/).pop() ?? '').replace(/[<>:"|?*\u0000-\u001f]/g, '_').replace(/^[\s.]+|[\s.]+$/g, '').slice(0, 128);
  return !value ? 'file' : /^(con|prn|aux|nul|com\d|lpt\d)(\.|$)/i.test(value) ? '_' + value : value;
}

/** Stores a message attachment by content so Codex on this host can read it by path. Unused files expire after 30 days. */
export async function stageFile(root: string, input: Agent.StageFileInput, now = Date.now()): Promise<Agent.StagedFile> {
  const bytes = Buffer.from(input.dataBase64, 'base64');
  requireThat(bytes.length <= maximumBytes && bytes.toString('base64') === input.dataBase64,
    'invalid_arguments', 'A staged file must be canonical base64 up to 8 MiB.');
  const contentHash = digest(bytes), name = stagedName(input.name), directory = join(root, contentHash.slice('sha256:'.length)), path = join(directory, name);
  await mkdir(directory, { recursive: true });
  await writeFile(path, bytes);
  await utimes(directory, new Date(now), new Date(now));
  for (const entry of await readdir(root, { withFileTypes: true }))
    if (entry.isDirectory() && now - (await stat(join(root, entry.name))).mtimeMs > retentionMs)
      await rm(join(root, entry.name), { recursive: true, force: true });
  return { path, name, byteLength: bytes.length, contentHash };
}
