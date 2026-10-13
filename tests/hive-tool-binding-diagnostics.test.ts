import test from 'node:test';
import assert from 'node:assert/strict';
import type { Wire } from '../packages/contracts/src/generated.js';
import { ToolBindingDiagnostics } from '../services/hive/src/tool-binding-diagnostics.js';

test('binding rejections retain safe caller/target evidence and resolve only after that caller completes a current call', () => {
  let now = Date.now();
  const records: Wire.Diagnostic[] = [], target = { callerPrincipalId: 'reader', serviceNodeId: 'provider', qualifiedName: 'fixture.read' };
  const monitor = new ToolBindingDiagnostics(value => records.push(value), () => now);
  monitor.rejected(target); monitor.rejected(target);
  assert.equal(records.length, 1); assert.deepEqual(records[0]!.resource, target);
  now += 30_000; monitor.rejected(target);
  assert.equal(records.length, 2); assert.equal(records[0]!.firstObservedAt, records[1]!.firstObservedAt);
  const restored = new ToolBindingDiagnostics(value => records.push(value), () => now);
  restored.restore([records[1]!]);
  restored.completed({ ...target, callerPrincipalId: 'another reader' }); assert.equal(records.length, 2);
  restored.completed(target); assert.equal(records.at(-1)!.status, 'resolved');
  restored.completed(target); assert.equal(records.length, 3);
});
