import assert from 'node:assert/strict';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { parseArgs } from 'node:util';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium, expect } from '@playwright/test';
import { HiveClient, discover, callBound } from '../../../dist/packages/sdk/src/client.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';
import { digest, hashJson } from '../../../dist/packages/contracts/src/canonical.js';
import { validateTaskBoard } from '../../../dist/packages/contracts/src/validation.js';
import { nativeReviewFlow, nativeContinuationFlow } from '../support/native-review-flow.mjs';
import { publishUi } from '../../../dist/packages/cli/src/publish-ui.js';

// Explicitly continues an earlier failed acceptance's SAME Task/Run, with separate evidence.
const { values } = parseArgs({ options: Object.fromEntries(['config', 'prior-report', 'evidence', 'expected-build', 'observed-epoch', 'continuation-report', 'pending-report'].map(key => [key, { type: 'string' }])) });
for (const key of ['config', 'prior-report', 'evidence', 'expected-build']) assert.ok(values[key], 'Missing --' + key);
const priorBytes = await readFile(resolve(values['prior-report'])), prior = JSON.parse(priorBytes);
const config = JSON.parse(await readFile(resolve(values.config), 'utf8'));
assert.equal(prior.hostId, config.hostId); assert.ok(config.hostId.endsWith('-acceptance'));
assert.equal(prior.phase, 'failed'); assert.equal(prior.failure.message, 'Acceptance deadline: saved native result');
assert.ok(prior.question && prior.taskId && prior.plan && prior.release && prior.workspace);
const continuation = values['continuation-report'] ? JSON.parse(await readFile(resolve(values['continuation-report']), 'utf8')) : null;
if (continuation) {
  assert.equal(continuation.phase, 'failed'); assert.equal(continuation.failure.message, 'Acceptance deadline: continued native question');
  assert.equal(continuation.taskId, prior.taskId); assert.equal(continuation.originalAcceptanceId, prior.id);
  assert.ok(continuation.firstRun && continuation.firstReview && continuation.firstResult);
}
const pending = values['pending-report'] ? JSON.parse(await readFile(resolve(values['pending-report']), 'utf8')) : null;
if (pending) {
  assert.ok(continuation); assert.equal(pending.phase, 'failed'); assert.equal(pending.failure.message, 'Acceptance deadline: continued native question');
  assert.equal(pending.taskId, prior.taskId); assert.equal(pending.continuationReportHash, hashJson(continuation)); assert.ok(pending.retainedFailedRun);
  assert.equal(pending.operations.filter(operation => operation.method === 'task-board.continue').length, 1);
}
assert.ok(new URL(config.publicBaseUrl).protocol === 'https:' || new URL(config.publicBaseUrl).hostname === '127.0.0.1');
const hive = config.instances.find(i => i.componentId === 'hive'); assert.ok(hive?.settings.credentials[0]?.token);
const client = new HiveClient(config.publicBaseUrl, { credential: hive.settings.credentials[0].token });
const root = resolve(values.evidence); await mkdir(root, { recursive: true });
const reportPath = join(root, 'report.json'); await writeFile(reportPath, '{}\n', { flag: 'wx' });
const id = randomUUID(), owner = prior.owner, taskBoard = prior.taskBoard, question = prior.question;
const report = { schemaVersion: 1, id, hostId: config.hostId, owner, taskBoard, taskId: prior.taskId, originalAcceptanceId: prior.id,
  originalFailure: prior.failure, priorReportHash: digest(priorBytes), startedAt: new Date().toISOString(), phase: 'starting', operations: [] };
const save = async (phase, details = {}) => { Object.assign(report, details, { phase, observedAt: new Date().toISOString() }); await atomicJson(reportPath, report); console.log(JSON.stringify({ phase, reportPath, taskId: report.taskId })); };
const call = async (node, name, args, operationId) => callBound(client, await discover(client, name, { serviceNodeId: node }), args, operationId, { timeoutMs: 65000 });
let workspace;
const action = async (request, label) => {
  const operationId = id + '-' + label, args = { ...request, operationId, expectedWorkspace: { principalId: workspace.principalId, rootObjectId: workspace.rootObjectId, callerPrincipalId: workspace.callerPrincipalId } };
  validateTaskBoard('ActionInput', args); report.operations.push({ method: 'task-board.' + request.action, operationId, arguments: args }); await atomicJson(reportPath, report);
  return call(taskBoard, 'task-board.' + request.action, args, operationId);
};
const native = async (method, args, label) => {
  const operationId = id + '-' + label; report.operations.push({ method, operationId }); await atomicJson(reportPath, report);
  return call(owner, 'codex.' + method, args, operationId);
};
const task = async () => (await client.request('objects.read', { objectId: report.taskId })).content.value;
const until = async (check, label, timeout = 240000) => { const deadline = Date.now() + timeout; while (Date.now() < deadline) { const result = await check(); if (result) return result; await delay(1000); } throw new Error('Acceptance deadline: ' + label); };
let browser, page, pump, stopped = false; const pageErrors = [], outsideRequests = [], pumpErrors = [];
try {
  assert.equal((await client.request('system.status', {})).ready, true);
  const node = await client.request('serviceNodes.get', { serviceNodeId: taskBoard }); assert.equal(node.buildId, values['expected-build']);
  workspace = await call(taskBoard, 'task-board.workspace', {}); assert.equal(workspace.role, 'user'); assert.equal(workspace.recovered, true);
  assert.equal(workspace.principalId, prior.workspace.principalId); assert.equal(workspace.rootObjectId, prior.workspace.rootObjectId);
  const status = await call(owner, 'agent.status', {}); assert.equal(status.state, 'ready');
  assert.equal(status.epoch, values['observed-epoch'] ?? question.identity.epoch);
  for (const key of ['hostId', 'serviceNodeId', 'nativeVersion', 'nativeExecutableHash', 'catalogHash']) assert.equal(status[key], prior.nativeStatus[key]);
  report.nativeRecovery = { originalEpoch: question.identity.epoch, observedEpoch: status.epoch, changed: status.epoch !== question.identity.epoch };
  const original = await task(); assert.ok((pending ? ['running', 'waiting'] : ['waiting', 'review']).includes(original.status)); assert.equal(original.attemptCount, pending ? 3 : continuation ? 2 : 1);
  const originalRun = await client.request('objects.read', continuation?.firstRun ?? original.lastRun);
  if (pending) {
    const current = await client.request('objects.read', original.lastRun);
    assert.deepEqual(current.content.value.originalRequest, pending.operations.find(operation => operation.method === 'task-board.continue').arguments);
    assert.deepEqual(current.content.value.primaryResourceRef, originalRun.content.value.primaryResourceRef);
    assert.ok(['starting', 'running', 'waiting_input'].includes(current.content.value.phase));
    report.pendingRun = original.lastRun; report.pendingReportHash = hashJson(pending);
  }
  let retainedFailure;
  if (continuation) {
    const failurePin = pending?.retainedFailedRun.pin ?? original.lastRun;
    retainedFailure = await client.request('objects.read', failurePin);
    assert.equal(retainedFailure.content.value.phase, 'failed'); assert.equal(retainedFailure.content.value.attempt, 2);
    assert.deepEqual(retainedFailure.content.value.primaryResourceRef, originalRun.content.value.primaryResourceRef);
    const terminal = (await native('thread/read', { threadId: question.threadId, includeTurns: true }, 'verify-failed-continuation')).thread.turns.find(turn => turn.id === retainedFailure.content.value.turnId);
    assert.equal(terminal.status, 'failed'); assert.ok(terminal.error.message.includes('401 Unauthorized: Missing bearer or basic authentication'));
    report.retainedFailedRun = { pin: failurePin, hash: hashJson(retainedFailure.content.value), turnId: terminal.id, error: terminal.error };
    report.continuationReportHash = hashJson(continuation);
  }
  assert.equal(originalRun.content.value.turnId, question.turnId); assert.equal(originalRun.content.value.primaryResourceRef.nativeId, question.threadId);
  assert.deepEqual(originalRun.content.value.originalRequest.intent, prior.plan);
  const answered = (await call(owner, 'agent.inputs', { identity: question.identity })).items[0]; assert.equal(answered.state, 'answered');
  assert.deepEqual(answered.identity, question.identity);
  if (report.nativeRecovery.changed) {
    // A new epoch cannot inherit live control. Only the exact persisted terminal turn may cross it;
    // nativeReviewFlow verifies the saved result and a fresh explicit resume of the same primary.
    const terminal = (await native('thread/read', { threadId: question.threadId, includeTurns: true }, 'verify-persisted-terminal')).thread.turns.find(turn => turn.id === question.turnId);
    assert.equal(terminal?.status, 'completed'); assert.equal(originalRun.content.value.nativeEpoch, question.identity.epoch);
    assert.equal(originalRun.content.value.phase, 'completed'); if (!pending) assert.equal(original.status, 'review');
  }
  const plan = (await client.request('objects.read', prior.plan)).content.value; validateTaskBoard('NativePlan', plan);
  await save('recovering-original-run', { originalRun: { objectId: originalRun.object.id, revision: originalRun.revision.revision }, originalTurn: question, installedTaskBoard: node, workspace });
  const directory = fileURLToPath(new URL('../../../dist/apps/task-board-ui', import.meta.url)), definition = JSON.parse(await readFile(join(directory, 'ivy-ui.json'), 'utf8'));
  const release = await publishUi(client, { directory, definition, expectedReleaseId: (await client.request('uis.get', { uiId: 'task-board-ui' })).currentReleaseId, mutationId: id + '-ui' });
  await save('opening-recovered-result', { release });
  browser = await chromium.launch({ ...(process.platform === 'win32' ? { channel: 'msedge' } : {}), headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } }); context.setDefaultTimeout(30000);
  await context.route('**/*', route => { if (new URL(route.request().url()).origin !== new URL(config.publicBaseUrl).origin) { outsideRequests.push(route.request().url()); return route.abort(); } return route.continue(); });
  page = await context.newPage(); page.on('pageerror', error => pageErrors.push(error.message));
  await page.goto(config.publicBaseUrl + '/'); await page.getByLabel('Hive credential').fill(hive.settings.credentials[0].token);
  const login = page.waitForResponse(r => r.request().method() === 'POST' && r.url().endsWith('/login'));
  await page.getByRole('button', { name: 'Sign in', exact: true }).click(); assert.equal((await login).status(), 303);
  await page.waitForURL(url => url.pathname === new URL(config.publicBaseUrl + '/').pathname);
  await page.goto(config.publicBaseUrl + '/ui/task-board-ui/#/tasks?id=' + report.taskId + '&node=' + encodeURIComponent(taskBoard));
  await expect(page.getByRole('heading', { name: original.fields.title, exact: true })).toBeVisible();
  pump = (async () => { while (!stopped) {
    try { for (const input of (await call(owner, 'agent.inputs', {})).items.filter(i => i.state === 'pending' && i.method === 'currentTime/read')) {
      const operationId = id + '-clock-' + input.identity.epoch + '-' + input.identity.requestId;
      await call(owner, 'agent.answer', { operationId, identity: input.identity, reply: { result: { currentTimeAt: Math.floor(Date.now() / 1000) } } }, operationId);
    } } catch (error) { pumpErrors.push({ code: error.code ?? error.name, at: new Date().toISOString() }); }
    await delay(1000);
  } })();
  const marker = 'IVY_TASK_BOARD_ACTUAL_' + prior.id;
  let accepted;
  if (continuation) {
    accepted = { ...original, latestResult: continuation.firstResult, acceptedReview: continuation.firstReview, lastRun: continuation.firstRun };
    const review = (await client.request('objects.read', accepted.acceptedReview)).content.value;
    assert.equal(review.taskId, prior.taskId);
    await nativeContinuationFlow({ root, client, owner, report, task, until, native, action, page, plan, question, call, accepted, pendingRun: report.pendingRun });
    const stillFailed = await client.request('objects.read', { objectId: report.retainedFailedRun.pin.objectId });
    assert.equal(stillFailed.object.currentRevision, report.retainedFailedRun.pin.revision);
    assert.equal(hashJson(stillFailed.content.value), report.retainedFailedRun.hash);
  } else accepted = await nativeReviewFlow({ root, client, owner, report, task, until, native, action, page, plan, marker, question, save, call });
  assert.equal((await call(owner, 'agent.status', {})).epoch, status.epoch);
  assert.equal(accepted.lastRun.objectId, originalRun.object.id);
  assert.equal(digest(await readFile(resolve(values['prior-report']))), report.priorReportHash);
  assert.deepEqual(pageErrors, []); assert.deepEqual(outsideRequests, []);
  await save('passed', { browser: browser.version(), recoveredResult: accepted.latestResult, acceptedReview: accepted.acceptedReview, pageErrors, outsideRequests, pumpErrors, finishedAt: new Date().toISOString() });
} catch (error) {
  await page?.screenshot({ path: join(root, 'failure.png'), fullPage: true }).catch(() => undefined);
  await save('failed', { failure: { code: error.code ?? error.name, message: error.message }, pageErrors, outsideRequests, pumpErrors, liveRunRetainedForReconciliation: true }); process.exitCode = 1;
} finally { stopped = true; await pump; await browser?.close(); }
