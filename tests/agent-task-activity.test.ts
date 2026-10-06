import test from 'node:test';
import assert from 'node:assert/strict';
import type { Operation } from '../packages/contracts/src/generated.js';
import { liveStateMs, liveWorking, markSeen, readSeen, taskActivity, taskKey } from '../ui/agent-ui/src/task-activity.js';

const now = Date.parse('2026-09-25T12:00:00.000Z');
const task = (status: string, updatedAtMs: number): Operation.InventoryItem => ({
  resourceRef: { serviceNodeId: 'node', namespace: 'codex', kind: 'thread', nativeId: 'task' }, schemaVersion: '1.0.0',
  summary: { nativeId: 'task', status: { type: status }, updatedAt: Math.floor(updatedAtMs / 1000) }, observedAt: '2026-09-25T12:00:00.000Z', stale: false, snapshotRevision: 1,
});
const key = taskKey('node', 'task');

test('live lifecycle events outrank a stale inventory until it can have caught up', () => {
  const seen = { since: now - 60_000, tasks: {} };
  assert.equal(taskActivity(task('idle', now - 120_000), new Map([[key, { working: true, at: now - 1000 }]]), seen, '', now), 'working');
  assert.equal(taskActivity(task('active', now - 1000), new Map([[key, { working: false, at: now - 500 }]]), seen, '', now), 'unread');
  assert.equal(taskActivity(task('idle', now - 120_000), new Map([[key, { working: true, at: now - liveStateMs }]]), seen, '', now), null);
});

test('finished work is unread until seen, never while open, and not before tracking began', () => {
  const finished = task('idle', now - 10_000);
  assert.equal(taskActivity(finished, new Map(), { since: now - 60_000, tasks: {} }, '', now), 'unread');
  assert.equal(taskActivity(finished, new Map(), { since: now - 60_000, tasks: {} }, key, now), null);
  assert.equal(taskActivity(finished, new Map(), readSeen(null, now), '', now), null);
  // A host clock ahead of the browser cannot resurrect a task the user already opened.
  const ahead = task('idle', now + 30_000), seen = markSeen({ since: now - 60_000, tasks: {} }, key, Math.floor((now + 30_000) / 1000), now);
  assert.equal(taskActivity(ahead, new Map(), seen, '', now), null);
  assert.deepEqual(readSeen(JSON.stringify(seen), now), seen);
});

test('only lifecycle notifications change the working state', () => {
  assert.equal(liveWorking('turn/started', {}), true);
  assert.equal(liveWorking('turn/completed', {}), false);
  assert.equal(liveWorking('thread/status/changed', { status: { type: 'active' } }), true);
  assert.equal(liveWorking('item/completed', {}), undefined);
});
