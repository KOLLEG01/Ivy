import { realpath, stat, readdir } from 'node:fs/promises';
import { isAbsolute, join, dirname, basename, delimiter } from 'node:path';
import { requireThat } from '../../contracts/src/errors.js';

/** Explicit host tools for nested build runners, never arbitrary parent environment. */
export async function preparationTools(executables: Record<string, string>, bootstrapRoot: string, temporaryRoot: string): Promise<{
  environment: Record<string, string>;
}> {
  const temporary = await realpath(temporaryRoot);
  const environment: Record<string, string> = { TEMP: temporary, TMP: temporary, TMPDIR: temporary };
  const mappings = { ...executables, '$node': process.execPath,
    ...(process.platform === 'win32' ? { '$job': join(bootstrapRoot, 'dist/native/ivy-job.exe') } : {}) };
  const compilers: Record<string, string> = {};
  let node: string | undefined;
  for (const [name, requested] of Object.entries(mappings)) {
    requireThat(isAbsolute(requested), 'invalid_arguments', 'Preparation tool mappings require absolute executable paths.');
    const path = await realpath(requested);
    requireThat((await stat(path)).isFile() && !/\.(?:cmd|bat|ps1)$/i.test(path),
      'invalid_arguments', 'Preparation tools require direct executable files.');
    if (name === 'node' || (name === '$node' && !node)) node = path;
    if (name === 'dotnet') environment['IVY_DOTNET'] = path;
    if (name === 'gcc') environment['IVY_EVS_CC'] = path;
    if (['cargo', 'rustc', 'cl'].includes(name)) compilers[name] = path;
  }
  // realpath supplies the selected runtime's shell-usable directory on Windows.
  const search: string[] = node ? [dirname(node)] : [];
  for (const [tool, key] of [['cargo', 'IVY_CARGO'], ['rustc', 'RUSTC'], ['cl', 'CC']] as const) {
    const path = compilers[tool];
    if (path) { environment[key] = path; search.push(dirname(path)); }
  }
  if (compilers.cargo && process.env.CARGO_HOME) {
    requireThat(isAbsolute(process.env.CARGO_HOME) && (await stat(process.env.CARGO_HOME)).isDirectory(),
      'invalid_arguments', 'CARGO_HOME must identify the selected Cargo cache directory.');
    environment.CARGO_HOME = process.env.CARGO_HOME;
  }
  if (process.platform === 'win32' && compilers.cl) {
    if (!process.env.INCLUDE || !process.env.LIB) {
      const paths = await windowsCompilerPaths(compilers.cl);
      environment.INCLUDE = process.env.INCLUDE || paths.include;
      environment.LIB = process.env.LIB || paths.lib;
      search.push(paths.bin);
    } else { environment.INCLUDE = process.env.INCLUDE; environment.LIB = process.env.LIB; }
  }
  if (search.length) environment.PATH = [...search, process.env.PATH ?? process.env.Path ?? ''].join(delimiter);
  return { environment };
}

async function windowsCompilerPaths(compiler: string): Promise<{include: string; lib: string; bin: string}> {
  const arch = basename(dirname(compiler));
  requireThat(['x64', 'x86', 'arm64'].includes(arch), 'invalid_arguments', 'The selected MSVC compiler needs its target architecture directory.');
  const msvc = dirname(dirname(dirname(dirname(compiler))));
  const sdk = process.env.WindowsSdkDir || (process.env['ProgramFiles(x86)'] && join(process.env['ProgramFiles(x86)']!, 'Windows Kits/10'));
  requireThat(Boolean(sdk), 'invalid_arguments', 'Select the installed Windows SDK with WindowsSdkDir.');
  const pinned = process.env.WindowsSDKVersion?.replace(/[\\/]+$/, '');
  const versions = (await readdir(join(sdk!, 'Include'))).filter(value => /^\d+\.\d+\.\d+\.\d+$/.test(value) && (!pinned || value === pinned))
    .sort((left, right) => { const a = left.split('.').map(Number), b = right.split('.').map(Number); for (let i = 0; i < 4; i++) { if (a[i] !== b[i]) return b[i]! - a[i]!; } return 0; });
  for (const version of versions) {
    const includes = [join(msvc, 'include'), ...['ucrt', 'shared', 'um'].map(part => join(sdk!, 'Include', version, part))];
    const libraries = [join(msvc, 'lib', arch), ...['ucrt', 'um'].map(part => join(sdk!, 'Lib', version, part, arch))];
    const bin = join(sdk!, 'bin', version, arch);
    if ((await Promise.all([...includes, ...libraries, bin].map(path => stat(path).then(value => value.isDirectory(), () => false)))).every(Boolean))
      return {include: includes.join(delimiter), lib: libraries.join(delimiter), bin};
  }
  requireThat(false, 'invalid_arguments', 'The selected MSVC compiler needs a complete Windows SDK for its target architecture.');
}
