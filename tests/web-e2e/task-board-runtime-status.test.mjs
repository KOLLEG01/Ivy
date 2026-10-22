import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
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
  const store = runtime.engine.store, current = await store.read('task-board/task', id);
  const comments = Array.from({ length: 120 }, (_, index) => ({
    commentId: 'history-' + index, sequence: index + 1, body: 'Previous progress ' + index,
    author: { principalId: 'browser-fixture', serviceNodeId: 'browser-task-board', source: 'worker', generation: 1 },
    authorKind: 'agent', requests: [], responses: [], attachments: [], delivery: null, run: null,
    turnId: null, replyTo: null, createdAt: current.value.createdAt, operationId: 'history-' + index,
  }));
  await store.write('task-board/task', { ...current.value, comments, agentCommentCount: comments.length }, randomUUID(),
    { objectId: id, expectedRevision: current.pin.revision });
  const nodes = await f.client.request('objects.query', { contractKey: 'task-board/comment-node',
    where: { field: 'object.parentId', op: 'eq', value: id }, limit: 200 });
  assert.equal(nodes.nextCursor, null);
  const historyIds = new Set(nodes.items.map(item => item.objectId)), historyReads = [];
  await f.addAgent(undefined, 'slow-agent-host');
  let publicationVisible = true, failModelRefresh = false, editCalls = 0, releaseEdit;
  const blocked = new Set();
  let releaseSlow;
  const slowReads = new Promise(resolve => { releaseSlow = resolve; });
  t.after(() => { releaseSlow(); releaseEdit?.(); });
  await f.page.route('**/api/v1/rpc', async route => {
    const body = route.request().postDataJSON(), requests = [].concat(body);
    if (publicationVisible && requests.some(request => request.method === 'objects.read' && request.params.objectId === id)) {
      const response = await route.fetch(), json = await response.json();
      for (const reply of [].concat(json))
        if (requests.some(request => request.id === reply.id && request.method === 'objects.read' && request.params.objectId === id))
          reply.result.content.value.publication = current.pin;
      return route.fulfill({ response, json });
    }
    const calls = requests.filter(request => request.method === 'tools.call');
    if (failModelRefresh && calls.some(request => request.params.qualifiedName === 'codex.model/list')) return route.abort('failed');
    const slow = calls.filter(request => request.params.qualifiedName === 'agent.projects' || request.params.serviceNodeId === 'browser-second-agent');
    if (slow.length) {
      for (const request of slow) blocked.add(request.params.serviceNodeId + ':' + request.params.qualifiedName);
      await slowReads;
    }
    for (const request of calls) {
      const name = request.params.qualifiedName;
      if (name === 'task-board.edit') {
        editCalls++;
        await new Promise(resolve => { releaseEdit = resolve; });
      }
    }
    return route.continue();
  });
  await f.open('#/tasks?node=browser-task-board&id=' + id);
  await expect(f.page.locator('[data-slot=conversation-message]')).toHaveCount(50);
  f.page.on('request', request => {
    if (!request.url().endsWith('/api/v1/rpc') || request.method() !== 'POST') return;
    for (const body of [].concat(request.postDataJSON()))
      if (body?.method === 'objects.read' && historyIds.has(body.params.objectId)) historyReads.push(body.params);
  });
  for (const [width, effort, speed] of [[1440, 'Low', 'Fast'], [390, 'High', 'Standard']]) {
    await f.page.setViewportSize({ width, height: 1000 });
    await f.page.getByRole('button', { name: 'Edit task', exact: true }).click();
    const editor = f.page.getByRole('region', { name: 'Edit task', exact: true });
    await expect(editor.getByLabel('Model', { exact: true })).toBeEnabled();
    assert.deepEqual([...blocked], [], 'A placed task needs neither project lists nor other hosts.');
    publicationVisible = false;
    failModelRefresh = true;
    await f.page.evaluate(() => window.dispatchEvent(new Event('online')));
    await expect(editor.getByText('Execution options could not be refreshed', { exact: true })).toBeVisible();
    await expect(editor.getByLabel('Model', { exact: true })).toBeEnabled();
    await expect(editor.getByLabel('Title', { exact: true })).toBeDisabled();
    await editor.getByRole('group', { name: 'Reasoning', exact: true }).getByRole('button', { name: effort, exact: true }).click();
    await editor.getByRole('group', { name: 'Speed', exact: true }).getByRole('button', { name: speed, exact: true }).click();
    const save = editor.getByRole('button', { name: 'Save changes', exact: true });
    const beforeSave = editCalls;
    await save.click();
    await expect.poll(() => editCalls).toBe(beforeSave + 1);
    await expect(save).toBeDisabled();
    releaseEdit(); releaseEdit = null;
    await expect(editor).not.toBeVisible();
    await expect(f.page.getByText('Settings change pending', { exact: true })).toBeVisible();
    const saved = await f.task(id);
    assert.equal(saved.executionChangeRequested, true);
    assert.equal(saved.fields.nativeOptions.reasoningEffort, effort.toLowerCase());
    assert.equal(saved.fields.nativeOptions.serviceTier, speed.toLowerCase());
    assert.equal(saved.claim.run.objectId, runId);
    assert.equal(saved.workflowState, 'in_progress');
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    failModelRefresh = false;
  }
  releaseSlow();
  assert.deepEqual(historyReads, [], 'Changing execution settings needs no older comment pages.');
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
