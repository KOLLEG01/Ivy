import test from 'node:test';
import assert from 'node:assert/strict';
import { appendFile } from 'node:fs/promises';
import { expect } from '@playwright/test';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

const fields = title => ({ title, description: '', acceptanceCriteria: [], category: null, control: 'user',
  allowParallel: false, priority: 2, executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' },
  userContact: 'ticket', dueAt: null });
const toolCall = (body, name, taskId) => body.method === 'tools.call' &&
  body.params.qualifiedName === 'task-board.' + name && (!taskId || body.params.arguments.taskId === taskId);
const gate = t => {
  let release;
  const promise = new Promise(resolve => { release = resolve; });
  t.after(() => release());
  return { promise, release };
};
const create = async (f, title) => (await f.invoke({ action: 'create', workflowState: 'backlog', fields: fields(title) })).task;
const card = (f, title, state) => (state ? f.page.getByRole('region', { name: state, exact: true }) : f.page)
  .locator('article').filter({ has: f.page.getByRole('link', { name: title, exact: true }) });
const move = async (f, title, label = 'Move to Todo') => {
  await f.page.getByRole('button', { name: 'Actions for ' + title, exact: true }).click();
  await f.page.getByRole('menuitem', { name: label, exact: true }).click();
};
const noTechnicalProgress = async page => {
  for (const label of ['Action identity', 'Check original action', 'Resume original action', 'Saving task update'])
    await expect(page.getByText(label, { exact: true })).toHaveCount(0);
};
const commentEditor = f => f.page.getByRole('textbox', { name: 'Comment', exact: true });
const send = f => f.page.getByRole('button', { name: 'Add comment', exact: true });
const copiedTab = async f => {
  const popup = f.page.waitForEvent('popup');
  await f.page.evaluate(() => { window.open('about:blank', '_blank'); });
  const page = await popup;
  page.on('pageerror', error => f.pageErrors.push(error.message));
  return { ...f, page };
};
const measure = async (t, sample) => {
  t.diagnostic(JSON.stringify(sample));
  if (process.env.IVY_TASKBOARD_TIMINGS) await appendFile(process.env.IVY_TASKBOARD_TIMINGS, JSON.stringify(sample) + '\n');
};

for (const width of [1440, 390]) {
  test(`independent board moves survive a lost reply and a late older page at ${width}px`, { timeout: 120000 }, async t => {
    const f = await taskBoardFixture(t, { viewport: { width, height: 900 } });
    const a = await create(f, 'Delayed move'), b = await create(f, 'Independent move');
    await f.open('#/tasks?node=browser-task-board');
    const dispatch = gate(t), acknowledgement = gate(t), oldPage = gate(t);
    const requests = [], preparation = [];
    let started = false, saved = false, dropped = false, stale = null, staleDelivered = false, captureStale = false;
    let clickedAt, dispatchedAt, persistedAt;
    await f.page.route('**/api/v1/rpc', async route => {
      const body = route.request().postDataJSON();
      const list = [].concat(body).find(frame => toolCall(frame, 'list') && JSON.stringify(frame.params.arguments.where).includes('"backlog"'));
      if (list && captureStale && !stale) {
        captureStale = false;
        const response = await route.fetch();
        stale = [].concat(await response.json()).find(frame => frame.id === list.id);
        await oldPage.promise;
        await route.fulfill({ response }); staleDelivered = true;
        return;
      }
      if (!toolCall(body, 'transition', a.objectId)) {
        if (clickedAt && !started) preparation.push(body);
        return route.continue();
      }
      requests.push(body.params); started = true;
      await dispatch.promise; dispatchedAt = Date.now();
      const response = await route.fetch(); persistedAt = Date.now();
      assert.ok(!(await response.json()).error);
      saved = true; await acknowledgement.promise;
      await route.abort(); dropped = true;
    });
    clickedAt = Date.now();
    await move(f, 'Delayed move');
    await expect(card(f, 'Delayed move', 'Waiting')).toBeVisible();
    const visibleAt = Date.now();
    await expect.poll(() => started).toBe(true);
    assert.equal((await f.task(a.objectId)).workflowState, 'backlog', 'The projected move has no authoritative effect before dispatch.');
    await f.page.getByRole('button', { name: 'Actions for Delayed move', exact: true }).click();
    await expect(f.page.getByRole('menuitem', { name: 'Move to Todo', exact: true })).toBeDisabled();
    await f.page.keyboard.press('Escape');
    await noTechnicalProgress(f.page);
    captureStale = true;
    await move(f, 'Independent move');
    await expect.poll(async () => (await f.task(b.objectId)).workflowState).toBe('todo');
    await expect.poll(() => !!stale).toBe(true);
    const oldCard = stale.result.items.find(item => item.object.objectId === a.objectId);
    assert.equal(oldCard.task.workflowState, 'backlog');
    dispatch.release();
    await expect.poll(() => saved).toBe(true);
    await expect.poll(async () => (await f.task(a.objectId)).workflowState).toBe('todo');
    oldPage.release();
    await expect.poll(() => staleDelivered).toBe(true);
    await expect(card(f, 'Delayed move', 'Waiting')).toBeVisible();
    await expect(card(f, 'Delayed move', 'Backlog')).toHaveCount(0);
    acknowledgement.release();
    await expect.poll(() => dropped).toBe(true);
    await f.page.reload();
    await expect(card(f, 'Delayed move', 'Waiting')).toBeVisible();
    await expect.poll(() => f.page.evaluate(id => {
      for (const key of Object.keys(sessionStorage)) {
        if (!key.startsWith('ivy:task-board:action:')) continue;
        const value = JSON.parse(sessionStorage.getItem(key));
        if (value.call?.arguments?.taskId === id) return value.phase;
      }
      return 'released';
    }, a.objectId), { timeout: 20000 }).toMatch(/^(succeeded|released)$/);
    assert.equal(requests.length, 1, 'The original receipt settles the lost reply without another mutation.');
    assert.ok(preparation.filter(body => toolCall(body, 'read') && body.params.arguments.task === a.objectId)
      .every(body => body.params.arguments.comments?.limit === 0), 'Preparing a move must not read its conversation.');
    await move(f, 'Independent move', 'Start manual work');
    await expect.poll(async () => (await f.task(b.objectId)).workflowState).toBe('in_progress');
    await noTechnicalProgress(f.page);
    assert.deepEqual(f.pageErrors, []);
    await measure(t, { scenario: 'independent moves and lost reply', width,
      visibleMs: visibleAt - clickedAt, dispatchHeldMs: dispatchedAt - clickedAt,
      ownerCallMs: persistedAt - dispatchedAt, confirmedMs: Date.now() - clickedAt, preparationCalls: preparation.length });
  });

  test(`a real rejected move preserves the newer edit and other ticket actions at ${width}px`, { timeout: 120000 }, async t => {
    const f = await taskBoardFixture(t, { viewport: { width, height: 900 } });
    const a = await create(f, 'Conflicting move'), b = await create(f, 'Unaffected ticket');
    await f.open('#/tasks?node=browser-task-board');
    const dispatch = gate(t);
    let request, result;
    await f.page.route('**/api/v1/rpc', async route => {
      const body = route.request().postDataJSON();
      if (!toolCall(body, 'transition', a.objectId)) return route.continue();
      assert.equal(request, undefined, 'A rejected original must not dispatch again automatically.');
      request = body.params;
      await dispatch.promise;
      const response = await route.fetch(); result = await response.json();
      await route.fulfill({ response });
    });
    await move(f, 'Conflicting move');
    await expect(card(f, 'Conflicting move', 'Waiting')).toBeVisible();
    await expect.poll(() => !!request).toBe(true);
    const current = await f.client.request('objects.read', { objectId: a.objectId });
    await f.invoke({ action: 'edit', taskId: a.objectId, expectedRevision: current.revision.revision,
      fields: { ...current.content.value.fields, title: 'Newer authoritative title' } });
    await move(f, 'Unaffected ticket');
    await expect.poll(async () => (await f.task(b.objectId)).workflowState).toBe('todo');
    dispatch.release();
    await expect.poll(() => !!result).toBe(true);
    assert.equal(result.error.data.code, 'revision_conflict');
    await expect(card(f, 'Newer authoritative title', 'Backlog')).toBeVisible();
    await expect(card(f, 'Newer authoritative title', 'Waiting')).toHaveCount(0);
    const authoritative = await f.task(a.objectId);
    assert.equal(authoritative.workflowState, 'backlog');
    assert.equal(authoritative.fields.title, 'Newer authoritative title');
    await move(f, 'Unaffected ticket', 'Start manual work');
    await expect.poll(async () => (await f.task(b.objectId)).workflowState).toBe('in_progress');
    assert.deepEqual(f.pageErrors, []);
  });

  test(`rapid comments retain their identities and next draft through offline reload at ${width}px`, { timeout: 120000 }, async t => {
    const f = await taskBoardFixture(t, { viewport: { width, height: 900 } });
    const a = await create(f, 'Queued conversation');
    await f.open('#/tasks?node=browser-task-board&id=' + a.objectId);
    const dispatch = gate(t), acknowledgement = gate(t);
    const requests = [];
    let saved = false, dropped = false;
    await f.page.route('**/api/v1/rpc', async route => {
      const body = route.request().postDataJSON();
      if (!toolCall(body, 'comment', a.objectId)) return route.continue();
      requests.push(body.params);
      if (requests.length !== 1) return route.continue();
      await dispatch.promise;
      const response = await route.fetch();
      assert.ok(!(await response.json()).error);
      saved = true; await acknowledgement.promise;
      await route.abort(); dropped = true;
    });
    const texts = ['First rapid comment', 'Second rapid comment', 'Third rapid comment'];
    const visiblePerCommentMs = [];
    const startedAt = Date.now();
    for (const text of texts) {
      await commentEditor(f).fill(text);
      const clickedAt = Date.now();
      await expect(send(f)).toBeEnabled(); await send(f).click();
      await expect(f.page.locator('[data-slot=conversation-message]').filter({ hasText: text })).toHaveCount(1);
      visiblePerCommentMs.push(Date.now() - clickedAt);
    }
    const visibleAt = Date.now();
    const retained = await f.page.evaluate(() => {
      const key = Object.keys(sessionStorage).find(key => key.startsWith('ivy.task-board.pending-comment:'));
      const value = JSON.parse(sessionStorage.getItem(key));
      return [value.pending, ...value.queued].filter(Boolean).map(item => item.request.commentId);
    });
    assert.equal(retained.length, 3); assert.equal(new Set(retained).size, 3);
    assert.equal((await f.task(a.objectId)).comments.length, 0);
    await commentEditor(f).fill('My next unsent draft');
    await noTechnicalProgress(f.page);
    dispatch.release(); await expect.poll(() => saved).toBe(true);
    const current = await f.client.request('objects.read', { objectId: a.objectId });
    await f.invoke({ action: 'comment', taskId: a.objectId, expectedRevision: current.revision.revision,
      authorKind: 'agent', commentId: 'parallel-agent-comment', body: 'An independent agent update',
      requests: [], responses: [], attachments: [], replyTo: null });
    await f.context.setOffline(true); acknowledgement.release();
    await expect.poll(() => dropped).toBe(true);
    await expect(commentEditor(f)).toContainText('My next unsent draft');
    await f.context.setOffline(false); await f.page.reload();
    await expect(commentEditor(f)).toContainText('My next unsent draft');
    await expect.poll(async () => {
      const comments = (await f.task(a.objectId)).comments;
      return retained.map(id => comments.filter(comment => comment.commentId === id).length);
    }, { timeout: 25000 }).toEqual([1, 1, 1]);
    const comments = (await f.task(a.objectId)).comments;
    assert.deepEqual(retained.map(id => comments.find(comment => comment.commentId === id).body), texts);
    assert.equal(comments.filter(comment => comment.commentId === 'parallel-agent-comment').length, 1);
    for (const text of texts) await expect(f.page.locator('[data-slot=conversation-message]').filter({ hasText: text })).toHaveCount(1);
    assert.equal(requests.length, 3);
    assert.deepEqual(requests.map(request => request.arguments.commentId).sort(), [...retained].sort());
    await expect(send(f)).toBeEnabled();
    await expect(commentEditor(f)).toContainText('My next unsent draft');
    await noTechnicalProgress(f.page); assert.deepEqual(f.pageErrors, []);
    await measure(t, { scenario: 'rapid comments and offline reload', width,
      visiblePerCommentMs, threeVisibleMs: visibleAt - startedAt, allAuthoritativeMs: Date.now() - startedAt, mutationCalls: requests.length });
  });

  test(`copied tabs reconcile one original comment while keeping independent drafts at ${width}px`, { timeout: 120000 }, async t => {
    const f = await taskBoardFixture(t, { viewport: { width, height: 900 } });
    const a = await create(f, 'Copied conversation'), b = await create(f, 'Another tab ticket');
    const url = f.url + '#/tasks?node=browser-task-board&id=' + a.objectId;
    await f.open('#/tasks?node=browser-task-board&id=' + a.objectId);
    const dispatch = gate(t), acknowledgement = gate(t), requests = [];
    let saved = false, dropped = false;
    await f.context.route('**/api/v1/rpc', async route => {
      const body = route.request().postDataJSON();
      if (!toolCall(body, 'comment', a.objectId)) return route.continue();
      requests.push(body.params);
      assert.equal(requests.length, 1, 'Both tabs must reconcile the same accepted original without sending another comment.');
      await dispatch.promise;
      const response = await route.fetch();
      assert.ok(!(await response.json()).error);
      saved = true; await acknowledgement.promise;
      await route.abort(); dropped = true;
    });
    await commentEditor(f).fill('One original across two tabs'); await send(f).click();
    await expect.poll(() => requests.length).toBe(1);
    await expect(f.page.locator('[data-slot=conversation-message]').filter({ hasText: 'One original across two tabs' })).toHaveCount(1);
    await commentEditor(f).fill('First tab future draft');
    const retained = await f.page.evaluate(() => Object.fromEntries(Object.entries(sessionStorage)
      .filter(([key]) => key.startsWith('ivy:task-board:action:') || key.startsWith('ivy.task-board.pending-comment:'))));
    const other = await copiedTab(f);
    assert.deepEqual(await other.page.evaluate(() => Object.fromEntries(Object.entries(sessionStorage)
      .filter(([key]) => key.startsWith('ivy:task-board:action:') || key.startsWith('ivy.task-board.pending-comment:')))), retained,
    'A real opener-created tab inherits the retained original, unlike an unrelated new browser page.');
    assert.ok(Object.values(retained).some(raw => JSON.parse(raw).call?.operationId === requests[0].operationId));
    assert.equal((await f.task(a.objectId)).comments.length, 0, 'The optimistic message has no backend effect before dispatch.');
    dispatch.release(); await expect.poll(() => saved).toBe(true);
    let current = await f.client.request('objects.read', { objectId: a.objectId });
    await f.invoke({ action: 'comment', taskId: a.objectId, expectedRevision: current.revision.revision,
      authorKind: 'agent', commentId: 'copied-tab-agent-comment', body: 'A later agent contribution',
      requests: [], responses: [], attachments: [], replyTo: null });
    current = await f.client.request('objects.read', { objectId: a.objectId });
    await f.invoke({ action: 'edit', taskId: a.objectId, expectedRevision: current.revision.revision,
      fields: { ...current.content.value.fields, title: 'Newer title across tabs' } });
    await other.page.goto(url);
    await expect(commentEditor(other)).toContainText('First tab future draft');
    await commentEditor(other).fill('Second tab future draft');
    for (const body of ['One original across two tabs', 'A later agent contribution'])
      await expect(other.page.locator('[data-slot=conversation-message]').filter({ hasText: body })).toHaveCount(1);
    await expect(send(other)).toBeEnabled({ timeout: 20000 });
    await other.page.getByRole('button', { name: 'Close', exact: true }).click();
    await move(other, 'Another tab ticket');
    await expect.poll(async () => (await f.task(b.objectId)).workflowState).toBe('todo');
    await other.page.getByRole('link', { name: 'Newer title across tabs', exact: true }).click();
    await expect(commentEditor(other)).toContainText('Second tab future draft');
    acknowledgement.release(); await expect.poll(() => dropped).toBe(true);
    await f.page.reload();
    await expect(commentEditor(f)).toContainText('First tab future draft');
    await expect(send(f)).toBeEnabled({ timeout: 20000 });
    const task = await f.task(a.objectId);
    assert.equal(task.fields.title, 'Newer title across tabs');
    assert.equal(task.comments.length, 2);
    assert.equal(task.comments.filter(comment => comment.commentId === requests[0].arguments.commentId).length, 1);
    assert.equal(requests.length, 1);
    for (const tab of [f, other]) {
      for (const body of ['One original across two tabs', 'A later agent contribution'])
        await expect(tab.page.locator('[data-slot=conversation-message]').filter({ hasText: body })).toHaveCount(1);
      await noTechnicalProgress(tab.page);
    }
    assert.deepEqual(f.pageErrors, []); assert.deepEqual(f.externalRequests, []);
    await measure(t, { scenario: 'copied pending original and independent tab drafts', width, mutationCalls: requests.length });
  });

  test(`a late archive reply cannot undo another tab's restore and newer edit at ${width}px`, { timeout: 120000 }, async t => {
    const f = await taskBoardFixture(t, { viewport: { width, height: 900 } });
    const a = await create(f, 'Archive across tabs');
    const b = await create(f, 'Independent during archive');
    const url = f.url + '#/tasks?node=browser-task-board&id=' + a.objectId + '&tab=details';
    await f.open('#/tasks?node=browser-task-board&id=' + a.objectId + '&tab=details');
    const acknowledgement = gate(t), requests = [];
    let saved = false, delivered = false;
    await f.context.route('**/api/v1/rpc', async route => {
      const body = route.request().postDataJSON();
      if (!toolCall(body, 'archive', a.objectId)) return route.continue();
      requests.push(body.params);
      const response = await route.fetch();
      assert.ok(!(await response.json()).error);
      if (body.params.arguments.archived) {
        assert.equal(requests.filter(request => request.arguments.archived).length, 1);
        saved = true; await acknowledgement.promise;
        await route.fulfill({ response }); delivered = true;
      } else await route.fulfill({ response });
    });
    const archiveAction = async (tab, name) => {
      await tab.page.getByRole('button', { name: 'More actions', exact: true }).click();
      await expect(tab.page.getByRole('menuitem', { name, exact: true })).toBeEnabled({ timeout: 20000 });
      await tab.page.getByRole('menuitem', { name, exact: true }).click();
    };
    await archiveAction(f, 'Archive task'); await expect.poll(() => saved).toBe(true);
    assert.equal((await f.client.request('objects.stat', { objectId: a.objectId })).effectivelyArchived, true);
    const other = await copiedTab(f);
    await other.page.goto(url);
    await archiveAction(other, 'Restore task');
    await expect.poll(async () => (await f.client.request('objects.stat', { objectId: a.objectId })).effectivelyArchived).toBe(false);
    await expect(other.page.getByRole('button', { name: 'Edit task', exact: true })).toBeEnabled();
    await other.page.getByRole('button', { name: 'Edit task', exact: true }).click();
    await other.page.getByLabel('Title', { exact: true }).fill('Edited after another tab restored');
    await other.page.getByRole('button', { name: 'Save changes', exact: true }).click();
    await expect.poll(async () => (await f.task(a.objectId)).fields.title).toBe('Edited after another tab restored');
    await other.page.getByRole('button', { name: 'Close', exact: true }).click();
    await move(other, 'Independent during archive');
    await expect.poll(async () => (await f.task(b.objectId)).workflowState).toBe('todo');
    const metadata = await f.client.request('objects.stat', { objectId: a.objectId });
    const content = await f.task(a.objectId);
    acknowledgement.release(); await expect.poll(() => delivered).toBe(true);
    await expect(f.page.getByRole('heading', { name: /Edited after another tab restored$/ }).first()).toBeVisible();
    await f.page.getByRole('button', { name: 'More actions', exact: true }).click();
    await expect(f.page.getByRole('menuitem', { name: 'Archive task', exact: true })).toBeEnabled({ timeout: 20000 });
    await expect(f.page.getByRole('menuitem', { name: 'Restore task', exact: true })).toHaveCount(0);
    await f.page.keyboard.press('Escape');
    const finalMetadata = await f.client.request('objects.stat', { objectId: a.objectId });
    const final = await f.task(a.objectId);
    assert.equal(finalMetadata.effectivelyArchived, false);
    assert.equal(finalMetadata.archivedAt, null);
    assert.equal(finalMetadata.currentRevision, metadata.currentRevision);
    assert.equal(final.fields.title, content.fields.title); assert.equal(final.workflowState, 'backlog');
    assert.deepEqual(requests.map(request => request.arguments.archived), [true, false]);
    assert.equal(new Set(requests.map(request => request.operationId)).size, 2);
    await noTechnicalProgress(f.page); await noTechnicalProgress(other.page);
    assert.deepEqual(f.pageErrors, []); assert.deepEqual(f.externalRequests, []);
    await measure(t, { scenario: 'late archive receipt after another tab restore and edit', width, archiveCalls: requests.length });
  });

  test(`a browser storage failure retains the unsent draft without blocking another ticket at ${width}px`, { timeout: 90000 }, async t => {
    const f = await taskBoardFixture(t, { viewport: { width, height: 900 } });
    const a = await create(f, 'Storage issue'), b = await create(f, 'Writable neighbor');
    await f.invoke({ action: 'comment', taskId: a.objectId, expectedRevision: a.revision, authorKind: 'agent',
      purpose: 'question', recipient: { kind: 'user' }, commentId: 'retained-reply-question', body: 'Which retained draft should we send?',
      requests: [], responses: [], attachments: [], replyTo: null });
    await f.open('#/tasks?node=browser-task-board&id=' + a.objectId);
    await f.page.locator('[data-slot=task-pending-questions]').getByRole('button', { name: 'Answer question', exact: true }).click();
    await f.page.locator('input[type=file]').setInputFiles({ name: 'retained-draft.txt', mimeType: 'text/plain', buffer: Buffer.from('Retained unsent draft attachment') });
    await expect(commentEditor(f).locator('a')).toHaveCount(1);
    const attachment = (await f.task(a.objectId)).attachments[0];
    const requests = [];
    f.page.on('request', request => {
      if (!request.url().endsWith('/api/v1/rpc') || request.method() !== 'POST') return;
      const body = request.postDataJSON();
      if (toolCall(body, 'comment')) requests.push(body.params);
    });
    await f.page.evaluate(id => {
      const original = Storage.prototype.setItem;
      window.blockCommentStorage = true;
      Storage.prototype.setItem = function (key, value) {
        if (window.blockCommentStorage && key.startsWith('ivy.task-board.pending-comment:') && key.includes(id))
          throw new DOMException('Isolated storage failure', 'QuotaExceededError');
        return original.call(this, key, value);
      };
    }, a.objectId);
    await commentEditor(f).press('End'); await commentEditor(f).pressSequentially(' The draft must survive'); await send(f).click();
    await expect(f.page.getByRole('alert').filter({ hasText: 'The message could not be retained' })).toContainText('Your draft is retained.');
    await expect(commentEditor(f)).toContainText('The draft must survive');
    assert.equal(requests.length, 0); assert.equal((await f.task(a.objectId)).comments.length, 1);
    await f.page.getByRole('button', { name: 'Close', exact: true }).click();
    await f.page.getByRole('link', { name: 'Writable neighbor', exact: true }).click();
    await commentEditor(f).fill('The other ticket stays writable'); await send(f).click();
    await expect.poll(async () => (await f.task(b.objectId)).comments.length).toBe(1);
    await f.page.getByRole('button', { name: 'Close', exact: true }).click();
    await f.page.getByRole('link', { name: 'Storage issue', exact: true }).click();
    await expect(commentEditor(f)).toContainText('The draft must survive');
    await expect(commentEditor(f).locator('a')).toHaveCount(1);
    await expect(f.page.locator('.markdown-composer [data-slot=reply-context]')).toContainText('Which retained draft should we send?');
    await f.page.evaluate(() => { window.blockCommentStorage = false; });
    await send(f).click();
    await expect.poll(async () => (await f.task(a.objectId)).comments.length).toBe(2);
    const answer = (await f.task(a.objectId)).comments[1];
    assert.match(answer.body, /The draft must survive/);
    assert.equal(answer.replyTo, 'retained-reply-question');
    assert.deepEqual(answer.attachments, [attachment]);
    assert.equal(requests.filter(request => request.arguments.taskId === a.objectId).length, 1);
    await noTechnicalProgress(f.page); assert.deepEqual(f.pageErrors, []);
  });
}
