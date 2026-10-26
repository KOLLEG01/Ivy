import { mkdirSync, writeFileSync } from 'node:fs';
import { parseArgs } from 'node:util';
import { buildBackend, backendComponents } from './build-backend.mjs';
import { buildWeb } from './build-web.mjs';
import { buildSdk } from './build-sdk.mjs';
import { buildIdentity } from '../../packages/host-runtime/src/build-identity.mjs';

const { values } = parseArgs({ options: {
  component: { type: 'string' }, all: { type: 'boolean' }, target: { type: 'string' },
} });
const component = values.component;
if (Boolean(component) === Boolean(values.all)) throw Error('Use --component <component> or --all.');
const web = { 'dashboards-ui': 'dashboards-ui', hive: 'console', 'wiki-ui': 'wiki-ui', 'agent-ui': 'agent-ui',
  'task-board-ui': 'task-board-ui', 'secretary-ui': 'secretary-ui', 'data-collector-ui': 'data-collector-ui' };
const native = backendComponents();
if (component && component !== 'automation-example' && !native.includes(component) && !Object.hasOwn(web, component)) throw Error('Unknown component: ' + component);
if (values.target && !native.includes(component)) throw Error('--target requires one explicit native component.');
const identity = values.all || native.includes(component)
  ? await buildBackend({ component, target: values.target }) : component === 'automation-example' ? buildIdentity() : null;
if (component === 'automation-example') {
  buildSdk({ example: true });
  writeFileSync('dist/build-info.json', JSON.stringify(identity, null, 2) + '\n');
}
if (values.all) await buildWeb();
else if (web[component]) await buildWeb({ uis: [web[component]] });
mkdirSync('dist', { recursive: true });
writeFileSync('dist/build-set.json', JSON.stringify({ schemaVersion: 1, build: identity, component: component ?? null,
  all: Boolean(values.all), tests: false }, null, 2) + '\n');
