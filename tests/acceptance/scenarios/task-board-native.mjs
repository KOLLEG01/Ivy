import { nativeReviewFlow } from '../support/native-review-flow.mjs';
import { validateQualityCase, qualityPrompt } from '../support/native-quality.mjs';
import { collectTokenEvidence } from '../support/native-token-evidence.mjs';
import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { parseArgs } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium, expect } from '@playwright/test';
import { HiveClient, discover, callBound } from '../../../dist/packages/sdk/src/client.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';
import { digest, hashJson } from '../../../dist/packages/contracts/src/canonical.js';
import { validateTaskBoard } from '../../../dist/packages/contracts/src/validation.js';
import { nativeCatalogPath } from '../../../dist/packages/contracts/src/checked-native-contract.js';
import { publishUi } from '../../../dist/packages/cli/src/publish-ui.js';

// Actual installed services and native model. The caller restarts only TaskBoard when the saved
// report requests it, then writes control.json. Native input stays on its original live owner.
const { values } = parseArgs({ options: Object.fromEntries(['config', 'host-id', 'owner', 'task-board-node', 'project-path', 'auth-file', 'evidence', 'model', 'effort', 'quality-case'].map(key => [key, { type: 'string' }])) });
const qualityCase = values['quality-case'] ? validateQualityCase(JSON.parse(await readFile(values['quality-case'], 'utf8'))) : null;
for (const key of ['config', 'host-id', 'owner', 'task-board-node', 'project-path', 'auth-file', 'evidence', 'model', 'effort']) assert.ok(values[key], 'Missing --' + key);
const config = JSON.parse(await readFile(resolve(values.config), 'utf8'));
assert.equal(config.hostId, values['host-id']); assert.ok(config.hostId.endsWith('-acceptance'));
assert.ok(new URL(config.publicBaseUrl).protocol === 'https:' || new URL(config.publicBaseUrl).hostname === '127.0.0.1', 'Select the configured acceptance installation through verified HTTPS or explicit loopback.');
const hive = config.instances.find(i => i.componentId === 'hive'); assert.ok(hive?.settings.credentials[0]?.token);
const client = new HiveClient(config.publicBaseUrl, { credential: hive.settings.credentials[0].token });
const root = resolve(values.evidence), reportPath = join(root, 'report.json'), controlPath = join(root, 'control.json');
await mkdir(root, { recursive: true }); await writeFile(reportPath, '{}\n', { flag: 'wx' });
const id = randomUUID(), owner = values.owner, taskBoard = values['task-board-node'];
const report = { schemaVersion: 1, id, hostId: config.hostId, owner, taskBoard, startedAt: new Date().toISOString(), phase: 'starting', operations: [] };
const save = async (phase, details = {}) => { Object.assign(report, details, { phase, observedAt: new Date().toISOString() }); await atomicJson(reportPath, report); console.log(JSON.stringify({ phase, reportPath, taskId: report.taskId })); };
const call = async (node, name, args, operationId) => callBound(client, await discover(client, name, { serviceNodeId: node }), args, operationId, { timeoutMs: 65000 });
const native = async (method, args, label) => {
  const operationId = id + '-' + label; report.operations.push({ method, operationId }); await atomicJson(reportPath, report);
  return call(owner, 'codex.' + method, args, operationId);
};
let workspace;
const action = async (request, label) => {
  const operationId = id + '-' + label;
  const args = { ...request, operationId, expectedWorkspace: { principalId: workspace.principalId, rootObjectId: workspace.rootObjectId, callerPrincipalId: workspace.callerPrincipalId } };
  report.operations.push({ method: 'task-board.' + request.action, operationId, arguments: args, phase: 'intent' }); await atomicJson(reportPath, report);
  validateTaskBoard('ActionInput', args);
  return call(taskBoard, 'task-board.' + request.action, args, operationId);
};
const task = async () => (await client.request('objects.read', { objectId: report.taskId })).content.value;
const until = async (check, label, timeout = 240000) => {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const result = await check(); if (result) return result; await delay(1000); }
  throw new Error('Acceptance deadline: ' + label);
};
let browser, page, stopped = false, pump; const pageErrors = [], outsideRequests = [], pumpErrors = [];
const authBefore = digest(await readFile(values['auth-file']));
try {
  const before = await client.request('system.status', {}); assert.equal(before.ready, true);
  workspace = await call(taskBoard, 'task-board.workspace', {}); assert.equal(workspace.role, 'user'); assert.equal(workspace.recovered, true);
  const status = await call(owner, 'agent.status', {}); assert.equal(status.state, 'ready');
  const pending = await call(owner, 'agent.inputs', {}); assert.equal(pending.items.filter(i => ['pending', 'answering'].includes(i.state)).length, 0);
  const projectData = await call(owner, 'agent.projects', {});
  const project = projectData.projects.find(p => p.paths.includes(values['project-path'])); assert.ok(project, 'Select an actual project on the explicit native owner.');
  const repository = fileURLToPath(new URL('../../../', import.meta.url));
  const catalog = JSON.parse(await readFile(nativeCatalogPath(repository, status.nativeVersion), 'utf8'));
  assert.equal(hashJson(catalog), status.catalogHash, 'The installed native catalog must match the plan source.');
  await save('authenticating-exact-native-owner', { before, nativeStatus: status, project: { id: project.nativeId, path: values['project-path'] }, workspace });
  const auth = JSON.parse(await readFile(values['auth-file'], 'utf8')).tokens;
  assert.ok(auth.access_token && auth.account_id);
  await native('account/login/start', { type: 'chatgptAuthTokens', accessToken: auth.access_token, chatgptAccountId: auth.account_id, chatgptPlanType: null }, 'login');
  const models = await native('model/list', {}, 'models'); const model = models.data.find(m => m.model === values.model && !m.hidden); assert.ok(model, 'Requested model must be available; no fallback.');
  const effort = values.effort;
  assert.ok(model.supportedReasoningEfforts.some(e => e.reasoningEffort === effort), 'Requested reasoning effort must be supported.');
  const bindings = await Promise.all(['thread/start', 'thread/resume', 'turn/start', 'turn/interrupt'].map(method => discover(client, 'codex.' + method, { serviceNodeId: owner })));
  const marker = 'IVY_TASK_BOARD_ACTUAL_' + id;
  const prompt = qualityCase ? qualityPrompt(qualityCase, marker) : 'This is an isolated native protocol acceptance task. Use request_user_input now to ask exactly one synthetic choice: Alpha or Beta. Wait for the answer. Then acknowledge the chosen label and include the marker ' + marker + '. Do not use any other tool, inspect or modify files, send messages, browse, or make any external change.';
  const permissions = { permissions: ':read-only' };
  const plan = { nativeVersion: status.nativeVersion, catalogSourceHash: catalog.sourceHash,
    definitions: Object.fromEntries(['threadStart', 'threadResume', 'turnStart', 'turnInterrupt'].map((key, index) => [key, bindings[index].definitionHash])),
    threadStart: { cwd: values['project-path'], model: model.model, allowProviderModelFallback: false, approvalPolicy: 'never', ...permissions }, threadResume: { cwd: values['project-path'], model: model.model, approvalPolicy: 'never', ...permissions },
    turnStart: { collaborationMode: { mode: 'plan', settings: { model: model.model, reasoning_effort: effort, developer_instructions: null } }, input: [{ type: 'text', text: prompt, text_elements: [] }] } };
  await save('validating-native-plan', { planDraft: plan, model: model.model, effort, qualityCase, qualityCaseHash: qualityCase ? hashJson(qualityCase) : null });
  validateTaskBoard('NativePlanDraft', plan);
  const savedPlan = await action({ action: 'savePlan', plan }, 'plan'); assert.ok(savedPlan.plan);
  const title = config.hostId + ' native review ' + id;
  const created = await action({ action: 'create', fields: { title, description: 'An actual native question and saved result for user review.', acceptanceCriteria: ['Retain the original live question over TaskBoard restart.', 'Save native output before explicit user acceptance.'],
    project: { namespace: 'codex', kind: 'project', serviceNodeId: owner, nativeId: project.nativeId }, control: 'user', priority: 2, target: { serviceNodeId: owner, hostId: status.hostId }, executionPlan: savedPlan.plan, dependencies: [], nextReviewAt: null, dueAt: null } }, 'create');
  assert.ok(created.task); await save('opening-browser', { taskId: created.task.objectId, plan: savedPlan.plan, model: model.model, effort });
  const directory = join(repository, 'dist/apps/task-board-ui'), definition = JSON.parse(await readFile(join(directory, 'ivy-ui.json'), 'utf8'));
  let expectedReleaseId = null;
  try { expectedReleaseId = (await client.request('uis.get', { uiId: 'task-board-ui' })).currentReleaseId; } catch (error) { if (error.code !== 'not_found') throw error; }
  const release = await publishUi(client, { directory, definition, expectedReleaseId, mutationId: id + '-ui' }); report.release = release;
  browser = await chromium.launch({ ...(process.platform === 'win32' ? { channel: 'msedge' } : {}), headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } }); context.setDefaultTimeout(30000);
  await context.route('**/*', route => { if (new URL(route.request().url()).origin !== new URL(config.publicBaseUrl).origin) { outsideRequests.push(route.request().url()); return route.abort(); } return route.continue(); });
  page = await context.newPage(); page.on('pageerror', error => pageErrors.push(error.message));
  await page.goto(config.publicBaseUrl + '/'); await page.getByLabel('Hive credential').fill(hive.settings.credentials[0].token);
  const login = page.waitForResponse(r => r.request().method() === 'POST' && r.url().endsWith('/login'));
  await page.getByRole('button', { name: 'Sign in', exact: true }).click(); assert.equal((await login).status(), 303);
  await page.waitForURL(url => url.pathname === new URL(config.publicBaseUrl + '/').pathname);
  await page.goto(config.publicBaseUrl + '/ui/task-board-ui/#/tasks?id=' + report.taskId + '&node=' + encodeURIComponent(taskBoard));
  await expect(page.getByRole('heading', { name: title, exact: true })).toBeVisible();
  pump = (async () => {
    while (!stopped) {
      try {
        const inputs = await call(owner, 'agent.inputs', {});
        for (const input of inputs.items.filter(i => i.state === 'pending' && i.method === 'currentTime/read')) {
          const operationId = id + '-clock-' + input.identity.epoch + '-' + input.identity.requestId;
          await call(owner, 'agent.answer', { operationId, identity: input.identity, reply: { result: { currentTimeAt: Math.floor(Date.now() / 1000) } } }, operationId);
        }
      } catch (error) { pumpErrors.push({ code: error.code ?? error.name, at: new Date().toISOString() }); }
      await delay(1000);
    }
  })();
  await page.getByRole('button', { name: 'Mark ready', exact: true }).click(); await page.getByRole('button', { name: 'Start execution', exact: true }).click();
  const question = await until(async () => {
    const value = await task(); if (!value.lastRun) return null;
    const run = (await client.request('objects.read', value.lastRun)).content.value;
    return (await call(owner, 'agent.inputs', {})).items.find(i => i.state === 'pending' && i.method === 'item/tool/requestUserInput' && i.turnId === run.turnId);
  }, 'actual native question');
  assert.equal(question.identity.epoch, status.epoch);
  const choice = question.params.questions[0].options.find(option => (qualityCase ? /^B(?:\s|$)/ : /^Alpha(?:\s|$)/).test(option.label)); assert.ok(choice);
  if (qualityCase) {
    assert.equal(question.params.questions.length, 1);
    assert.equal(question.params.questions[0].options.length, 2);
    assert.ok(question.params.questions[0].options.some(option => /^A(?:\s|$)/.test(option.label)));
    const observation = await call(owner, 'agent.read', { nativeVersion: status.nativeVersion, method: 'thread/read', params: { threadId: question.threadId, includeTurns: true } });
    assert.ok('result' in observation.reply);
    await atomicJson(join(root, 'quality-before-answer.json'), observation.reply.result);
    const pendingTurn = observation.reply.result.thread.turns.find(turn => turn.id === question.turnId);
    assert.ok(pendingTurn); assert.notEqual(pendingTurn.status, 'completed');
    const text = pendingTurn.items.filter(item => item.type === 'agentMessage').map(item => item.text).join('\n');
    assert.doesNotMatch(text, /Auswahl:\s*[AB](?:\s|$)/, 'No choice may be invented before the supplied answer');
    assert.ok(!qualityCase.rows.every(row => row.every(cell => text.includes(cell))), 'The final comparison must wait for the answer');
  }
  await page.getByRole('button', { name: choice.label, exact: true }).click();
  await page.screenshot({ path: join(root, 'native-question-before-restart.png'), fullPage: true });
  await save('awaiting-task-board-restart', { question: { identity: question.identity, threadId: question.threadId, turnId: question.turnId }, controlPath, taskBoardGeneration: workspace.generation });
  const control = await until(async () => { try { const value = JSON.parse(await readFile(controlPath, 'utf8')); if (value.action === 'abort') throw new Error('Caller stopped the unfinished acceptance.'); return value.action === 'verify-restart' ? value : null; } catch (error) { if (error.code !== 'ENOENT') throw error; return null; } }, 'caller-controlled TaskBoard restart', 600000);
  assert.ok(control.deploymentId);
  const current = await call(taskBoard, 'task-board.workspace', {}); assert.notEqual(current.generation, workspace.generation); assert.equal(current.principalId, workspace.principalId);
  const retained = (await call(owner, 'agent.inputs', { identity: question.identity })).items[0]; assert.equal(retained.state, 'pending'); assert.deepEqual(retained.identity, question.identity);
  await page.reload(); await expect(page.getByLabel(question.params.questions[0].question, { exact: true })).toHaveValue(choice.label);
  await page.getByRole('button', { name: 'Send response', exact: true }).click();
  await save('awaiting-saved-native-result', { restart: control, answeredQuestion: question.identity });
  const accepted = await nativeReviewFlow({ root, client, owner, report, task, until, native, action, page, plan, marker, question, save, call, qualityCase });
  report.tokenEvidence = await collectTokenEvidence(client, owner, status.epoch, [question.turnId, report.continuation.turnId]);
  assert.equal(digest(await readFile(values['auth-file'])), authBefore); assert.deepEqual(pageErrors, []); assert.deepEqual(outsideRequests, []);
  await save('passed', { browser: browser.version(), nativeThreadId: question.threadId, nativeTurnId: question.turnId, latestResult: accepted.latestResult, acceptedReview: accepted.acceptedReview, restart: control, pageErrors, outsideRequests, pumpErrors, finishedAt: new Date().toISOString() });
} catch (error) {
  await page?.screenshot({ path: join(root, 'failure.png'), fullPage: true }).catch(() => undefined);
  await save('failed', { failure: { code: error.code ?? error.name, message: error.message }, pageErrors, outsideRequests, pumpErrors, liveRunRetainedForReconciliation: !!report.taskId }); process.exitCode = 1;
} finally { stopped = true; await pump; await browser?.close(); }
