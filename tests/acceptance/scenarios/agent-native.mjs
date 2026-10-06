import assert from 'node:assert/strict';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { parseArgs } from 'node:util';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium, expect as browserExpect } from '@playwright/test';
import { HiveClient, discover, callBound } from '../../../dist/packages/sdk/src/client.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';
import { hashJson } from '../../../dist/packages/contracts/src/canonical.js';
import { nativeModesFrom, nativeModePayload } from '../../../dist/packages/ui-client/src/native-modes.js';
import { modelsFrom } from '../../../dist/packages/ui-client/src/native.js';
import { choose } from '../../web-e2e/fixtures/select.mjs';

// Actual browser/native acceptance on a new or explicitly retained isolated thread. Uses published AgentUI;
// no project/files/configuration, external messages or calls are created by the native prompts.
const expect = browserExpect.configure({ timeout: 35000 });
const { values } = parseArgs({ options: Object.fromEntries(['config', 'owner', 'project', 'evidence', 'prior-report', 'working-mode', 'preserved-output', 'observed-epoch'].map(key => [key, { type: 'string' }])) });
for (const key of ['config', 'owner', 'project', 'evidence']) assert.ok(values[key], 'Missing --' + key);
const config = JSON.parse(await readFile(resolve(values.config), 'utf8')); assert.ok(config.hostId.endsWith('-acceptance'));
const hive = config.instances.find(instance => instance.componentId === 'hive'), token = hive?.settings.credentials[0]?.token; assert.ok(token);
const client = new HiveClient(config.publicBaseUrl, { credential: token }), root = resolve(values.evidence);
await mkdir(root, { recursive: true }); const reportPath = join(root, 'report.json'); await writeFile(reportPath, '{}\n', { flag: 'wx' });
const prior = values['prior-report'] ? JSON.parse(await readFile(resolve(values['prior-report']), 'utf8')) : null;
const baselineTurns = prior?.calls.filter(call => call.name === 'codex.turn/start').length ?? 0;
const baselineResumes = prior?.calls.filter(call => call.name === 'codex.thread/resume').length ?? 0;
if (prior) {
  assert.equal(prior.phase, 'failed'); assert.equal(prior.owner, values.owner); assert.equal(prior.project, values.project); assert.ok(prior.threadId);
  assert.equal(prior.calls.filter(call => call.name === 'codex.thread/start').length, 1);
  assert.ok(prior.calls.every(call => ['codex.thread/start', 'codex.turn/start', 'codex.thread/resume'].includes(call.name)));
  assert.ok(baselineTurns <= 2 && baselineResumes <= 1);
  if (baselineTurns) { assert.equal(prior.failure.message, 'Acceptance deadline: first native question'); assert.ok(values['preserved-output']); assert.ok(values['working-mode']); }
}
const id = prior?.id ?? randomUUID(), marker = 'IVY_AGENT_UI_ACTUAL_' + id;
const firstPrompt = 'This is an isolated AgentUI acceptance task. Use request_user_input now to ask exactly one synthetic choice: Alpha or Beta. Wait for the answer. Then acknowledge the chosen label and include the marker ' + marker + '. Do not use any other tool, inspect or modify files, send messages, browse, or make any external change.';
const secondPrompt = 'Continue this same isolated AgentUI acceptance task. Use request_user_input to ask one new synthetic choice and wait for the answer. Do not finish before an answer; this turn will be interrupted by the test. Do not use any other tool, inspect or modify files, send messages, browse, or make any external change.';
const report = { schemaVersion: 1, id, hostId: config.hostId, owner: values.owner, project: values.project, marker,
  startedAt: new Date().toISOString(), phase: 'starting', calls: structuredClone(prior?.calls ?? []), pageErrors: [], outsideRequests: [], pumpErrors: [],
  ...(prior ? { threadId: prior.threadId, priorReportHash: hashJson(prior), originalCreateOperation: prior.calls[0].operationId } : {}) };
const save = async (phase, details = {}) => { Object.assign(report, details, { phase, observedAt: new Date().toISOString() }); assert.equal(JSON.stringify(report).includes(token), false); await atomicJson(reportPath, report); console.log(JSON.stringify({ phase, threadId: report.threadId, reportPath })); };
const call = async (name, args, operationId) => callBound(client, await discover(client, name, { serviceNodeId: values.owner }), args, operationId, { timeoutMs: 35000 });
const until = async (check, label, timeout = 180000) => { const end = Date.now() + timeout; while (Date.now() < end) { const value = await check(); if (value) return value; await delay(1000); } throw new Error('Acceptance deadline: ' + label); };
const readThread = async () => (await call('codex.thread/read', { threadId: report.threadId, includeTurns: true }, id + '-read-' + randomUUID())).thread;
let browser, page, pump, stopped = false;
try {
  report.before = await call('agent.status', {}); assert.equal(report.before.state, 'ready');
  if (prior) {
    assert.equal(report.before.epoch, values['observed-epoch'] ?? prior.before.epoch);
    for (const key of ['hostId', 'serviceNodeId', 'nativeVersion', 'nativeExecutableHash', 'catalogHash']) assert.equal(report.before[key], prior.before[key]);
    const read = await call('agent.read', { nativeVersion: report.before.nativeVersion, method: 'thread/turns/list', params: { threadId: prior.threadId, limit: 5, sortDirection: 'desc', itemsView: 'full' } });
    assert.equal(read.epoch, report.before.epoch);
    if (baselineTurns) {
      const preserved = JSON.parse(await readFile(resolve(values['preserved-output']), 'utf8'));
      assert.equal(preserved.epoch, prior.preservedTurn?.originalEpoch ?? prior.before.epoch); assert.equal(preserved.params.threadId, prior.threadId);
      assert.equal(read.reply.result.data.length, baselineTurns); assert.equal(read.reply.result.nextCursor, null);
      const turn = read.reply.result.data.find(turn => turn.id === preserved.reply.result.data[0].id); assert.equal(turn.status, 'completed');
      assert.deepEqual(turn, preserved.reply.result.data[0]);
      report.preservedTurn = { id: turn.id, hash: hashJson(turn), originalEpoch: preserved.epoch, observedEpoch: read.epoch };
      report.retainedFailedTurns = read.reply.result.data.filter(value => value.id !== turn.id).map(value => {
        assert.equal(value.status, 'failed'); assert.ok(value.error.message.includes('401 Unauthorized: Missing bearer or basic authentication'));
        assert.equal(value.items.length, 1); assert.equal(value.items[0].type, 'userMessage'); assert.equal(value.items[0].content[0].text, firstPrompt);
        return { id: value.id, hash: hashJson(value), error: value.error };
      });
    } else assert.deepEqual(read.reply.error, { code: -32600, message: `thread ${prior.threadId} is not materialized yet; thread/turns/list is unavailable before first user message` });
    const state = (await call('codex.thread/read', { threadId: prior.threadId, includeTurns: false }, id + '-prior-state')).thread;
    assert.ok(['idle', 'systemError', 'notLoaded'].includes(state.status.type)); report.needsAttach = state.canAcceptDirectInput !== true;
    assert.ok(!(await call('agent.inputs', {})).items.some(input => input.threadId === prior.threadId && input.state === 'pending'));
  }
  if (values['working-mode']) {
    const binding = await discover(client, 'codex.turn/start', { serviceNodeId: values.owner });
    const modes = nativeModesFrom(binding, await call('codex.collaborationMode/list', {}, id + '-modes'));
    const rawModels = await call('codex.model/list', { limit: 100 }, id + '-models'); assert.equal(rawModels.nextCursor, null);
    const chosen = rawModels.data.find(model => model.isDefault && !model.hidden); assert.ok(chosen);
    const model = modelsFrom(rawModels).find(model => model.id === chosen.id), mode = modes.find(mode => mode.mode === values['working-mode']); assert.ok(model && mode);
    const effort = model.efforts.includes('low') ? 'low' : model.defaultEffort;
    const payload = nativeModePayload(binding, mode, model, effort); assert.ok(payload);
    report.workingMode = { mode: mode.mode, model: model.model, effort, payload, definitionHash: binding.definitionHash };
  }
  const projects = await call('agent.projects', {}); assert.ok(projects.projects.some(project => project.paths.includes(values.project)));
  const ui = await client.request('uis.get', { uiId: 'agent-ui' }); assert.ok(ui.currentReleaseId); report.releaseId = ui.currentReleaseId;
  browser = await chromium.launch({ ...(process.platform === 'win32' ? { channel: 'msedge' } : {}), headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, acceptDownloads: true }); context.setDefaultTimeout(35000);
  const controlNames = new Set(['codex.thread/start', 'codex.thread/resume', 'codex.turn/start', 'codex.turn/interrupt', 'agent.answer']);
  await context.route('**/*', route => {
    if (new URL(route.request().url()).origin !== new URL(config.publicBaseUrl).origin) { report.outsideRequests.push(route.request().url()); return route.abort(); }
    if (route.request().method() === 'POST' && route.request().url().endsWith('/api/v1/rpc')) {
      const request = route.request().postDataJSON();
      if (request.method === 'tools.call' && controlNames.has(request.params.qualifiedName)) {
        const { qualifiedName: name, arguments: args, operationId } = request.params;
        const allowed = name === 'codex.thread/start' ? !report.threadId && args.cwd === values.project && Object.keys(args).length === 1
          : name === 'codex.thread/resume' ? report.needsAttach && args.threadId === report.threadId && Object.keys(args).length === 1 && report.calls.filter(call => call.name === name).length === baselineResumes
          : name === 'codex.turn/start' ? args.threadId === report.threadId && args.input.length === 1 && [firstPrompt, secondPrompt].includes(args.input[0].text) && Object.keys(args).length === (report.workingMode ? 3 : 2) && (!report.workingMode || hashJson(args.collaborationMode) === hashJson(report.workingMode.payload))
          : name === 'codex.turn/interrupt' ? args.threadId === report.threadId && args.turnId === report.secondQuestion?.turnId
          : report.firstQuestion && hashJson(args.identity) === hashJson(report.firstQuestion.identity);
        if (!allowed) { report.refusedControl = name; return route.abort(); }
        report.calls.push({ name, operationId, argumentsHash: hashJson(args), at: new Date().toISOString() });
      }
    }
    return route.continue();
  });
  page = await context.newPage(); page.on('pageerror', error => report.pageErrors.push(error.message));
  await page.goto(config.publicBaseUrl + '/'); await page.getByLabel('Hive credential').fill(token);
  const login = page.waitForResponse(response => response.request().method() === 'POST' && response.url().endsWith('/login'));
  await page.getByRole('button', { name: 'Sign in', exact: true }).click(); assert.equal((await login).status(), 303);
  await page.waitForURL(url => url.pathname === new URL(config.publicBaseUrl + '/').pathname);
  const entry = config.publicBaseUrl + '/ui/agent-ui/';
  if (prior) await page.goto(entry + '#/task?node=' + encodeURIComponent(values.owner) + '&id=' + encodeURIComponent(report.threadId));
  else {
    await page.goto(entry + '#/host?node=' + encodeURIComponent(values.owner));
    await choose(page.getByLabel('Project', { exact: true }), values.project);
    await page.getByRole('button', { name: 'Create task', exact: true }).click();
    await page.getByRole('link', { name: 'Open created task', exact: true }).click();
    report.threadId = new URLSearchParams(page.url().split('?').at(-1)).get('id'); assert.ok(report.threadId);
  }
  if (report.needsAttach) {
    const direct = page.getByText('Direct input confirmed on this native connection', { exact: true });
    const attach = page.getByRole('button', { name: 'Attach historical task', exact: true });
    await expect.poll(async () => await direct.isVisible() || await attach.isEnabled().catch(() => false)).toBe(true);
    if (await direct.isVisible()) report.needsAttach = false;
    else await attach.click();
  }
  await expect(page.getByText('Direct input confirmed on this native connection', { exact: true })).toBeVisible();
  if (report.workingMode) {
    await page.getByLabel('Message', { exact: true }).fill(firstPrompt);
    await choose(page.getByLabel('Working mode', { exact: true }), report.workingMode.mode);
    await expect(page.getByRole('button', { name: 'Send message', exact: true })).toBeDisabled();
    await choose(page.getByLabel('Model', { exact: true }), report.workingMode.model);
    await choose(page.getByLabel('Reasoning effort', { exact: true }), report.workingMode.effort);
    await page.reload();
    for (const [label, value] of [['Working mode', report.workingMode.mode], ['Model', report.workingMode.model], ['Reasoning effort', report.workingMode.effort], ['Message', firstPrompt]]) await expect(page.getByLabel(label, { exact: true })).toHaveValue(value);
  }
  await save('thread_created');
  pump = (async () => {
    while (!stopped) {
      try {
        for (const input of (await call('agent.inputs', {})).items.filter(input => input.state === 'pending' && input.method === 'currentTime/read')) {
          const operationId = id + '-clock-' + input.identity.epoch + '-' + input.identity.requestId;
          await call('agent.answer', { operationId, identity: input.identity, reply: { result: { currentTimeAt: Math.floor(Date.now() / 1000) } } }, operationId);
        }
      } catch (error) { if (report.pumpErrors.length < 100) report.pumpErrors.push({ code: error.code ?? error.name, at: new Date().toISOString() }); }
      await delay(1000);
    }
  })();
  await page.getByLabel('Message', { exact: true }).fill(firstPrompt); await page.getByRole('button', { name: 'Send message', exact: true }).click();
  const question = await until(async () => (await call('agent.inputs', {})).items.find(input => input.threadId === report.threadId && input.state === 'pending' && input.method === 'item/tool/requestUserInput'), 'first native question');
  report.firstQuestion = { identity: question.identity, turnId: question.turnId }; assert.equal(question.identity.epoch, report.before.epoch);
  await page.getByRole('button', { name: 'Refresh requests', exact: true }).click();
  const choice = question.params.questions[0].options.find(option => /^Alpha(?:\s|$)/.test(option.label)); assert.ok(choice);
  await page.getByRole('button', { name: choice.label, exact: true }).click();
  await page.reload(); await expect(page.getByLabel(question.params.questions[0].question, { exact: true })).toHaveValue(choice.label);
  await page.screenshot({ path: join(root, 'native-question-reloaded.png'), fullPage: true }); await save('question_reloaded');
  await page.getByRole('button', { name: 'Send response', exact: true }).click();
  const completed = await until(async () => { const turn = (await readThread()).turns.find(turn => turn.id === question.turnId); return turn?.status === 'completed' ? turn : null; }, 'completed first turn');
  assert.ok(completed.items.some(item => item.type === 'agentMessage' && item.text.includes('Alpha') && item.text.includes(marker)));
  report.firstTurnHash = hashJson(completed); await page.getByRole('button', { name: 'Refresh', exact: true }).click();
  await page.goto(entry + '#/task?node=' + encodeURIComponent(values.owner) + '&id=' + encodeURIComponent(report.threadId) + '&turn=' + encodeURIComponent(question.turnId));
  await expect(page.getByRole('region', { name: 'Saved native output', exact: true })).toContainText('Alpha', { timeout: 35000 });
  await page.getByRole('button', { name: 'Task actions', exact: true }).click();
  const downloading = page.waitForEvent('download'); await page.getByRole('menuitem', { name: 'Download complete turn', exact: true }).click();
  await (await downloading).saveAs(join(root, 'first-complete-turn.json'));
  const observed = JSON.parse(await readFile(join(root, 'first-complete-turn.json'), 'utf8'));
  assert.equal(observed.reply.result.data[0].id, question.turnId); assert.ok(JSON.stringify(observed.reply.result.data[0].items).includes(marker));
  await save('first_result_verified');
  await page.getByRole('link', { name: 'Show the complete conversation', exact: true }).click();
  await page.getByLabel('Message', { exact: true }).fill(secondPrompt); await page.getByRole('button', { name: 'Send message', exact: true }).click();
  const nextQuestion = await until(async () => (await call('agent.inputs', {})).items.find(input => input.threadId === report.threadId && input.turnId !== question.turnId && input.state === 'pending' && input.method === 'item/tool/requestUserInput'), 'continuation native question');
  report.secondQuestion = { identity: nextQuestion.identity, turnId: nextQuestion.turnId }; await save('continuation_waiting');
  await page.getByRole('button', { name: 'Refresh', exact: true }).click(); await page.getByRole('button', { name: 'Request interruption', exact: true }).click();
  const interrupted = await until(async () => { const turn = (await readThread()).turns.find(turn => turn.id === nextQuestion.turnId); return turn?.status === 'interrupted' ? turn : null; }, 'native confirmed interruption');
  report.interruptedTurn = { id: interrupted.id, status: interrupted.status };
  assert.equal(hashJson((await readThread()).turns.find(turn => turn.id === question.turnId)), report.firstTurnHash);
  await page.reload(); await expect(page.getByRole('region', { name: 'Saved native output', exact: true })).toContainText('Alpha', { timeout: 35000 });
  await expect(page.getByRole('button', { name: 'Request interruption', exact: true })).toHaveCount(0);
  await page.screenshot({ path: join(root, 'native-continuation-interrupted.png'), fullPage: true });
  report.after = await call('agent.status', {}); assert.equal(report.after.epoch, report.before.epoch);
  if (report.preservedTurn) {
    const read = await call('agent.read', { nativeVersion: report.before.nativeVersion, method: 'thread/turns/list', params: { threadId: report.threadId, limit: 5, sortDirection: 'desc', itemsView: 'full' } });
    assert.equal(hashJson(read.reply.result.data.find(turn => turn.id === report.preservedTurn.id)), report.preservedTurn.hash);
    for (const failed of report.retainedFailedTurns) assert.equal(hashJson(read.reply.result.data.find(turn => turn.id === failed.id)), failed.hash);
  }
  for (const [name, count] of [['codex.thread/start', 1], ['codex.thread/resume', baselineResumes + (report.needsAttach ? 1 : 0)], ['codex.turn/start', baselineTurns + 2], ['agent.answer', 1], ['codex.turn/interrupt', 1]]) assert.equal(report.calls.filter(call => call.name === name).length, count);
  assert.equal(report.refusedControl, undefined); assert.deepEqual(report.pageErrors, []); assert.deepEqual(report.outsideRequests, []);
  await save('passed', { browser: browser.version(), finishedAt: new Date().toISOString() });
} catch (error) {
  await page?.screenshot({ path: join(root, 'failure.png'), fullPage: true }).catch(() => undefined);
  await save('failed', { failure: { code: error.code ?? error.name, message: error.message }, retainedNativeThread: report.threadId ?? null }); process.exitCode = 1;
} finally { stopped = true; await pump; await browser?.close(); }
