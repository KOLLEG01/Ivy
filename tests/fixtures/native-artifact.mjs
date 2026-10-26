import { copyFile, mkdir, readdir, symlink } from 'node:fs/promises';
import { join } from 'node:path';
import { artifactRoot, writeJson } from '../../tools/testing/native-service.mjs';

// Keep the real package/executables, with one retained catalog pinned to the
// synthetic external executable. All native consumers must see the same hash.
export async function catalogArtifact(component, root, catalog) {
  const source = artifactRoot(component), target = join(root, component + '-artifact');
  const link = (from, to) => symlink(from, to, process.platform === 'win32' ? 'junction' : 'dir');
  await mkdir(join(target, 'dist/specs/native'), { recursive: true });
  await copyFile(join(source, 'dist/build-info.json'), join(target, 'dist/build-info.json'));
  for (const entry of await readdir(join(source, 'dist'), { withFileTypes: true })) {
    if (entry.name === 'specs' || entry.name === 'build-info.json') continue;
    if (entry.isDirectory()) await link(join(source, 'dist', entry.name), join(target, 'dist', entry.name));
    else await copyFile(join(source, 'dist', entry.name), join(target, 'dist', entry.name));
  }
  for (const entry of await readdir(join(source, 'dist/specs'), { withFileTypes: true })) {
    if (entry.name === 'native') continue;
    const from = join(source, 'dist/specs', entry.name), to = join(target, 'dist/specs', entry.name);
    if (entry.isDirectory()) await link(from, to); else await copyFile(from, to);
  }
  for (const entry of await readdir(join(source, 'dist/specs/native'), { withFileTypes: true })) {
    if (entry.name === 'codex-' + catalog.version) continue;
    const from = join(source, 'dist/specs/native', entry.name), to = join(target, 'dist/specs/native', entry.name);
    if (entry.isDirectory()) await link(from, to); else await copyFile(from, to);
  }
  await writeJson(join(target, 'dist/specs/native/codex-' + catalog.version + '/catalog.json'), catalog);
  return target;
}
