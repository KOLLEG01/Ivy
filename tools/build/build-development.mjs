import { compileBackend, stageBackend } from './build-backend.mjs';
import { buildWeb } from './build-web.mjs';
import { fingerprint } from './build-cache.mjs';
import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { selectTests } from '../testing/test-selection.mjs';
import * as testConfig from '../testing/test-config.mjs';

const componentFlag = process.argv.indexOf('--component');
const component = componentFlag >= 0 ? process.argv[componentFlag + 1] : undefined;
if (!component) throw new Error('Usage: npm run dev -- --component <component>.');
const scopes = {
  hive: 'hive', 'service-manager': 'host', 'host-executor': 'host', 'agent-manager': 'agent',
  'chat-bridge': 'chat', secretary: 'secretary', taskBoard: 'task-board', 'phone-bridge': 'phone', 'automation-example': 'automation',
  'wiki-ui': 'ui', 'agent-ui': 'ui', 'task-board-ui': 'ui', 'secretary-ui': 'ui'
};
const scope = scopes[component];
if (!scope) throw new Error('Unknown component: ' + component);
const identity = scope === 'ui' ? null : compileBackend({ native: true, component });
if (identity) stageBackend(identity, component);
if (scope === 'ui' || component === 'hive') await buildWeb({ uis: [component === 'hive' ? 'console' : component] });
if (component === 'phone-bridge') {
  const phone = spawnSync(process.execPath, ['tools/build/build-phone-runtime.mjs'], { stdio: 'inherit', windowsHide: true });
  if (phone.error) throw phone.error;
  if (phone.status !== 0) process.exit(phone.status ?? 1);
}
const tests = spawnSync(process.execPath, ['node_modules/typescript/bin/tsc', '-p', 'tsconfig.test.json', '--incremental', '--tsBuildInfoFile', 'dist/.test.tsbuildinfo'], { stdio: 'inherit', windowsHide: true });
if (tests.error) throw tests.error;
if (tests.status !== 0) process.exit(tests.status ?? 1);
const selected = selectTests(resolve('.'), testConfig, { scopes: [scope] });
const testInputs = ['tests', 'dist/packages', 'dist/services', 'tsconfig.test.json', 'tsconfig.runtime.json'];
mkdirSync('dist', { recursive: true });
writeFileSync('dist/.test-build.json', JSON.stringify({ fingerprint: fingerprint(testInputs, { selected: selected.map(test => test.file).sort() }) }));
const result = spawnSync(process.execPath, ['tools/testing/test.mjs', '--scope', scope, '--no-build'], { stdio: 'inherit', windowsHide: true });
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
