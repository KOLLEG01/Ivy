import test from 'node:test';
import assert from 'node:assert/strict';
import { expect } from '@playwright/test';
import { agentFixture } from './fixtures/agent-fixture.mjs';

for (const nativeVersion of ['0.159.2', '0.142.3']) test(
  `AgentManager and AgentUI leave IvyInternal silent (${nativeVersion})`,
  { timeout: 120000 },
  async t => {
    const f = await agentFixture(t, undefined, false, nativeVersion, false, true);
    const seenKey = 'ivy.agent.seen:' + new URL(f.base + '/').href;
    await f.context.addInitScript(({ key, value }) => localStorage.setItem(key, value), {
      key: seenKey, value: JSON.stringify({ since: Date.now(), tasks: { 'browser-agent:internal-task': 1 } }),
    });
    await f.context.addInitScript(key => localStorage.setItem(key, 'true'), 'ivy.agent.show-internal:' + new URL(f.base + '/').href);
    const delivered = [];
    f.page.on('websocket', socket => socket.on('framereceived', ({ payload }) => {
      const frame = JSON.parse(String(payload));
      if (frame.params?.namespace === 'agent') delivered.push(frame.params);
    }));
    const notices = async () => (await f.client.request('events.read', {
      afterSequence: 0, filter: { topics: ['agent.browser-notification'] },
    })).items;
    const turnEvent = (threadId, id, status) => ({
      method: status === 'inProgress' ? 'turn/started' : 'turn/completed',
      params: { threadId, turn: { id, status, items: [], error: null } },
    });
    const approval = (threadId, id) => ({ id, method: 'item/commandExecution/requestApproval', params: {
      threadId, turnId: 'notice-turn', itemId: id, command: 'echo fixture', cwd: f.config.settings.internalProjectRoot,
      startedAtMs: 1788690000000, availableDecisions: ['accept', 'decline'],
    } });
    await f.open('#/host?node=browser-agent');
    const navigation = f.page.locator('#ivy-navigation');
    await expect(navigation.getByRole('link', { name: 'saved-task', exact: true })).toBeVisible({ timeout: 35000 });
    await navigation.getByRole('button', { name: 'IvyInternal', exact: true }).click();
    const internal = navigation.getByRole('link', { name: 'Internal service work', exact: true });
    await expect(internal).toBeVisible();
    assert.equal(await f.page.evaluate(key => JSON.parse(localStorage.getItem(key)).tasks['browser-agent:internal-task'], seenKey), undefined);

    const before = f.current().sent.length;
    f.current().emit(turnEvent('internal-task', 'internal-turn', 'inProgress'));
    f.current().emit(turnEvent('internal-task', 'internal-turn', 'completed'));
    f.current().emit(approval('internal-task', 'internal-approval'));
    await expect.poll(() => delivered.some(event => event.name === 'notification' && event.payload.method === 'turn/completed' &&
      event.payload.params.threadId === 'internal-task')).toBe(true);
    assert.equal((await notices()).length, 0, 'internal completion and approval emit no browser notices');
    assert.equal(f.current().sent.slice(before).filter(frame => ['thread/list', 'thread/read'].includes(frame.method)).length, 0,
      'known internal activity requires no task inventory or notification-policy reads');
    await expect(navigation.getByRole('img', { name: /Working|Unread results/ })).toHaveCount(0);

    // A completion can also arrive for history not yet observed by the inventory.
    const unobserved = { ...f.threads.get('internal-task'), id: 'unobserved-internal', name: 'Unobserved internal work' };
    f.threads.set(unobserved.id, unobserved);
    f.current().emit(turnEvent(unobserved.id, 'unobserved-turn', 'completed'));
    f.current().emit(approval(unobserved.id, 'unobserved-approval'));
    await expect.poll(() => f.current().sent.filter(frame => frame.method === 'thread/read' && frame.params.threadId === unobserved.id).length).toBe(1);

    // A task can finish before the first inventory containing it is published.
    const fast = { ...f.threads.get('internal-task'), id: 'fast-internal', name: 'Fast internal work' };
    f.threads.set(fast.id, fast);
    f.current().emit({ method: 'thread/started', params: { thread: fast } });
    f.current().emit(turnEvent(fast.id, 'fast-turn', 'failed'));
    f.current().emit(approval(fast.id, 'fast-approval'));
    f.current().emit(turnEvent('saved-task', 'user-turn', 'inProgress'));
    await expect(navigation.getByRole('img', { name: 'Working', exact: true })).toBeVisible();
    f.current().emit(turnEvent('saved-task', 'user-turn', 'completed'));
    f.current().emit(approval('saved-task', 'user-approval'));
    await expect.poll(async () => (await notices()).length).toBe(2);
    assert.equal(f.current().sent.filter(frame => frame.method === 'thread/read' && frame.params.threadId === unobserved.id).length, 1,
      'completion and input share a single metadata read for an unknown task');
    assert.ok((await notices()).every(event => new URLSearchParams(event.payload.target.fragment.split('?')[1]).get('id') === 'saved-task'));
    await expect(navigation.getByRole('img', { name: 'Unread results', exact: true })).toBeVisible();

    f.threads.get('saved-task').projectId = 'fixture-internal-project';
    f.current().emit({ method: 'thread/project/updated', params: { threadId: 'saved-task', projectId: 'fixture-internal-project' } });
    f.current().emit(turnEvent('saved-task', 'moved-turn', 'completed'));
    await expect(navigation.getByRole('img', { name: /Working|Unread results/ })).toHaveCount(0);
    assert.equal((await notices()).length, 2, 'a live project change immediately suppresses subsequent notices');
    await internal.click();
    await expect(f.page.getByLabel('Decision', { exact: true })).toBeVisible({ timeout: 20000 });
    assert.equal(await f.page.evaluate(key => JSON.parse(localStorage.getItem(key)).tasks['browser-agent:internal-task'], seenKey), undefined,
      'opening an internal conversation never starts unread tracking');
    await f.page.setViewportSize({ width: 390, height: 844 });
    await expect(f.page.getByLabel('Decision', { exact: true })).toBeVisible();
    assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    assert.deepEqual(f.pageErrors, []);
  },
);
