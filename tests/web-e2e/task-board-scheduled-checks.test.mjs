import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

const fields = title => ({ title, description: 'Isolated next-step and conversation test.', acceptanceCriteria: [], category: null,
  control: 'user', priority: 3, executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' }, userContact: 'ticket', dueAt: null });
const question = async (fixture, task, commentId, body) => fixture.invoke({ action: 'comment', taskId: task.objectId, expectedRevision: task.revision,
  commentId, authorKind: 'agent', purpose: 'question', body, requests: [], responses: [], attachments: [], replyTo: null });

for (const [layout, viewport] of [['desktop', { width: 1440, height: 1000 }], ['mobile', { width: 390, height: 844 }]]) {
  test(`foreign questions never mark a dependent ticket; own questions remain pinned on ${layout}`, { timeout: 180000 }, async t => {
    const fixture = await taskBoardFixture(t, { viewport });
    const parent = await fixture.invoke({ action: 'create', fields: fields('Parent approval') });
    await question(fixture, parent.task, 'parent-approval', 'May I publish the prepared delivery?');
    const child = await fixture.invoke({ action: 'create', workflowState: 'waiting', fields: fields('Dependent worker'),
      nextSteps: [{ id: 'dependency', scope: 'Verify the parent result.', requires: [{ kind: 'task', taskId: parent.task.objectId, until: 'done' }] }] });
    await fixture.open('#/tasks?node=browser-task-board');
    const parentCard = fixture.page.getByRole('article').filter({ hasText: 'Parent approval' });
    const childCard = fixture.page.locator('[data-waiting-group=dependency]').getByRole('article').filter({ hasText: 'Dependent worker' });
    await expect(parentCard).toHaveAttribute('data-attention', 'question');
    await expect(childCard).toBeVisible();
    await expect(childCard).not.toHaveAttribute('data-attention', 'question');
    await fixture.page.goto(fixture.url + '#/tasks?node=browser-task-board&id=' + parent.task.objectId);
    const pinned = fixture.page.locator('[data-slot=task-pending-questions]');
    await expect(pinned).toContainText('May I publish the prepared delivery?');
    await expect(fixture.page.locator('[data-slot=task-comment-composer]')).toBeVisible();
    const bounds = await pinned.boundingBox();
    assert.ok(bounds && bounds.y + bounds.height <= viewport.height + 1);
    await fixture.page.getByRole('tab', { name: 'Details', exact: true }).click();
    await expect(pinned).toContainText('May I publish the prepared delivery?');
    assert.equal(await fixture.page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false);
    assert.deepEqual(fixture.pageErrors, []);
  });

  test(`time, questions and negative or unavailable findings stay independent after an addressed comment on ${layout}`, { timeout: 180000 }, async t => {
    const fixture = await taskBoardFixture(t, { viewport });
    const at = new Date().toISOString(), appointment = new Date(Date.now() + 6 * 60 * 60_000).toISOString();
    const nextSteps = [{ id: 'observe', scope: 'Read the pending approval.', requires: [{ kind: 'time', at: appointment }] }];
    const created = await fixture.invoke({ action: 'create', workflowState: 'waiting', fields: fields('Approval watch'), nextSteps });
    const asked = await question(fixture, created.task, 'approve-write', 'May I publish the prepared change?');
    const store = fixture.taskBoard().runtime.engine.store, current = await store.read('task-board/task', asked.task);
    const latestChecks = [
      { key: 'approval', execution: 'completed', finding: { conditionMet: false, detail: 'Review is still pending.' }, issue: null, checkedAt: at, evidence: [] },
      { key: 'portal', execution: 'blocked', finding: null, issue: { code: 'login_required', detail: 'Sign in before checking this target.' }, checkedAt: at, evidence: [] },
    ];
    await store.write('task-board/task', { ...current.value, latestChecks }, randomUUID(), { objectId: current.pin.objectId, expectedRevision: current.pin.revision });
    await fixture.open('#/tasks?node=browser-task-board');
    const card = fixture.page.locator('[data-waiting-group=time]').getByRole('article').filter({ hasText: 'Approval watch' });
    await expect(card).toHaveAttribute('data-attention', 'question');
    await expect(card).not.toContainText('Not yet met');
    await expect(card).not.toContainText('Check blocked');
    await expect(card.getByText('Check failed', { exact: true })).toHaveCount(0);
    await fixture.page.goto(fixture.url + '#/tasks?node=browser-task-board&id=' + created.task.objectId);
    const retainedNote = fixture.page.locator('[data-slot=conversation-message]').filter({ hasText: 'Preserved verification findings' });
    await expect(retainedNote).toContainText('System');
    await expect(retainedNote.locator('code')).toContainText('Review is still pending.');
    await expect(retainedNote.getByRole('button', { name: 'Show more', exact: true })).toBeVisible();
    const pinned = fixture.page.locator('[data-slot=task-pending-questions]');
    await pinned.getByRole('button', { name: 'Answer question', exact: true }).click();
    await expect(fixture.page.getByText('Answering:', { exact: false })).toBeVisible();
    await fixture.page.getByRole('textbox', { name: 'Comment', exact: true }).fill('Approved for the prepared change.');
    await fixture.page.getByRole('button', { name: 'Add comment', exact: true }).click();
    await expect(pinned).toHaveCount(0);
    const task = await fixture.task(created.task.objectId);
    assert.equal(task.comments.at(-1).replyTo, 'approve-write');
    assert.equal(task.openQuestionCount, 0);
    assert.deepEqual(task.nextSteps, nextSteps);
    assert.equal('latestChecks' in task, false);
    assert.ok(task.comments.some(comment => comment.body.includes('Review is still pending.') && comment.body.includes('Sign in before checking this target.')));
    assert.equal(await fixture.page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false);
    assert.deepEqual(fixture.pageErrors, []);
  });
}
