import test from 'node:test';
import assert from 'node:assert/strict';
import { PhoneAppToolsPool } from '../services/phone-bridge/src/runtime/desktop-app-tools-pool.js';
import type { CodexAppTools, AppToolsSettings } from '../packages/sdk/src/codex-app-tools.js';

test('a failed pooled App Tools connection is closed and replaced for the next observation', async () => {
  let created = 0, closed = 0;
  const pool = new PhoneAppToolsPool(() => {
    const instance = ++created;
    let isClosed = false;
    return {
      get isClosed() { return isClosed; },
      async verifyActor() {
        if (instance === 1) throw new Error('dead Desktop pipe');
        return { id: 'fixture' };
      },
      async close() { isClosed = true; closed++; },
    } as unknown as CodexAppTools;
  });
  const settings = { actorThreadId: 'fixture' } as AppToolsSettings;
  await assert.rejects(pool.open(settings).verifyActor(), /dead Desktop pipe/);
  assert.equal(closed, 1);
  await pool.open(settings).verifyActor();
  assert.equal(created, 2);
  await pool.close();
  assert.equal(closed, 2);
});

test('a newly installed App Tools release replaces the pool after in-flight work completes', async () => {
  let current = true, created = 0, closed = 0, requests = 0;
  const gate: { finish?: () => void } = {};
  const pool = new PhoneAppToolsPool(() => {
    const instance = ++created;
    return {
      get isClosed() { return false; },
      get isCurrentInstallation() { return instance !== 1 || current; },
      async verifyActor() {
        requests++;
        return instance === 1
          ? new Promise(resolve => { gate.finish = () => resolve({ id: 'first' }); })
          : { id: 'second' };
      },
      async close() { closed++; },
    } as unknown as CodexAppTools;
  });
  const settings = { actorThreadId: 'fixture' } as AppToolsSettings;
  const oldLease = pool.open(settings), oldRequest = oldLease.verifyActor;
  const first = oldRequest();
  current = false;
  const nextLease = pool.open(settings);
  assert.equal((await nextLease.verifyActor()).id, 'second');
  assert.equal(closed, 0);
  await assert.rejects(oldRequest(), { code: 'app_tools_closed' });
  assert.equal(requests, 2, 'retired connections cannot start new work while an original request finishes');
  assert.ok(gate.finish);
  gate.finish();
  assert.equal((await first).id, 'first');
  assert.equal(closed, 1);
  await pool.close();
  assert.equal(closed, 2);
  await assert.rejects(nextLease.verifyActor(), { code: 'app_tools_closed' });
  assert.equal(requests, 2, 'closing the pool invalidates its outstanding leases');
});
