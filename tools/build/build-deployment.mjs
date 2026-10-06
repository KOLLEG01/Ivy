import { compileBackend, stageBackend } from './build-backend.mjs';
import { buildWeb } from './build-web.mjs';
import { mkdirSync, writeFileSync } from 'node:fs';

const index = process.argv.indexOf('--component');
const requested = index >= 0 ? process.argv[index + 1] : null;
const all = process.argv.includes('--all');
const webByComponent = { 'dashboards-ui': 'dashboards-ui', hive: 'console', 'wiki-ui': 'wiki-ui', 'agent-ui': 'agent-ui', 'task-board-ui': 'task-board-ui', 'secretary-ui': 'secretary-ui', 'data-collector-ui': 'data-collector-ui' };
const backendComponents = new Set(['dashboards', 'hive', 'service-manager', 'host-executor', 'agent-manager', 'chat-bridge', 'secretary', 'task-board', 'phone-bridge', 'automation-example', 'data-collector']);
const known = new Set([...backendComponents, ...Object.keys(webByComponent)]);
if (!requested && !all) throw new Error('A component target is required; use --component <component> or --all.');
if (requested && !known.has(requested)) throw new Error('Unknown component: ' + requested);
const native = all || (requested && backendComponents.has(requested));
const identity = all ? compileBackend({ native: true }) : backendComponents.has(requested) ? compileBackend({ native, component: requested }) : null;
if (all || requested === 'host-executor') await buildWeb();
else if (webByComponent[requested]) await buildWeb({ uis: [webByComponent[requested]] });
if (identity) stageBackend(identity, requested);
mkdirSync('dist', { recursive: true });
writeFileSync('dist/build-set.json', JSON.stringify({ schemaVersion: 1, build: identity, component: requested ?? null, all, tests: false }, null, 2) + '\n');
