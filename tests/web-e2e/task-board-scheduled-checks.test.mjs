import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { newOperationId } from '../../dist/packages/sdk/src/client.js';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

for (const [layout, viewport] of [['desktop', { width: 1440, height: 1000 }], ['mobile', { width: 390, height: 844 }]]) {
  test(`compact conversations separate missing handoff from a linked user question on ${layout}`, { timeout: 180000 }, async t => {
    const f = await taskBoardFixture(t, { viewport });
    const errors = []; f.page.on('pageerror', error => errors.push(error.message));
    const fields = { title: 'Parent approval', description: 'The question is held by the parent.', acceptanceCriteria: [], category: null,
      control: 'user', priority: 3, executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' },
      dependencies: [], userContact: 'ticket', nextReviewAt: null, dueAt: null };
    const parent = await f.invoke({ action: 'create', workflowState: 'waiting', waiting: { reason: 'user', detail: 'An approval is outstanding.' }, fields });
    await f.invoke({ action: 'comment', taskId: parent.task.objectId, expectedRevision: parent.task.revision,
      commentId: 'parent-approval', authorKind: 'agent', purpose: 'question', body: 'May I publish the prepared delivery?',
      requests: [], responses: [], attachments: [], replyTo: null });
    const child = await f.invoke({ action: 'create', workflowState: 'waiting', waiting: { reason: 'user', detail: 'The execution ended without a handoff.' },
      fields: { ...fields, title: 'Worker handoff' } });
    const store = f.taskBoard().runtime.engine.store, task = await store.read('task-board/task', child.task);
    const issue = { code: 'task_board_handoff_missing', detail: 'The original execution needs its structured handoff.', run: { objectId: randomUUID(), revision: 1 }, operationId: 'original-completion', attempts: 2, retryAt: null };
    await store.write('task-board/task', { ...task.value, completionIssue: issue }, randomUUID(), { objectId: task.pin.objectId, expectedRevision: task.pin.revision });
    await f.open('#/tasks?node=browser-task-board');
    const card = f.page.getByRole('article').filter({ hasText: 'Worker handoff' });
    await expect(card.getByText('Workflow needs attention', { exact: true })).toBeVisible();
    await expect(card.getByText('Needs you', { exact: true })).toHaveCount(0);
    await f.page.goto(f.url + '#/tasks?node=browser-task-board&id=' + child.task.objectId);
    await expect(f.page.getByText(issue.detail, { exact: true })).toBeVisible();
    const current = await store.read('task-board/task', child.task.objectId);
    await store.write('task-board/task', { ...current.value, questionWait: { taskId: parent.task.objectId, commentId: 'parent-approval' } }, randomUUID(),
      { objectId: current.pin.objectId, expectedRevision: current.pin.revision });
    await f.page.reload();
    await expect(f.page.getByText('Linked user question', { exact: true })).toBeVisible();
    const link = f.page.getByRole('link', { name: 'View the open question in the linked ticket', exact: true });
    await expect(link).toHaveAttribute('href', /question=parent-approval/);
    await link.click();
    await expect(f.page.getByText('May I publish the prepared delivery?', { exact: true })).toBeVisible();
    await expect(f.page.getByText('Open question', { exact: true })).toBeVisible();
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth + 1), false);
    assert.deepEqual(errors, []);
  });
  test(`mixed-version board pages retain questions and failed checks, with an addressed answer on ${layout}`, { timeout: 180000 }, async t => {
    const f = await taskBoardFixture(t, { viewport });
    const errors = []; f.page.on('pageerror', error => errors.push(error.message));
    const at = new Date().toISOString(), nextReviewAt = new Date(Date.now() + 6 * 60 * 60_000).toISOString();
    const created = await f.invoke({ action: 'create', workflowState: 'waiting', waiting: { reason: 'user', detail: 'An answer is needed; the independent check remains scheduled.' },
      fields: { title: 'Approval watch', description: 'A finished check does not answer the pending question.', acceptanceCriteria: [], category: null,
        control: 'user', priority: 4, executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' },
        dependencies: [], userContact: 'ticket', nextReviewAt, dueAt: null } });
    const store = f.taskBoard().runtime.engine.store, original = await store.read('task-board/task', created.task);
    await store.write('task-board/task', { ...original.value, openQuestionCount: 1,
      scheduledCheck: { scope: 'Read the approval status.', failure: { code: 'browser_unavailable', detail: 'The approval portal could not be checked.', at } },
      comments: [{ commentId: 'approve-write', sequence: 1, author: { principalId: 'worker', serviceNodeId: 'browser-task-board', generation: 1, source: 'worker' }, authorKind: 'agent',
        purpose: 'question', body: 'May I publish the prepared change?', requests: [], responses: [], attachments: [], delivery: null, run: null, turnId: null,
        replyTo: null, createdAt: at, operationId: 'question' }] }, randomUUID(), { objectId: original.pin.objectId, expectedRevision: original.pin.revision });
    // Historical contracts must remain readable alongside the new check fields.
    const { openQuestionCount: _questions, scheduledCheck: _check, ...legacy } = original.value;
    for (let index = 0; index < 21; index++) await f.client.request('objects.write', {
      mutationId: await newOperationId(f.client), contractVersion: '1.8.0', references: {},
      create: { contractKey: 'task-board/task', parentId: store.rootObjectId, ownerObjectId: null, name: 'Legacy watch ' + index },
      content: { encoding: 'json', value: { ...legacy, taskKey: 'TASK-' + (9000 + index),
        fields: { ...legacy.fields, title: 'Legacy watch ' + index, priority: 2, nextReviewAt: null } } },
    });
    await f.open('#/tasks?node=browser-task-board');
    const blocked = f.page.getByRole('region', { name: 'Blocked', exact: true });
    const currentCard = blocked.getByRole('article').filter({ hasText: 'Approval watch' });
    await expect(blocked.getByRole('article')).toHaveCount(20);
    await expect(currentCard.getByText('Needs you', { exact: true })).toBeVisible();
    await expect(currentCard.getByText('Check failed', { exact: true })).toBeVisible();
    await expect(blocked.getByRole('article').filter({ hasText: 'Legacy watch' })).toHaveCount(19);
    for (const name of ['Todo', 'In progress', 'Review', 'Done'])
      await expect(f.page.getByRole('region', { name, exact: true }).getByText('No tasks', { exact: true })).toBeVisible();
    await blocked.getByRole('button', { name: 'Next', exact: true }).click();
    await expect(blocked.getByRole('article')).toHaveCount(2);
    await expect(blocked.getByText('Check failed', { exact: true })).toHaveCount(0);
    await blocked.getByRole('button', { name: 'Previous', exact: true }).click();
    await expect(currentCard.getByText('Check failed', { exact: true })).toBeVisible();
    await f.page.goto(f.url + '#/tasks?node=browser-task-board&q=Approval');
    await expect(blocked.getByRole('article')).toHaveCount(1);
    await expect(currentCard.getByText('Check failed', { exact: true })).toBeVisible();
    await expect(f.page.getByText('This view is unavailable', { exact: true })).toHaveCount(0);
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth + 1), false);
    await f.page.goto(f.url + '#/tasks?node=browser-task-board&id=' + created.task.objectId);
    await expect(f.page.getByText('Scheduled check failed', { exact: true })).toBeVisible();
    await expect(f.page.getByText('The approval portal could not be checked.', { exact: false }).first()).toBeVisible();
    await f.page.getByRole('tab', { name: 'Details', exact: true }).click();
    await expect(f.page.getByText('Next automatic check', { exact: true })).toBeVisible();
    await f.page.getByRole('tab', { name: 'Chat', exact: true }).click();
    await expect(f.page.getByText('Open question', { exact: true })).toBeVisible();
    await f.page.getByRole('button', { name: 'Answer question', exact: true }).click();
    await expect(f.page.getByText('Answering:', { exact: false })).toBeVisible();
    const composer = f.page.getByRole('textbox', { name: 'Comment', exact: true });
    await composer.fill('Approved for the prepared change.');
    await f.page.getByRole('button', { name: 'Add comment', exact: true }).click();
    await expect(f.page.getByText('Open question', { exact: true })).toHaveCount(0);
    const task = await f.task(created.task.objectId);
    assert.equal(task.comments.at(-1).replyTo, 'approve-write');
    assert.equal(task.openQuestionCount, 0);
    assert.equal(task.fields.nextReviewAt, nextReviewAt);
    assert.equal(task.scheduledCheck.failure.code, 'browser_unavailable');
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth + 1), false);
    assert.deepEqual(errors, []);
  });
}
