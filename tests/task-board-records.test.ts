import test from 'node:test';
import assert from 'node:assert/strict';
import type { TaskBoard } from '../packages/contracts/src/generated.js';
import { upgradeTaskRecord } from '../packages/contracts/src/task-board-records.js';
import { validateTaskBoard } from '../packages/contracts/src/task-board-validation.js';

test('retained single-owner routes normalize to current mappings without changing saved records or current mappings', () => {
  const route: TaskBoard.NativeRoute = { source: { hostId: 'host', serviceNodeId: 'old' },
    target: { hostId: 'host', serviceNodeId: 'joint', backend: { backendId: 'codex', provider: 'codex', accountId: 'main' } } };
  validateTaskBoard('NativeRoute', route);
  const selected = { ...route, catalog: { nativeVersion: '0.163.0', catalogSourceHash: 'sha256:' + 'a'.repeat(64) } };
  validateTaskBoard('NativeRoute', selected);
  assert.throws(() => validateTaskBoard('NativeRoute', { ...selected, catalog: { ...selected.catalog, catalogSourceHash: 'unknown' } }));
  const stored = { fields: { control: 'user', executionRequirement: null }, comments: [],
    workflowState: 'review', updatedAt: '2026-01-01T00:00:00.000Z', nativeRoute: route } as unknown as TaskBoard.Task;
  const before = structuredClone(stored);
  const current = upgradeTaskRecord(stored);
  assert.equal('nativeRoute' in current, false);
  assert.deepEqual(current.nativeRoutes, [route]);
  assert.deepEqual(stored, before);
  assert.deepEqual(upgradeTaskRecord(current), current);
  const alreadyCurrent = { ...stored, nativeRoutes: [] };
  assert.deepEqual(upgradeTaskRecord(alreadyCurrent).nativeRoutes, []);
  assert.deepEqual(upgradeTaskRecord({ ...stored, nativeRoutes: [selected] }).nativeRoutes, [selected]);
});
