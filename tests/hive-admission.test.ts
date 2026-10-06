import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { AuthenticationRate, DispatchGate, RequestBudget } from '../services/hive/src/admission.js';
import { HiveServer } from '../services/hive/src/server.js';
import type { WorkerClient } from '../services/hive/src/worker-client.js';
import type { IncomingMessage } from 'node:http';

test('proxy attribution ignores untrusted headers and preserves a direct local maintenance connection', () => {
  const server = new HiveServer({ filename: ':memory:', publicBaseUrl: 'http://127.0.0.1/ivy', credentials: [], version: 'test', buildId: 'test', trustedProxyAddresses: ['127.0.0.1'] });
  const address = (peer: string, header?: string) => (server as unknown as { clientAddress(request: IncomingMessage): string }).clientAddress({
    socket: { remoteAddress: peer }, headers: header === undefined ? {} : { 'x-ivy-client-ip': header },
  } as IncomingMessage);
  assert.equal(address('192.0.2.1', '192.0.2.2'), '192.0.2.1');
  assert.equal(address('127.0.0.1'), '127.0.0.1');
  assert.equal(address('::ffff:127.0.0.1', '192.0.2.2'), '192.0.2.2');
  assert.throws(() => address('127.0.0.1', '192.0.2.1, 192.0.2.2'), { code: 'unauthenticated' });
});

test('authentication arrival bounds reject bursts, preserve other clients, and recover after expiry', () => {
  const rate = new AuthenticationRate(1000, 3, 2, 3);
  rate.admit('a', 1000); rate.admit('a', 1000);
  assert.throws(() => rate.admit('a', 1001), { code: 'limit_exceeded' });
  rate.admit('b', 1001); assert.throws(() => rate.admit('c', 1001), { code: 'limit_exceeded' });
  rate.admit('a', 2000); rate.admit('new', 2001);
  const identities = new AuthenticationRate(1000, 10, 10, 2);
  identities.admit('a', 1000); identities.admit('b', 1000);
  assert.throws(() => identities.admit('c', 1000), { code: 'limit_exceeded' }); identities.admit('c', 2000);
});

test('request reservations retain global and consumer bytes until completion and recover after refusal', () => {
  const budget = new RequestBudget(3, 100, 2, 60), first = budget.acquire('a', 30), second = budget.acquire('a', 30);
  assert.throws(() => budget.acquire('a', 0), {
    code: 'limit_exceeded',
    outcome: 'not_executed',
    details: { budget: 'request', scope: "consumer", active: 2, limit: 2 },
  });
  assert.throws(() => first.grow(1), { code: 'limit_exceeded' });
  const other = budget.acquire('b', 40);
  assert.throws(() => other.grow(1), {
    code: 'limit_exceeded',
    details: {
      budget: "request_bytes",
      scope: "global",
      active: 100,
      requested: 1,
      limit: 100,
    },
  });
  first.release();
  first.release();
  second.release();
  other.release();
  const recovered = budget.acquire('a', 60);
  recovered.release();
});

test('expired dispatch wait cannot execute later or consume a retained queue slot', async () => {
  const gate = new DispatchGate(2, 20); let release!: () => void, invoked = false;
  const active = gate.run(() => new Promise<void>(resolve => { release = resolve; }));
  const queued = gate.run(async () => { invoked = true; });
  await assert.rejects(gate.run(async () => undefined), { code: 'limit_exceeded', outcome: 'not_executed' });
  await assert.rejects(queued, { code: 'deadline_exceeded', outcome: 'not_executed' });
  release(); await active; await delay(0);
  await gate.run(async () => undefined); assert.equal(invoked, false);
});

test("production invoke bounds 2100 callers before a stalled storage worker and preserves admitted outcomes", async () => {
  const server = new HiveServer({ filename: ':memory:', publicBaseUrl: 'http://127.0.0.1/ivy', credentials: [], version: 'test', buildId: 'test' });
  let release!: () => void, executions = 0;
  const stalled = new Promise<void>(resolve => { release = resolve; });
  server.worker.request = (async () => { executions++; await stalled; return { kind: 'result', value: 'original' }; }) as WorkerClient['request'];
  const calls = Array.from({ length: 2100 }, (_, index) => server.invoke({ credentialDigest: 'consumer-' + index, principalId: 'consumer-' + index, transport: 'http' }, { id: index }),
  );
  const results = Promise.allSettled(calls);
  await delay(0);
  assert.equal(executions, 1);
  release();
  const completed = await results;
  assert.equal(
    completed.filter(value => value.status === 'fulfilled').length,
    1024,
  );
  const rejected = completed.filter(value => value.status === 'rejected');
  assert.equal(rejected.length, 1076);
  for (const value of rejected) { assert.equal(value.reason.code, 'limit_exceeded'); assert.equal(value.reason.outcome, 'not_executed'); }
  assert.equal(
    rejected.filter((value) => value.reason.details.budget === 'request')
      .length,
    52,
  );
  assert.equal(
    rejected.filter((value) => value.reason.details.budget === "dispatch")
      .length,
    1024,
  );
  assert.equal(executions, 1024);
});
