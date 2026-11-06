import { readFileSync, writeFileSync } from 'node:fs';

// Embed the small shared selector in self-contained Collector contract schemas.
// The normative identity and its validation rules belong to the Agent schema.
const backend = JSON.parse(readFileSync('specs/schemas/agent.schema.json', 'utf8')).$defs.BackendIdentity;
let count = 0;
function project(value) {
  if (!value || typeof value !== 'object') return;
  if (value.type === 'object' && ['serviceNodeId', 'cwd', 'server', 'tools'].every(key => value.required?.includes(key))) {
    value.properties.backend = structuredClone(backend);
    value.properties.readOnly = {
      type: 'boolean',
      description: 'Operator assertion that every granted MCP tool is read-only. Allows a fresh read after confirmed retirement of an old native context; unresolved original receipts are preserved.',
    };
    count++;
  }
  for (const child of Object.values(value)) project(child);
}
for (const file of ['schemas.json', 'registry.json']) {
  const path = 'services/data-collector/rust/assets/' + file;
  const source = readFileSync(path, 'utf8');
  const schemas = JSON.parse(source);
  for (const contract of schemas.contracts ?? []) {
    if (contract.key === 'data-collector/definition') contract.version = '1.3.0';
  }
  for (const contract of schemas.requiredContracts ?? []) {
    if (contract.key === 'data-collector/definition') {
      contract.readVersions = [...new Set([...contract.readVersions, '1.3.0'])];
      contract.writeVersions = ['1.3.0'];
    }
  }
  count = 0;
  project(schemas);
  if (!count) throw new Error('No native Collector MCP configurations found in ' + path);
  const output = JSON.stringify(schemas, null, 2) + '\n';
  if (process.argv.includes('--check')) {
    if (source !== output) throw new Error('Collector backend schema projections are stale in ' + path);
  } else writeFileSync(path, output);
}
