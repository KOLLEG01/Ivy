import { appendFileSync, mkdirSync, lstatSync, renameSync, unlinkSync } from 'node:fs';
import { join } from 'node:path';
import { requireThat } from '../../contracts/src/errors.js';

/** Bounded process diagnostics only; permanent work/data are never log-retention targets. */
export function serviceLog(root: string): (stream: 'stdout' | 'stderr', text: string) => void {
  mkdirSync(root, { recursive: true, mode: 0o700 });
  const info = lstatSync(root); requireThat(info.isDirectory() && !info.isSymbolicLink(), 'target_conflict', 'Service log directory must not be a link.');
  const current = join(root, 'process.log'), previous = join(root, 'process.previous.log');
  const metadata = (path: string) => { try { const value = lstatSync(path); requireThat(value.isFile() && !value.isSymbolicLink(), 'target_conflict', 'Service log target is not a regular file.'); return value; }
    catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null; throw error; } };
  metadata(current); metadata(previous);
  return (stream, text) => {
    const bytes = Buffer.from(new Date().toISOString() + ' ' + stream + ' ' + text).subarray(-65536);
    if ((metadata(current)?.size ?? 0) + bytes.length > 1048576) {
      if (metadata(previous)) unlinkSync(previous); if (metadata(current)) renameSync(current, previous);
    }
    appendFileSync(current, bytes, { mode: 0o600 });
  };
}
