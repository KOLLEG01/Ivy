import {existsSync, readFileSync} from 'node:fs';
import {lstat, readFile} from 'node:fs/promises';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';

const input = JSON.parse(readFileSync(0, 'utf8'));
registerHooks({resolve(specifier, context, next) {
  if (input.operation === 'relative' && specifier === 'node:path'
      && /\/(?:backup-files|config)\.ts$/.test(context.parentURL ?? '')) {
    return next('node:path/' + input.style, context);
  }
  if (specifier.startsWith('.') && specifier.endsWith('.js')) {
    const source = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
    if (existsSync(fileURLToPath(source))) return next(source.href, context);
  }
  return next(specifier, context);
}});
const {IvyError} = await import('../../packages/contracts/src/errors.ts');
const backup = await import('../../packages/host-runtime/src/backup-files.ts');
const wrap = async work => {
  try {return {ok: true, value: (await work()) ?? null};}
  catch (error) {return {ok: false, error: IvyError.from(error).toWire()};}
};
if (input.operation === 'relative') {
  process.cwd = () => input.cwd;
  process.stdout.write(JSON.stringify(await Promise.all(input.cases.map(c => wrap(() => backup.relativeFile(c.root, c.member))))));
} else {
  // The oracle may only access the disposable directory supplied by its test.
  const root = path.resolve(input.fixtureRoot);
  if (!path.basename(root).startsWith('ivy-config-')) throw Error('Expected disposable fixture root');
  for (const value of [input.root, input.source, input.destination].filter(v => v !== undefined)) {
    const local = path.relative(root, path.resolve(value));
    if (local === '..' || local.startsWith('..' + path.sep) || path.isAbsolute(local)) throw Error('Fixture leaves its root');
  }
  const exclusions = [];
  let result;
  if (input.operation === 'inventory') {
    result = await wrap(() => backup.inventoryFiles(input.root, local => {
      exclusions.push(local);
      if (local === input.throwAt) throw new IvyError('fixture_exclusion', 'Exclusion failed.');
      return (input.exclude ?? []).includes(local);
    }));
  } else if (input.operation === 'copy') {
    result = await wrap(() => backup.copyVerified(input.source, input.destination, input.expected ?? undefined, input.preserveMode ?? false));
  } else if (input.operation === 'sync') {
    result = await wrap(() => backup.syncDirectories(input.root));
  } else throw Error('Unknown backup operation');
  let destination = null;
  if (input.operation === 'copy') {
    let metadata;
    try {metadata = await lstat(input.destination);} catch (error) {if (!['ENOENT', 'ENOTDIR'].includes(error.code)) throw error;}
    if (metadata) destination = {
      kind: metadata.isSymbolicLink() ? 'link' : metadata.isDirectory() ? 'directory' : 'file',
      ...(metadata.isFile() ? {hash: 'sha256:' + createHash('sha256').update(await readFile(input.destination)).digest('hex'), bytes: metadata.size, mode: metadata.mode & 0o777} : {}),
    };
  }
  process.stdout.write(JSON.stringify({result, exclusions, destination,
    parentExists: input.operation === 'copy' ? existsSync(path.dirname(input.destination)) : null}));
}
