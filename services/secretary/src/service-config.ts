import { lstat, readFile, realpath } from 'node:fs/promises';
import { basename, dirname, resolve } from 'node:path';

export type ServiceConfiguration = { schemaVersion: 1; [section: string]: unknown };

/** Read the one operator-authored configuration file at the service root. */
export async function loadServiceConfiguration(path: string, maximumBytes: number, invalid: () => Error): Promise<ServiceConfiguration | null> {
  try {
    const info = await lstat(path);
    if (!info.isFile() || info.isSymbolicLink() || info.nlink !== 1 || info.size > maximumBytes ||
      (process.platform !== 'win32' && (info.mode & 0o077) !== 0) ||
      await realpath(path) !== resolve(await realpath(dirname(path)), basename(path))) throw invalid();
    const value: unknown = JSON.parse(await readFile(path, 'utf8'));
    if (!value || typeof value !== 'object' || Array.isArray(value) || (value as { schemaVersion?: unknown }).schemaVersion !== 1) throw invalid();
    return value as ServiceConfiguration;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw error instanceof SyntaxError ? invalid() : error;
  }
}
