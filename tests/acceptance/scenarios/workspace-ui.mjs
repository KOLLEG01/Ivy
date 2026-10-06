import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { parseArgs } from 'node:util';
import { chromium, expect } from '@playwright/test';
import { HiveClient } from '../../../dist/packages/sdk/src/client.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';
import { publishUi } from '../../../dist/packages/cli/src/publish-ui.js';
import { digest } from '../../../dist/packages/contracts/src/canonical.js';

// Installed target, real HTTP/browser/storage. Only synthetic, uniquely named notes are changed.
// The caller provides the explicit isolated host; this runner performs no host lifecycle action.
const { values } = parseArgs({ options: { config: { type: 'string' }, 'host-id': { type: 'string' }, evidence: { type: 'string' } } });
assert.ok(values.config && values['host-id'] && values.evidence, 'Use --config, --host-id and --evidence.');
const config = JSON.parse(await readFile(resolve(values.config), 'utf8'));
assert.equal(config.hostId, values['host-id']); assert.ok(config.hostId.endsWith('-acceptance'));
assert.ok(new URL(config.publicBaseUrl).protocol === 'https:' || new URL(config.publicBaseUrl).hostname === '127.0.0.1', 'Select the configured acceptance installation through verified HTTPS or explicit loopback.');
const hive = config.instances.find(i => i.componentId === 'hive'); assert.ok(hive?.settings.credentials[0]?.token);
const client = new HiveClient(config.publicBaseUrl, { credential: hive.settings.credentials[0].token });
const root = resolve(values.evidence), reportPath = join(root, 'report.json'); await mkdir(root, { recursive: true });
await writeFile(reportPath, '{}\n', { flag: 'wx' });
const id = randomUUID(), prefix = config.hostId + ' UI ' + id;
const report = { schemaVersion: 1, id, hostId: config.hostId, endpoint: config.publicBaseUrl, startedAt: new Date().toISOString(), phase: 'starting', cases: [], objects: [] };
const save = async (phase, details = {}) => {
  Object.assign(report, details, { phase, observedAt: new Date().toISOString() }); await atomicJson(reportPath, report);
  console.log(JSON.stringify({ phase, reportPath, casesPassed: report.cases.length }));
};
let browser, page; const pageErrors = [], outsideRequests = [];
const runCase = async (name, action) => { await save(name); const started = Date.now(); await action(); report.cases.push({ name, durationMs: Date.now() - started }); await save(name + '-passed'); };
const navigation = p => p.getByRole('navigation', { name: 'Page navigation' });
const objectId = p => { const value = new URLSearchParams(new URL(p.url()).hash.split('?')[1]).get('id'); assert.ok(value); return value; };
const createNote = async (title, text) => {
  if (!await navigation(page).isVisible()) await page.getByRole('button', { name: 'Toggle navigation', exact: true }).click();
  await navigation(page).getByRole('link', { name: 'New page', exact: true }).click();
  await page.getByLabel('Page title', { exact: true }).fill(title); await page.getByLabel('Markdown', { exact: true }).fill(text);
  await page.getByRole('button', { name: 'Save page', exact: true }).click(); await expect(page.getByRole('heading', { name: title, exact: true })).toBeVisible();
  const value = objectId(page); report.objects.push(value); return value;
};
try {
  const before = await client.request('system.status', {}); assert.equal(before.ready, true); await save('publishing-ui', { before });
  let expectedReleaseId = null;
  try { expectedReleaseId = (await client.request('uis.get', { uiId: 'wiki-ui' })).currentReleaseId; } catch (error) { if (error.code !== 'not_found') throw error; }
  const directory = fileURLToPath(new URL('../../../dist/apps/wiki-ui', import.meta.url));
  const definition = JSON.parse(await readFile(join(directory, 'ivy-ui.json'), 'utf8'));
  const release = await publishUi(client, { directory, definition, expectedReleaseId, mutationId: 'installed-ui-' + id });
  await save('opening-browser', { release });
  browser = await chromium.launch({ ...(process.platform === 'win32' ? { channel: 'msedge' } : {}), headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, colorScheme: 'light' });
  context.setDefaultTimeout(20_000); context.setDefaultNavigationTimeout(30_000);
  context.on('page', p => p.on('pageerror', error => pageErrors.push(error.message)));
  await context.route('**/*', route => {
    if (new URL(route.request().url()).origin !== new URL(config.publicBaseUrl).origin) { outsideRequests.push(route.request().url()); return route.abort(); }
    return route.continue();
  });
  page = await context.newPage();
  await runCase('console-login-and-views', async () => {
    await page.goto(config.publicBaseUrl + '/'); await page.getByLabel('Hive credential').fill(hive.settings.credentials[0].token);
    const submitted = page.waitForResponse(response => response.request().method() === 'POST' && response.url() === config.publicBaseUrl + '/login');
    await page.getByRole('button', { name: 'Sign in', exact: true }).click(); const login = await submitted;
    assert.equal(login.status(), 303); assert.equal(login.request().headers()['origin'], new URL(config.publicBaseUrl).origin);
    await page.waitForURL(url => url.pathname === new URL(config.publicBaseUrl + '/').pathname);
    const cookie = (await context.cookies()).find(item => item.name === 'ivy_session'); assert.ok(cookie.httpOnly && cookie.secure); assert.equal(cookie.sameSite, 'Strict');
    assert.equal(await page.evaluate(() => document.cookie), '');
    for (const label of ['Hosts & services', 'Definitions', 'Deployments', 'Problems', 'Object Browser', 'Uis']) {
      await navigation(page).getByRole('link', { name: label, exact: true }).click();
      await expect(page.getByRole('heading', { name: label, exact: true })).toBeVisible();
      await expect(page.getByRole('alert')).toHaveCount(0);
    }
    await page.screenshot({ path: join(root, 'console-uis.png'), fullPage: true });
  });
  const wikiUrl = config.publicBaseUrl + '/ui/wiki-ui/';
  await page.goto(wikiUrl + '#/home'); await expect(page.getByRole('heading', { name: 'Wiki', exact: true })).toBeVisible();
  await runCase('fresh-edit-and-two-browser-conflict', async () => {
    const note = await createNote(prefix + ' conflict', 'Original saved text.');
    await client.request('objects.write', { mutationId: id + '-before-edit', objectId: note, expectedRevision: 1, contractVersion: '1.0.0', content: { encoding: 'text', value: 'Newer content before Edit.' } });
    await page.getByRole('button', { name: 'Edit page', exact: true }).click();
    await expect(page.getByLabel('Markdown', { exact: true })).toHaveValue('Newer content before Edit.');
    const second = await context.newPage(); await second.goto(page.url()); await second.getByRole('button', { name: 'Edit page', exact: true }).click();
    await page.getByLabel('Markdown', { exact: true }).fill('First writer keeps this result.'); await second.getByLabel('Markdown', { exact: true }).fill('Second writer retains this draft.');
    await page.getByRole('button', { name: 'Save page', exact: true }).click(); await expect(page.getByText('Saved revision 3.', { exact: true })).toBeVisible();
    await second.getByLabel('Markdown', { exact: true }).focus(); await second.getByRole('button', { name: 'Refresh', exact: true }).click();
    await expect(second.getByLabel('Markdown', { exact: true })).toHaveValue('Second writer retains this draft.');
    await expect(second.getByText('Save base: revision 2. Refreshing does not replace your draft.', { exact: true })).toBeVisible();
    await second.getByRole('button', { name: 'Save page', exact: true }).click();
    await expect(second.getByRole('heading', { name: 'This page changed while you were editing', exact: true })).toBeVisible();
    await expect(second.getByRole('button', { name: 'Save page', exact: true })).toBeDisabled();
    await second.screenshot({ path: join(root, 'wiki-conflict.png'), fullPage: true });
    await second.getByRole('button', { name: 'Use revision 3 as save base', exact: true }).click(); await second.getByRole('button', { name: 'Save page', exact: true }).click();
    await expect(second.getByText('Saved revision 4.', { exact: true })).toBeVisible();
    assert.equal((await client.request('objects.read', { objectId: note })).content.value, 'Second writer retains this draft.'); await second.close();
  });
  await runCase('links-safe-content-attachment-and-history', async () => {
    const note = await createNote(prefix + ' reference', '## Saved knowledge\n\n<script>alert("inert")</script>\n\n![Not fetched](https://invalid.test/pixel)\n');
    await expect(page.getByText('<script>alert("inert")</script>', { exact: true })).toBeVisible(); assert.equal(await page.locator('main img, main script, main iframe').count(), 0);
    const stable = await page.getByLabel('Stable Object link', { exact: true }).inputValue();
    await page.getByRole('link', { name: 'Add child page', exact: true }).click();
    await page.getByLabel('Page title', { exact: true }).fill(prefix + ' child'); await page.getByLabel('Markdown', { exact: true }).fill('A connected page.');
    await page.getByRole('button', { name: 'Save page', exact: true }).click(); await expect(page.getByRole('heading', { name: prefix + ' child', exact: true })).toBeVisible();
    const child = objectId(page); report.objects.push(child);
    await page.goto(stable); await page.getByRole('button', { name: 'Edit page', exact: true }).click();
    await page.getByLabel('Markdown', { exact: true }).fill('Follow the [related note](#/page?id=' + child + ').'); await page.getByRole('button', { name: 'Save page', exact: true }).click();
    await page.getByRole('link', { name: 'related note', exact: true }).click(); await expect(page.getByText('A connected page.', { exact: true })).toBeVisible(); await page.goto(stable);
    const bytes = Buffer.from([0, 1, 254, 255, 60, 62, 0, 42]);
    await page.getByLabel('Choose attachment', { exact: true }).setInputFiles({ name: 'proof.bin', mimeType: 'application/octet-stream', buffer: bytes });
    await page.getByRole('button', { name: 'Upload attachment', exact: true }).click();
    await expect(page.getByLabel('Markdown link to the saved revision', { exact: true })).toHaveValue(/^\[proof\.bin\]\(#\/page\?id=.*&revision=1\)$/);
    await page.getByRole('link', { name: /^proof\.bin/ }).click(); await expect(page.getByText('This is a binary attachment. Download its exact saved revision to inspect it.', { exact: true })).toBeVisible();
    report.objects.push(objectId(page)); const downloading = page.waitForEvent('download'); await page.getByRole('button', { name: 'Download revision', exact: true }).click();
    const downloaded = join(root, 'wiki-proof.bin'); await (await downloading).saveAs(downloaded); assert.deepEqual(await readFile(downloaded), bytes); report.attachmentHash = digest(bytes);
    await client.request('objects.move', { mutationId: id + '-rename', objectId: note, parentId: null, name: prefix + ' renamed' });
    await page.goto(stable); await expect(page.getByRole('heading', { name: prefix + ' renamed', exact: true })).toBeVisible();
    await page.getByRole('link', { name: /^Revision 1\b/ }).click(); await expect(page.getByText('<script>alert("inert")</script>', { exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Edit page', exact: true })).toHaveCount(0); assert.equal((await client.request('objects.stat', { objectId: note })).currentRevision, 2);
    await page.screenshot({ path: join(root, 'wiki-history.png'), fullPage: true });
  });
  await runCase('lost-save-original-identity', async () => {
    const note = await createNote(prefix + ' recovery', 'Before the lost response.');
    await page.getByRole('button', { name: 'Edit page', exact: true }).click(); await page.getByLabel('Markdown', { exact: true }).fill('Saved exactly once on the installed Hive.');
    const calls = []; const observed = request => { if (request.url().endsWith('/api/v1/rpc')) { const body = request.postDataJSON(); if (body.method === 'objects.write') calls.push(body.params); } };
    page.on('request', observed);
    const lost = async route => { if (route.request().postDataJSON().method !== 'objects.write') return route.continue(); await route.fetch(); await route.abort(); };
    await page.route('**/api/v1/rpc', lost); await page.getByRole('button', { name: 'Save page', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Retry original save', exact: true })).toBeEnabled();
    await page.unroute('**/api/v1/rpc', lost); await page.reload(); await expect(page.getByRole('button', { name: 'Retry original save', exact: true })).toBeEnabled(); assert.equal(calls.length, 1);
    await page.getByRole('button', { name: 'Retry original save', exact: true }).click(); await expect(page.getByText('Saved revision 2.', { exact: true })).toBeVisible();
    assert.equal(calls.length, 2); assert.deepEqual(calls[1], calls[0]); assert.equal((await client.request('objects.stat', { objectId: note })).currentRevision, 2);
    report.replayedMutation = { mutationId: calls[0].mutationId, requestHash: digest(Buffer.from(JSON.stringify(calls[0]))), requests: calls.length, resultingRevision: 2 }; page.off('request', observed);
  });
  await runCase('mobile-keyboard-theme-and-draft', async () => {
    await page.setViewportSize({ width: 390, height: 844 }); await page.reload();
    await page.keyboard.press('Tab'); await expect(page.getByRole('link', { name: 'Skip to content', exact: true })).toBeFocused();
    const url = page.url(); await page.keyboard.press('Enter'); await expect(page.locator('#main-content')).toBeFocused(); assert.equal(page.url(), url);
    await page.getByRole('button', { name: 'Use dark theme', exact: true }).click(); await expect(page.locator('html')).toHaveClass('dark');
    await page.getByRole('button', { name: 'Edit page', exact: true }).click(); await page.getByLabel('Markdown', { exact: true }).fill('Retained narrow-screen draft.');
    await page.reload(); await expect(page.locator('html')).toHaveClass('dark'); await expect(page.getByLabel('Markdown', { exact: true })).toHaveValue('Retained narrow-screen draft.');
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)); await page.screenshot({ path: join(root, 'wiki-mobile-dark.png'), fullPage: true });
  });
  assert.deepEqual(pageErrors, []); assert.deepEqual(outsideRequests, []);
  await save('passed', { browser: browser.version(), after: await client.request('system.status', {}), pageErrors, outsideRequests, finishedAt: new Date().toISOString() });
} catch (error) {
  await page?.screenshot({ path: join(root, 'failure.png'), fullPage: true }).catch(() => undefined);
  await save('failed', { failure: { code: error.code ?? error.name, message: error.message }, pageErrors, outsideRequests }); process.exitCode = 1;
} finally { await browser?.close(); }
