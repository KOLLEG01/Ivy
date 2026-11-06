import test from 'node:test';
import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { createServer } from 'vite';
import vue from '@vitejs/plugin-vue';
import tailwindcss from '@tailwindcss/vite';
import { chromium, expect } from '@playwright/test';
import { choose, options } from './fixtures/select.mjs';

async function fixture(t, width) {
  const server = await createServer({ configFile: false, root: resolve('.'), logLevel: 'error', appType: 'custom',
    resolve: { alias: [{ find: /^@ivy\/ui$/, replacement: resolve('packages/ui/src/index.ts') }, { find: /^@ivy\/ui\/(.+)$/, replacement: resolve('packages/ui/src/$1') }] },
    plugins: [vue(), tailwindcss(), { name: 'inert-execution-transport', enforce: 'pre', transform(source, id) {
      if (!id.replaceAll('\\', '/').endsWith('/packages/ui-client/src/runtime.ts')) return;
      assert(source.includes('const notifications = browserNotifications(base.href);'));
      return "import { fixtureClient, fixtureNotifications } from '/tests/web-e2e/fixtures/execution-rpc.mjs';\n" + source
        .replace('const notifications = browserNotifications(base.href);', 'const notifications = fixtureNotifications;')
        .replace('client: browserClient(base.href)', 'client: fixtureClient');
    }, configureServer(server) {
      server.middlewares.use((request, response, next) => {
        const path = new URL(request.url, 'http://localhost').pathname;
        if (!['/taskboard/', '/agents/'].includes(path)) return next();
        response.setHeader('Content-Type', 'text/html');
        response.end('<!doctype html><html lang="en"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Account selection fixture</title><body><div id="ui"></div><script type="module" src="/tests/web-e2e/fixtures/' + (path === '/agents/' ? 'agent-execution-entry' : 'execution-entry') + '.mjs"></script></body></html>');
      });
    } }], server: { host: '127.0.0.1', port: 0, hmr: false, watch: { ignored: ['**'] } } });
  let browser;
  t.after(async () => { await browser?.close(); await server.close(); }); await server.listen();
  browser = await chromium.launch({ ...(process.platform === 'win32' ? { channel: 'msedge' } : {}), headless: true });
  const context = await browser.newContext({ viewport: { width, height: 1000 }, locale: 'en-US' });
  const page = await context.newPage(), errors = [];
  page.on('pageerror', error => errors.push({ url: page.url(), message: error.message, stack: error.stack }));
  const base = 'http://127.0.0.1:' + server.httpServer.address().port;
  await context.route('**/*', route => new URL(route.request().url()).origin === base ? route.continue() : route.abort());
  return { page, errors, open: async mode => {
    await page.goto(base + (mode.startsWith('agents') ? '/agents/' : '/taskboard/') + '?mode=' + mode, { waitUntil: 'commit' });
    try { await page.locator('#ui > *').first().waitFor({ state: 'attached', timeout: 60000 }); }
    catch (error) { throw new Error('Fixture did not mount: ' + JSON.stringify(errors), { cause: error }); }
    assert.deepEqual(errors, []);
  } };
}
const claude = { backendId: 'secondary', provider: 'claude', accountId: 'personal' };
const backendLabel = 'Claude · personal (secondary)';

for (const width of [1440, 390]) test(`Agent UI stops claiming stale inventory is working on ${width}px`, { timeout: 180000 }, async t => {
  const f = await fixture(t, width), page = f.page;
  await page.clock.install({ time: new Date('2026-09-25T12:00:00.000Z') });
  await f.open('agents-freshness');
  if (width < 768) await page.getByRole('button', { name: 'Toggle navigation', exact: true }).click();
  const link = page.getByRole('navigation', { name: 'Recent tasks', exact: true }).getByRole('link', { name: 'codex work task', exact: true });
  await expect(link).toBeVisible({ timeout: 35000 });
  const row = link.locator('..');
  const working = row.getByRole('img', { name: 'Working', exact: true });
  await expect(working).toHaveCount(0);
  const primary = { backendId: 'primary', provider: 'codex', accountId: 'work' };
  await page.evaluate(backend => window.executionFixture.notify(backend, 'thread/status/changed', { threadId: 'same-thread', status: { type: 'active' } }), primary);
  await expect(working).toBeVisible();
  await page.clock.fastForward(120001);
  await expect(working).toHaveCount(0);
  await page.evaluate(backend => window.executionFixture.notify(backend, 'thread/status/changed', { threadId: 'same-thread', status: { type: 'idle' } }), primary);
  await expect(row.getByRole('img', { name: 'Unread results', exact: true })).toBeVisible();
  assert.equal(await page.evaluate(() => window.executionFixture.actions.length), 0);
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(f.errors, []);
});

for (const width of [1440, 390]) test(`Task history loads native pages on demand and keeps account and generation boundaries on ${width}px`, { timeout: 180000 }, async t => {
  const f = await fixture(t, width), page = f.page;
  await f.open('agents-history');
  await page.evaluate(() => { location.hash = '#/host?' + new URLSearchParams({ node: 'fixture-host.agent-manager', namespace: 'secondary', backend: 'secondary', provider: 'claude', account: 'personal' }); });
  await expect(page.getByLabel('First message', { exact: true })).toBeVisible();
  assert.equal(await page.evaluate(() => window.executionFixture.historyReads.length), 0);
  await page.getByRole('button', { name: 'Browse task history', exact: true }).click();
  const history = page.getByRole('region', { name: 'Task history', exact: true });
  await expect(history.getByRole('link', { name: 'Current claude personal task 0', exact: true })).toBeVisible();
  await expect(history.getByRole('link')).toHaveCount(50);
  assert.equal(await page.evaluate(() => window.executionFixture.historyReads.length), 1);
  await history.getByRole('button', { name: 'Next', exact: true }).click();
  await expect(history.getByRole('link', { name: 'Current claude personal task 50', exact: true })).toBeVisible();
  await expect(history.getByRole('link', { name: 'Current claude personal task 0', exact: true })).toHaveCount(0);
  await history.getByRole('button', { name: 'Previous', exact: true }).click();
  await expect(history.getByRole('link', { name: 'Current claude personal task 0', exact: true })).toBeVisible();
  await choose(history.getByLabel('Tasks', { exact: true }), 'Archived');
  await expect(history.getByRole('link', { name: 'Archived claude personal task 0', exact: true })).toBeVisible();
  await history.getByRole('button', { name: 'Next', exact: true }).click();
  const saved = history.getByRole('link', { name: 'Archived claude personal task 50', exact: true });
  await expect(saved).toBeVisible(); await expect(history.getByRole('link')).toHaveCount(1);
  await expect(history.getByRole('button', { name: 'Next', exact: true })).toBeDisabled();
  const address = new URLSearchParams((await saved.getAttribute('href')).split('?')[1]);
  assert.deepEqual(Object.fromEntries(address), { node: 'fixture-host.agent-manager', namespace: 'secondary', backend: 'secondary', provider: 'claude', account: 'personal', id: 'history-50' });
  await choose(history.getByLabel('Tasks', { exact: true }), 'Current');
  await expect(history.getByRole('link', { name: 'Current claude personal task 0', exact: true })).toBeVisible();
  await history.getByRole('button', { name: 'Next', exact: true }).click();
  await expect(history.getByRole('link', { name: 'Current claude personal task 50', exact: true })).toBeVisible();
  const before = await page.evaluate(() => { window.executionFixture.historyEpochs.secondary = 'replacement-epoch'; return window.executionFixture.historyReads.length; });
  await history.getByRole('button', { name: 'Next', exact: true }).click();
  await expect(history.getByText('Task history changed. Restart from the first page.', { exact: true })).toBeVisible();
  assert.equal(await page.evaluate(() => window.executionFixture.historyReads.length), before);
  await history.getByRole('button', { name: 'Latest tasks', exact: true }).click();
  await expect(history.getByRole('link', { name: 'Current claude personal task 0', exact: true })).toBeVisible();
  await choose(page.getByLabel('Provider and account', { exact: true }), 'Codex · work (primary)');
  await expect(history).toHaveCount(0);
  assert((await page.evaluate(() => window.executionFixture.historyReads)).every(call => call.backend.provider === 'claude'));
  await page.getByRole('button', { name: 'Browse task history', exact: true }).click();
  await expect(history.getByRole('link', { name: 'Current codex work task 0', exact: true })).toBeVisible();
  await expect(history.getByRole('link', { name: 'Current claude personal task 0', exact: true })).toHaveCount(0);
  const calls = await page.evaluate(() => window.executionFixture.historyReads);
  assert(calls.slice(0, -1).every(call => call.backend.provider === 'claude' && call.backend.accountId === 'personal'));
  assert.deepEqual(calls.at(-1).args, { archived: false, limit: 50, sourceKinds: ['cli', 'vscode', 'exec', 'appServer', 'subAgent', 'subAgentReview', 'subAgentCompact', 'subAgentThreadSpawn', 'subAgentOther', 'unknown'], modelProviders: [], sortKey: 'recency_at', sortDirection: 'desc', useStateDbOnly: true });
  assert.equal(await page.evaluate(() => window.executionFixture.actions.length), 0);
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(f.errors, []);
});

test('Task history retains bounded back navigation while forward history and restart remain available', { timeout: 180000 }, async t => {
  const f = await fixture(t, 1440), page = f.page;
  await f.open('agents-history');
  await page.evaluate(() => { location.hash = '#/host?' + new URLSearchParams({ node: 'fixture-host.agent-manager', namespace: 'secondary', backend: 'secondary', provider: 'claude', account: 'personal' }); });
  await page.getByRole('button', { name: 'Browse task history', exact: true }).click();
  const history = page.getByRole('region', { name: 'Task history', exact: true });
  await expect(history.getByRole('link', { name: 'Current claude personal task 0', exact: true })).toBeVisible();
  for (let n = 1; n <= 35; n++) {
    await history.getByRole('button', { name: 'Next', exact: true }).click();
    await expect(history.getByRole('link', { name: `Current claude personal task ${n * 50}`, exact: true })).toBeVisible();
    await expect(history.getByRole('link')).toHaveCount(50);
  }
  for (let n = 34; n >= 3; n--) {
    await history.getByRole('button', { name: 'Previous', exact: true }).click();
    await expect(history.getByRole('link', { name: `Current claude personal task ${n * 50}`, exact: true })).toBeVisible();
  }
  await expect(history.getByRole('button', { name: 'Previous', exact: true })).toBeDisabled();
  await expect(history.getByRole('button', { name: 'Next', exact: true })).toBeEnabled();
  await history.getByRole('button', { name: 'Latest tasks', exact: true }).click();
  await expect(history.getByRole('link', { name: 'Current claude personal task 0', exact: true })).toBeVisible();
  await expect(history.getByRole('button', { name: 'Previous', exact: true })).toBeDisabled();
  assert.equal(await page.evaluate(() => window.executionFixture.actions.length), 0);
  assert.deepEqual(f.errors, []);
});

for (const width of [1440, 390]) test(`Attachment admission feedback and removal work in both account composers on ${width}px`, { timeout: 150000 }, async t => {
  const f = await fixture(t, width), page = f.page;
  await f.open('agents');
  const route = '#/host?' + new URLSearchParams({ node: 'fixture-host.agent-manager', namespace: 'secondary', backend: 'secondary', provider: 'claude', account: 'personal' });
  await page.evaluate(route => { location.hash = route; }, route);
  await expect(page.getByLabel('First message', { exact: true })).toBeVisible();
  await page.getByLabel('First message', { exact: true }).fill('Host composer admission');
  const files = Array.from({ length: 9 }, (_, n) => ({ name: 'bounded-' + n + '.txt', mimeType: 'text/plain', buffer: Buffer.from('small inert file') }));
  await page.getByLabel('Choose photos and files', { exact: true }).setInputFiles(files);
  const error = page.getByText('At most 8 attachments can be added to a message.', { exact: true });
  await expect(error).toBeVisible(); await expect(page.getByRole('list', { name: 'Attachments', exact: true }).getByRole('listitem')).toHaveCount(8);
  await expect(page.getByRole('button', { name: 'Create task', exact: true })).toBeDisabled();
  for (let n = 0; n < 8; n++) await page.getByRole('button', { name: 'Remove bounded-' + n + '.txt', exact: true }).click();
  await expect(error).toHaveCount(0); await expect(page.getByRole('button', { name: 'Create task', exact: true })).toBeEnabled();
  await page.evaluate(() => { location.hash = '#/task?' + new URLSearchParams({ node: 'fixture-host.agent-manager', namespace: 'secondary', backend: 'secondary', provider: 'claude', account: 'personal', id: 'same-thread' }); });
  await expect(page.getByLabel('Message', { exact: true })).toBeVisible();
  await page.getByLabel('Message', { exact: true }).fill('Task composer admission');
  await page.getByLabel('Choose photos and files', { exact: true }).setInputFiles(files);
  await expect(error).toBeVisible(); await expect(page.getByRole('button', { name: 'Send message', exact: true })).toBeDisabled();
  for (let n = 0; n < 8; n++) await page.getByRole('button', { name: 'Remove bounded-' + n + '.txt', exact: true }).click();
  await expect(error).toHaveCount(0); await expect(page.getByRole('button', { name: 'Send message', exact: true })).toBeEnabled();
  assert((await page.evaluate(() => window.executionFixture.actions)).every(call => call.backend.provider === 'claude' && call.backend.accountId === 'personal'));
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false); assert.deepEqual(f.errors, []);
});

for (const width of [1440, 390]) test(`Account defaults isolate identities, retain original saves and route native speed on ${width}px`, { timeout: 180000 }, async t => {
  const f = await fixture(t, width), page = f.page;
  await f.open('agents');
  const primary = { backendId: 'primary', provider: 'codex', accountId: 'work' };
  const original = { model: 'alternate-model', effort: 'high', mode: null, permission: null, serviceTier: 'fast' };
  const legacy = { model: 'legacy-claude-model', effort: 'max', mode: null, permission: ':workspace' };
  await page.evaluate(({ primary, original, legacy }) => {
    window.executionFixture.accountDefaults = { schemaVersion: 1, accounts: [{ backend: primary, defaults: original }] };
    window.executionFixture.defaultsRevision = 4;
    window.executionFixture.legacyDefaults = { schemaVersion: 1, codex: { model: 'legacy-codex-model', effort: 'low', mode: null, permission: null }, claude: legacy };
    location.hash = '#/settings';
  }, { primary, original, legacy });
  await expect(page.getByRole('heading', { name: 'Defaults by account', exact: true })).toBeVisible();
  const account = () => page.getByLabel('Defaults account', { exact: true });
  const save = () => page.getByRole('button', { name: /^(Save defaults|Retry original save)$/ });
  await choose(page.getByLabel('Defaults host', { exact: true }), 'fixture-host');
  await choose(account(), 'Codex · work (primary)');
  await expect(page.getByLabel('Default model for account')).toHaveText('alternate-model');
  await expect(page.getByLabel('Default speed for account')).toHaveText('Fast');
  await choose(account(), backendLabel);
  await expect(page.getByLabel('Default model for account')).toHaveText(/Shared model|same-model/);
  await expect(page.getByLabel('Default effort for account')).toHaveText(/medium/);
  await page.getByRole('button', { name: 'Copy previous provider defaults', exact: true }).click();
  await expect(page.getByLabel('Default model for account')).toHaveText('legacy-claude-model (unavailable)');
  await expect(save()).toBeDisabled();
  await page.getByRole('button', { name: 'Inherit everything from the server', exact: true }).click();
  await choose(page.getByLabel('Default model for account'), 'alternate-model');
  await choose(page.getByLabel('Default effort for account'), 'max');
  await page.getByLabel('Default speed for account').click();
  await expect(page.getByRole('option', { name: 'Fast', exact: true })).toHaveAttribute('aria-disabled', 'true');
  await expect(page.getByRole('option', { name: 'Flexible', exact: true })).toHaveAttribute('aria-disabled', 'true');
  await page.keyboard.press('Escape');
  await expect(save()).toBeEnabled();
  await page.evaluate(() => { window.executionFixture.loseDefaultReplyOnce = true; });
  await save().click();
  await expect(page.getByText('Lost default write response', { exact: true })).toBeVisible();
  await expect(account()).toBeDisabled();
  await expect(page.getByLabel('Default effort for account')).toBeDisabled();
  await save().click();
  await expect(page.getByText('Saved. New tasks for this account use these defaults.', { exact: true })).toBeVisible();
  const written = await page.evaluate(() => ({ calls: window.executionFixture.defaultWrites, commits: window.executionFixture.defaultCommits, document: window.executionFixture.accountDefaults, old: window.executionFixture.legacyDefaults }));
  assert.equal(written.commits.length, 1); assert.equal(written.calls.length, 2);
  assert.deepEqual(written.calls[1], written.calls[0], 'A lost reply retries the exact original Hive mutation.');
  assert.deepEqual(written.document.accounts.find(value => value.backend.accountId === 'work').defaults, original);
  assert.deepEqual(written.document.accounts.find(value => value.backend.provider === 'claude'), { backend: claude,
    defaults: { model: 'alternate-model', effort: 'max', mode: null, permission: null, serviceTier: null } });
  assert.deepEqual(written.old.claude, legacy, 'Explicit copying does not change the original provider document.');
  await choose(account(), 'Codex · work (primary)');
  await choose(page.getByLabel('Default speed for account'), 'flex');
  await page.evaluate(() => {
    window.executionFixture.defaultsRevision++;
    window.executionFixture.accountDefaults.accounts.find(value => value.backend.accountId === 'work').defaults.effort = 'low';
    window.executionFixture.accountDefaults.accounts.push({ backend: { backendId: 'other', provider: 'codex', accountId: 'concurrent' },
      defaults: { model: 'concurrent-model', mode: null, effort: null, permission: null, serviceTier: null } });
  });
  await page.getByRole('region', { name: 'Defaults by account', exact: true }).getByRole('button', { name: 'Reload', exact: true }).click();
  await expect.poll(() => page.evaluate(() => window.executionFixture.calls.filter(call => call.method === 'objects.read' && call.params.objectId === 'account-defaults').at(-1)?.params.revision)).toBe(6);
  await save().click();
  await expect(page.getByText('Defaults changed; your draft is retained. Review and save again.', { exact: true })).toBeVisible();
  await expect(page.getByLabel('Default speed for account')).toHaveText('Flexible');
  await expect(account()).toBeEnabled(); await save().click();
  await expect.poll(() => page.evaluate(() => window.executionFixture.defaultCommits.length)).toBe(2);
  const afterConflict = await page.evaluate(() => window.executionFixture.accountDefaults);
  assert.equal(await page.evaluate(() => window.executionFixture.defaultWrites[2].expectedRevision), 5, 'A refreshed document cannot replace the dirty draft CAS base.');
  assert(afterConflict.accounts.some(value => value.backend.accountId === 'concurrent'), 'Concurrent neighbour edits survive the reviewed CAS retry.');
  assert.equal(afterConflict.accounts.find(value => value.backend.accountId === 'work').defaults.serviceTier, 'flex');
  await page.evaluate(() => { location.hash = '#/host?node=fixture-host.agent-manager&namespace=primary&backend=primary&provider=codex&account=work'; });
  await expect(page.getByLabel('Model', { exact: true })).toHaveText('alternate-model');
  await expect(page.getByLabel('Reasoning effort', { exact: true })).toHaveText(/high/i);
  await expect(page.getByLabel('Speed', { exact: true })).toHaveText(/Flexible/);
  await page.getByLabel('First message', { exact: true }).fill('Use this account default');
  const creationReads = await page.evaluate(() => window.executionFixture.calls.filter(call => call.method === 'objects.stat' && call.params.path === '/ivy-agent-account-execution-defaults').length);
  await page.getByRole('button', { name: 'Create task', exact: true }).click();
  await expect(page.getByLabel('Message', { exact: true })).toBeVisible();
  const launch = await page.evaluate(() => window.executionFixture.actions);
  assert.deepEqual(launch.map(call => call.method), ['thread/start', 'turn/start']);
  assert(launch.every(call => JSON.stringify(call.backend) === JSON.stringify(primary)));
  assert.equal(launch[0].params.model, 'alternate-model');
  assert.equal(launch[0].params.serviceTier, 'flex'); assert.equal(launch[1].params.serviceTier, 'flex');
  // This empty existing conversation reports native same-model. Creation defaults must stay out of it.
  await expect(page.getByLabel('Model', { exact: true })).toHaveText('Shared model');
  assert.equal(await page.evaluate(() => window.executionFixture.calls.filter(call => call.method === 'objects.stat' && call.params.path === '/ivy-agent-account-execution-defaults').length), creationReads, 'An existing conversation does not load creation defaults.');
  await page.evaluate(primary => {
    window.executionFixture.threadRunning.primary = true;
    window.executionFixture.notify(primary, 'thread/status/changed', { threadId: 'same-thread', status: { type: 'active', activeFlags: [] } });
  }, primary);
  await choose(page.getByLabel('Speed', { exact: true }), 'fast');
  await page.getByLabel('Message', { exact: true }).fill('Keep the queued speed');
  await page.getByRole('button', { name: 'Queue message', exact: true }).click();
  await expect(page.getByRole('region', { name: 'Queued messages', exact: true })).toBeVisible();
  await choose(page.getByLabel('Speed', { exact: true }), 'standard');
  await page.getByRole('button', { name: 'Edit queued message Keep the queued speed', exact: true }).click();
  await expect(page.getByLabel('Speed', { exact: true })).toHaveText('Fast');
  await page.getByRole('button', { name: 'Queue message', exact: true }).click();
  await choose(page.getByLabel('Speed', { exact: true }), 'standard');
  await page.evaluate(primary => {
    window.executionFixture.threadRunning.primary = false;
    window.executionFixture.notify(primary, 'thread/status/changed', { threadId: 'same-thread', status: { type: 'idle' } });
  }, primary);
  await expect(page.getByRole('region', { name: 'Queued messages', exact: true })).toHaveCount(0);
  assert.equal(await page.evaluate(() => window.executionFixture.actions.at(-1).params.serviceTier), 'fast', 'A queued message retains its original speed after composer changes.');
  await choose(page.getByLabel('Speed', { exact: true }), 'standard');
  await page.getByLabel('Message', { exact: true }).fill('Explicitly reset native speed');
  await page.getByRole('button', { name: 'Send message', exact: true }).click();
  await expect(page.getByLabel('Message', { exact: true })).toHaveValue('');
  assert.equal(await page.evaluate(() => window.executionFixture.actions.at(-1).params.serviceTier), null);
  await page.evaluate(() => { location.hash = '#/host?node=fixture-host.agent-manager&namespace=other&backend=other&provider=codex&account=personal'; });
  await expect(page.getByLabel('Model', { exact: true })).toHaveText(/Shared model/);
  await expect(page.getByLabel('Speed', { exact: true })).toHaveText(/Standard/);
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(f.errors, []);
});

for (const width of [1440, 390]) test(`TaskBoard links retain the original Run account and reject a foreign conversation on ${width}px`, { timeout: 120000 }, async t => {
  const f = await fixture(t, width), page = f.page;
  await f.open('linked');
  const link = page.getByRole('link', { name: 'Open agent task', exact: true });
  await expect(link).toBeVisible();
  const url = new URL(await link.getAttribute('href')), query = new URLSearchParams(url.hash.split('?')[1]);
  assert.deepEqual(Object.fromEntries(query), { node: 'fixture-host.agent-manager', namespace: 'secondary', backend: 'secondary', provider: 'claude', account: 'personal', id: 'retained' });
  assert.equal((await page.evaluate(() => window.executionFixture.actions)).length, 0);
  await f.open('linked-wrong');
  await expect.poll(() => page.evaluate(() => window.executionFixture.calls.some(call => call.method === 'objects.read' && call.params.objectId === 'run'))).toBe(true);
  await expect(link).toHaveCount(0);
  await page.getByRole('tab', { name: 'Agent', exact: true }).click();
  await expect(page.getByText('The retained execution belongs to another conversation or account.', { exact: true })).toBeVisible();
  await expect(page.locator('iframe')).toHaveCount(0);
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(f.errors, []);
});

for (const width of [1440, 390]) test(`Agent UI retains one host and account-specific creation, output, actions and drafts on ${width}px`, { timeout: 180000 }, async t => {
  const f = await fixture(t, width), page = f.page;
  await f.open('agents');
  const hosts = page.locator('section[aria-labelledby="hosts-heading"]').getByRole('link');
  await expect(hosts).toHaveCount(1);
  await expect.poll(() => page.evaluate(() => window.executionFixture.calls.filter(call => call.method === 'inventory.list' && call.params.kind === 'thread').length)).toBeGreaterThan(0);
  assert((await page.evaluate(() => window.executionFixture.calls.filter(call => call.method === 'inventory.list' && call.params.kind === 'thread'))).every(call => call.params.cursor === undefined), 'Initial navigation does not walk every page.');
  if (width < 768) await page.getByRole('button', { name: 'Account and navigation', exact: true }).click();
  await page.getByRole('button', { name: 'Next tasks', exact: true }).click();
  await expect.poll(() => page.evaluate(() => window.executionFixture.calls.some(call => call.method === 'inventory.list' && call.params.cursor === 'next-tasks'))).toBe(true);
  await page.getByRole('button', { name: 'Previous tasks', exact: true }).click();
  if (width < 768) await page.keyboard.press('Escape');
  await hosts.click();
  await expect(page.getByLabel('Provider and account', { exact: true })).toBeEnabled();
  await choose(page.getByLabel('Provider and account', { exact: true }), backendLabel);
  await page.getByLabel('First message', { exact: true }).fill('First message for the personal account');
  await choose(page.getByLabel('Project', { exact: true }), '/personal');
  await choose(page.getByLabel('Reasoning effort', { exact: true }), 'max');
  await page.getByLabel('Choose photos and files', { exact: true }).setInputFiles({ name: 'note.txt', mimeType: 'text/plain', buffer: Buffer.from('Small inert fixture') });
  await expect(page.getByRole('button', { name: 'Create task', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: 'Create task', exact: true }).click();
  await expect.poll(() => new URLSearchParams(page.url().split('?').at(-1)).get('account')).toBe('personal');
  await expect(page.getByLabel('Message', { exact: true })).toBeVisible();
  const launch = await page.evaluate(() => window.executionFixture.actions);
  assert.deepEqual(launch.map(call => call.method ?? 'stageFile'), ['stageFile', 'thread/start', 'turn/start']);
  for (const call of launch) assert.deepEqual(call.backend, claude);
  assert.deepEqual(launch[1].params, { cwd: '/personal', projectId: 'same-project', model: 'same-model', config: { model_reasoning_effort: 'max' }, permissions: ':workspace' });
  assert.equal(launch[2].params.threadId, 'same-thread');
  assert(launch[2].params.input[0].text.includes('/personal/note.txt'));
  await expect(page.getByText('Saved answer from claude personal', { exact: false })).toBeVisible();
  await expect(page.locator('img.ivy-embedded-image')).toHaveCount(1);
  await expect.poll(() => page.locator('img.ivy-embedded-image').evaluate(image => image.complete && image.naturalWidth)).toBe(1);
  await page.getByLabel('Message', { exact: true }).fill('Follow-up on this account');
  await page.getByRole('button', { name: 'Send message', exact: true }).click();
  await expect(page.getByLabel('Message', { exact: true })).toHaveValue('');
  await page.getByRole('button', { name: 'Task actions', exact: true }).click();
  await page.getByLabel('Task name', { exact: true }).fill('Renamed personal task');
  await page.getByRole('button', { name: 'Rename', exact: true }).click();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('heading', { name: 'Renamed personal task', exact: true })).toBeVisible();
  await page.evaluate(() => window.executionFixture.notify({ backendId: 'primary', provider: 'codex', accountId: 'work' }, 'thread/name/updated', { threadId: 'same-thread', threadName: 'Foreign title' }));
  await expect(page.getByRole('heading', { name: 'Renamed personal task', exact: true })).toBeVisible();
  await page.getByLabel('Message', { exact: true }).fill('Private retained draft');
  const personal = page.url().split('#')[1];
  await page.evaluate(() => { location.hash = '#/task?' + new URLSearchParams({ node: 'fixture-host.agent-manager', namespace: 'primary', backend: 'primary', provider: 'codex', account: 'work', id: 'same-thread' }); });
  await expect(page.getByText('Saved answer from codex work', { exact: false })).toBeVisible();
  await expect(page.getByLabel('Message', { exact: true })).toHaveValue('');
  assert.equal(await page.getByText('Saved answer from claude personal', { exact: false }).count(), 0);
  await page.evaluate(hash => { location.hash = hash; }, personal);
  await expect(page.getByLabel('Message', { exact: true })).toHaveValue('Private retained draft');
  const beforeLegacy = await page.evaluate(() => window.executionFixture.calls.length);
  await page.evaluate(() => { location.hash = '#/task?node=fixture-host.agent-manager&id=same-thread'; });
  await expect(page.getByText('Missing or wrong account selector: agent.status', { exact: false }).first()).toBeVisible();
  await expect(page.getByRole('button', { name: 'Send message', exact: true })).toBeDisabled();
  assert.equal(await page.getByText('Saved answer from', { exact: false }).count(), 0);
  const legacyCalls = await page.evaluate(offset => window.executionFixture.calls.slice(offset).filter(call => call.method === 'tools.call'), beforeLegacy);
  assert(!legacyCalls.some(call => call.params.qualifiedName.startsWith('codex.')), 'An unscoped historical route cannot read the replacement namespace.');
  const actions = await page.evaluate(() => window.executionFixture.actions);
  assert.equal(actions.length, 5);
  for (const action of actions) assert.deepEqual(action.backend, claude);
  const native = await page.evaluate(() => window.executionFixture.calls.filter(call => call.method === 'tools.call' && call.params.qualifiedName.includes('.fs/readFile')));
  assert(native.every(call => ['primary.fs/readFile', 'secondary.fs/readFile'].includes(call.params.qualifiedName)));
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(f.errors, []);
});

for (const width of [1440, 390]) test(`TaskBoard retains account selection, options and projects on ${width}px`, { timeout: 180000 }, async t => {
  const f = await fixture(t, width), page = f.page;
  await f.open('editor');
  const account = () => page.getByLabel('Provider and account', { exact: true });
  await expect(account()).toBeEnabled();
  const accounts = await options(account());
  assert.equal(accounts.length, 4);
  assert.equal(accounts.filter(label => label.includes('(primary)')).length, 2, 'Inherited and explicit account choices remain distinct.');
  assert.equal((await options(page.getByLabel('Host', { exact: true }))).length, 1, 'All accounts share one physical host choice.');
  await page.getByLabel('Title', { exact: true }).fill('Use the selected account');
  await choose(page.getByLabel('Project', { exact: true }), 'project:' + JSON.stringify(['same-project', '/work']));
  await choose(account(), backendLabel);
  await expect(page.getByLabel('Project', { exact: true })).toHaveText('No project');
  await expect(page.getByLabel('Model', { exact: true })).toHaveText('Shared model');
  await expect(page.getByRole('button', { name: 'Fast', exact: true })).toBeDisabled();
  await expect(page.getByRole('group', { name: 'Reasoning', exact: true }).getByRole('button', { name: 'High', exact: true })).toHaveCount(0);
  await choose(page.getByLabel('Model', { exact: true }), 'alternate-model');
  await choose(page.getByLabel('Model', { exact: true }), 'same-model');
  await page.getByRole('button', { name: 'Max', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Flexible', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: 'Standard', exact: true }).click();
  await choose(page.getByLabel('Project', { exact: true }), 'project:' + JSON.stringify(['same-project', '/personal']));
  await page.reload();
  await expect(account()).toHaveText(backendLabel);
  await expect(page.getByLabel('Project', { exact: true })).toHaveText('Personal project');
  await expect(page.getByRole('button', { name: 'Max', exact: true })).toHaveAttribute('data-state', 'on');
  await page.getByRole('button', { name: 'Create task', exact: true }).click();
  await expect.poll(() => page.evaluate(() => window.executionFixture.actions.length)).toBe(1);
  const created = await page.evaluate(() => window.executionFixture.actions[0]);
  assert.deepEqual(created.fields.nativeOptions, { backend: claude, model: 'same-model', reasoningEffort: 'max', serviceTier: null });
  assert.deepEqual(created.fields.workspaceRequirement, { kind: 'existing_project', projectId: 'same-project', path: '/personal', useWorktree: false });

  await f.open('settings');
  await expect(account()).toBeEnabled(); await choose(account(), backendLabel);
  await choose(page.getByLabel('Default model', { exact: true }), 'same-model');
  await choose(page.getByLabel('Reasoning', { exact: true }), 'max');
  await expect(page.getByLabel('Speed', { exact: true })).toHaveText('Standard');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect.poll(() => page.evaluate(() => window.executionFixture.actions.length)).toBe(1);
  const configured = await page.evaluate(() => window.executionFixture.actions[0]);
  assert.deepEqual(configured.value.defaults.nativeOptions, { backend: claude, model: 'same-model', reasoningEffort: 'max', serviceTier: 'standard' });

  await f.open('detail');
  await expect(page.getByText(backendLabel, { exact: true })).toBeVisible();
  await expect(page.getByText('Personal project', { exact: true })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Execution', exact: true }).getByText('Host default (default)', { exact: true })).toBeVisible();
  if (width >= 768) await expect(page.getByRole('button', { name: 'Model: Host default', exact: true })).toBeVisible();
  assert.equal(await page.getByText('codex-only', { exact: false }).count(), 0, 'Another account cannot supply inherited model defaults.');
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);

  await f.open('placed');
  await expect(account()).toBeDisabled();
  await expect(page.getByText('This conversation retains its account.', { exact: true })).toBeVisible();
  await expect(page.getByLabel('Model', { exact: true })).toHaveText('Shared model');
  await expect(page.getByRole('button', { name: 'Max', exact: true })).toHaveAttribute('data-state', 'on');
  await expect(page.getByRole('button', { name: 'Flexible', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Standard', exact: true })).toHaveAttribute('data-state', 'on');
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);

  await f.open('directory');
  await expect.poll(() => page.evaluate(() => window.executionFixture.directories.length)).toBe(1);
  await page.evaluate(() => window.executionFixture.changeDirectoryBackend());
  await expect(page.getByLabel('Folder path')).toHaveValue('/personal');
  await page.evaluate(() => window.executionFixture.heldDirectory());
  await expect(page.getByLabel('Folder path')).toHaveValue('/personal');
  await page.getByRole('button', { name: 'Use this folder', exact: true }).click();
  await expect.poll(() => page.evaluate(() => window.executionFixture.selectedDirectory)).toBe('/personal');
  assert.deepEqual((await page.evaluate(() => window.executionFixture.directories)).map(call => call.backend), [
    { backendId: 'primary', provider: 'codex', accountId: 'work' }, claude,
  ]);
  assert.deepEqual(f.errors, []);
});

for (const width of [1440, 390]) test(`Native approval preserves the original account and rejects a foreign current request on ${width}px`, { timeout: 120000 }, async t => {
  const f = await fixture(t, width), page = f.page;
  await f.open('input-history');
  await expect(page.getByRole('heading', { name: 'Native approval', exact: true })).toBeVisible();
  await expect(page.getByText('This request retains its original service or account binding and is not answerable through the current route.', { exact: true })).toBeVisible();
  assert.equal(await page.getByRole('button', { name: 'Send response', exact: true }).count(), 0);
  const historicalCalls = await page.evaluate(() => window.executionFixture.calls.filter(call => call.method === 'tools.call'));
  assert.deepEqual(historicalCalls.map(call => call.params.qualifiedName), ['agent.inputs']);
  await f.open('input');
  await expect(page.getByRole('heading', { name: 'Native approval', exact: true })).toBeVisible();
  await choose(page.getByLabel('Decision', { exact: true }), 'decline');
  await page.evaluate(() => { window.executionFixture.wrongInput = true; });
  await page.getByRole('button', { name: 'Send response', exact: true }).click();
  await expect(page.getByText('This native request has been answered, expired or belongs to another account. The response draft is retained.', { exact: true })).toBeVisible();
  assert.equal(await page.evaluate(() => window.executionFixture.actions.length), 0);
  await page.evaluate(() => { window.executionFixture.wrongInput = false; });
  await page.getByRole('button', { name: 'Send response', exact: true }).click();
  await expect.poll(() => page.evaluate(() => window.executionFixture.actions.length)).toBe(1);
  const action = await page.evaluate(() => window.executionFixture.actions[0]);
  assert.deepEqual(action.backend, claude);
  assert.deepEqual(action.identity, { serviceNodeId: 'fixture-host.agent-manager', backend: claude, epoch: 'secondary-epoch', requestId: 7 });
  assert.deepEqual(action.reply, { result: { decision: 'decline' } });
  await expect(page.getByRole('heading', { name: 'Native approval', exact: true })).toHaveCount(0);
  const calls = await page.evaluate(() => window.executionFixture.calls.filter(call => call.method === 'tools.call' && ['agent.inputs', 'agent.inputDefinition', 'agent.status', 'agent.answer'].includes(call.params.qualifiedName)));
  assert(calls.length >= 5);
  for (const call of calls) assert.deepEqual(call.params.arguments.backend, claude);
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  assert.deepEqual(f.errors, []);
});
