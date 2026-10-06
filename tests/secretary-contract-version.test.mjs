import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { secretaryRegistry, contractVersion, readableVersions } from '../dist/services/secretary/src/schema.js';

test('Secretary publishes only current definitions and declares retained assignment versions readable', () => {
  const registry = secretaryRegistry();
  assert.equal(new Set(registry.contracts.map(contract => contract.key)).size, registry.contracts.length);
  for (const contract of registry.contracts) {
    assert.equal(contract.version, contractVersion(contract.key));
    const required = registry.requiredContracts.find(required => required.key === contract.key);
    assert.deepEqual(required.readVersions, readableVersions(contract.key));
    assert.deepEqual(required.writeVersions, [contract.version]);
  }
  assert.deepEqual(registry.contracts.find(contract => contract.key === 'secretary/assignment').retention,
    { objects: { mode: 'retain' }, revisions: { mode: 'all' } });
  assert.ok(readableVersions('secretary/assignment').includes('1.2.0'));
  assert.ok(readableVersions('secretary/execution').includes('1.3.0'));
  const ui = JSON.parse(readFileSync('ui/secretary-ui/ui.json', 'utf8'));
  for (const required of ui.requirements.contracts)
    assert.deepEqual(required.readVersions, readableVersions(required.key));
});
