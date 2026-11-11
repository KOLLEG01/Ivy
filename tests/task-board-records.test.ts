import test from 'node:test';
import assert from 'node:assert/strict';
import type { TaskBoard } from '../packages/contracts/src/generated.js';
import { upgradeTaskRecord, upgradeCommentRecord } from '../packages/contracts/src/task-board-records.js';
import { CommentReader } from '../packages/sdk/src/task-comments.js';
import type { RpcClient } from '../packages/sdk/src/client.js';
import { validateTaskBoard } from '../packages/contracts/src/task-board-validation.js';

test('context-only handoffs and current comment nodes preserve state and ticket ownership', async () => {
  const handoff: TaskBoard.Handoff = { nativeContext: 'release' };
  validateTaskBoard('Handoff', handoff);
  assert.throws(() => validateTaskBoard('Handoff', {}));
  assert.throws(() => validateTaskBoard('Handoff', { ...handoff, wakeAt: null }));
  const comment = { handoff } as TaskBoard.Comment;
  assert.equal(upgradeCommentRecord(comment), comment);
  const legacy = { handoff: { workflowState: 'review', nativeContext: 'retain', nextSteps: [] } } as unknown as TaskBoard.Comment;
  assert.equal(upgradeCommentRecord(legacy).handoff?.nativeContext, 'retain');
  for (const [version, owner, valid] of [
    ['1.0.0', 'task', true], ['2.0.0', 'task', true], ['2.1.0', 'task', true],
    ['2.1.0', 'other', false], ['2.2.0', 'task', false],
  ] as const) {
    const saved = { object: { contractKey: 'task-board/comment-node', parentId: 'task', ownerObjectId: owner },
      revision: { revision: 1, contractVersion: version },
      content: { encoding: 'json', value: { schemaVersion: 1, taskId: 'task', kind: 'comment', entry: { comment, receipt: null } } } };
    const reader = new CommentReader({ request: async () => saved } as unknown as RpcClient);
    const read = reader.node({ objectId: 'node', revision: 1 }, 'task');
    if (valid) assert.deepEqual(await read, saved.content.value);
    else await assert.rejects(read, /another ticket/);
  }
});

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
