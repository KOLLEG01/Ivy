import test from 'node:test';
import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

test('running card errors and recovery update without ticket writes on desktop and mobile', { timeout: 180000 }, async t => {
  const f = await taskBoardFixture(t, { native: true, scheduler: true });
  const created = await f.invoke({ action: 'create', workflowState: 'todo', fields: {
    title: 'Runtime recovery display', description: '', acceptanceCriteria: [], category: null,
    control: 'agent', priority: 2, executionRequirement: { kind: 'host', hostId: 'isolated-agent-host' },
    workspaceRequirement: { kind: 'task_workspace' }, userContact: 'ticket', dueAt: null,
  } });
  const runtime = f.taskBoard().runtime, id = created.task.objectId;
  await expect.poll(async () => (await f.task(id)).claim?.run?.objectId, { timeout: 30000 }).toBeTruthy();
  const runId = (await f.task(id)).claim.run.objectId;
  await runtime.stop(); await runtime.native.drain(runId);
  await expect.poll(async () => (await f.task(id)).claim?.phase).toBe('running');
  const before = await f.client.request('objects.stat', { objectId: id });
  await f.open('#/tasks?node=browser-task-board');
  for (const width of [1440, 390]) {
    await f.page.setViewportSize({ width, height: 1000 });
    const link = f.page.getByRole('link', { name: 'Runtime recovery display', exact: true });
    await expect(link).toBeVisible();
    const error = f.page.locator('[data-attention="error"]').filter({ has: link });
    await expect(error).toHaveCount(0);
    runtime.record(runId, new Error('Fixture reconciliation failure.'));
    await expect(error).toHaveCount(1, { timeout: 15000 });
    runtime.record(runId);
    await expect(error).toHaveCount(0, { timeout: 15000 });
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  }
  const after = await f.client.request('objects.stat', { objectId: id });
  assert.equal(after.currentRevision, before.currentRevision, 'Runtime diagnostics must not manufacture ticket revisions.');
  assert.deepEqual(f.pageErrors, []);
});
