import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

for (const [layout, viewport] of [['desktop', { width: 1440, height: 1000 }], ['mobile', { width: 390, height: 844 }]]) {
  test(`TaskBoard image gallery stays within one ticket across comments and attachments on ${layout}`, { timeout: 180000 }, async t => {
    const f = await taskBoardFixture(t, { viewport });
    const create = async title => (await f.invoke({ action: 'create', fields: {
      title, description: '', acceptanceCriteria: [], category: null, control: 'user', priority: 2,
      executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' }, dependencies: [],
      userContact: 'ticket', nextReviewAt: null, dueAt: null,
    } })).task.objectId;
    const first = await create('Ticket image gallery'), second = await create('Separate ticket image');
    const png = await f.page.evaluate(() => {
      const canvas = document.createElement('canvas'); canvas.width = 1200; canvas.height = 800;
      const context = canvas.getContext('2d'); context.fillStyle = '#447788'; context.fillRect(0, 0, canvas.width, canvas.height);
      return canvas.toDataURL('image/png').split(',')[1];
    });
    const revision = async id => (await f.client.request('objects.read', { objectId: id })).revision.revision;
    const upload = async (taskId, filename) => (await f.invoke({ action: 'uploadAttachment', taskId,
      expectedRevision: await revision(taskId), attachmentId: randomUUID(), filename, mediaType: 'image/png', bytesBase64: png,
    })).attachment;
    const overview = await upload(first, 'overview.png'), inline = await upload(first, 'inline.png'),
      attached = await upload(first, 'attached.png'), remaining = await upload(first, 'remaining.png'),
      foreign = await upload(second, 'other-ticket.png');
    const source = attachment => 'attachment:' + attachment.attachmentId;
    const comment = async (taskId, body, attachments = []) => f.invoke({ action: 'comment', taskId,
      expectedRevision: await revision(taskId), commentId: randomUUID(), body, attachments,
      requests: [], responses: [], replyTo: null, moveToTodo: false,
    });
    await f.invoke({ action: 'edit', taskId: first, expectedRevision: await revision(first), fields: {
      ...(await f.task(first)).fields,
      description: `![Overview][saved]\n\n[saved]: ${source(overview)}\n\n\`![Example](${source(remaining)})\``,
    } });
    await comment(first, `![First comment](${source(inline)})\n\n![External](https://tracker.test/pixel)\n\n![Foreign](${source(foreign)})`, [inline]);
    await comment(first, 'Image attached to the next comment.', [attached]);
    await comment(first, `The same image again: ![Repeated](${source(inline)})`);
    await comment(second, `![Other ticket](${source(foreign)})`, [foreign]);

    const reads = new Map();
    let failRemaining = true;
    await f.page.route('**/api/v1/rpc', async route => {
      const body = route.request().postDataJSON();
      let fail = false;
      for (const request of Array.isArray(body) ? body : [body]) {
        if (request.method !== 'objects.read') continue;
        const id = request.params.objectId;
        reads.set(id, (reads.get(id) ?? 0) + 1);
        if (id === remaining.object.objectId && failRemaining) { failRemaining = false; fail = true; }
      }
      if (fail) { await route.abort('failed'); return; }
      await route.continue();
    });
    const hash = id => '#/tasks?node=browser-task-board&panel=sheet&id=' + id;
    await f.open(hash(first));
    const inlineComment = f.page.locator('[data-slot="conversation-message"]').filter({
      has: f.page.getByRole('button', { name: 'Enlarge First comment', exact: true }),
    });
    await inlineComment.getByRole('button', { name: 'Show more', exact: true }).click();
    await expect(f.page.getByRole('button', { name: 'Enlarge First comment', exact: true })).toBeVisible();
    await expect(f.page.getByRole('button', { name: 'Enlarge attached.png', exact: true })).toBeVisible();
    await expect(f.page.getByRole('button', { name: 'Enlarge Foreign', exact: true })).toHaveCount(0);
    await f.page.getByRole('tab', { name: 'Details', exact: true }).click();
    await expect(f.page.getByRole('button', { name: 'Enlarge Overview', exact: true })).toBeVisible();
    await f.page.getByRole('tab', { name: 'Chat', exact: true }).click();
    assert.equal(reads.get(remaining.object.objectId) ?? 0, 0);
    assert.equal(reads.get(foreign.object.objectId) ?? 0, 0);

    await f.page.getByRole('button', { name: 'Enlarge First comment', exact: true }).click();
    const dialog = f.page.getByRole('dialog').filter({ has: f.page.getByRole('link', { name: 'Download image', exact: true }) });
    const image = filename => dialog.getByRole('img', { name: filename, exact: true });
    const show = async (filename, position) => {
      await expect(image(filename)).toBeVisible();
      await expect.poll(() => image(filename).evaluate(image => image.complete && image.naturalWidth === 1200)).toBe(true);
      await expect(dialog).toContainText(position + ' / 4');
      assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    };
    await show('inline.png', 2);
    await f.page.keyboard.press('ArrowLeft');
    await show('overview.png', 1);
    await expect(dialog.getByRole('button', { name: 'Previous image', exact: true })).toBeDisabled();
    await f.page.keyboard.press('ArrowLeft');
    await show('overview.png', 1);
    await dialog.getByRole('button', { name: 'Next image', exact: true }).click();
    await show('inline.png', 2);
    await f.page.keyboard.press('ArrowRight');
    await show('attached.png', 3);
    await f.page.keyboard.press('ArrowRight');
    const failed = f.page.getByRole('dialog', { name: 'remaining.png', exact: true });
    await expect(failed.getByRole('alert')).toBeVisible();
    await failed.getByRole('button', { name: 'Retry image', exact: true }).click();
    await show('remaining.png', 4);
    await expect(dialog.getByRole('button', { name: 'Next image', exact: true })).toBeDisabled();
    await dialog.getByRole('button', { name: 'Original size', exact: true }).click();
    await expect(dialog.getByRole('button', { name: 'Fit to window', exact: true })).toBeVisible();
    await dialog.getByRole('button', { name: 'Previous image', exact: true }).click();
    await show('attached.png', 3);
    await expect(dialog.getByRole('button', { name: 'Original size', exact: true })).toBeVisible();
    const oldSource = await image('attached.png').getAttribute('src');
    await expect(dialog.getByRole('link', { name: 'Download image', exact: true })).toHaveAttribute('href', oldSource);
    await f.page.keyboard.press('Escape');
    await expect(f.page.getByRole('dialog', { name: 'Task details', exact: true })).toBeVisible();
    await f.page.getByRole('button', { name: 'Enlarge attached.png', exact: true }).click();
    await show('attached.png', 3);
    await f.page.keyboard.press('Escape');
    await f.page.getByRole('tab', { name: 'Details', exact: true }).click();
    await f.page.getByRole('button', { name: 'Attachments (4)', exact: true }).click();
    await f.page.getByRole('button', { name: 'Enlarge overview.png', exact: true }).click();
    await show('overview.png', 1);
    assert.equal(reads.get(overview.object.objectId), 1);
    assert.equal(reads.get(inline.object.objectId), 1);
    assert.equal(reads.get(attached.object.objectId), 1);
    assert.equal(reads.get(remaining.object.objectId), 2);

    // SPA navigation tears down the old ticket and its open gallery.
    await f.page.evaluate(hash => { location.hash = hash; }, hash(second));
    await expect(f.page.getByRole('dialog', { name: 'overview.png', exact: true })).toHaveCount(0);
    await f.page.getByRole('button', { name: 'Enlarge Other ticket', exact: true }).click();
    const other = f.page.getByRole('dialog', { name: 'other-ticket.png', exact: true });
    await expect(other.getByRole('img', { name: 'other-ticket.png', exact: true })).toBeVisible();
    await expect(other.getByRole('button', { name: 'Next image', exact: true })).toHaveCount(0);
    await expect(other.getByRole('button', { name: 'Previous image', exact: true })).toHaveCount(0);
    await f.page.keyboard.press('ArrowRight');
    assert.notEqual(await other.getByRole('img', { name: 'other-ticket.png', exact: true }).getAttribute('src'), oldSource);
    await expect(other.getByRole('heading', { name: 'other-ticket.png', exact: true })).toBeVisible();
    await f.page.keyboard.press('Escape');
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  });
}
