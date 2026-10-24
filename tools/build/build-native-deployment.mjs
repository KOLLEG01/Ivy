import { readFileSync, readdirSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { parseArgs } from 'node:util';
import { resolve } from 'node:path';
import { materializeNativePlan } from '../../packages/host-runtime/src/native-plan.mjs';
import { buildIdentity } from '../../packages/host-runtime/src/build-identity.mjs';
import { compileRustBackend, compileRustHelpers, rustBackendTargets } from './build-rust-backend.mjs';
import { stageNativePayloads } from './stage-native-payloads.mjs';
import { componentBuildInputs } from './build-inputs.mjs';

function buildHostUis(root) {
  const uis = readdirSync(resolve(root, 'ui'), { withFileTypes: true }).filter(entry => entry.isDirectory())
    .filter(entry => JSON.parse(readFileSync(resolve(root, 'ui', entry.name, 'deploy.json'), 'utf8')).kind === 'app')
    .map(entry => entry.name);
  const result = spawnSync(process.execPath, ['tools/build/build-web.mjs', '--ui', uis.join(',')],
    { cwd: root, encoding: 'utf8', windowsHide: true, maxBuffer: 16 * 1024 * 1024 });
  if (result.error || result.status !== 0) throw result.error ?? Error(result.stderr + result.stdout);
  return readdirSync(resolve(root, 'dist/apps'), { recursive: true, withFileTypes: true })
    .filter(entry => entry.isFile()).map(entry => resolve(entry.parentPath, entry.name));
}

/** Build and stage only the selected host. A prepared plan never advertises
 * binaries from other targets, and normal preparation needs no retained matrix. */
export function buildNativeDeployment({ component, target, root = process.cwd(), identity, cargo, buildUi = buildHostUis } = {}) {
  if (!/^[a-z][a-z0-9-]*$/.test(component ?? '')) throw Error('A component name is required.');
  const source = JSON.parse(readFileSync(resolve(root, 'services', component, 'deploy.json'), 'utf8'));
  const platform = target === undefined ? `${process.platform}-${process.arch}`
    : typeof target === 'string' && Object.hasOwn(rustBackendTargets, target) ? rustBackendTargets[target] : undefined;
  if (!platform) throw Error('An explicit supported Rust target is required.');
  const [os, arch] = platform.split('-');
  identity ??= buildIdentity(root);
  const plan = materializeNativePlan(source, { os, arch });
  if (!plan.entrypoint?.executable.startsWith('dist/native/')) throw Error('The component does not declare native entry and readiness commands.');
  target ??= Object.keys(rustBackendTargets).find(target => rustBackendTargets[target] === platform);
  const runtimeFiles = componentBuildInputs({ root, component, platform });
  const binaries = ['ivy-health', ...(os === 'win32'
    ? ['ivy-job', ...(component === 'host-executor' ? ['ivy-host-job'] : [])] : [])];
  const requiredBinaries = [...binaries, ...(component === 'host-executor' ? ['ivy', 'ivy-bootstrap-auto-worker'] : [])];
  const payloads = [
    compileRustBackend({ component, target, root, identity, cargo, runtimeFiles }),
    compileRustHelpers({ component: 'host-executor', target, binaries, root, identity, cargo }),
  ];
  const result = stageNativePayloads({ component, root, identity: payloads[0].identity, payloads, plan,
    requiredBinaries: { [os]: requiredBinaries } });
  // Runtime reset republishes checked UI bytes after clearing Hive. They are
  // native CLI data dependencies, produced once by the regular Web builder.
  if (component === 'host-executor') result.files.push(...buildUi(root));
  return { ...result, plan, staged: true };
}

if (import.meta.main) {
  const { values } = parseArgs({ options: {
    component: { type: 'string' }, target: { type: 'string' },
  } });
  buildNativeDeployment({ component: values.component, target: values.target });
}
