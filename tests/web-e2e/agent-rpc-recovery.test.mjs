import test from 'node:test';
import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { agentFixture } from './fixtures/agent-fixture.mjs';

for (const width of [1440, 390]) test(`AgentUI suppresses unavailable owner RPCs and recovers at ${width}px`, { timeout: 100000 }, async t => {
  const f = await agentFixture(t, { width, height: 1000 });
  const owner = await f.addAgent();
  let calls = 0;
  f.page.on('request', request => {
    if (!request.url().endsWith('/api/v1/rpc')) return;
    const body = request.postDataJSON();
    if (body.method === 'tools.call' && body.params.serviceNodeId === 'browser-second-agent') calls++;
  });
  await f.open('#/task?node=browser-second-agent&id=saved-task');
  await expect(f.page.getByRole('alert')).toHaveCount(0);
  await expect(f.page.getByRole('button', { name: 'Attach historical task', exact: true })).toBeVisible({ timeout: 20000 });
  await owner.close();
  await expect(f.page.getByText('Provider connection is closed.', { exact: true }).first()).toBeVisible({ timeout: 20000 });
  // Allow a previously ready observation to expire, then cover both snapshot and journal retries.
  await f.page.waitForTimeout(6000);
  const failedCalls = calls;
  await f.page.waitForTimeout(16000);
  assert.equal(calls, failedCalls, 'known unavailable owners receive no repeated failing provider calls');
  await f.addAgent();
  await expect(f.page.getByRole('alert')).toHaveCount(0, { timeout: 35000 });
  await expect(f.page.getByRole('button', { name: 'Attach historical task', exact: true })).toBeVisible();
  assert.ok(calls > failedCalls, 'the original owner resumes serving reads after recovery');
  assert.deepEqual(f.pageErrors, []);
});

test('AgentUI keeps task RPCs quiet when another owner changes', { timeout: 70000 }, async t => {
  const f = await agentFixture(t, { width: 1440, height: 1000 });
  const other = await f.addAgent();
  const calls = [];
  f.page.on('request', request => {
    if (!request.url().endsWith('/api/v1/rpc')) return;
    const body = request.postDataJSON();
    if (body.method === 'tools.call' && body.params.serviceNodeId === f.config.serviceNodeId) calls.push(body.params.qualifiedName);
  });
  await f.open('#/task?node=' + f.config.serviceNodeId + '&id=saved-task');
  await expect(f.page.getByRole('button', { name: 'Attach historical task', exact: true })).toBeVisible({ timeout: 20000 });
  await expect(f.page.getByRole('alert')).toHaveCount(0);
  await f.page.waitForTimeout(2000);
  const before = calls.length;
  for (let index = 0; index < 5; index++) {
    await other.service.connection.request('service.heartbeat', { ready: index % 2 === 0, diagnostics: [] });
    await f.page.waitForTimeout(400);
  }
  await f.page.waitForTimeout(2000);
  assert.equal(calls.length, before, 'foreign service hints must not refetch settings, thread, output, inputs or journal');
  assert.deepEqual(f.pageErrors, []);
});
