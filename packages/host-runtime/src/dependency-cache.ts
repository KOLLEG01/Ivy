import { lstat, mkdir, readdir, realpath, rename, rm, utimes } from 'node:fs/promises';
import { join } from 'node:path';
import { setTimeout } from 'node:timers/promises';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { inside, jsonFile } from './config.js';
import { ExecutorLock } from './journal.js';
import type { RetentionProgress } from './storage-space.js';

const cacheName = /^[0-9a-f]{64}$/;
const retiredName = /^\.retiring-[0-9a-f]{64}$/;
const missing = (error: unknown) => (error as NodeJS.ErrnoException).code === 'ENOENT';
const lockRoot = (stagingRoot: string, key: string) => join(stagingRoot, 'dependency-cache-locks', key);

/** Shared leases protect cache links without serializing independent builds. */
export async function useDependencyCache(stagingRoot: string, key: string): Promise<ExecutorLock> {
  const deadline = Date.now() + 5000;
  for (;;) {
    try { return new ExecutorLock(lockRoot(stagingRoot, key), true); }
    catch (error) {
      if (!(error instanceof IvyError) || error.code !== 'executor_already_running' || Date.now() >= deadline) throw error;
      await setTimeout(25);
    }
  }
}

export async function touchDependencyCache(root: string): Promise<void> {
  const now = new Date();
  await utimes(root, now, now).catch(error => { if (!missing(error)) throw error; });
}

/** Only the latest reusable cache and caches leased by running builds stay local. */
export async function collectDependencyCaches(stagingRoot: string, progress: RetentionProgress): Promise<void> {
  const root = join(stagingRoot, 'dependency-cache');
  await mkdir(root, { recursive: true });
  const boundary = await realpath(root), caches: Array<{ name: string; usedAt: number }> = [];
  for (const entry of await readdir(root, { withFileTypes: true })) {
    if (!entry.isDirectory() || !cacheName.test(entry.name) && !retiredName.test(entry.name)) continue;
    const target = join(root, entry.name);
    requireThat(inside(boundary, await realpath(target)), 'target_conflict', 'Dependency cache leaves its storage root.');
    if (retiredName.test(entry.name)) {
      await rm(target, { recursive: true }); progress.removed('payloads'); continue;
    }
    const metadata = await jsonFile<{ key?: string }>(join(target, 'cache.json'), 65536).catch(error => {
      if (missing(error)) return null; throw error;
    });
    if (metadata?.key !== 'sha256:' + entry.name) { progress.skipped('dependency-cache', 'unrecognized_manifest', true); continue; }
    caches.push({ name: entry.name, usedAt: (await lstat(target)).mtimeMs });
  }
  caches.sort((a, b) => b.usedAt - a.usedAt || a.name.localeCompare(b.name));
  for (const cache of caches.slice(1)) {
    let lock: ExecutorLock | null = null, retired: string | null = null;
    try {
      lock = new ExecutorLock(lockRoot(stagingRoot, cache.name));
      const target = join(root, cache.name);
      // A completed build may have reused this entry since the directory scan.
      if ((await lstat(target)).mtimeMs !== cache.usedAt) continue;
      requireThat(inside(boundary, await realpath(target)), 'target_conflict', 'Dependency cache leaves its storage root.');
      retired = join(root, '.retiring-' + cache.name);
      await rename(target, retired);
    } catch (error) {
      if (error instanceof IvyError && error.code === 'executor_already_running') progress.skipped('dependency-cache', 'active_build');
      else if (!missing(error)) throw error;
    } finally { lock?.close(); }
    if (retired) { await rm(retired, { recursive: true }); progress.removed('payloads'); }
  }
}
