import test from 'node:test';
import assert from 'node:assert/strict';
import { automationRegistry } from '../docs/examples/services/automation-example/src/engine.js';

test('automation-example declares every durable record through its service registry', () => {
  const registry = automationRegistry();
  assert.deepEqual(registry.namespaces, []);
  assert.deepEqual(registry.contracts.map(contract => contract.key), ['automation-example/checkpoint', 'automation-example/period', 'automation-example/event-receipt']);
  assert.deepEqual(registry.requiredContracts.map(contract => contract.key), registry.contracts.map(contract => contract.key));
});
