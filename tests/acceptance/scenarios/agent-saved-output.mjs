import assert from 'node:assert/strict';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { parseArgs } from 'node:util';
import { chromium, expect } from '@playwright/test';
import { HiveClient, discover, callBound } from '../../../dist/packages/sdk/src/client.js';
import { publishUi } from '../../../dist/packages/cli/src/publish-ui.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';
import { digest, hashJson } from '../../../dist/packages/contracts/src/canonical.js';
import { validateAgent } from '../../../dist/packages/contracts/src/validation.js';

// Reads an existing completed acceptance turn. The only publication is this independently versioned
// AgentUI release; native task creation, continuation, attachment, answers and interruption are refused.
const { values } = parseArgs({ options: Object.fromEntries(['config', 'owner', 'thread', 'turn', 'marker', 'epoch', 'evidence', 'operation-id'].map(key => [key, { type: 'string' }])) });
for (const key of ['config', 'owner', 'thread', 'turn', 'marker', 'epoch', 'evidence', 'operation-id']) assert.ok(values[key], 'Missing --' + key);
const config = JSON.parse(await readFile(resolve(values.config), 'utf8')); assert.ok(config.hostId.endsWith('-acceptance'));
const hive = config.instances.find(instance => instance.componentId === 'hive'); assert.ok(hive?.settings.credentials[0]?.token);
const client = new HiveClient(config.publicBaseUrl, { credential: hive.settings.credentials[0].token });
const root = resolve(values.evidence); await mkdir(root, { recursive: true });
const reportPath = join(root, 'report.json'); await writeFile(reportPath, '{}\n', { flag: 'wx' });
const report = { schemaVersion: 1, hostId: config.hostId, owner: values.owner, threadId: values.thread, turnId: values.turn,
  startedAt: new Date().toISOString(), nativeMutationDispatched: false, phase: 'starting', pageErrors: [], outsideRequests: [], nativeReadMethods: [] };
const save = async () => atomicJson(reportPath, report);
const status = async () => callBound(client, await discover(client, 'agent.status', { serviceNodeId: values.owner }), {});
const browser = await chromium.launch({ ...(process.platform === 'win32' ? { channel: 'msedge' } : {}), headless: true });
try {
  report.before = await status(); assert.equal(report.before.state, 'ready'); assert.equal(report.before.epoch, values.epoch);
  const directory = resolve('dist/apps/agent-ui'), definition = JSON.parse(await readFile(join(directory, 'ivy-ui.json'), 'utf8'));
  let current = null; try { current = (await client.request('uis.get', { uiId: 'agent-ui' })).currentReleaseId; } catch (error) { if (error.code !== 'not_found') throw error; }
  report.release = await publishUi(client, { directory, definition, expectedReleaseId: current, mutationId: values['operation-id'] }); await save();
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, acceptDownloads: true }); context.setDefaultTimeout(35000);
  const readOnly = new Set(['agent.status', 'agent.read', 'agent.inputs', 'agent.inputDefinition', 'agent.notifications',
    'codex.thread/read', 'codex.thread/turns/list', 'codex.thread/items/list', 'codex.model/list',
    'codex.collaborationMode/list', 'codex.account/rateLimits/read']);
  await context.route('**/*', route => {
    if (new URL(route.request().url()).origin !== new URL(config.publicBaseUrl).origin) { report.outsideRequests.push(route.request().url()); return route.abort(); }
    if (route.request().url().endsWith('/api/v1/rpc') && route.request().method() === 'POST') {
      const request = route.request().postDataJSON();
      if (request.method === 'tools.call') {
        const name = request.params.qualifiedName;
        if (!readOnly.has(name) || name === 'agent.read' && request.params.arguments.method !== 'thread/turns/list') {
          report.rejectedNativeAction = name; return route.abort();
        }
        if (!report.nativeReadMethods.includes(name)) report.nativeReadMethods.push(name);
      }
    }
    return route.continue();
  });
  const page = await context.newPage(); page.on('pageerror', error => report.pageErrors.push(error.message));
  await page.goto(config.publicBaseUrl + '/'); await page.getByLabel('Hive credential').fill(hive.settings.credentials[0].token);
  const login = page.waitForResponse(response => response.request().method() === 'POST' && response.url().endsWith('/login'));
  await page.getByRole('button', { name: 'Sign in', exact: true }).click(); assert.equal((await login).status(), 303);
  await page.waitForURL(url => url.pathname === new URL(config.publicBaseUrl + '/').pathname);
  const url = config.publicBaseUrl + '/ui/agent-ui/#/task?node=' + encodeURIComponent(values.owner) + '&id=' + encodeURIComponent(values.thread) + '&turn=' + encodeURIComponent(values.turn);
  await page.goto(url);
  const output = page.getByRole('region', { name: 'Saved native output', exact: true });
  await expect(output).toContainText(values.marker, { timeout: 35000 });
  await page.getByRole('button', { name: 'Task actions', exact: true }).click();
  const pendingDownload = page.waitForEvent('download'); await page.getByRole('menuitem', { name: 'Download complete turn', exact: true }).click();
  const download = await pendingDownload, bytes = await readFile(await download.path()), observed = JSON.parse(bytes.toString('utf8'));
  validateAgent('ReadObservation', observed);
  assert.equal(observed.serviceNodeId, values.owner); assert.equal(observed.epoch, values.epoch); assert.equal(observed.method, 'thread/turns/list');
  assert.equal(observed.params.threadId, values.thread); assert.equal(observed.params.itemsView, 'full'); assert.equal(observed.params.limit, 1);
  assert.equal(observed.requestHash, hashJson({ method: observed.method, params: observed.params }));
  assert.equal(observed.reply.result.data.length, 1); const turn = observed.reply.result.data[0];
  assert.equal(turn.id, values.turn); assert.equal(turn.status, 'completed'); assert.equal(turn.itemsView, 'full'); assert.ok(JSON.stringify(turn.items).includes(values.marker));
  await writeFile(join(root, 'complete-native-turn.json'), bytes, { flag: 'wx' });
  report.download = { filename: download.suggestedFilename(), bytes: bytes.length, hash: digest(bytes), itemCount: turn.items.length, observationId: observed.observationId };
  await page.reload();
  await expect(output).toContainText(values.marker, { timeout: 35000 });
  report.savedResults = (await page.locator('section[aria-labelledby="results-heading"]').innerText()).slice(0,16000);
  await page.screenshot({ path: join(root, 'saved-output-after.png'), fullPage: true });
  report.browser = browser.version(); report.after = await status();
  for (const key of ['serviceNodeId', 'state', 'epoch', 'nativeVersion', 'nativeExecutableHash', 'catalogHash']) assert.equal(report.after[key], report.before[key]);
  assert.equal(report.rejectedNativeAction, undefined); assert.deepEqual(report.pageErrors, []); assert.deepEqual(report.outsideRequests, []);
  report.phase = 'passed';
} catch (error) { report.phase = 'failed'; report.failure = { code: error.code ?? error.name, message: error.message }; process.exitCode = 1; }
finally { report.completedAt = new Date().toISOString(); await save(); await browser.close(); }
console.log(JSON.stringify({ reportPath, phase: report.phase, releaseId: report.release?.releaseId, download: report.download, failure: report.failure }));
