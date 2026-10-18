import test from 'node:test';
import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

for (const width of [1440, 390]) test(`lost upload and comment replies recover without losing the draft or duplicating work at ${width}px`, { timeout: 90000 }, async t => {
  const f = await taskBoardFixture(t, { viewport: { width, height: 900 } });
  const created = await f.invoke({ action: 'create', workflowState: 'backlog', fields: {
    title: 'Recover uncertain conversation writes', description: '', acceptanceCriteria: [], category: null,
    control: 'user', priority: 2, executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' },
    dependencies: [], userContact: 'ticket', nextReviewAt: null, dueAt: null,
  } });
  const id = created.task.objectId;
  await f.open('#/tasks?id=' + id + '&node=browser-task-board' + (width === 390 ? '&panel=sheet' : ''));
  const editor = f.page.getByRole('textbox', { name: 'Comment', exact: true });
  const composer = f.page.locator('[data-slot="task-comment-composer"]');
  const send = f.page.getByRole('button', { name: 'Add comment', exact: true });
  await editor.fill('Draft survives the lost upload reply');
  let hidden = true;
  const lost = new Set();
  await f.page.route('**/api/v1/rpc', async route => {
    const body = route.request().postDataJSON();
    if (body.method === 'tools.call' && ['task-board.uploadAttachment', 'task-board.comment'].includes(body.params.qualifiedName) && !lost.has(body.params.qualifiedName)) {
      lost.add(body.params.qualifiedName);
      await route.fetch(); // The owner commits, but the browser never gets its reply.
      return route.abort();
    }
    if (hidden && body.method === 'tools.call' && body.params.qualifiedName === 'task-board.operation') return route.abort();
    return route.continue();
  });
  await f.page.locator('input[type=file]').setInputFiles({ name: 'evidence.txt', mimeType: 'text/plain', buffer: Buffer.from('Exact uploaded bytes') });
  await expect(composer.getByRole('button', { name: 'Check original action', exact: true })).toBeVisible();
  await expect(editor).toBeEditable();
  await editor.press('End'); await editor.pressSequentially(' and further typing');
  await expect(send).toBeDisabled();
  hidden = false;
  await expect(editor.locator('a')).toHaveCount(1, { timeout: 20000 });
  await expect(send).toBeEnabled();
  await expect(editor).toContainText('and further typing');
  assert.equal((await f.task(id)).attachments.length, 1);
  hidden = true;
  await send.click();
  await expect(composer.getByRole('button', { name: 'Check original action', exact: true })).toBeVisible();
  await expect(editor).toContainText('Draft survives the lost upload reply');
  await expect(editor).toBeEditable();
  await expect(send).toBeDisabled();
  await expect.poll(async () => (await f.task(id)).comments.length).toBe(1);
  hidden = false;
  await expect(editor).not.toContainText('Draft survives', { timeout: 20000 });
  await expect(composer.getByRole('button', { name: 'Check original action', exact: true })).toHaveCount(0);
  const saved = await f.task(id);
  assert.equal(saved.attachments.length, 1); assert.equal(saved.comments.length, 1);
  assert.equal(saved.comments[0].attachments.length, 1);
  assert.match(saved.comments[0].body, /and further typing/);
  // Under history pressure the owner may release a completed receipt before
  // the browser sees it. This must unlock the draft without resending it.
  hidden = true; lost.delete('task-board.comment');
  await editor.fill('Keep this draft when the completed receipt expires');
  await send.click();
  await expect.poll(async () => (await f.task(id)).comments.length).toBe(2);
  const store = f.taskBoard().runtime.engine.store, priorLimit = store.historyLimits.count;
  store.historyLimits.count = 0;
  try { await store.collectLocal(); } finally { store.historyLimits.count = priorLimit; }
  hidden = false;
  await expect(composer.getByText('The original action has finished and its receipt was released. Check the task before repeating it.', { exact: true })).toBeVisible({ timeout: 20000 });
  await expect(send).toBeEnabled();
  await expect(editor).toContainText('Keep this draft');
  assert.equal((await f.task(id)).comments.length, 2);
  assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(f.pageErrors, []);
});
