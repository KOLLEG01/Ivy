import { createReadStream, createWriteStream } from 'node:fs';
import { chmod, lstat, mkdir, open, readdir, realpath } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { inside } from './config.js';
import { fileHash } from './artifact.js';
import { runtimeEnvironment } from './process.js';
import { requireThat } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';

export const missing = (error: unknown) => (error as NodeJS.ErrnoException).code === 'ENOENT';
export async function exists(path: string): Promise<boolean> { try { await lstat(path); return true; } catch (error) { if (missing(error)) return false; throw error; } }
export function relativeFile(root: string, path: string): string {
  requireThat(!isAbsolute(path) && path.split('/').every(part => part && part !== '.' && part !== '..' && !/[\\:\x00-\x1f]/.test(part)),
    'backup_invalid', 'Backup members require safe relative file paths.');
  const result = resolve(root, path); requireThat(inside(root, result) && result !== resolve(root), 'backup_invalid', 'Backup member leaves its root.'); return result;
}
export async function privateDirectory(path: string): Promise<void> {
  requireThat(isAbsolute(path), 'invalid_arguments', 'Private backup/restore directories require absolute paths.');
  const parent = await realpath(dirname(path));
  requireThat((await lstat(parent)).isDirectory() && !await exists(path), 'target_conflict', 'Backup/restore needs a fresh directory.');
  await mkdir(path, { mode: 0o700 });
  requireThat(dirname(await realpath(path)) === parent && !(await lstat(path)).isSymbolicLink(), 'target_conflict', 'Private directory ownership changed.');
  if (process.platform === 'win32') {
    const exec = promisify(execFile), system = join(process.env['SystemRoot'] ?? 'C:\\Windows', 'System32');
    const result = await exec(join(system, 'whoami.exe'), ['/user', '/fo', 'csv', '/nh'], { windowsHide: true, timeout: 10_000, env: runtimeEnvironment() });
    const sid = result.stdout.match(/S-1-\d+(?:-\d+)+/)?.[0]; requireThat(sid, 'private_storage_unavailable', 'Current private storage principal could not be established.');
    await exec(join(system, 'icacls.exe'), [path, '/inheritance:r', '/grant:r', '*' + sid + ':(OI)(CI)F', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F'],
      { windowsHide: true, timeout: 10_000, env: runtimeEnvironment(), maxBuffer: 65536 });
  } else { const directory = await open(parent, 'r'); try { await directory.sync(); } finally { await directory.close(); } }
}
export async function syncDirectories(root: string): Promise<void> {
  if (process.platform === 'win32') return;
  const visit = async (directory: string) => {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      requireThat(!entry.isSymbolicLink(), 'target_conflict', 'Durable backup publication cannot follow directory links.');
      if (entry.isDirectory()) await visit(join(directory, entry.name));
    }
    const descriptor = await open(directory, 'r'); try { await descriptor.sync(); } finally { await descriptor.close(); }
  };
  await visit(root);
}
export async function inventoryFiles(root: string, exclude: (path: string) => boolean = () => false): Promise<Host.BackupFile[]> {
  const actual = await realpath(root), entries: Host.BackupFile[] = []; let total = 0;
  requireThat(!(await lstat(root)).isSymbolicLink(), 'target_conflict', 'Backup roots cannot be filesystem links.');
  const visit = async (directory: string, prefix: string, depth: number) => {
    requireThat(depth <= 64, 'limit_exceeded', 'Backup directory nesting is too deep.');
    for (const name of (await readdir(directory)).sort()) {
      const local = prefix + name; if (exclude(local)) continue;
      const path = relativeFile(root, local), metadata = await lstat(path);
      requireThat(!metadata.isSymbolicLink() && inside(actual, await realpath(path)), 'target_conflict', 'Backup cannot follow filesystem links.');
      if (metadata.isDirectory()) await visit(path, local + '/', depth + 1);
      else {
        requireThat(metadata.isFile(), 'backup_unsupported_entry', 'Backup contains an unsupported filesystem entry.');
        total += metadata.size; requireThat(entries.length < 50000 && total <= 64 * 1024 ** 3, 'limit_exceeded', 'Private backup exceeds its bounded file/byte workload.');
        entries.push({ path: local, hash: await fileHash(path), bytes: metadata.size, mode: metadata.mode & 0o777 });
      }
    }
  };
  await visit(actual, '', 0); return entries.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
}
/** Bounded, flushed copy of a regular immutable observation; never overwrite a target file. */
export async function copyVerified(source: string, destination: string, expected?: Host.BackupFile, preserveMode = false): Promise<Host.BackupFile> {
  const before = await lstat(source); requireThat(before.isFile() && !before.isSymbolicLink(), 'target_conflict', 'Backup copies only regular files.');
  requireThat(before.size <= 64 * 1024 ** 3 && (!expected || before.size === expected.bytes), 'backup_changed', 'Backup source size changed.');
  await mkdir(dirname(destination), { recursive: true, mode: 0o700 });
  const hash = createHash('sha256'); let bytes = 0;
  const checker = new Transform({ transform(chunk: Buffer, _encoding, done) {
    bytes += chunk.length;
    if (bytes > before.size) { done(new Error('Backup source grew during its bounded copy.')); return; }
    hash.update(chunk); done(null, chunk);
  } });
  await pipeline(createReadStream(source), checker, createWriteStream(destination, { flags: 'wx', mode: 0o600, flush: true }));
  const after = await lstat(source), digest = 'sha256:' + hash.digest('hex');
  requireThat(bytes === before.size && after.size === before.size && after.mtimeMs === before.mtimeMs && after.ino === before.ino && (!expected || digest === expected.hash),
    'backup_changed', 'Backup source changed during its verified copy.');
  const mode = expected?.mode ?? before.mode & 0o777;
  if (preserveMode && process.platform !== 'win32') {
    await chmod(destination, mode); const descriptor = await open(destination, 'r'); try { await descriptor.sync(); } finally { await descriptor.close(); }
  }
  return { path: basename(destination), hash: digest, bytes, mode };
}
