import { lstat, readdir, realpath, statfs } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import { inside } from './config.js';
import type { Host } from '../../contracts/src/generated.js';

export interface RetentionProgress {
  removed(area: keyof Host.StorageRetentionStatus['removed']): void;
  skipped(area: string, reason: string, requiresAttention?: boolean): void;
}

/** Count logical file bytes without following links into shared caches. */
export async function treeBytes(path: string, boundary?: string): Promise<number> {
  const root = boundary ?? await realpath(path);
  let files = 0, bytes = 0;
  const visit = async (path: string, depth: number) => {
    requireThat(depth <= 64 && ++files <= 200000, 'limit_exceeded', 'Storage inventory exceeds its traversal limit.');
    const metadata = await lstat(path);
    if (metadata.isSymbolicLink()) return;
    requireThat(inside(root, await realpath(path)), 'target_conflict', 'Storage inventory leaves its owned directory.');
    if (metadata.isDirectory()) for (const item of await readdir(path)) await visit(join(path, item), depth + 1);
    else { requireThat(metadata.isFile(), 'target_conflict', 'Storage has an unsupported payload type.'); bytes += metadata.size; }
    requireThat(Number.isSafeInteger(bytes), 'limit_exceeded', 'Storage byte count is out of range.');
  };
  await visit(path, 0); return bytes;
}

export function storageIsLow(availableBytes: number, totalBytes: number): boolean {
  return availableBytes < 2 * 1024 ** 3 || (totalBytes > 0 && availableBytes / totalBytes < 0.1);
}
export async function hostStorageSpace(config: Host.HostConfig): Promise<{ availableBytes: Host.StorageRetentionStatus['availableBytes']; lowSpace: boolean }> {
  const probe = async (path: string): Promise<{ bytes: number; low: boolean }> => {
    try {
      const value = await statfs(path), bytes = Math.min(Number.MAX_SAFE_INTEGER, Math.max(0, value.bavail * value.bsize));
      return { bytes, low: storageIsLow(bytes, value.blocks * value.bsize) };
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT' && dirname(path) !== path) return probe(dirname(path));
      throw error;
    }
  };
  const [runtime, artifacts, staging] = await Promise.all([config.runtimeRoot, config.artifactRoot, config.stagingRoot].map(probe));
  return { availableBytes: { runtime: runtime!.bytes, artifacts: artifacts!.bytes, staging: staging!.bytes },
    lowSpace: [runtime!, artifacts!, staging!].some(value => value.low) };
}

/** Pressure retries remain throttled even if nothing safe can be deleted. */
export class RetentionSchedule {
  private nextScheduled: number;
  private nextPressure = 0;
  constructor(now: number) { this.nextScheduled = now + 60_000; }
  due(now: number, lowSpace: boolean): boolean { return now >= this.nextScheduled || lowSpace && now >= this.nextPressure; }
  started(now: number): void { this.nextScheduled = now + 86400000; this.nextPressure = now + 3600000; }
  failed(now: number): void { this.nextScheduled = now + 3600000; this.nextPressure = now + 3600000; }
}

export async function availableStorageBytes(path: string): Promise<number> {
  const value = await statfs(path);
  return Math.min(Number.MAX_SAFE_INTEGER, Math.max(0, value.bavail * value.bsize));
}
export async function requireStorageSpace(path: string, requiredBytes: number): Promise<void> {
  const availableBytes = await availableStorageBytes(path);
  if (availableBytes < requiredBytes) throw new IvyError('storage_pressure', 'Insufficient measured free space for safe preparation; inspect and compact verified duplicate workspaces.', 'not_executed', { availableBytes, requiredBytes });
}
