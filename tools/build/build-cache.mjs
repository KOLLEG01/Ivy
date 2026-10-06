import { createHash } from 'node:crypto';
import { existsSync, lstatSync, readdirSync, readFileSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';

// Hash names and bytes: additions, removals and damaged outputs invalidate the cache.
export function fingerprint(paths, context = {}, relativeRoot = null) {
  const hash = createHash('sha256');
  hash.update(JSON.stringify([process.version, process.platform, process.arch, context]));
  const base = relativeRoot === null ? null : resolve(relativeRoot);
  function visit(path) {
    const label = base === null ? path : relative(base, resolve(path)).replaceAll('\\', '/');
    if (base !== null && (label === '..' || label.startsWith('../'))) throw Error('Cache input leaves its relative root: ' + path);
    hash.update(JSON.stringify(label));
    if (!existsSync(path)) { hash.update('missing'); return; }
    const stat = lstatSync(path);
    if (stat.isSymbolicLink()) throw Error('Cache inputs must not be symbolic links: ' + path);
    if (stat.isDirectory()) {
      hash.update('directory');
      for (const name of readdirSync(path).sort()) visit(join(path, name));
    } else { hash.update('file'); hash.update(createHash('sha256').update(readFileSync(path)).digest('hex')); }
  }
  for (const path of [...paths].sort()) visit(path);
  return hash.digest('hex');
}

// TypeScript's build info alone does not detect deleted emitted files.
export function compilerCache(info, outputs, force = false) {
  const stamp = info + '.outputs.json';
  let previous;
  try { previous = JSON.parse(readFileSync(stamp, 'utf8')); } catch {}
  if (force || previous?.output !== fingerprint(outputs)) rmSync(info, { force: true });
  rmSync(stamp, { force: true });
  return () => {
    mkdirSync(dirname(stamp), { recursive: true });
    writeFileSync(stamp, JSON.stringify({ output: fingerprint(outputs) }));
  };
}

export async function cachedBuild({ stamp, inputs, outputs, context = {}, force = false }, build) {
  const before = fingerprint(inputs, context);
  let previous;
  try { previous = JSON.parse(readFileSync(stamp, 'utf8')); } catch { /* Missing/corrupt cache is a miss. */ }
  if (!force && outputs.every(existsSync) && previous?.input === before && previous.output === fingerprint(outputs)) return false;
  rmSync(stamp, { force: true });
  await build();
  if (!outputs.every(existsSync)) throw Error('Build did not produce all expected outputs.');
  if (before === fingerprint(inputs, context)) {
    mkdirSync(dirname(stamp), { recursive: true });
    writeFileSync(stamp, JSON.stringify({ input: before, output: fingerprint(outputs) }));
  }
  return true;
}
