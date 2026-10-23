import { readFileSync } from 'node:fs';
import { parseArgs } from 'node:util';
import { resolve } from 'node:path';
import { materializeNativePlan } from '../../packages/host-runtime/src/native-plan.mjs';
import { buildIdentity } from '../../packages/host-runtime/src/build-identity.mjs';
import { compileRustBackend, compileRustHelpers, rustBackendTargets } from './build-rust-backend.mjs';
import { stageNativePayloads } from './stage-native-payloads.mjs';

/** Build and stage only the selected host. A prepared plan never advertises
 * binaries from other targets, and normal preparation needs no retained matrix. */
export function buildNativeDeployment({ component, target, root = process.cwd(), identity, cargo } = {}) {
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
  const binaries = ['ivy-health', ...(os === 'win32'
    ? ['ivy-job', ...(component === 'host-executor' ? ['ivy-host-job'] : [])] : [])];
  const requiredBinaries = [...binaries, ...(component === 'host-executor' ? ['ivy-bootstrap-auto-worker'] : [])];
  const payloads = [
    compileRustBackend({ component, target, root, identity, cargo }),
    compileRustHelpers({ component: 'host-executor', target, binaries, root, identity, cargo }),
  ];
  const result = stageNativePayloads({ component, root, identity: payloads[0].identity, payloads, plan,
    requiredBinaries: { [os]: requiredBinaries } });
  return { ...result, plan, staged: true };
}

if (import.meta.main) {
  const { values } = parseArgs({ options: {
    component: { type: 'string' }, target: { type: 'string' },
  } });
  buildNativeDeployment({ component: values.component, target: values.target });
}
