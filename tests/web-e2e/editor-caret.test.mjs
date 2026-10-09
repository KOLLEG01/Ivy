import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { expect } from '@playwright/test';
import { publishUi } from '../../dist/packages/cli/src/publish-ui.js';
import { newOperationId } from '../../dist/packages/sdk/src/client.js';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';

for (const width of [1440, 390]) test(`Wiki and task editors keep a steady reduced-motion caret and preserve typing at ${width}px`, { timeout: 90000 }, async t => {
  const f = await taskBoardFixture(t, { viewport: { width, height: 1000 } });
  const directory = resolve('dist/apps/wiki-ui'), definition = JSON.parse(await readFile(resolve(directory, 'ivy-ui.json'), 'utf8'));
  await publishUi(f.client, { directory, definition, expectedReleaseId: null, mutationId: await newOperationId(f.client) });
  await f.login();
  for (const [url, label] of [[f.base + '/wiki/#/new', 'Markdown'], [f.url + '#/new?node=browser-task-board', 'Description']]) {
    await f.page.goto(url);
    const editor = f.page.getByRole('textbox', { name: label, exact: true });
    await expect(editor).toBeVisible();
    await editor.pressSequentially('Steady editor text');
    const original = await editor.elementHandle();
    for (const reducedMotion of ['no-preference', 'reduce']) {
      await f.page.emulateMedia({ reducedMotion });
      await editor.click();
      const caret = f.page.locator('.ProseMirror-focused .prosemirror-virtual-cursor');
      await expect(caret).toBeVisible();
      const style = await caret.evaluate(el => { const css = getComputedStyle(el); return { duration: css.animationDuration, iterations: css.animationIterationCount }; });
      assert.equal(style.iterations, reducedMotion === 'reduce' ? '1' : 'infinite');
      if (reducedMotion === 'no-preference') assert.equal(style.duration, '1s');
      else {
        await f.page.waitForTimeout(750);
        assert.equal(await caret.evaluate(el => getComputedStyle(el).opacity), '1');
      }
      assert.equal(await original.evaluate(el => el === document.activeElement && el.isConnected), true);
    }
    await editor.press('End');
    await editor.pressSequentially(' remains editable');
    await expect(editor).toContainText('Steady editor text remains editable');
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  }
  assert.deepEqual(f.pageErrors, []);
});
