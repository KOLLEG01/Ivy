import test from 'node:test';
import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

test('running execution settings remain editable and visibly pending on desktop and mobile', { timeout: 180000 }, async t => {
  const f = await taskBoardFixture(t, { native: true, scheduler: true });
  const created = await f.invoke({ action: 'create', workflowState: 'todo', fields: {
    title: 'Change running execution settings', description: '', acceptanceCriteria: [], category: null,
    control: 'agent', priority: 2, executionRequirement: { kind: 'host', hostId: 'isolated-agent-host' },
    workspaceRequirement: { kind: 'task_workspace' }, userContact: 'ticket', dueAt: null,
  } });
  const runtime = f.taskBoard().runtime, id = created.task.objectId;
  await expect.poll(async () => (await f.task(id)).claim?.run?.objectId, { timeout: 30000 }).toBeTruthy();
  const runId = (await f.task(id)).claim.run.objectId;
  await runtime.stop(); await runtime.native.drain(runId);
  await expect.poll(async () => (await f.task(id)).claim?.phase).toBe('running');
  await f.open('#/tasks?node=browser-task-board&id=' + id);
  for (const [width, effort, speed] of [[1440, 'Low', 'Fast'], [390, 'High', 'Standard']]) {
    await f.page.setViewportSize({ width, height: 1000 });
    await f.page.getByRole('button', { name: 'Edit task', exact: true }).click();
    const editor = f.page.getByRole('region', { name: 'Edit task', exact: true });
    await expect(editor.getByLabel('Model', { exact: true })).toBeEnabled();
    await expect(editor.getByLabel('Title', { exact: true })).toBeDisabled();
    await editor.getByRole('group', { name: 'Reasoning', exact: true }).getByRole('button', { name: effort, exact: true }).click();
    await editor.getByRole('group', { name: 'Speed', exact: true }).getByRole('button', { name: speed, exact: true }).click();
    await editor.getByRole('button', { name: 'Save changes', exact: true }).click();
    await expect(editor).not.toBeVisible();
    await expect(f.page.getByText('Settings change pending', { exact: true })).toBeVisible();
    const saved = await f.task(id);
    assert.equal(saved.executionChangeRequested, true);
    assert.equal(saved.fields.nativeOptions.reasoningEffort, effort.toLowerCase());
    assert.equal(saved.fields.nativeOptions.serviceTier, speed.toLowerCase());
    assert.equal(saved.claim.run.objectId, runId);
    assert.equal(saved.workflowState, 'in_progress');
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  }
  assert.deepEqual(f.pageErrors, []);
});

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
