import { requireThat } from '../../contracts/src/errors.js';

const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value);
const packageLocation = /^(?:node_modules\/(?:@[a-z0-9_~-][a-z0-9._~-]*\/)?[a-z0-9_~-][a-z0-9._~-]*)(?:\/node_modules\/(?:@[a-z0-9_~-][a-z0-9._~-]*\/)?[a-z0-9_~-][a-z0-9._~-]*)*$/i;
const safeWorkspaceTarget = (value: unknown): value is string => typeof value === 'string' && value.length > 0 && value.length <= 2048 &&
  !value.startsWith('/') && !/^[a-z]:[\\/]/i.test(value) && !value.split(/[\\/]/).some(part => part === '..' || part === '');
function packageMetadata(lock: unknown): Record<string, Record<string, unknown>> {
  requireThat(object(lock) && lock['lockfileVersion'] === 3 && object(lock['packages']),
    'invalid_arguments', 'Runtime dependency packaging requires npm v3 package location metadata.');
  const packages = lock['packages'] as Record<string, unknown>;
  for (const [location, metadata] of Object.entries(packages)) {
    requireThat(object(metadata), 'invalid_arguments', 'Lockfile package metadata must be an object.');
    for (const key of ['dev', 'devOptional', 'optional', 'link']) requireThat(metadata[key] === undefined || typeof metadata[key] === 'boolean',
      'invalid_arguments', 'Lockfile dependency classification must use booleans.');
    if (location.startsWith('node_modules/')) requireThat(packageLocation.test(location),
      'invalid_arguments', 'Dependency location is not an exact contained npm package path.');
  }
  return Object.fromEntries(Object.entries(packages).map(([location, metadata]) => [location, metadata as Record<string, unknown>]));
}
/** Exact package locations, never package-name or path-prefix guesses. */
export function developmentPackageLocations(lock: unknown): ReadonlySet<string> {
  const packages = packageMetadata(lock);
  const excluded = new Set<string>();
  for (const [location, metadata] of Object.entries(packages)) {
    if (metadata['dev'] !== true || !location.startsWith('node_modules/')) continue;
    requireThat(metadata['devOptional'] !== true, 'invalid_arguments', 'A package cannot be both exclusively developmental and optionally required at runtime.');
    excluded.add(location);
  }
  return excluded;
}

/** npm's exact workspace junction/symlink locations and their source directories. */
export function workspacePackageLocations(lock: unknown): ReadonlyMap<string, string> {
  if (object(lock) && lock['lockfileVersion'] === 3 && lock['packages'] === undefined) return new Map();
  const packages = packageMetadata(lock), links = new Map<string, string>();
  for (const [location, metadata] of Object.entries(packages)) {
    if (metadata['link'] !== true) continue;
    requireThat(location.startsWith('node_modules/') && packageLocation.test(location) && safeWorkspaceTarget(metadata['resolved']),
      'invalid_arguments', 'Workspace link must identify one exact package and a contained source directory.');
    links.set(location, String(metadata['resolved']).replaceAll('\\', '/'));
  }
  return links;
}

function parentPackageLocation(location: string): string {
  const nested = location.lastIndexOf('/node_modules/');
  return nested < 0 ? '' : location.slice(0, nested);
}
function resolvedDependency(packages: Record<string, Record<string, unknown>>, owner: string, name: string): string | null {
  for (let current = owner;; current = parentPackageLocation(current)) {
    const candidate = (current ? current + '/' : '') + 'node_modules/' + name;
    if (packages[candidate]) return candidate;
    if (!current) return null;
  }
}

/** Exact installed package closure for the external specifiers emitted by one component build. */
export function runtimePackageLocations(lock: unknown, names: Iterable<string>): ReadonlySet<string> {
  const requested = new Set(names);
  if (requested.size === 0) return new Set();
  const packages = packageMetadata(lock), selected = new Set<string>(), pending: string[] = [];
  for (const name of requested) {
    requireThat(/^(?:@[a-z0-9_~-][a-z0-9._~-]*\/)?[a-z0-9_~-][a-z0-9._~-]*$/i.test(name),
      'invalid_arguments', 'Runtime package name is invalid.');
    const location = 'node_modules/' + name;
    requireThat(packages[location] && packages[location]['link'] !== true, 'artifact_missing', 'Emitted runtime dependency is absent from the exact lock.');
    if (!selected.has(location)) { selected.add(location); pending.push(location); }
  }
  for (let index = 0; index < pending.length; index++) {
    const owner = pending[index]!, metadata = packages[owner]!;
    for (const [field, required] of [['dependencies', true], ['optionalDependencies', false], ['peerDependencies', false]] as const) {
      const dependencies = metadata[field];
      requireThat(dependencies === undefined || object(dependencies), 'invalid_arguments', 'Runtime dependency metadata must be an object.');
      if (!object(dependencies)) continue;
      for (const name of Object.keys(dependencies)) {
        const location = resolvedDependency(packages, owner, name);
        requireThat(location || !required, 'artifact_missing', 'Locked runtime dependency is unavailable.');
        if (location && !selected.has(location)) { selected.add(location); pending.push(location); }
      }
    }
  }
  return selected;
}
