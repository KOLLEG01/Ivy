import { createHash, randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

/** Preparation supplies the immutable release ID. Standalone builds receive a
 * fresh opaque identity; source trees are never inventoried or hashed here. */
export function buildIdentity(root = process.cwd()) {
  const pkg = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf8'));
  const supplied = process.env.IVY_BUILD_ID;
  const buildId = supplied && /^sha256:[0-9a-f]{64}$/.test(supplied) ? supplied :
    'sha256:' + createHash('sha256').update(`${pkg.version}\0${Date.now()}\0${randomUUID()}`).digest('hex');
  return { schemaVersion: 1, version: pkg.version, buildId };
}
