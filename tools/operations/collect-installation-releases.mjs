import { lstat, readFile, readdir, realpath, rm, rmdir } from 'node:fs/promises';
import { isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const build = /^sha256:[0-9a-f]{64}$/;
const missing = error => error.code === 'ENOENT';

/** For standalone installers: call under their update lock after verifying the live build.
 * Keep the live build, the last successful predecessor and every unfinished update target.
 */
export async function collectInstallationReleases(root, componentId, protectedBuilds) {
  if (!isAbsolute(root) || !/^[a-z][a-z0-9-]{0,127}$/.test(componentId) ||
    !protectedBuilds.size || [...protectedBuilds].some(value => !build.test(value)))
    throw new Error('Release collection requires an absolute root, component and verified protected builds.');
  const boundary = await realpath(root);
  const removed = [];
  for (const entry of await readdir(boundary, { withFileTypes: true })) {
    if (!entry.isDirectory() || !/^[0-9a-f]{64}$/.test(entry.name) || protectedBuilds.has('sha256:' + entry.name)) continue;
    const directory = join(boundary, entry.name);
    const resolved = await realpath(directory), path = relative(boundary, resolved);
    if (!path || path.startsWith('..') || isAbsolute(path)) throw new Error('Release directory leaves its storage root.');
    const manifestPath = join(directory, 'component.json');
    const info = await lstat(manifestPath).catch(error => { if (missing(error)) return null; throw error; });
    if (!info) {
      if (!(await readdir(directory)).length) { await rmdir(directory); removed.push('sha256:' + entry.name); }
      continue;
    }
    if (!info.isFile() || info.isSymbolicLink() || info.size > 1024 * 1024) continue;
    const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
    if (manifest.schemaVersion !== 1 || manifest.componentId !== componentId || manifest.buildId !== 'sha256:' + entry.name) continue;
    // The hash directory is reserved by the installer; interrupted deletion is retried.
    const entries = await readdir(directory);
    for (const name of entries) if (name !== 'component.json') await rm(join(directory, name), { recursive: true, force: true });
    await rm(directory, { recursive: true });
    removed.push(manifest.buildId);
  }
  return removed;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [root, componentId, ...keep] = process.argv.slice(2);
  if (!root || !componentId || !keep.length) throw new Error('Usage: collect-installation-releases.mjs ABSOLUTE_ROOT COMPONENT BUILD_ID...');
  console.log(JSON.stringify({ removed: await collectInstallationReleases(root, componentId, new Set(keep)) }));
}
