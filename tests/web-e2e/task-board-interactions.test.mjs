import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';
import { expect } from '@playwright/test';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

const fields = title => ({ title, description: '', acceptanceCriteria: [], category: null, control: 'user', priority: 2,
  executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' },  userContact: 'ticket',  dueAt: null });
const gate = () => { let release; return { promise: new Promise(resolve => { release = resolve; }), release: () => release() }; };
const routeTo = id => '#/tasks?node=browser-task-board&id=' + id;
const isComment = body => body.method === 'tools.call' && body.params.qualifiedName === 'task-board.comment';

test('optimistic comments render once before dispatch and acknowledgement, retaining a new draft', { timeout: 180000 }, async t => {
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
  await expect(message.getByRole('status')).toHaveText('Sending…');
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

for (const width of [1440, 390]) test(`posting with an open question stays writable across conversation pages at ${width}px`, { timeout: 120000 }, async t => {
  const f = await taskBoardFixture(t, { viewport: { width, height: 900 } });
  const created = await f.invoke({ action: 'create', fields: fields('Long conversation with an open question') }), id = created.task.objectId;
  const store = f.taskBoard().runtime.engine.store, current = await store.read('task-board/task', id);
  const comments = Array.from({ length: 120 }, (_, index) => ({
    commentId: 'existing-' + (index + 1), sequence: index + 1,
    author: { principalId: 'browser-fixture', serviceNodeId: 'browser-task-board', source: 'worker', generation: 1 },
    authorKind: 'agent', body: index ? 'Existing message ' + (index + 1) : 'Which implementation should continue?',
    ...(index ? {} : { purpose: 'question', recipient: { kind: 'user' } }),
    requests: [], responses: [], attachments: [], delivery: null, run: null, turnId: null, replyTo: null,
    createdAt: current.value.createdAt, operationId: 'existing-' + (index + 1),
  }));
  await store.write('task-board/task', { ...current.value, comments, agentCommentCount: 120, openQuestionCount: 1 }, randomUUID(),
    { objectId: id, expectedRevision: current.pin.revision });
  const nodes = await f.client.request('objects.query', { contractKey: 'task-board/comment-node',
    where: { field: 'object.parentId', op: 'eq', value: id }, limit: 200 });
  assert.equal(nodes.nextCursor, null);
  const historyIds = new Set(nodes.items.map(item => item.objectId)), historyReads = [];
  await f.open(routeTo(id));
  const questions = f.page.locator('[data-slot=task-pending-questions]');
  await expect(questions).toContainText('Which implementation should continue?');
  await expect(f.page.locator('[data-slot=conversation-message]')).toHaveCount(50);
  f.page.on('request', request => {
    if (!request.url().endsWith('/api/v1/rpc') || request.method() !== 'POST') return;
    for (const body of [].concat(request.postDataJSON()))
      if (body?.method === 'objects.read' && historyIds.has(body.params.objectId)) historyReads.push(body.params);
  });
  const editor = f.page.getByRole('textbox', { name: 'Comment', exact: true });
  const send = f.page.getByRole('button', { name: 'Add comment', exact: true });
  await editor.fill('An ordinary comment while the question stays open');
  await send.click();
  await editor.fill('The next draft remains writable');
  await expect.poll(async () => (await f.task(id)).comments.some(comment => comment.body === 'An ordinary comment while the question stays open')).toBe(true);
  await expect(send).toBeEnabled();
  await expect(editor).toContainText('The next draft remains writable');
  await expect(questions).toContainText('Which implementation should continue?');
  assert.deepEqual(historyReads, [], 'Posting needs the current ticket revision, not its comment tree.');
  const latest = await store.read('task-board/task', id);
  const newer = Array.from({ length: 60 }, (_, index) => ({
    ...comments[1], commentId: 'newer-' + index, operationId: 'newer-' + index,
    sequence: latest.value.comments.length + index + 1, body: 'Later agent update ' + index,
  }));
  await store.write('task-board/task', { ...latest.value, comments: [...latest.value.comments, ...newer],
    agentCommentCount: latest.value.agentCommentCount + newer.length }, randomUUID(),
    { objectId: id, expectedRevision: latest.pin.revision });
  await f.page.reload();
  await expect(f.page.locator('[data-slot=conversation-message]')).toHaveCount(50);
  await expect(f.page.locator('[data-slot=conversation-message]').filter({ hasText: 'An ordinary comment while the question stays open' })).toHaveCount(0);
  await editor.fill('A new message after reopening');
  await expect(send).toBeEnabled();
  await send.click();
  await expect.poll(async () => (await f.task(id)).comments.some(comment => comment.body === 'A new message after reopening')).toBe(true);
  assert.equal((await f.task(id)).comments.filter(comment => comment.body === 'An ordinary comment while the question stays open').length, 1);
  assert.deepEqual(f.pageErrors, []);
});

for (const width of [1440, 390]) test(`a retained capacity rejection does not lock new comments or question answers at ${width}px`, { timeout: 120000 }, async t => {
  const f = await taskBoardFixture(t, { viewport: { width, height: 900 } });
  const created = await f.invoke({ action: 'create', fields: fields('Rejected comment recovery') }), id = created.task.objectId;
  await f.invoke({ action: 'comment', taskId: id, expectedRevision: created.task.revision, authorKind: 'agent', purpose: 'question',
    recipient: { kind: 'user' }, commentId: 'own-question', body: 'Which implementation should continue?', requests: [], responses: [], attachments: [], replyTo: null });
  await f.open(routeTo(id));
  await f.page.locator('[data-slot=task-pending-questions]').getByRole('button', { name: 'Answer question', exact: true }).click();
  const editor = f.page.getByRole('textbox', { name: 'Comment', exact: true });
  const send = f.page.getByRole('button', { name: 'Add comment', exact: true });
  const reject = gate(), started = gate();
  t.after(() => reject.release());
  const calls = [];
  await f.page.route('**/api/v1/rpc', async route => {
    const body = route.request().postDataJSON();
    if (!isComment(body)) return route.continue();
    calls.push(body.params);
    if (calls.length !== 1) return route.continue();
    started.release(); await reject.promise;
    return route.fulfill({ json: { jsonrpc: '2.0', id: body.id, error: { code: -32000,
      message: 'The Task has reached its comment capacity.', data: { code: 'content_too_large', outcome: 'not_executed' } } } });
  });
  await editor.fill('Previously rejected answer');
  await f.page.locator('input[type=file]').setInputFiles({ name: 'retained.txt', mimeType: 'text/plain', buffer: Buffer.from('Retained answer attachment') });
  await expect(editor.locator('a')).toHaveCount(1);
  await send.click();
  await started.promise;
  await editor.fill('A new ordinary comment');
  await expect(send).toBeDisabled();
  reject.release();
  const failure = f.page.locator('[data-slot=conversation-message]').filter({ hasText: 'Previously rejected answer' });
  await expect(failure).toContainText('content_too_large; not_executed');
  await expect(send).toBeEnabled();
  await expect(editor).toContainText('A new ordinary comment');
  await expect(f.page.locator('[data-slot=task-pending-questions]').getByRole('button', { name: 'Answer question', exact: true })).toBeEnabled();
  await f.page.evaluate(() => {
    const key = Object.keys(sessionStorage).find(value => value.startsWith('ivy.task-board.pending-comment:'));
    const retained = JSON.parse(sessionStorage.getItem(key));
    if (retained.failed) sessionStorage.setItem(key, JSON.stringify(retained.failed[0]));
  });
  await f.page.reload();
  await expect(failure).toHaveCount(1);
  await expect(f.page.locator('[data-slot=task-pending-questions]').getByRole('button', { name: 'Answer question', exact: true })).toBeEnabled();
  await editor.fill('A new ordinary comment');
  await send.click();
  await expect.poll(async () => (await f.task(id)).comments.some(comment => comment.body === 'A new ordinary comment')).toBe(true);
  await expect(failure.getByRole('button', { name: 'Retry message', exact: true })).toBeEnabled();
  await f.page.reload();
  await expect(failure).toHaveCount(1);
  await f.page.locator('[data-slot=task-pending-questions]').getByRole('button', { name: 'Answer question', exact: true }).click();
  await editor.fill('A new addressed answer');
  await expect(send).toBeEnabled();
  await send.click();
  await expect.poll(async () => (await f.task(id)).comments.find(comment => comment.body === 'A new addressed answer')?.replyTo).toBe('own-question');
  await expect(f.page.locator('[data-slot=task-pending-questions]')).toHaveCount(0);
  await editor.fill('Another unsent draft');
  await failure.getByRole('button', { name: 'Retry message', exact: true }).click();
  await expect.poll(async () => (await f.task(id)).comments.filter(comment => comment.commentId === calls[0].arguments.commentId).length).toBe(1);
  await expect(failure.getByRole('button', { name: 'Retry message', exact: true })).toHaveCount(0);
  const recovered = (await f.task(id)).comments.find(comment => comment.commentId === calls[0].arguments.commentId);
  assert.equal(recovered.replyTo, 'own-question');
  assert.equal(recovered.attachments[0].filename, 'retained.txt');
  assert.equal(calls.length, 4);
  assert.notEqual(calls[0].operationId, calls[3].operationId);
  await expect(editor).toContainText('Another unsent draft');
  await expect(send).toBeEnabled();
  await f.page.unroute('**/api/v1/rpc');
  await f.page.route('**/api/v1/rpc', route => {
    const body = route.request().postDataJSON();
    if (!isComment(body)) return route.continue();
    return route.fulfill({ json: { jsonrpc: '2.0', id: body.id, error: { code: -32000,
      message: 'Rejected unsent comment.', data: { code: 'content_too_large', outcome: 'not_executed' } } } });
  });
  await editor.fill('Discarded failed message');
  await send.click();
  const discarded = f.page.locator('[data-slot=conversation-message]').filter({ hasText: 'Discarded failed message' });
  await discarded.getByRole('button', { name: 'Discard unsent message', exact: true }).click();
  await f.page.reload();
  await expect(discarded).toHaveCount(0);
  await editor.fill('Still able to comment');
  await expect(send).toBeEnabled();
  assert.deepEqual(f.pageErrors, []);
});

test('definitive failure retries the retained message and unknown outcomes only observe the original identity', { timeout: 240000 }, async t => {
  const f = await taskBoardFixture(t);
  const created = await f.invoke({ action: 'create', fields: fields('Retry conversation') }), id = created.task.objectId;
  await f.invoke({ action: 'comment', taskId: id, expectedRevision: created.task.revision, authorKind: 'agent', purpose: 'question',
    recipient: { kind: 'user' }, commentId: 'original-question', body: 'Reply to this question', requests: [], responses: [], attachments: [], replyTo: null });
  await f.open(routeTo(id));
  await f.page.locator('[data-slot=task-pending-questions]').getByRole('button', { name: 'Answer question', exact: true }).click();
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
  await expect(editor.locator('a')).toHaveCount(1, { timeout: 30000 });
  await f.page.getByRole('button', { name: 'Add comment', exact: true }).click();
  const failed = f.page.locator('[data-slot=conversation-message]').filter({ hasText: 'Retain on failure' });
  await expect(failed.getByRole('button', { name: 'Retry message', exact: true })).toBeVisible({ timeout: 30000 });
  await failed.getByRole('button', { name: 'Retry message', exact: true }).click();
  await expect.poll(async () => (await f.task(id)).comments.filter(comment => comment.commentId === firstId).length).toBe(1);
  const comment = (await f.task(id)).comments.find(comment => comment.commentId === firstId);
  assert.equal(comment.attachments[0].filename, 'retained.txt');
  assert.equal(comment.replyTo, 'original-question');
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
  await editor.fill('A draft waiting for the original outcome');
  await expect(f.page.getByRole('button', { name: 'Add comment', exact: true })).toBeDisabled();
  await expect(f.page.locator('[data-slot=task-comment-composer]').getByRole('button', { name: 'Check original message', exact: true })).toBeEnabled();
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

test('approval responses use one optimistic message and retain the composer draft', { timeout: 180000 }, async t => {
  const f = await taskBoardFixture(t);
  const created = await f.invoke({ action: 'create', fields: fields('Approval conversation') }), id = created.task.objectId;
  await f.invoke({ action: 'comment', taskId: id, expectedRevision: created.task.revision, authorKind: 'agent',
    commentId: 'approval-question', body: 'Approve the requested action',
    requests: [{ requestId: 'approval-request', type: 'approval', title: 'Requested action', detail: 'Approve this action.' }],
    responses: [], attachments: [], replyTo: null });
  await f.open(routeTo(id));
  const dispatch = gate(), started = gate();
  t.after(() => dispatch.release());
  let calls = 0;
  await f.page.route('**/api/v1/rpc', async route => {
    if (!isComment(route.request().postDataJSON())) return route.continue();
    calls++; started.release(); await dispatch.promise; return route.continue();
  });
  const editor = f.page.getByRole('textbox', { name: 'Comment', exact: true });
  await editor.fill('A separate unsent draft');
  await f.page.getByRole('button', { name: 'Approve', exact: true }).click();
  const message = f.page.locator('[data-slot=conversation-message]').filter({ hasText: 'Approved.' });
  await expect(message).toHaveCount(1);
  await expect(message).toBeVisible();
  await expect(editor).toContainText('A separate unsent draft');
  await started.promise;
  assert.equal((await f.task(id)).comments.length, 1);
  await expect(f.page.getByRole('button', { name: 'Approve', exact: true })).toBeDisabled();
  dispatch.release();
  await expect.poll(async () => (await f.task(id)).comments.length).toBe(2);
  const response = (await f.task(id)).comments[1];
  assert.equal(response.replyTo, 'approval-question');
  assert.deepEqual(response.responses, [{ requestId: 'approval-request', type: 'approval', decision: 'approved', detail: '' }]);
  await expect(message).toHaveCount(1);
  await expect(editor).toContainText('A separate unsent draft');
  assert.equal(calls, 1);
  assert.deepEqual(f.pageErrors, []);
});

for (const width of [1440, 390]) test(`a paged conversation loads older messages on scroll at ${width}px`, { timeout: 120000 }, async t => {
  const f = await taskBoardFixture(t, { viewport: { width, height: 900 } });
  const created = await f.invoke({ action: 'create', fields: fields('Paged conversation') }), id = created.task.objectId;
  const store = f.taskBoard().runtime.engine.store, current = await store.read('task-board/task', id);
  const comments = Array.from({ length: 120 }, (_, index) => ({
    commentId: 'paged-' + (index + 1), sequence: index + 1,
    author: { principalId: 'browser-fixture', serviceNodeId: 'browser-task-board', source: 'user', generation: 1 },
    authorKind: 'user', body: 'Message ' + (index + 1), requests: [], responses: [], attachments: [], delivery: null,
    run: null, turnId: null, replyTo: null, createdAt: current.value.createdAt, operationId: 'paged-' + (index + 1),
  }));
  await store.write('task-board/task', { ...current.value, comments }, randomUUID(),
    { objectId: id, expectedRevision: current.pin.revision });
  const olderReads = [];
  f.page.on('request', request => {
    if (!request.url().endsWith('/api/v1/rpc') || request.method() !== 'POST') return;
    for (const body of [].concat(request.postDataJSON())) if (body?.params?.qualifiedName === 'task-board.read' &&
      body.params.arguments?.task === id && body.params.arguments.comments?.before)
      olderReads.push(body.params.arguments.comments);
  });
  await f.open(routeTo(id));
  const messages = f.page.locator('[data-slot=conversation-message]');
  await expect(messages).toHaveCount(50);
  await expect(f.page.locator('#comment-paged-120')).toBeVisible();
  assert.deepEqual(olderReads, []);
  const loader = f.page.getByRole('button', { name: 'Load older messages', exact: true });
  await loader.evaluate(element => element.scrollIntoView({ block: 'center' }));
  await expect(messages).toHaveCount(100);
  assert.deepEqual(olderReads, [{ before: 71, limit: 50 }]);
  await loader.evaluate(element => element.scrollIntoView({ block: 'center' }));
  await expect(messages).toHaveCount(120);
  await expect(loader).toHaveCount(0);
  assert.deepEqual(olderReads, [{ before: 71, limit: 50 }, { before: 21, limit: 50 }]);
  assert.equal(new Set(await messages.evaluateAll(elements => elements.map(element => element.closest('[id]')?.id))).size, 120);
  assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(f.pageErrors, []);
});

for (const width of [1440, 390]) test(`the common overlay, filtered counters and all Waiting groups work at ${width}px`, { timeout: 180000 }, async t => {
  const f = await taskBoardFixture(t, { native: true, viewport: { width, height: 900 } });
  const create = async (title, changes = {}, rest = {}) => (await f.invoke({ action: 'create', fields: { ...fields(title), ...changes }, ...rest })).task;
  const user = await create('User feedback');
  const dependency = await create('Dependency wait', {}, { nextSteps: [{ id: 'dependency', scope: 'Wait for the first ticket.', requires: [{ kind: 'task', taskId: user.objectId, until: 'done' }] }] });
  const runtime = await create('Runtime wait');
  const store = f.taskBoard().runtime.engine.store;
  const runtimeTask = await store.read('task-board/task', runtime.objectId);
  await store.write('task-board/task', { ...runtimeTask.value, workflowState: 'waiting',
    nextSteps: [{ id: 'resume', scope: 'Resume when the workspace returns.', requires: [] }],
    runtimeWait: { reason: 'workspace', detail: 'Workspace is unavailable.', since: new Date().toISOString() } }, randomUUID(),
    { objectId: runtime.objectId, expectedRevision: runtimeTask.pin.revision });
  await create('Time wait', {}, { workflowState: 'waiting', nextSteps: [{ id: 'appointment', scope: 'Resume on the scheduled date.', requires: [{ kind: 'time', at: '2099-01-01T12:00:00.000Z' }] }] });
  const ready = await create('Ready without a blocker', { control: 'agent' });
  await create('Due is not a restart', { control: 'agent', dueAt: '2099-01-01T12:00:00.000Z' });
  const unscheduled = await create('No restart set', {}, { nextSteps: [] });
  // Real retained mixed comments exercise visibility without per-user read tracking.
  const current = await store.read('task-board/task', user.objectId);
  const noDate = await store.read('task-board/task', unscheduled.objectId);
  await store.write('task-board/task', { ...noDate.value, workflowState: 'waiting', nextSteps: [] }, randomUUID(),
    { objectId: noDate.pin.objectId, expectedRevision: noDate.pin.revision });
  const now = new Date().toISOString();
  const comment = (id, authorKind, sourceTaskId) => ({ commentId: id, sequence: 0, author: { principalId: 'browser-fixture', serviceNodeId: 'browser-task-board', source: 'worker', generation: 1 },
    authorKind, body: id, requests: [], responses: [], delivery: null, attachments: [], run: null, turnId: null, replyTo: null, createdAt: now, operationId: id, ...(sourceTaskId ? { sourceTaskId } : {}) });
  const comments = [comment('User message', 'user'), comment('Direct agent answer', 'agent'), comment('Cross-agent note', 'agent', dependency.objectId)].map((value, i) => ({ ...value, sequence: i + 1 }));
  await store.write('task-board/task', { ...current.value, comments: comments.map(value => value.commentId === 'Direct agent answer' ? { ...value, purpose: 'question' } : value), agentCommentCount: 2, openQuestionCount: 1,
    nextSteps: [{ id: 'own-answer', scope: 'Wait for the own answer.', requires: [{ kind: 'user', questionId: 'Direct agent answer' }] }] }, randomUUID(), { objectId: user.objectId, expectedRevision: current.pin.revision });
  await f.open('#/tasks?node=browser-task-board');
  const waiting = f.page.getByRole('region', { name: 'Waiting', exact: true });
  for (const [group, title] of [['user', 'User feedback'], ['dependency', 'Dependency wait'], ['runtime', 'Runtime wait'], ['time', 'Time wait'], ['ready', 'Ready without a blocker'], ['ready', 'Due is not a restart'], ['runtime', 'No restart set']]) {
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
  await expect(f.page.locator('[id="comment-Direct agent answer"]').getByText('Direct agent answer', { exact: true })).toBeVisible();
  await expect(f.page.getByText('Cross-agent note', { exact: true })).toHaveCount(0);
  assert.equal((await store.page('task-board/comment-read-state')).items.length, 0);
  await expect(card.locator('[aria-label*="unread"]')).toHaveCount(0);
  // The retained board is intentionally aria-hidden while the overlay is open.
  const backgroundCard = f.page.locator('[data-slot=task-board-background] article').filter({ hasText: 'User feedback' });
  await toggle.click();
  await expect(backgroundCard.locator('[title="3 comments"]')).toHaveCount(1, { timeout: 1000 });
  await toggle.click();
  await expect(backgroundCard.locator('[title="2 comments"]')).toHaveCount(1, { timeout: 1000 });
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
  await expect(f.page.locator('[id="comment-Direct agent answer"]').getByText('Direct agent answer', { exact: true })).toBeVisible();
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
