import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';
import { expect } from '@playwright/test';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

const fields = title => ({ title, description: '', acceptanceCriteria: [], category: null, control: 'user', priority: 2,
  executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' }, dependencies: [], userContact: 'ticket', nextReviewAt: null, dueAt: null });
const gate = () => { let release; return { promise: new Promise(resolve => { release = resolve; }), release: () => release() }; };
const routeTo = id => '#/tasks?node=browser-task-board&id=' + id;
const isComment = body => body.method === 'tools.call' && body.params.qualifiedName === 'task-board.comment';

test('optimistic comments render once before dispatch and acknowledgement, retaining a new draft', { timeout: 90000 }, async t => {
  const f = await taskBoardFixture(t);
  const created = await f.invoke({ action: 'create', fields: fields('Optimistic conversation') }), id = created.task.objectId;
  await f.open(routeTo(id));
  const dispatch = gate(), acknowledge = gate(), started = gate(), saved = gate();
  t.after(() => { dispatch.release(); acknowledge.release(); });
  let calls = 0;
  await f.page.route('**/api/v1/rpc', async route => {
    const body = route.request().postDataJSON();
    if (!isComment(body)) return route.continue();
    calls++; started.release(); await dispatch.promise;
    const response = await route.fetch(); saved.release(); await acknowledge.promise;
    await route.fulfill({ response });
  });
  const editor = f.page.getByRole('textbox', { name: 'Comment', exact: true });
  await editor.fill('Immediate message');
  await f.page.getByRole('button', { name: 'Add comment', exact: true }).click();
  const message = f.page.locator('[data-slot=conversation-message]').filter({ hasText: 'Immediate message' });
  await expect(message).toHaveCount(1);
  await expect(message).toBeVisible();
  await expect(editor).toBeEmpty();
  await started.promise;
  assert.equal((await f.task(id)).comments.length, 0);
  await expect(f.page.getByText('Add task comment', { exact: true })).toHaveCount(0);
  await expect(f.page.getByText('Saving task update', { exact: true })).toHaveCount(0);
  await editor.fill('Immediate message'); // Same text is a distinct, unsent draft.
  await editor.press('Enter');
  assert.equal(calls, 1);
  dispatch.release(); await saved.promise;
  await f.page.evaluate(() => window.dispatchEvent(new Event('online')));
  await expect.poll(async () => (await f.task(id)).comments.length).toBe(1);
  await expect(message).toHaveCount(1);
  acknowledge.release();
  await expect(f.page.getByRole('button', { name: 'Add comment', exact: true })).toBeEnabled();
  await expect(editor).toContainText('Immediate message');
  await expect(message).toHaveCount(1);
  assert.deepEqual(f.pageErrors, []);
});

test('definitive failure retries the retained message and unknown outcomes only observe the original identity', { timeout: 120000 }, async t => {
  const f = await taskBoardFixture(t);
  const created = await f.invoke({ action: 'create', fields: fields('Retry conversation') }), id = created.task.objectId;
  await f.open(routeTo(id));
  const editor = f.page.getByRole('textbox', { name: 'Comment', exact: true });
  let attempt = 0, firstId;
  await f.page.route('**/api/v1/rpc', async route => {
    const body = route.request().postDataJSON();
    if (!isComment(body)) return route.continue();
    attempt++; firstId ??= body.params.arguments.commentId;
    assert.equal(body.params.arguments.commentId, firstId);
    if (attempt === 1) {
      // Make the observed revision stale before the actual operation is admitted.
      await f.invoke({ action: 'comment', taskId: id, expectedRevision: (await f.client.request('objects.stat', { objectId: id })).currentRevision,
        commentId: randomUUID(), body: 'Concurrent update', requests: [], responses: [], attachments: [], replyTo: null });
    }
    return route.continue();
  });
  await editor.fill('Retain on failure');
  await f.page.locator('input[type=file]').setInputFiles({ name: 'retained.txt', mimeType: 'text/plain', buffer: Buffer.from('Retained file') });
  await expect(editor.locator('a')).toHaveCount(1);
  await f.page.getByRole('button', { name: 'Add comment', exact: true }).click();
  const failed = f.page.locator('[data-slot=conversation-message]').filter({ hasText: 'Retain on failure' });
  await expect(failed.getByRole('button', { name: 'Retry message', exact: true })).toBeVisible();
  await failed.getByRole('button', { name: 'Retry message', exact: true }).click();
  await expect.poll(async () => (await f.task(id)).comments.filter(comment => comment.commentId === firstId).length).toBe(1);
  const comment = (await f.task(id)).comments.find(comment => comment.commentId === firstId);
  assert.equal(comment.attachments[0].filename, 'retained.txt');
  await expect(failed).toHaveCount(1);
  await f.page.unroute('**/api/v1/rpc');
  let original, calls = 0;
  await f.page.route('**/api/v1/rpc', route => {
    const body = route.request().postDataJSON();
    if (!isComment(body)) return route.continue();
    original = body.params; calls++; return route.abort();
  });
  await expect(f.page.getByRole('button', { name: 'Add comment', exact: true })).toBeDisabled();
  await editor.fill('Unknown original outcome');
  await expect(f.page.getByRole('button', { name: 'Add comment', exact: true })).toBeEnabled();
  await f.page.getByRole('button', { name: 'Add comment', exact: true }).click();
  const unknown = f.page.locator('[data-slot=conversation-message]').filter({ hasText: 'Unknown original outcome' });
  await expect(unknown.getByRole('button', { name: 'Retry message', exact: true })).toBeVisible();
  await unknown.getByRole('button', { name: 'Retry message', exact: true }).click();
  assert.equal(calls, 1);
  await f.page.reload();
  await expect(unknown).toHaveCount(1);
  await unknown.getByRole('button', { name: 'Retry message', exact: true }).click();
  assert.equal(calls, 1);
  assert.equal((await f.task(id)).comments.filter(comment => comment.commentId === original.arguments.commentId).length, 0);
  await f.client.request('tools.call', original); // Resolve the exact lost request, rather than inventing another comment.
  await f.page.evaluate(() => window.dispatchEvent(new Event('online')));
  await expect(unknown.getByRole('button', { name: 'Retry message', exact: true })).toHaveCount(0);
  await expect(unknown).toHaveCount(1);
  assert.equal(calls, 1);
  assert.deepEqual(f.pageErrors, []);
});

for (const width of [1440, 390]) test(`the common overlay, filtered counters and all Waiting groups work at ${width}px`, { timeout: 180000 }, async t => {
  const f = await taskBoardFixture(t, { native: true, viewport: { width, height: 900 } });
  const create = async (title, changes = {}, rest = {}) => (await f.invoke({ action: 'create', fields: { ...fields(title), ...changes }, ...rest })).task;
  const user = await create('User feedback', {}, { workflowState: 'waiting', waiting: { reason: 'user', detail: 'An answer is needed.' } });
  const dependency = await create('Dependency wait', { dependencies: [user.objectId] });
  await create('Runtime wait', {}, { workflowState: 'waiting', waiting: { reason: 'workspace', detail: 'Workspace is unavailable.' } });
  await create('Time wait', { nextReviewAt: '2099-01-01T12:00:00.000Z' }, { workflowState: 'waiting', waiting: { reason: 'time', detail: 'Resume on the scheduled date.' } });
  const ready = await create('Ready without a blocker', { control: 'agent' });
  await create('Due is not a restart', { control: 'agent', dueAt: '2099-01-01T12:00:00.000Z' });
  const unscheduled = await create('No restart set', { nextReviewAt: '2099-01-01T12:00:00.000Z' }, { workflowState: 'waiting', waiting: { reason: 'time', detail: 'The previous restart date is unavailable.' } });
  // Read-state tests use real retained mixed comments; source-task scheduling is tested separately.
  const store = f.taskBoard().runtime.engine.store, current = await store.read('task-board/task', user.objectId);
  const noDate = await store.read('task-board/task', unscheduled.objectId);
  await store.write('task-board/task', { ...noDate.value, fields: { ...noDate.value.fields, nextReviewAt: null } }, randomUUID(),
    { objectId: noDate.pin.objectId, expectedRevision: noDate.pin.revision });
  const now = new Date().toISOString();
  const comment = (id, authorKind, sourceTaskId) => ({ commentId: id, sequence: 0, author: { principalId: 'browser-fixture', serviceNodeId: 'browser-task-board', source: 'worker', generation: 1 },
    authorKind, body: id, requests: [], responses: [], delivery: null, attachments: [], run: null, turnId: null, replyTo: null, createdAt: now, operationId: id, ...(sourceTaskId ? { sourceTaskId } : {}) });
  const comments = [comment('User message', 'user'), comment('Direct agent answer', 'agent'), comment('Cross-agent note', 'agent', dependency.objectId)].map((value, i) => ({ ...value, sequence: i + 1 }));
  await store.write('task-board/task', { ...current.value, comments, agentCommentCount: 2 }, randomUUID(), { objectId: user.objectId, expectedRevision: current.pin.revision });
  await f.open('#/tasks?node=browser-task-board');
  const waiting = f.page.getByRole('region', { name: 'Waiting', exact: true });
  for (const [group, title] of [['user', 'User feedback'], ['dependency', 'Dependency wait'], ['runtime', 'Runtime wait'], ['time', 'Time wait'], ['ready', 'Ready without a blocker'], ['ready', 'Due is not a restart'], ['unscheduled', 'No restart set']]) {
    await expect(waiting.locator('[data-waiting-group=' + group + ']').getByRole('link', { name: title, exact: true })).toBeVisible();
  }
  await expect(f.page.getByRole('region', { name: 'Todo', exact: true })).toHaveCount(0);
  await expect(f.page.getByRole('region', { name: 'Blocked', exact: true })).toHaveCount(0);
  await expect(waiting.getByText('Needs you', { exact: true })).toHaveCount(0);
  const toggle = f.page.getByRole('button', { name: 'Cross-agent communication', exact: true });
  await expect(toggle).toHaveCount(1); await expect(toggle).toHaveAttribute('aria-pressed', 'false');
  const card = waiting.locator('article').filter({ hasText: 'User feedback' });
  await expect(card.locator('[title="2 comments"]')).toHaveCount(1);
  const board = await f.page.locator('[data-slot=task-board-background]').elementHandle();
  await card.getByRole('link', { name: 'User feedback', exact: true }).click();
  await expect(f.page.getByRole('dialog', { name: 'Task details' })).toBeVisible();
  await expect(f.page.getByRole('link', { name: 'Back to board' })).toHaveCount(0);
  await expect(f.page.getByText('Direct agent answer', { exact: true })).toBeVisible();
  await expect(f.page.getByText('Cross-agent note', { exact: true })).toHaveCount(0);
  await expect.poll(async () => (await f.taskBoard().runtime.engine.task({ task: user.objectId }, 'browser-fixture')).unreadAgentComments).toBe(1);
  await toggle.click();
  await expect(f.page.getByRole('dialog', { name: 'Task details' })).toBeVisible();
  await expect(f.page.getByText('Cross-agent note', { exact: true })).toBeVisible();
  await f.page.getByRole('button', { name: 'Close', exact: true }).click();
  assert.equal(await board.evaluate(element => element.isConnected), true);
  await expect(toggle).toHaveAttribute('aria-pressed', 'true');
  await f.page.goBack(); await expect(f.page.getByRole('dialog', { name: 'Task details' })).toBeVisible();
  await f.page.keyboard.press('Escape'); await expect(f.page.getByRole('dialog', { name: 'Task details' })).toHaveCount(0);
  await f.page.goto(f.url + routeTo(ready.objectId));
  await expect(f.page.getByRole('dialog', { name: 'Task details' })).toBeVisible();
  assert.equal(await f.page.locator('[data-slot=task-board-background] .task-board').count(), 1);
  await f.page.reload(); await expect(f.page.getByRole('button', { name: 'Close', exact: true })).toBeVisible();
  await expect(f.page.getByRole('textbox', { name: 'Comment', exact: true })).toBeEditable();
  await f.page.goto(f.url + routeTo(user.objectId));
  await expect(f.page.getByText('Direct agent answer', { exact: true })).toBeVisible();
  await expect(f.page.locator('[data-slot=task-board-background] .task-item')).toHaveCount(7);
  const screenshots = process.env.IVY_TASKBOARD_SCREENSHOTS;
  for (const theme of ['light', 'dark']) {
    await f.page.evaluate(theme => { document.documentElement.classList.toggle('dark', theme === 'dark'); localStorage.setItem('ivy.theme', theme); }, theme);
    if (screenshots) { await mkdir(screenshots, { recursive: true }); await f.page.screenshot({ path: join(screenshots, `ticket-${width}-${theme}.png`), animations: 'disabled' }); }
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  }
  await f.page.getByRole('button', { name: 'Close', exact: true }).click();
  await expect(f.page.getByRole('region', { name: 'Waiting', exact: true }).locator('article')).toHaveCount(7);
  await expect(f.page.getByRole('region', { name: 'Waiting', exact: true }).getByRole('link', { name: 'User feedback', exact: true })).toBeVisible();
  if (screenshots) for (const theme of ['light', 'dark']) {
    await f.page.evaluate(theme => document.documentElement.classList.toggle('dark', theme === 'dark'), theme);
    await f.page.screenshot({ path: join(screenshots, `board-${width}-${theme}.png`), fullPage: true, animations: 'disabled' });
  }
  assert.deepEqual(f.pageErrors, []);
});
