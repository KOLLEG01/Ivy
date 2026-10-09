import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

for (const [layout, viewport] of [['desktop', { width: 1440, height: 1000 }], ['mobile', { width: 390, height: 844 }]]) {
  test(`Done menus archive individual tasks and the entire filtered column on ${layout}`, { timeout: 180000 }, async t => {
    const f = await taskBoardFixture(t, { native: true, viewport });
    const create = async (title, category = 'Batch', states = ['todo', 'in_progress', 'done']) => {
      let outcome = await f.invoke({ action: 'create', fields: { title, description: '', acceptanceCriteria: [], category,
        control: 'user', priority: 2, executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' },
        dependencies: [], userContact: 'ticket', nextReviewAt: null, dueAt: null } });
      for (const workflowState of states) outcome = await f.invoke({ action: 'transition', taskId: outcome.task.objectId,
        expectedRevision: outcome.task.revision, workflowState, detail: null });
      return outcome.task;
    };
    const batch = [];
    for (let index = 0; index < 21; index++) batch.push(await create('Bulk task ' + index));
    batch.push(await create('Cancelled task', 'Batch', ['cancelled']));
    const linked = await create('Archive linked task'), store = f.taskBoard().runtime.engine.store;
    const original = await store.read('task-board/task', linked);
    await store.write('task-board/task', { ...original.value, primaryResourceRef: { serviceNodeId: 'browser-agent', namespace: 'codex', kind: 'thread', nativeId: 'saved-task' } },
      randomUUID(), { objectId: linked.objectId, expectedRevision: linked.revision });
    const excluded = await create('Other category', 'Other'), active = await create('Active task', 'Batch', ['todo']);
    await f.open('#/tasks?node=browser-task-board&category=Batch');
    const done = f.page.getByRole('region', { name: 'Done', exact: true });
    await done.getByRole('button', { name: 'Actions for Archive linked task', exact: true }).click();
    await f.page.getByRole('menuitem', { name: 'Archive task', exact: true }).click();
    await expect(f.page.getByText('Action identity', { exact: true })).toHaveCount(0);
    await expect(f.page.getByRole('button', { name: 'Check original action', exact: true })).toHaveCount(0);
    await expect.poll(async () => (await f.client.request('objects.stat', { objectId: linked.objectId })).effectivelyArchived).toBe(true);
    assert.equal(f.threads.get('saved-task').archived, true);
    await expect(done.getByRole('button', { name: 'Done actions', exact: true })).toBeEnabled();
    await done.getByRole('button', { name: 'Done actions', exact: true }).click();
    await f.page.getByRole('menuitem', { name: 'Archive all tasks', exact: true }).click();
    await expect.poll(async () => (await Promise.all(batch.map(task => f.client.request('objects.stat', { objectId: task.objectId })))).every(task => task.effectivelyArchived), { timeout: 60000 }).toBe(true);
    assert.equal((await f.client.request('objects.stat', { objectId: excluded.objectId })).effectivelyArchived, false);
    assert.equal((await f.client.request('objects.stat', { objectId: active.objectId })).effectivelyArchived, false);
    await expect(done.getByRole('article')).toHaveCount(0);
    await f.page.getByRole('button', { name: 'Archive', exact: true }).click();
    await expect(f.page.getByRole('region', { name: 'Archive', exact: true }).getByRole('article').first()).toBeVisible();
    if (layout === 'desktop') {
      const remaining = [await create('Uncertain archive one'), await create('Uncertain archive two')], calls = [];
      await f.page.goto(f.url + '#/tasks?node=browser-task-board&category=Batch');
      await f.page.route('**/api/v1/rpc', async route => {
        const request = route.request().postDataJSON();
        if (request.method !== 'tools.call' || request.params.qualifiedName !== 'task-board.archive') return route.continue();
        calls.push(request.params); await route.fetch(); await route.abort();
      });
      await done.getByRole('button', { name: 'Done actions', exact: true }).click();
      await f.page.getByRole('menuitem', { name: 'Archive all tasks', exact: true }).click();
      await expect.poll(async () => (await Promise.all(remaining.map(task => f.client.request('objects.stat', { objectId: task.objectId })))).every(task => task.effectivelyArchived)).toBe(true);
      await expect(done.getByRole('button', { name: 'Done actions', exact: true })).toBeEnabled();
      assert.equal(calls.length, 2);
      assert.equal(new Set(calls.map(call => call.operationId)).size, 2);
      await expect(f.page.getByText('Action identity', { exact: true })).toHaveCount(0);
      await expect(f.page.getByRole('alert')).toHaveCount(0);
      await f.page.unroute('**/api/v1/rpc');
    }
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    assert.deepEqual(f.pageErrors, []);
  });
  test(`ticket archive, restoration and explicit fresh Codex work on ${layout}`, { timeout: 180000 }, async t => {
    const f = await taskBoardFixture(t, { native: true, scheduler: true, viewport });
    let outcome = await f.invoke({ action: 'create', fields: { title: 'Explicit fresh Codex work', description: 'Keep this ticket and its comments.',
      acceptanceCriteria: [], category: null, control: 'user', priority: 2, executionRequirement: { kind: 'host', hostId: 'isolated-agent-host' },
      workspaceRequirement: { kind: 'task_workspace' }, dependencies: [], userContact: 'ticket', nextReviewAt: null, dueAt: null } });
    for (const workflowState of ['todo', 'in_progress', 'done']) outcome = await f.invoke({ action: 'transition', taskId: outcome.task.objectId,
      expectedRevision: outcome.task.revision, workflowState, detail: null });
    const id = outcome.task.objectId, store = f.taskBoard().runtime.engine.store;
    const original = await store.read('task-board/task', outcome.task);
    await store.write('task-board/task', { ...original.value, primaryResourceRef: { serviceNodeId: 'browser-agent', namespace: 'codex', kind: 'thread', nativeId: 'saved-task' } },
      randomUUID(), { objectId: id, expectedRevision: original.pin.revision });
    const starts = () => f.current().sent.filter(frame => frame.method === 'thread/start').length;
    await f.open('#/tasks?id=' + id + '&node=browser-task-board');
    const fresh = f.page.getByRole('button', { name: /^(Start a new Codex chat|Neuen Codex-Chat starten)$/ });
    await expect(fresh).toBeVisible(); await expect(fresh).toBeEnabled(); assert.equal(starts(), 0);
    const archiveAction = async name => {
      await f.page.getByRole('button', { name: 'More actions', exact: true }).click();
      await f.page.getByRole('menuitem', { name, exact: true }).click();
    };
    await archiveAction('Archive task');
    await expect.poll(async () => (await f.client.request('objects.stat', { objectId: id })).effectivelyArchived).toBe(true);
    assert.equal(f.threads.get('saved-task').archived, true);
    await expect(fresh).toBeDisabled();
    await archiveAction('Restore task');
    await expect.poll(async () => (await f.client.request('objects.stat', { objectId: id })).effectivelyArchived).toBe(false);
    assert.equal(f.threads.get('saved-task').archived, false); assert.equal((await f.task(id)).workflowState, 'done');
    await expect(fresh).toBeEnabled(); assert.equal(starts(), 0);
    const initialThreads = f.threads.size;
    await fresh.click();
    await expect.poll(async () => (await f.task(id)).primaryResourceRef.nativeId, { timeout: 30000 }).not.toBe('saved-task');
    const task = await f.task(id); assert.equal(task.fields.control, 'agent');
    assert.ok(['todo', 'in_progress'].includes(task.workflowState)); assert.equal(starts(), 1);
    assert.equal(f.threads.get('saved-task').archived, false); assert.equal(f.threads.size, initialThreads + 1);
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    assert.deepEqual(f.pageErrors, []);
  });
}
