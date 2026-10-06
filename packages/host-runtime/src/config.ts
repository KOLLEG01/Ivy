import { readFile, stat, mkdir, open, unlink } from 'node:fs/promises';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { canonical } from '../../contracts/src/canonical.js';
import { requireThat } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import type { Host } from '../../contracts/src/generated.js';
import { replaceFile } from './atomic-file.js';

export async function jsonFile<T>(path: string, limit = 1024 * 1024): Promise<T> {
  const metadata = await stat(path); requireThat(metadata.isFile() && metadata.size <= limit, 'invalid_arguments', 'Configuration must be a bounded regular file.');
  return JSON.parse(await readFile(path, 'utf8')) as T;
}
export async function atomicJson(path: string, value: unknown): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  const temporary = path + '.' + randomUUID() + '.tmp';
  const file = await open(temporary, 'wx', 0o600);
  try { await file.writeFile(canonical(value) + '\n'); await file.sync(); } finally { await file.close(); }
  try { await replaceFile(temporary, path); }
  catch (error) { await unlink(temporary).catch(() => undefined); throw error; }
  // POSIX needs the containing directory flushed as well as the file for rename durability.
  if (process.platform !== 'win32') { const directory = await open(dirname(path), 'r'); try { await directory.sync(); } finally { await directory.close(); } }
}
export function inside(root: string, path: string): boolean {
  const local = relative(resolve(root), resolve(path));
  return local !== '..' && !local.startsWith('..\\') && !local.startsWith('../') && !isAbsolute(local);
}
export async function instanceConfig(path: string): Promise<Host.InstanceConfig> {
  const config = await jsonFile<Host.InstanceConfig>(path); validateHost('InstanceConfig', config);
  requireThat(isAbsolute(config.dataRoot) && isAbsolute(config.artifactRoot) && !inside(config.artifactRoot, config.dataRoot) && !inside(config.dataRoot, config.artifactRoot), 'invalid_arguments', 'Instance data and artifact roots must be separate absolute paths.');
  requireThat(process.versions.node.startsWith('24.') && Number(process.versions.node.split('.')[1]) >= 18, 'unsupported_runtime', 'This distribution requires pinned Node 24.18 or newer in the 24.x line.');
  const build = await jsonFile<{ buildId: string; version: string }>(join(config.artifactRoot, 'dist', 'build-info.json'));
  requireThat(build.buildId === config.buildId && build.version === config.version, 'build_mismatch', 'Configuration identity does not match the prepared executable artifact.');
  await mkdir(config.dataRoot, { recursive: true });
  for (const root of [config.workRoot, config.logsRoot]) if (root) {
    requireThat(isAbsolute(root) && !inside(config.artifactRoot, root) && !inside(root, config.artifactRoot), 'invalid_arguments', 'Service work/logs must stay outside executable artifacts.');
    await mkdir(root, { recursive: true });
  }
  return config;
}
export function configurationPath(): string {
  const args = process.argv.slice(2), position = args.indexOf('--config');
  const path = position >= 0 ? args[position + 1] : process.env['IVY_INSTANCE_CONFIG'];
  requireThat(path && isAbsolute(path), 'invalid_arguments', 'Use --config ABSOLUTE_PATH or IVY_INSTANCE_CONFIG.');
  return path;
}
