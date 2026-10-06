import type { BigIntStats } from 'node:fs';
import { lstat, realpath } from 'node:fs/promises';
import { isAbsolute, join, parse, relative, resolve, sep } from 'node:path';
import { need } from './schema.js';

export const samePath = (a: string, b: string) => process.platform === 'win32'
  ? resolve(a).toLowerCase() === resolve(b).toLowerCase() : resolve(a) === resolve(b);

export const inside = (root: string, path: string) => {
  const local = relative(resolve(root), resolve(path));
  return local === '' || (!isAbsolute(local) && local !== '..' && !local.startsWith('..' + sep));
};

export async function safeDirectory(path: string): Promise<BigIntStats> {
  need(isAbsolute(path), 'path_unsafe', 'A directory must be an explicit absolute path.');
  const absolute = resolve(path), base = parse(absolute).root;
  let current = base, metadata = await lstat(base, { bigint: true });
  for (const segment of relative(base, absolute).split(sep).filter(Boolean)) {
    current = join(current, segment); metadata = await lstat(current, { bigint: true });
    need(metadata.isDirectory() && !metadata.isSymbolicLink() && samePath(await realpath(current), current), 'path_unsafe', 'The configured directory must contain no links.');
  }
  need(metadata.isDirectory(), 'path_unsafe', 'The configured source is not a directory.');
  return metadata;
}
