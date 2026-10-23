import { lstatSync, readFileSync, realpathSync, statSync } from 'node:fs';
import { isAbsolute, relative, resolve, sep } from 'node:path';

const targets = new Set(['win32-x64', 'win32-arm64', 'linux-x64', 'linux-arm64']);
const invalid = () => Error('Build inputs require a bounded version 1 target map of distinct checkout-relative file paths.');
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const folded = value => value.replace(/[A-Z]/g, c => c.toLowerCase());
const validPath = path => typeof path === 'string' && path.isWellFormed() && path.length > 0 && Buffer.byteLength(path) <= 1024
  && !/[\x00-\x1f\x7f-\x9f\\:*?\[\]{}]/u.test(path)
  && path.split('/').every(part => part && part !== '.' && part !== '..' && !/[. ]$/.test(part));

function directFile(root, path) {
  let file = root;
  const parts = path.split('/');
  for (let index = 0; index < parts.length; index++) {
    file = resolve(file, parts[index]);
    const metadata = lstatSync(file);
    if (metadata.isSymbolicLink() || (index + 1 === parts.length
      ? !metadata.isFile() || metadata.size === 0 : !metadata.isDirectory())) throw invalid();
  }
  const child = relative(root, realpathSync(file));
  if (!child || isAbsolute(child) || child === '..' || child.startsWith('..' + sep)) throw invalid();
  return file;
}

/** Source-only declaration, shared with component-aware Host capture. */
export function componentBuildInputs({ root = process.cwd(), component, platform } = {}) {
  if (!/^[a-z][a-z0-9-]*$/.test(component ?? '') || !targets.has(platform)) throw invalid();
  root = realpathSync(root);
  const declaration = `services/${component}/build-inputs.json`;
  try { lstatSync(resolve(root, declaration)); }
  catch (error) { if (error.code === 'ENOENT') return []; throw error; }
  const file = directFile(root, declaration);
  if (statSync(file).size > 64 * 1024) throw invalid();
  const bytes = readFileSync(file);
  if (bytes.length > 64 * 1024) throw invalid();
  const value = JSON.parse(new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes));
  if (!object(value) || Object.keys(value).length !== 2 || value.schemaVersion !== 1 || !object(value.targets)) throw invalid();
  for (const [target, files] of Object.entries(value.targets)) {
    if (!targets.has(target) || !Array.isArray(files) || files.length > 128 || files.some(path => !validPath(path))
      || new Set(files.map(folded)).size !== files.length) throw invalid();
  }
  return (value.targets[platform] ?? []).map(path => {
    const file = directFile(root, path);
    if (statSync(file).size > 64 * 1024 * 1024) throw invalid();
    return file;
  });
}
