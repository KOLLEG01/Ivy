import { spawnSync } from 'node:child_process';
import { parseArgs } from 'node:util';
import { buildBackend, backendComponents } from './build-backend.mjs';
import { buildSdk } from './build-sdk.mjs';
import { buildWeb } from './build-web.mjs';

const { values } = parseArgs({ options: { component: { type: 'string' } } });
const component = values.component;
const scopes = { hive: 'hive', 'service-manager': 'host', 'host-executor': 'host', 'agent-manager': 'agent',
  'chat-bridge': 'chat', secretary: 'secretary', 'task-board': 'task-board', 'phone-bridge': 'phone',
  dashboards: 'dashboards', 'data-collector': 'data-collector' };
const scope = scopes[component] ?? (component?.endsWith('-ui') ? 'ui' : null);
if (!scope) throw Error('Usage: npm run dev -- --component <component>.');
if (backendComponents().includes(component)) await buildBackend({ component });
if (scope === 'ui' || component === 'hive') await buildWeb({ uis: [component === 'hive' ? 'console' : component] });
buildSdk();
const result = spawnSync(process.execPath, ['tools/testing/test.mjs', '--scope', scope], {
  stdio: 'inherit', windowsHide: true,
});
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
