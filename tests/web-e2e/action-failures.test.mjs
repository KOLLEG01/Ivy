import test from 'node:test';
import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { agentFixture } from './fixtures/agent-fixture.mjs';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

for (const width of [1440, 390]) {
  test(`AgentUI shows the original native failure and recovers its receipt without resending at ${width}px`, { timeout: 90000 }, async t => {
    const f = await agentFixture(t, { width, height: 1000 });
    await f.open('#/task?node=browser-agent&id=saved-task');
    await f.page.getByRole('button', { name: 'Attach historical task', exact: true }).click();
    const thread = f.threads.get('saved-task');
    const failure = 'failed to submit turn input: internal error; agent loop died unexpectedly';
    thread.submitError = { code: -32603, message: failure };
    const message = f.page.getByLabel('Message', { exact: true });
    await message.fill('Keep my unsent command.');
    await f.page.getByRole('button', { name: 'Send message', exact: true }).click();
    await expect(f.page.getByRole('alert').getByText(failure, { exact: true })).toBeVisible();
    await expect(message).toHaveValue('Keep my unsent command.');
    assert.equal(thread.turns.length, 0);
    const original = await f.page.evaluate(() => {
      const key = Object.keys(sessionStorage).find(key => key.startsWith('ivy:agent-task-action:'));
      const value = JSON.parse(sessionStorage.getItem(key));
      value.detail = 'Native method returned its original error; inspect this caller operation.';
      sessionStorage.setItem(key, JSON.stringify(value));
      return value.operationId;
    });
    await f.page.reload();
    await expect(f.page.getByRole('alert').getByText(failure, { exact: true })).toBeVisible();
    await f.page.getByRole('button', { name: 'Check status', exact: true }).click();
    assert.equal(f.current().sent.filter(frame => frame.method === 'turn/start').length, 1);
    assert.equal(await f.page.evaluate(() => JSON.parse(Object.values(sessionStorage).find(raw => raw.includes('"label":"Send message"'))).operationId), original);
    delete thread.submitError;
    await message.fill('Send after recovery.');
    await f.page.getByRole('button', { name: 'Send message', exact: true }).click();
    await expect.poll(() => thread.turns.length).toBe(1);
    assert.equal(f.current().sent.filter(frame => frame.method === 'turn/start').length, 2);
    assert.deepEqual(f.pageErrors, []);
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  });

  test(`TaskBoard dismisses an old terminal failure without retrying its action at ${width}px`, { timeout: 90000 }, async t => {
    const f = await taskBoardFixture(t, { viewport: { width, height: 1000 } });
    await f.open('#/tasks?node=browser-task-board');
    await expect(f.page.getByRole('heading', { name: 'Task board', exact: true })).toBeVisible();
    const key = 'ivy:task-board:action:' + f.base + '/browser-task-board:board';
    await f.page.evaluate(({ key }) => sessionStorage.setItem(key, JSON.stringify({
      label: 'Archive task', callerPrincipalId: 'browser-fixture', principalId: 'browser-fixture', rootObjectId: null,
      call: { operationId: 'retained-failed-action', serviceNodeId: 'browser-task-board', qualifiedName: 'task-board.archive', arguments: {} },
      phase: 'failed', detail: 'Object revision was pruned by its retention policy. (revision_pruned; not_executed)', outcome: null,
    })), { key });
    let mutations = 0;
    f.page.on('request', request => {
      const body = request.url().endsWith('/api/v1/rpc') ? request.postDataJSON() : null;
      if (body?.method === 'tools.call' && body.params.qualifiedName === 'task-board.archive') mutations++;
    });
    await f.page.reload();
    await expect(f.page.getByText(/Object revision was pruned/)).toBeVisible();
    await f.page.getByRole('button', { name: 'Dismiss', exact: true }).click();
    await expect(f.page.getByText(/Object revision was pruned/)).toHaveCount(0);
    await f.page.reload();
    await expect(f.page.getByText(/Object revision was pruned/)).toHaveCount(0);
    await expect(f.page.getByRole('alert')).toHaveCount(0);
    assert.equal(await f.page.evaluate(key => sessionStorage.getItem(key), key), null);
    assert.equal(mutations, 0);
    await f.page.evaluate(({ key }) => sessionStorage.setItem(key, JSON.stringify({
      label: 'Archive task', callerPrincipalId: 'browser-fixture', principalId: 'browser-fixture', rootObjectId: null,
      call: { operationId: 'unresolved-action', serviceNodeId: 'browser-task-board', qualifiedName: 'task-board.archive', arguments: {} },
      phase: 'unknown', detail: 'The original result is unresolved.', outcome: null,
    })), { key });
    await f.page.reload();
    await expect(f.page.getByText('The original result is unresolved.', { exact: true })).toBeVisible();
    await expect(f.page.getByRole('button', { name: 'Dismiss', exact: true })).toHaveCount(0);
    assert.deepEqual(f.pageErrors, []);
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  });
}
