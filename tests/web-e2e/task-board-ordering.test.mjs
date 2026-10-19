import test from 'node:test';
import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';
import { choose } from './fixtures/select.mjs';

for (const width of [1440, 390]) test(`category swimlanes keep every priority and manual column available at ${width}px`, { timeout: 120000 }, async t => {
  const f = await taskBoardFixture(t, { viewport: { width, height: 1000 } });
  const result = { summary: 'Retained manual result.', artifacts: [], checks: [], codeReferences: [], limitations: [], repositoryResult: null };
  const columns = [['Waiting', 'todo'], ['In progress', 'in_progress'], ['Review', 'review'], ['Done', 'done'], ['Backlog', 'backlog']];
  for (const [label, workflowState] of columns) {
    for (const [suffix, category, priority] of [['A low', 'A', 1], ['B high', 'B', 4], ['A high', 'A', 4]]) {
      await f.invoke({ action: 'create', workflowState, ...(workflowState === 'review' ? { initialResult: result } : {}), fields: {
        title: label + ' ' + suffix, description: '', acceptanceCriteria: [], category, priority, control: 'user',
        executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' }, dependencies: [], userContact: 'ticket', nextReviewAt: null, dueAt: null,
      } });
    }
  }
  await f.open('#/tasks?node=browser-task-board');
  await f.page.getByRole('button', { name: 'Display options', exact: true }).click();
  await choose(f.page.getByLabel('Board swimlanes', { exact: true }), 'category');
  await choose(f.page.getByLabel('Backlog swimlanes', { exact: true }), 'category');
  await f.page.keyboard.press('Escape');
  for (const [label] of columns) {
    const column = f.page.getByRole('region', { name: label, exact: true });
    if (label === 'Backlog') await column.getByRole('button', { name: 'Order Backlog manually', exact: true }).click();
    const titles = () => column.locator('article h3').allTextContents().then(values => values.map(value => value.trim()));
    await expect.poll(titles).toEqual([label + ' A high', label + ' A low', label + ' B high']);
    await column.getByRole('button', { name: 'Order ' + label + ' by priority', exact: true }).click();
    await expect.poll(titles).toEqual([label + ' A low', label + ' A high', label + ' B high']);
    await column.getByRole('button', { name: 'Order ' + label + ' manually', exact: true }).click();
    await expect.poll(titles).toEqual([label + ' A high', label + ' A low', label + ' B high']);
  }
  await f.page.reload();
  for (const [label] of columns) await expect(f.page.getByRole('region', { name: label, exact: true }).locator('article')).toHaveCount(3);
  await expect(f.page.getByText('This view is unavailable', { exact: true })).toHaveCount(0);
  assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(f.pageErrors, []);
});
