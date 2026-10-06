import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import ts from 'typescript';

export const slash = path => path.replaceAll('\\', '/');
const walk = (root, dir) => !existsSync(join(root, dir)) ? [] : readdirSync(join(root, dir), { withFileTypes: true })
  .filter(entry => !['bin', 'obj', 'node_modules'].includes(entry.name))
  .flatMap(entry => entry.isDirectory() ? walk(root, dir + '/' + entry.name) : [dir + '/' + entry.name]);

export function inventory(root, config) {
  return walk(root, 'tests')
    .filter(file => /\.(test|integration)\.(ts|mjs)$/.test(file))
    .map(file => ({ file, scope: config.scope(file), kind: config.kind(file) }));
}

// Follow local imports through fixtures and shared modules, including emitted .js -> source .ts.
function directDependencies(root, file, cache) {
  if (cache.has(file)) return cache.get(file);
  const result = new Set([file]); cache.set(file, result);
  if (!existsSync(join(root, file))) return result;
  const text = readFileSync(join(root, file), 'utf8');
  const refs = ts.preProcessFile(text, true, true).importedFiles.map(ref => ref.fileName);
  // Include literal runtime inputs and dynamic imports as well as static TypeScript imports.
  for (const match of text.matchAll(/['"`]((?:\.\.?\/|dist\/|specs\/|tools\/|packages\/|services\/|native\/)[^'"`\n$]+)['"`]/g)) refs.push(match[1]);
  for (let ref of refs) {
    let path = ref.startsWith('.') ? slash(relative(root, resolve(root, dirname(file), ref))) : ref;
    path = path.replace(/^dist\//, '');
    if (path.startsWith('../')) continue;
    const candidates = [path.replace(/\.js$/, '.ts'), path, path + '.ts', path + '.mjs', path + '/index.ts'];
    const target = candidates.find(candidate => existsSync(join(root, candidate)) && /\.(?:ts|mjs|json|vue|cs|c|h)$/.test(candidate));
    if (!target) continue;
    result.add(target);
  }
  return result;
}

export function dependencies(root, file, cache = new Map()) {
  const result = new Set(), pending = [file];
  while (pending.length) {
    const next = pending.pop();
    if (result.has(next)) continue;
    result.add(next);
    if (/\.(?:ts|mjs|vue)$/.test(next)) for (const child of directDependencies(root, next, cache)) if (!result.has(child)) pending.push(child);
  }
  return result;
}

export function selectTests(root, config, { changed = [], scopes = [], files = [], all = false, kinds = [] } = {}) {
  const tests = inventory(root, config);
  for (const scope of scopes) if (!config.scopes.includes(scope)) throw Error('Unknown test scope: ' + scope);
  const normalize = file => slash(relative(root, resolve(root, file))).replace(/^dist\//, '').replace(/\.test\.js$/, '.test.ts').replace(/\.integration\.js$/, '.integration.ts');
  const explicit = files.flatMap(file => {
    const pattern = normalize(file);
    const regex = new RegExp('^' + pattern.split('*').map(part => part.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('.*') + '$');
    const matches = tests.filter(test => regex.test(test.file)).map(test => test.file);
    if (!matches.length) throw Error('Unknown test file: ' + file);
    return matches;
  });
  const changes = changed.map(normalize);
  const affectedScopes = new Set(changes.flatMap(file => config.affected(file)));
  const cache = new Map();
  let selected = tests.filter(test => {
    if (all || explicit.includes(test.file) || scopes.includes(test.scope) || affectedScopes.has(test.scope)) return true;
    if (!changes.length) return false;
    const inputs = dependencies(root, test.file, cache);
    return changes.some(file => inputs.has(file));
  });
  // Live/installed checks have an explicit entry point, never an accidental wildcard execution.
  selected = selected.filter(test => (kinds.length ? kinds.includes(test.kind) : test.kind !== 'live') || explicit.includes(test.file));
  return selected.sort((a, b) => a.file.localeCompare(b.file));
}

export function implicitSelectionTooLarge(count, explicit) { return !explicit && count > 20; }
