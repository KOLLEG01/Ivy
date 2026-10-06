import { lstat, mkdir, open, readFile, unlink } from 'node:fs/promises';
import { join } from 'node:path';
import { canonical } from '../../contracts/src/canonical.js';
import { IvyError } from '../../contracts/src/errors.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import type { Host } from '../../contracts/src/generated.js';
import { jsonFile } from './config.js';

const path = (dataRoot: string) => join(dataRoot, 'process-stop-fence.json');

export async function processStopFence(dataRoot: string): Promise<Host.ProcessStopFence | null> {
  try { await lstat(path(dataRoot)); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw new IvyError('outcome_unknown', 'The process stop fence cannot be observed safely.', 'unknown');
  }
  try {
    const fence = await jsonFile<Host.ProcessStopFence>(path(dataRoot), 4096); validateHost('ProcessStopFence', fence); return fence;
  } catch {
    throw new IvyError('outcome_unknown', 'The process stop fence cannot be verified; reconcile its owned runtime data before another launch.', 'unknown');
  }
}

/** Remove only the exact fence whose external owner-release proof just passed. */
export async function clearProcessStopFence(dataRoot: string, expected: Host.ProcessStopFence): Promise<boolean> {
  validateHost('ProcessStopFence', expected);
  let current: string;
  try { current = (await readFile(path(dataRoot), 'utf8')).trim(); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false; throw error; }
  if (current !== canonical(expected)) return false;
  await unlink(path(dataRoot)); return true;
}

/** The small stop marker survives restart; only explicit ownership reconciliation clears it. */
export async function recordProcessStopFence(dataRoot: string, fence: Host.ProcessStopFence): Promise<boolean> {
  validateHost('ProcessStopFence', fence); await mkdir(dataRoot, { recursive: true, mode: 0o700 });
  let file;
  try { file = await open(path(dataRoot), 'wx', 0o600); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === 'EEXIST') return false; throw error; }
  try { await file.writeFile(canonical(fence) + '\n', 'utf8'); await file.sync(); }
  finally { await file.close(); }
  if (process.platform !== 'win32') { const directory = await open(dataRoot, 'r'); try { await directory.sync(); } finally { await directory.close(); } }
  return true;
}

export async function requireClearProcessStopFence(dataRoot: string): Promise<void> {
  const fence = await processStopFence(dataRoot); if (!fence) return;
  throw new IvyError('outcome_unknown', 'A prior process tree did not establish its stopped outcome; reconcile its owned runtime before another launch.', 'unknown',
    { owner: fence.owner, ownerId: fence.ownerId, processId: fence.processId, observedAt: fence.observedAt });
}
