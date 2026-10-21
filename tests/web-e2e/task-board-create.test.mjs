import test from 'node:test';
import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { taskBoardFixture } from './fixtures/task-board-fixture.mjs';
import { choose, options } from './fixtures/select.mjs';

const fields = title => ({ title, description: '', acceptanceCriteria: [], category: null, control: 'user',
  priority: 2, executionRequirement: null, workspaceRequirement: { kind: 'task_workspace' },
   userContact: 'ticket',  dueAt: null });
const savedId = page => new URLSearchParams(new URL(page.url()).hash.split('?')[1]).get('id');

for (const width of [1440, 390]) test(`creation keeps explicit waiting and manual Review state through drafts at ${width}px`, { timeout: 120000 }, async t => {
  const f = await taskBoardFixture(t, { viewport: { width, height: 1000 } });
  const peer = await f.invoke({ action: 'create', workflowState: 'backlog', fields: fields('Peer to review') });
  await f.open('#/new?node=browser-task-board');
  const status = () => f.page.getByLabel('Initial status', { exact: true });
  await expect(status()).toHaveText('Todo');
  assert.deepEqual(await options(status()), ['Backlog', 'Todo', 'In progress', 'Waiting', 'Review', 'Done', 'Cancelled']);
  await f.page.getByRole('radio', { name: 'Me', exact: true }).check();
  await f.page.getByLabel('Title', { exact: true }).fill('Initially blocked manual work');
  await choose(status(), 'waiting');
  await f.page.getByRole('combobox', { name: 'Watched tickets', exact: true }).click();
  await f.page.getByRole('option', { name: /Peer to review/ }).click();
  await f.page.keyboard.press('Escape');
  await f.page.getByLabel('Next appointment', { exact: true }).fill('2099-01-01T12:00');
  await f.page.reload();
  await expect(status()).toHaveText('Waiting');
  await expect(f.page.getByLabel('Next appointment', { exact: true })).toHaveValue('2099-01-01T12:00');
  await f.page.getByRole('button', { name: 'Create task', exact: true }).click();
  await expect(f.page.getByRole('heading', { name: /TASK-.*Initially blocked manual work/ })).toBeVisible({ timeout: 20000 });
  const waiting = await f.task(savedId(f.page));
  assert.equal(waiting.workflowState, 'waiting');
  assert.equal(waiting.runtimeWait, null);
  assert.deepEqual(waiting.dependencies, [peer.task.objectId]);
  assert.ok(waiting.wakeAt);
  assert.equal(waiting.claim, null);
  assert.equal(waiting.lastRun, null);

  await f.page.goto(f.url + '#/new?node=browser-task-board');
  await f.page.getByRole('radio', { name: 'Me', exact: true }).check();
  await f.page.getByLabel('Title', { exact: true }).fill('Existing manual result');
  await choose(status(), 'review');
  await expect(f.page.getByRole('button', { name: 'Create task', exact: true })).toBeDisabled();
  await f.page.getByRole('textbox', { name: 'Final result', exact: true }).fill('This completed manual result is ready to read.');
  await f.page.getByRole('button', { name: 'Create task', exact: true }).click();
  await expect(f.page.getByRole('heading', { name: /TASK-.*Existing manual result/ })).toBeVisible();
  const reviewed = await f.task(savedId(f.page));
  assert.equal(reviewed.workflowState, 'review');
  assert.equal(reviewed.claim, null);
  assert.equal(reviewed.lastRun, null);
  const result = (await f.client.request('objects.read', reviewed.latestResult)).content.value;
  assert.equal(result.kind, 'final');
  assert.equal(result.run, null);
  assert.match(result.content.summary, /completed manual result/);
  assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(f.pageErrors, []);
});

test('an agent cannot start with incomplete uploads, while explicit Backlog preserves the selected status', { timeout: 90000 }, async t => {
  const f = await taskBoardFixture(t, { native: true, scheduler: true });
  await f.open('#/new?node=browser-task-board');
  await f.page.getByLabel('Title', { exact: true }).fill('Prepare the agent input');
  await f.page.locator('input[type=file]').first().setInputFiles({ name: 'input.txt', mimeType: 'text/plain', buffer: Buffer.from('Required input.') });
  await expect(f.page.getByText('Create this task in Backlog to finish uploading its files before an agent can start. Move it to Todo when ready.')).toBeVisible();
  await expect(f.page.getByRole('button', { name: 'Create task', exact: true })).toBeDisabled();
  await choose(f.page.getByLabel('Initial status', { exact: true }), 'waiting');
  await expect(f.page.getByRole('button', { name: 'Create task', exact: true })).toBeDisabled();
  await choose(f.page.getByLabel('Initial status', { exact: true }), 'backlog');
  await f.page.getByRole('button', { name: 'Create task', exact: true }).click();
  await expect(f.page.getByRole('heading', { name: /TASK-.*Prepare the agent input/ })).toBeVisible();
  const saved = await f.task(savedId(f.page));
  assert.equal(saved.workflowState, 'backlog');
  assert.equal(saved.fields.control, 'agent');
  assert.equal(saved.attachments.length, 1);
  assert.equal(saved.lastRun, null);
  assert.deepEqual(f.pageErrors, []);
});
