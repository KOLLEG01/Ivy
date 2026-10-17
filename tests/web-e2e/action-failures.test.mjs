import test from 'node:test';
import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { agentFixture } from './fixtures/agent-fixture.mjs';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

const taskFields = title => ({ title, description: '', acceptanceCriteria: [], category: null, control: 'user', priority: 2,
  executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' }, dependencies: [], userContact: 'ticket', nextReviewAt: null, dueAt: null });

for (const width of [1440, 390]) {
  test(`TaskBoard reconciles a lost archive after reload without blocking other tickets at ${width}px`, { timeout: 90000 }, async t => {
    const f = await taskBoardFixture(t, { viewport: { width, height: 1000 } });
    let target = await f.invoke({ action: 'create', workflowState: 'backlog', fields: taskFields('Lost archive response') });
    for (const workflowState of ['todo', 'in_progress', 'done']) target = await f.invoke({ action: 'transition', taskId: target.task.objectId,
      expectedRevision: target.task.revision, workflowState, detail: null });
    const other = await f.invoke({ action: 'create', workflowState: 'todo', fields: taskFields('Independent ticket') });
    let receipt = 'unavailable';
    const calls = [], checks = [];
    const failure = (request, code) => ({ jsonrpc: '2.0', id: request.id,
      error: { code: -32000, message: 'Archive receipt: ' + code, data: { code, outcome: code === 'service_unavailable' ? 'unknown' : 'not_executed' } } });
    await f.page.route('**/api/v1/rpc', async route => {
      const body = route.request().postDataJSON(), requests = Array.isArray(body) ? body : [body];
      const archive = requests.find(request => request.method === 'tools.call' && request.params.qualifiedName === 'task-board.archive');
      if (archive) {
        calls.push(archive.params);
        if (calls.length > 1) return route.fulfill({ json: failure(archive, 'operation_expired') });
        await route.fetch(); return route.abort();
      }
      const operations = requests.filter(request => request.method === 'tools.call' && request.params.qualifiedName === 'task-board.operation');
      if (!operations.length) return route.continue();
      const mode = receipt;
      const response = await route.fetch(), raw = await response.json();
      const replies = (Array.isArray(raw) ? raw : [raw]).map(reply => {
        const request = operations.find(request => request.id === reply.id);
        if (!request) return reply;
        if (mode === 'pending') return { ...reply, result: { ...reply.result, phase: 'needs_attention', outcome: null,
          error: { code: 'outcome_unknown', message: 'Original archive is still unresolved.', execution: 'unknown' } } };
        return failure(request, mode === 'unavailable' ? 'service_unavailable' : mode);
      });
      await route.fulfill({ response, json: Array.isArray(raw) ? replies : replies[0] });
      checks.push(mode);
    });
    await f.open('#/tasks?node=browser-task-board');
    const done = f.page.getByRole('region', { name: 'Done', exact: true });
    await done.getByRole('button', { name: 'Actions for Lost archive response', exact: true }).click();
    await f.page.getByRole('menuitem', { name: 'Archive task', exact: true }).click();
    const retry = done.getByRole('button', { name: 'Retry archive', exact: true });
    await expect(retry).toBeVisible();
    const retained = () => f.page.evaluate(() => {
      const key = Object.keys(sessionStorage).find(key => key.startsWith('ivy:task-board:archive:') && key.endsWith(':board'));
      return key ? JSON.parse(sessionStorage.getItem(key)) : null;
    });
    const original = await retained();
    assert.equal(original.objectId, target.task.objectId);
    assert.equal((await f.client.request('objects.stat', { objectId: original.objectId })).effectivelyArchived, true);
    await f.page.reload();
    await expect(retry).toBeVisible();
    assert.equal((await retained()).mutationId, original.mutationId);
    await expect(done.getByRole('link', { name: 'Open task', exact: true })).toHaveAttribute('href', new RegExp(original.objectId));
    const card = f.page.getByRole('article').filter({ hasText: 'Independent ticket' });
    await expect(card).toHaveAttribute('draggable', 'true');
    if (width === 1440) await card.dragTo(f.page.getByRole('region', { name: 'In progress', exact: true }));
    else {
      await card.getByRole('button', { name: 'Actions for Independent ticket', exact: true }).click();
      await f.page.getByRole('menuitem', { name: 'Start manual work', exact: true }).click();
    }
    await expect.poll(async () => (await f.task(other.task.objectId)).workflowState).toBe('in_progress');
    const recheck = async mode => {
      const before = checks.length;
      receipt = mode;
      await f.page.evaluate(() => window.dispatchEvent(new Event('online')));
      await expect.poll(() => checks.length).toBeGreaterThan(before);
      await expect(retry).toBeEnabled();
      assert.equal((await retained()).mutationId, original.mutationId);
    };
    // Even an already archived Object cannot settle a live/unknown owner workflow.
    await recheck('pending');
    await recheck('not_found');
    const metadata = await f.client.request('objects.stat', { objectId: original.objectId });
    await f.invoke({ action: 'archive', taskId: metadata.id, expectedRevision: metadata.currentRevision, archived: false });
    await recheck('operation_expired');
    const pendingCard = f.page.getByRole('article').filter({ hasText: 'Lost archive response' });
    await expect(pendingCard).toHaveAttribute('draggable', 'false');
    await pendingCard.getByRole('button', { name: 'Actions for Lost archive response', exact: true }).click();
    await expect(f.page.getByRole('menuitem', { name: 'Move to Todo', exact: true })).toBeDisabled();
    await f.page.keyboard.press('Escape');
    await retry.click();
    await expect(retry).toBeEnabled();
    assert.deepEqual(await retained(), original);
    assert.equal(calls.length, 2);
    assert.deepEqual(calls[0], calls[1]);
    const restored = await f.client.request('objects.stat', { objectId: original.objectId });
    await f.invoke({ action: 'archive', taskId: restored.id, expectedRevision: restored.currentRevision, archived: true });
    // A terminal eviction guard plus matching authoritative state needs no replay.
    await f.page.evaluate(() => window.dispatchEvent(new Event('online')));
    await expect.poll(retained).toBe(null);
    await expect(retry).toHaveCount(0);
    await f.page.reload();
    await expect(done).toBeVisible();
    await expect(retry).toHaveCount(0);
    assert.equal(calls.length, 2);
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    assert.deepEqual(f.pageErrors, []);
  });

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

  test(`TaskBoard clears a terminal archive failure and keeps unresolved archives inline at ${width}px`, { timeout: 90000 }, async t => {
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
    await expect(f.page.getByRole('region', { name: 'Done', exact: true })).toBeVisible();
    await expect(f.page.getByText(/Object revision was pruned/)).toHaveCount(0);
    await f.page.reload();
    await expect(f.page.getByRole('region', { name: 'Done', exact: true })).toBeVisible();
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
    const done = f.page.getByRole('region', { name: 'Done', exact: true });
    await expect(done.getByRole('button', { name: 'Retry archive', exact: true })).toBeVisible();
    await expect(f.page.getByText('Action identity', { exact: true })).toHaveCount(0);
    await expect(f.page.getByRole('button', { name: 'Dismiss', exact: true })).toHaveCount(0);
    const retry = done.getByRole('button', { name: 'Retry archive', exact: true });
    await retry.click();
    await expect.poll(() => mutations).toBe(1);
    await expect(retry).toBeEnabled();
    assert.equal(await f.page.evaluate(key => JSON.parse(sessionStorage.getItem(key)).phase, key), 'unknown');
    const independent = await f.invoke({ action: 'create', workflowState: 'todo', fields: taskFields('Move despite retained archive') });
    await f.page.evaluate(() => window.dispatchEvent(new Event('online')));
    const card = f.page.getByRole('article').filter({ hasText: 'Move despite retained archive' });
    await expect(card).toHaveAttribute('draggable', 'true');
    await card.getByRole('button', { name: 'Actions for Move despite retained archive', exact: true }).click();
    await f.page.getByRole('menuitem', { name: 'Start manual work', exact: true }).click();
    await expect.poll(async () => (await f.task(independent.task.objectId)).workflowState).toBe('in_progress');
    assert.equal(await f.page.evaluate(key => JSON.parse(sessionStorage.getItem(key)).call.operationId, key), 'unresolved-action');
    assert.deepEqual(f.pageErrors, []);
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  });
}
