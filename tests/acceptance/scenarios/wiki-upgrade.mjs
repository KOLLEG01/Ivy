import assert from 'node:assert/strict';
import { mkdir, readFile, readdir, cp, writeFile, unlink } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { parseArgs } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium, expect } from '@playwright/test';
import { HiveClient } from '../../../dist/packages/sdk/src/client.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';
import { publishUi } from '../../../dist/packages/cli/src/publish-ui.js';
import { digest } from '../../../dist/packages/contracts/src/canonical.js';

// Explicit installed-target acceptance. The caller controls backend deployment separately.
const { values } = parseArgs({ options: { config: { type: 'string' }, 'host-id': { type: 'string' }, evidence: { type: 'string' }, 'legacy-text-html': { type: 'boolean' } } });
assert.ok(values.config && values['host-id'] && values.evidence, 'Use --config, --host-id and --evidence.');
const config = JSON.parse(await readFile(resolve(values.config), 'utf8'));
assert.equal(config.hostId, values['host-id']); assert.ok(config.hostId.endsWith('-acceptance'));
assert.ok(new URL(config.publicBaseUrl).protocol === 'https:' || new URL(config.publicBaseUrl).hostname === '127.0.0.1', 'Select the configured acceptance installation through verified HTTPS or explicit loopback.');
const hive = config.instances.find(instance => instance.componentId === 'hive'); assert.ok(hive?.settings.credentials[0]?.token);
const client = new HiveClient(config.publicBaseUrl, { credential: hive.settings.credentials[0].token });
const project = fileURLToPath(new URL('../../../', import.meta.url));
const root = resolve(values.evidence); await mkdir(root, { recursive: true });
const controlPath = join(root, 'control.json'), reportPath = join(root, 'report.json');
let report;
try { await readFile(reportPath); throw new Error('Use a new evidence directory; never replace an earlier acceptance identity.'); }
catch (error) { if (error.code !== 'ENOENT') throw error; }
report = { schemaVersion: 1, hostId: config.hostId, id: randomUUID(), startedAt: new Date().toISOString(), phase: 'starting', observations: [] };
const save = async (phase, value = {}) => {
  report = { ...report, ...value, phase, observedAt: new Date().toISOString() }; await atomicJson(reportPath, report);
  process.stdout.write(JSON.stringify({ phase, reportPath, id: report.id }) + '\n');
};
let browser, page;
const pageErrors = [], outsideRequests = [], writes = [];
try {
  const before = await client.request('system.status', {}); assert.equal(before.ready, true);
  await save('publishing-ui', { before });
  let expectedReleaseId = null;
  try { expectedReleaseId = (await client.request('uis.get', { uiId: 'wiki-ui' })).currentReleaseId; }
  catch (error) { if (error.code !== 'not_found') throw error; }
  let directory = resolve(project, 'dist/apps/wiki-ui');
  if (values['legacy-text-html']) {
    // The early installed Hive predates text/plain static assets. Keep every notice/license as
    // escaped HTML text in this explicit baseline release; executable JS/CSS remain byte-identical.
    const copy = join(root, 'legacy-baseline-bundle'), transformed = []; await cp(directory, copy, { recursive: true, errorOnExist: true, force: false });
    const visit = async (relative = '') => {
      for (const entry of await readdir(join(copy, relative), { withFileTypes: true })) {
        assert.ok(!entry.isSymbolicLink()); const path = relative + entry.name;
        if (entry.isDirectory()) { await visit(path + '/'); continue; }
        if (!/\.(md|txt)$/.test(path)) continue;
        const bytes = await readFile(join(copy, path)), content = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
        const escaped = content.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
        const html = '<!doctype html><html lang="en"><meta charset="utf-8"><title>Retained license or notice</title><pre>' + escaped + '</pre></html>';
        await writeFile(join(copy, path + '.html'), html, { flag: 'wx' }); await unlink(join(copy, path));
        transformed.push({ originalPath: path, originalHash: digest(bytes), path: path + '.html', hash: digest(Buffer.from(html)) });
      }
    };
    await visit(); directory = copy; await save('publishing-legacy-baseline', { transformed });
  }
  const definition = JSON.parse(await readFile(join(directory, 'ivy-ui.json'), 'utf8'));
  const release = await publishUi(client, { directory, definition, expectedReleaseId, mutationId: 'wiki-upgrade-' + report.id });
  await save('opening-original-page', { release });
  browser = await chromium.launch({ ...(process.platform === 'win32' ? { channel: 'msedge' } : {}), headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, colorScheme: 'light' }); page = await context.newPage();
  page.on('pageerror', error => pageErrors.push(error.message));
  await context.route('**/*', route => {
    if (new URL(route.request().url()).origin !== new URL(config.publicBaseUrl).origin) { outsideRequests.push(route.request().url()); return route.abort(); }
    return route.continue();
  });
  await page.goto(config.publicBaseUrl + '/');
  await page.getByLabel('Hive credential').fill(hive.settings.credentials[0].token);
  const submitted = page.waitForResponse(response => response.request().method() === 'POST' && response.url() === config.publicBaseUrl + '/login');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  const login = await submitted;
  await save('checking-form-login', { login: { status: login.status(), origin: login.request().headers()['origin'] ?? null } });
  assert.equal(login.request().headers()['origin'], new URL(config.publicBaseUrl).origin, 'The actual form must keep its Origin.');
  assert.equal(login.status(), 303);
  await page.waitForURL(url => url.pathname === new URL(config.publicBaseUrl + '/').pathname);
  await page.goto(config.publicBaseUrl + '/ui/wiki-ui/#/home');
  await page.getByRole('heading', { name: 'Wiki', exact: true }).waitFor();
  page.on('request', request => {
    if (request.url().endsWith('/api/v1/rpc')) { const body = request.postDataJSON(); if (body.method === 'objects.write') writes.push({ id: body.params.mutationId, at: new Date().toISOString() }); }
  });
  const title = config.hostId + ' update evidence ' + report.id, original = 'Saved before the installed Hive update.';
  await page.getByRole('navigation', { name: 'Page navigation' }).getByRole('link', { name: 'New page', exact: true }).click();
  await page.getByLabel('Page title', { exact: true }).fill(title); await page.getByLabel('Markdown', { exact: true }).fill(original);
  await page.getByRole('button', { name: 'Save page', exact: true }).click();
  await page.getByRole('heading', { name: title, exact: true }).waitFor();
  const objectId = new URLSearchParams(new URL(page.url()).hash.split('?')[1]).get('id'); assert.ok(objectId);
  await page.getByRole('button', { name: 'Edit page', exact: true }).click();
  const draft = 'Retained draft across the actual installed Hive update. Evidence ' + report.id;
  await page.getByLabel('Markdown', { exact: true }).fill(draft); await page.getByLabel('Markdown', { exact: true }).focus();
  const originalUrl = page.url(), savedWrites = writes.length;
  await page.screenshot({ path: join(root, 'wiki-before-update.png'), fullPage: true });
  await save('waiting-for-backend-update', { browser: browser.version(), objectId, originalUrl, savedWrites, controlPath });
  const deadline = Date.now() + 2 * 60 * 60_000;
  let control;
  while (Date.now() < deadline) {
    try { control = JSON.parse(await readFile(controlPath, 'utf8')); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
    if (control?.action === 'abort') throw new Error('Caller stopped this unfinished acceptance run.');
    if (control?.action === 'verify-update') break;
    await delay(1000);
  }
  assert.equal(control?.action, 'verify-update', 'Timed out waiting for an actual backend activation.');
  assert.match(control.expectedBuildId, /^sha256:[0-9a-f]{64}$/);
  const after = await client.request('system.status', {}); assert.equal(after.buildId, control.expectedBuildId);
  assert.notEqual(after.buildId, before.buildId); assert.equal(after.ready, true);
  await save('verifying-original-page', { after, deploymentId: control.deploymentId });
  assert.equal(page.url(), originalUrl); await expect(page.getByLabel('Markdown', { exact: true })).toHaveValue(draft);
  assert.equal(writes.length, savedWrites, 'Connection recovery must not send a queued mutation.');
  await page.getByRole('button', { name: 'Refresh', exact: true }).click();
  await expect(page.getByLabel('Markdown', { exact: true })).toHaveValue(draft);
  assert.equal((await client.request('objects.read', { objectId })).content.value, original);
  await page.screenshot({ path: join(root, 'wiki-after-update-draft.png'), fullPage: true });
  await page.getByRole('button', { name: 'Save page', exact: true }).click();
  await expect(page.getByText('Saved revision 2.', { exact: true })).toBeVisible();
  const saved = await client.request('objects.read', { objectId }); assert.equal(saved.content.value, draft); assert.equal(saved.object.currentRevision, 2);
  assert.equal(writes.length, savedWrites + 1); assert.deepEqual(pageErrors, []); assert.deepEqual(outsideRequests, []);
  await page.screenshot({ path: join(root, 'wiki-after-update-saved.png'), fullPage: true });
  await save('passed', { writes, finishedAt: new Date().toISOString() });
} catch (error) {
  await page?.screenshot({ path: join(root, 'wiki-upgrade-failure.png'), fullPage: true }).catch(() => undefined);
  await save('failed', { failure: { code: error.code ?? error.name, message: error.message }, pageErrors, outsideRequests }); process.exitCode = 1;
} finally { await browser?.close(); }
