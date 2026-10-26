import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { buildIdentity } from '../../packages/host-runtime/src/build-identity.mjs';
import { buildNativeDeployment } from './build-native-deployment.mjs';
import { buildPhoneRuntime } from './build-phone-runtime.mjs';

export function backendComponents(root = process.cwd()) {
  return readdirSync(resolve(root, 'services'), { withFileTypes: true })
    .filter(entry => entry.isDirectory() && existsSync(resolve(root, 'services', entry.name, 'deploy.json')))
    .filter(entry => JSON.parse(readFileSync(resolve(root, 'services', entry.name, 'deploy.json'), 'utf8')).entrypoint?.executable.startsWith('dist/native/'))
    .map(entry => entry.name).sort();
}

function freshDirectory(path) {
  mkdirSync(path, { recursive: true });
  if (realpathSync(path) !== path) throw Error('Build output must not use directory links.');
  rmSync(path, { recursive: true, force: true });
  mkdirSync(path, { recursive: true });
}

/** Normal and aggregate builds use the same native component preparation.
 * Component staging stays isolated; the distribution shares one identity. */
export async function buildBackend({ component, target, root = process.cwd(), identity = buildIdentity(root),
  native = buildNativeDeployment, phone = buildPhoneRuntime, buildUi } = {}) {
  root = realpathSync(root);
  const components = backendComponents(root);
  if (component && !components.includes(component)) throw Error('Unknown native component: ' + component);
  if (target && !component) throw Error('--target requires one explicit native component.');
  const output = resolve(root, 'dist');
  freshDirectory(output);
  for (const selected of component ? [component] : components) {
    const staging = resolve(root, '.local', 'build', 'deployment', selected, 'dist');
    freshDirectory(staging);
    const options = { component: selected, root, outputRoot: staging, identity, target, ...(buildUi ? { buildUi } : {}) };
    await (selected === 'phone-bridge' ? phone : native)(options);
    cpSync(staging, output, { recursive: true });
  }
  if (!component) writeFileSync(resolve(output, 'build-info.json'), JSON.stringify(identity, null, 2) + '\n');
  return identity;
}

if (import.meta.main) {
  const { values } = parseArgs({ options: { component: { type: 'string' }, target: { type: 'string' } } });
  await buildBackend({ component: values.component, target: values.target });
}
