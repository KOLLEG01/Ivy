import { existsSync, lstatSync, readFileSync, realpathSync } from 'node:fs';
import { resolve } from 'node:path';
import { fingerprint } from './build-cache.mjs';

// Shared by source capture and build/test input selection. Only repository
// configuration belongs here; Cargo credentials, downloads and targets do not.
export const rustRootInputPaths = Object.freeze(JSON.parse(readFileSync(new URL('./rust-root-inputs.json', import.meta.url), 'utf8')));
export function isRustRootInput(path) {
  return rustRootInputPaths.includes(path);
}

// This identifies root configuration, not the complete source or a release.
// Cargo continues to validate its own incremental source/dependency inputs.
export function rustRootInputFingerprint(root) {
  root = realpathSync(root);
  const paths = rustRootInputPaths.map(path => resolve(root, path));
  for (const path of paths) {
    if (!existsSync(path)) continue;
    if (!lstatSync(path).isFile() || realpathSync(path) !== path) throw Error('Rust root inputs must be regular files without directory links.');
  }
  return fingerprint(paths, {}, root);
}
