import { realpath, stat } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { requireThat } from '../../contracts/src/errors.js';

/** Explicit host tools for nested build runners, never arbitrary parent environment. */
export async function preparationTools(executables: Record<string, string>, bootstrapRoot: string, temporaryRoot: string): Promise<{
  environment: Record<string, string>;
}> {
  const temporary = await realpath(temporaryRoot);
  const environment: Record<string, string> = { TEMP: temporary, TMP: temporary, TMPDIR: temporary };
  const mappings = { ...executables, '$node': process.execPath,
    ...(process.platform === 'win32' ? { '$job': join(bootstrapRoot, 'dist/native/ivy-job.exe') } : {}) };
  for (const [name, requested] of Object.entries(mappings)) {
    requireThat(isAbsolute(requested), 'invalid_arguments', 'Preparation tool mappings require absolute executable paths.');
    const path = await realpath(requested);
    requireThat((await stat(path)).isFile() && !/\.(?:cmd|bat|ps1)$/i.test(path),
      'invalid_arguments', 'Preparation tools require direct executable files.');
    if (name === 'dotnet') environment['IVY_DOTNET'] = path;
    if (name === 'gcc') environment['IVY_EVS_CC'] = path;
  }
  return { environment };
}
