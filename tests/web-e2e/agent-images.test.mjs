import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { expect } from '@playwright/test';
import { agentFixture } from './fixtures/agent-fixture.mjs';

for (const version of ['0.159.2', '0.142.3']) {
  test(`AgentUI displays saved and live reply images from the native ${version} owner on desktop and mobile`, { timeout: 120000 }, async t => {
    const f = await agentFixture(t, { width: 1280, height: 900 }, false, version);
    const png = Buffer.from(await f.page.evaluate(() => {
      const canvas = document.createElement('canvas'); canvas.width = 1200; canvas.height = 300;
      const context = canvas.getContext('2d'); context.fillStyle = '#447788'; context.fillRect(0, 0, canvas.width, canvas.height);
      return canvas.toDataURL('image/png').split(',')[1];
    }), 'base64');
    const folder = join(f.nativeProjects[0].path, 'shots'), absolute = join(folder, 'absolute.png'), relative = join(folder, 'light room (2).png'), missing = join(folder, 'later.png');
    await mkdir(folder); await writeFile(absolute, png); await writeFile(relative, png);
    const thread = f.threads.get('saved-task');
    thread.turns = [{ id: 'image-turn', status: 'inProgress', items: [{ id: 'saved-images', type: 'agentMessage', text:
      `![Absolute screenshot](<${absolute}>)\n\n![Relative screenshot](<shots/light room (2).png>)\n\n![Later screenshot](<shots/later.png>)\n\n` +
      '```md\n![Example](shots/code-only.png)\n```\n\n![External](https://tracker.example/image.png)' }] }];
    await f.open('#/task?node=browser-agent&id=saved-task');
    await expect(f.page.locator('img.ivy-embedded-image')).toHaveCount(2, { timeout: 35000 });
    for (const image of await f.page.locator('img.ivy-embedded-image').all()) {
      await image.scrollIntoViewIfNeeded();
      await expect.poll(() => image.evaluate(image => image.complete && image.naturalWidth === 1200)).toBe(true);
    }
    await expect(f.page.getByRole('button', { name: 'Retry images', exact: true })).toBeVisible();
    assert.equal(f.current().sent.filter(frame => frame.method === 'fs/readFile').length, 3);
    await writeFile(missing, png);
    await f.page.getByRole('button', { name: 'Retry images', exact: true }).click();
    await expect(f.page.locator('img.ivy-embedded-image')).toHaveCount(3);
    await expect(f.page.getByRole('button', { name: 'Retry images', exact: true })).toHaveCount(0);
    for (const width of [1280, 390]) {
      await f.page.setViewportSize({ width, height: 900 });
      await f.page.getByRole('button', { name: 'Enlarge Relative screenshot', exact: true }).click();
      const dialog = f.page.getByRole('dialog');
      await expect(dialog.getByRole('img', { name: 'Relative screenshot', exact: true })).toBeVisible();
      await dialog.getByRole('button', { name: 'Original size', exact: true }).click();
      await expect(dialog.getByRole('link', { name: 'Download image', exact: true })).toHaveAttribute('href', /^blob:/);
      await dialog.getByRole('button', { name: 'Fit to window', exact: true }).click();
      await f.page.keyboard.press('Escape');
      assert.ok(await f.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    }
    f.current().emit({ method: 'item/agentMessage/delta', params: { threadId: 'saved-task', turnId: 'image-turn', itemId: 'live-image', delta: '![Live screenshot](shots/absolute.png)' } });
    await expect(f.page.getByRole('article', { name: 'Live answer' }).locator('img.ivy-embedded-image')).toHaveCount(1, { timeout: 35000 });
    const reads = f.current().sent.filter(frame => frame.method === 'fs/readFile').length;
    f.current().emit({ method: 'item/agentMessage/delta', params: { threadId: 'saved-task', turnId: 'image-turn', itemId: 'live-image', delta: '\n\nMore text after the image.' } });
    await expect(f.page.getByRole('article', { name: 'Live answer' })).toContainText('More text after the image.');
    assert.equal(f.current().sent.filter(frame => frame.method === 'fs/readFile').length, reads);
    const turn = thread.turns[0];
    turn.items.push({ id: 'live-image', type: 'agentMessage', text: '![Live screenshot](shots/absolute.png)\n\nMore text after the image.' });
    turn.status = 'completed'; thread.status = { type: 'idle' };
    f.current().emit({ method: 'turn/completed', params: { threadId: 'saved-task', turn } });
    await f.page.evaluate(() => window.dispatchEvent(new Event('online')));
    await expect(f.page.getByRole('article', { name: 'Live answer' })).toHaveCount(0, { timeout: 35000 });
    await expect(f.page.locator('img.ivy-embedded-image')).toHaveCount(4);
    await f.page.reload();
    await expect(f.page.locator('img.ivy-embedded-image')).toHaveCount(4, { timeout: 35000 });
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  });
}
