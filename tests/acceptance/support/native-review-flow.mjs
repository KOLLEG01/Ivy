import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { expect } from '@playwright/test';
import { hashJson } from '../../../dist/packages/contracts/src/canonical.js';
import { verifyQualityOutput } from './native-quality.mjs';

// Shared real browser/result/continuation checks, also for recovery of an earlier retained Run.
export async function nativeReviewFlow({ root, client, owner, report, task, until, native, action, page, plan, marker, question, save, call, qualityCase }) {
  await until(async () => (await task()).status === 'review', 'saved native result'); await page.reload();
  const reviewing = await task(); assert.ok(reviewing.latestResult && reviewing.lastRun);
  const result = (await client.request('objects.read', reviewing.latestResult)).content.value;
  const expectedChoice = qualityCase?.answer ?? 'Alpha';
  assert.ok(result.content.summary.includes(marker)); assert.ok(result.content.summary.includes(expectedChoice));
  let downloadedFull = false;
  for (const artifact of result.content.artifacts) {
    const saved = await client.request('objects.read', artifact.object);
    if (saved.object.contractKey !== 'task-board/native-full-turn-transcript') continue;
    await page.getByRole('button', { name: artifact.label, exact: true }).first().click();
    const [fullDownload] = await Promise.all([page.waitForEvent('download'), page.getByRole('button', { name: 'Download full output', exact: true }).first().click()]);
    const fullPath = join(root, 'saved-full-native-output.json'); await fullDownload.saveAs(fullPath);
    const full = JSON.parse(await readFile(fullPath, 'utf8')), exact = full.reply.result.data[0];
    assert.equal(exact.id, question.turnId); assert.equal(exact.itemsView, 'full');
    assert.ok(exact.items.some(item => item.type === 'agentMessage' && item.text.includes(marker) && item.text.includes(expectedChoice)));
    if (qualityCase) verifyQualityOutput(exact.items.filter(item => item.type === 'agentMessage').map(item => item.text).join('\n'), qualityCase, marker);
    downloadedFull = true;
  }
  assert.ok(downloadedFull, 'The retained full native transcript must actually be downloaded and verified');
  const nativeRead = await native('thread/read', { threadId: question.threadId, includeTurns: true }, 'verify-native-output');
  const completed = nativeRead.thread.turns.find(turn => turn.id === question.turnId); assert.equal(completed.status, 'completed');
  assert.ok(completed.items.some(item => item.type === 'agentMessage' && item.text.includes(marker) && item.text.includes(expectedChoice)));
  if (qualityCase) verifyQualityOutput(completed.items.filter(item => item.type === 'agentMessage').map(item => item.text).join('\n'), qualityCase, marker);
  // Task summary and selected Run can both show this same saved result. Verify the downloaded pin.
  const [download] = await Promise.all([page.waitForEvent('download'), page.getByRole('button', { name: 'Download saved result', exact: true }).first().click()]);
  const resultPath = join(root, 'saved-result.json'); await download.saveAs(resultPath); assert.deepEqual(JSON.parse(await readFile(resultPath, 'utf8')), result);
  await page.getByRole('button', { name: 'Review result', exact: true }).click(); await page.getByLabel('Acceptance evidence', { exact: true }).fill('Verified the actual native ' + expectedChoice + ' answer, retained original Task/Run and native turn after recovery, and downloaded the saved result.' + (qualityCase ? ' The retained three-row comparison matches every supplied fact.' : ''));
  await page.getByRole('button', { name: 'Accept result and complete task', exact: true }).click(); await until(async () => (await task()).status === 'completed', 'explicit user acceptance');
  const accepted = await task(); assert.ok(accepted.acceptedReview); assert.deepEqual(accepted.primaryResourceRef, reviewing.primaryResourceRef);
  await page.screenshot({ path: join(root, 'native-result-accepted.png'), fullPage: true });
  await save('continuing-original-primary', { firstResult: accepted.latestResult, firstReview: accepted.acceptedReview, firstRun: accepted.lastRun });
  await nativeContinuationFlow({ root, client, owner, report, task, until, native, action, page, plan, question, call, accepted });
  return accepted;
}

export async function nativeContinuationFlow({ root, client, owner, report, task, until, native, action, page, plan, question, call, accepted, pendingRun }) {
  const feedback = 'Continue this same native task for one isolated cancellation check. Use request_user_input now to ask Alpha or Beta and wait for the answer. Do not use other tools, read or write files, or make external changes. The acceptance client will interrupt while the question is pending.';
  let continued = pendingRun ? { run: pendingRun } : null;
  if (!continued) {
    const continuedPlan = await action({ action: 'savePlan', plan: { ...plan, turnStart: { ...plan.turnStart, input: [{ type: 'text', text: feedback, text_elements: [] }] } } }, 'continued-plan');
    const fresh = await client.request('objects.read', { objectId: report.taskId });
    continued = await action({ action: 'continue', taskId: report.taskId, expectedRevision: fresh.object.currentRevision, intent: continuedPlan.plan, feedback }, 'continue');
  }
  assert.ok(continued.run);
  const secondQuestion = await until(async () => {
    const run = (await client.request('objects.read', { objectId: continued.run.objectId })).content.value;
    return (await call(owner, 'agent.inputs', {})).items.find(i => i.state === 'pending' && i.method === 'item/tool/requestUserInput' && i.turnId === run.turnId);
  }, 'continued native question');
  assert.equal(secondQuestion.threadId, question.threadId); assert.notEqual(secondQuestion.turnId, question.turnId);
  const continuedRun = (await client.request('objects.read', { objectId: continued.run.objectId })).content.value;
  assert.deepEqual(continuedRun.primaryResourceRef, accepted.primaryResourceRef);
  const resume = (await client.request('objects.read', continuedRun.calls.thread)).content.value;
  assert.equal(resume.request.method, 'thread/resume');
  const parameterPin = resume.request.params;
  const parameterDocument = await client.request('objects.read', { objectId: parameterPin.objectId, revision: parameterPin.revision });
  assert.equal(parameterDocument.object.contractKey, parameterPin.contractKey);
  assert.equal(parameterDocument.revision.contractVersion, parameterPin.contractVersion);
  assert.equal(parameterDocument.revision.contentHash, parameterPin.contentHash);
  assert.equal(hashJson(parameterDocument.content.value), parameterPin.contentHash);
  assert.equal(parameterDocument.content.value.method, 'thread/resume');
  assert.equal(parameterDocument.content.value.params.threadId, question.threadId);
  await page.reload(); await page.getByRole('button', { name: 'Request cancellation', exact: true }).click();
  await page.getByLabel('Reason', { exact: true }).fill('Stop only this isolated native continuation after its retained question.');
  await page.getByRole('button', { name: 'Save cancellation intent', exact: true }).click();
  await until(async () => (await task()).status === 'cancelled', 'native-confirmed cancellation'); await page.reload();
  const finalNative = await native('thread/read', { threadId: question.threadId, includeTurns: true }, 'verify-native-interruption');
  assert.equal(finalNative.thread.turns.find(turn => turn.id === secondQuestion.turnId).status, 'interrupted');
  const cancelled = (await client.request('objects.read', { objectId: continued.run.objectId })).content.value;
  assert.equal(cancelled.phase, 'cancelled'); assert.ok(cancelled.result && cancelled.cancellation);
  const oldRun = await client.request('objects.read', accepted.lastRun), currentOldRun = await client.request('objects.read', { objectId: accepted.lastRun.objectId });
  assert.equal(currentOldRun.object.currentRevision, accepted.lastRun.revision, 'The previous Run must not gain revisions during a new attempt.');
  assert.deepEqual(oldRun.content.value, currentOldRun.content.value, 'The previous completed Run must remain unchanged.');
  await expect(page.getByRole('link', { name: /^Run 1 · completed · / })).toBeVisible(); await expect(page.getByRole('link', { name: new RegExp('^Run ' + continuedRun.attempt + ' · cancelled · ') })).toBeVisible();
  await page.getByRole('link', { name: /^Run 1 · completed · / }).click(); await expect(page.getByRole('heading', { name: 'Run 1', exact: true })).toBeVisible();
  await page.screenshot({ path: join(root, 'native-historical-first-run.png'), fullPage: true });
  report.continuation = { run: continued.run, turnId: secondQuestion.turnId, resumeCall: continuedRun.calls.thread, cancellation: cancelled.cancellation, result: cancelled.result };
}
