import { mkdirSync, writeFileSync, readFileSync, existsSync } from 'node:fs';
import { buildIdentity } from '../../packages/host-runtime/src/build-identity.mjs';
import { buildNativeDeployment } from './build-native-deployment.mjs';

const index = process.argv.indexOf('--component');
const requested = index >= 0 ? process.argv[index + 1] : null;
const all = process.argv.includes('--all');
const webByComponent = { 'dashboards-ui': 'dashboards-ui', hive: 'console', 'wiki-ui': 'wiki-ui', 'agent-ui': 'agent-ui', 'task-board-ui': 'task-board-ui', 'secretary-ui': 'secretary-ui', 'data-collector-ui': 'data-collector-ui' };
const backendComponents = new Set(['dashboards', 'hive', 'service-manager', 'host-executor', 'agent-manager', 'chat-bridge', 'secretary', 'task-board', 'phone-bridge', 'automation-example', 'data-collector']);
const known = new Set([...backendComponents, ...Object.keys(webByComponent)]);
if (!requested && !all) throw new Error('A component target is required; use --component <component> or --all.');
if (requested && !known.has(requested)) throw new Error('Unknown component: ' + requested);
const native = all || (requested && backendComponents.has(requested));
const sourcePlan = requested && existsSync(`services/${requested}/deploy.json`)
  ? JSON.parse(readFileSync(`services/${requested}/deploy.json`, 'utf8')) : null;
const rust = sourcePlan?.entrypoint?.executable.startsWith('dist/native/');
const targetIndex = process.argv.indexOf('--target');
const target = targetIndex >= 0 ? process.argv[targetIndex + 1] : undefined;
if (targetIndex >= 0 && (!rust || !target || target.startsWith('--') || process.argv.lastIndexOf('--target') !== targetIndex))
  throw Error('--target requires one explicit native component target.');
const backend = !rust && (all || backendComponents.has(requested))
  ? await import('./build-backend.mjs') : null;
const nativeResult = rust ? requested === 'phone-bridge'
  ? await (await import('./build-phone-runtime.mjs')).buildPhoneRuntime({ target, identity: buildIdentity() })
  : buildNativeDeployment({ component: requested, target, identity: buildIdentity() }) : null;
const identity = rust ? nativeResult.identity
  : all ? backend.compileBackend({ native: true }) : backend ? backend.compileBackend({ native, component: requested }) : null;
if (all || requested === 'host-executor' && !rust || webByComponent[requested]) {
  const { buildWeb } = await import('./build-web.mjs');
  if (all || requested === 'host-executor' && !rust) await buildWeb();
  else await buildWeb({ uis: [webByComponent[requested]] });
}
if (identity && !rust) backend.stageBackend(identity, requested);
mkdirSync('dist', { recursive: true });
writeFileSync('dist/build-set.json', JSON.stringify({ schemaVersion: 1, build: identity, component: requested ?? null, all, tests: false }, null, 2) + '\n');
