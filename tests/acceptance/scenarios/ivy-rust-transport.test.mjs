import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { mkdtemp, open, readFile, readdir, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { HiveClient } from '../../../dist/packages/sdk/src/client.js';
import { runDifferential } from '../../../tools/testing/ivy-rust-differential.mjs';

async function fixture(t, respond) {
  const root = await mkdtemp(join(tmpdir(), 'ivy-rust-transport-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const requests = [], failures = [], clients = {};
  for (const lane of ['reference', 'candidate']) {
    const credential = `fixture-${lane}`;
    const server = createServer(async (request, response) => {
      try {
        assert.equal(request.method, 'POST');
        assert.equal(request.url, '/ivy/api/v1/rpc');
        assert.equal(request.headers.authorization, `Bearer ${credential}`);
        let body = '';
        for await (const chunk of request) body += chunk;
        const frame = JSON.parse(body);
        assert.equal(frame.jsonrpc, '2.0');
        assert.equal(typeof frame.id, 'string');
        // The actual HTTP peer observes evidence only after the recorder has flushed it.
        const recorded = JSON.parse(await readFile(join(root, `${frame.params.caseId}-${lane}-request.json`), 'utf8'));
        assert.deepEqual(recorded, { method: frame.method, params: frame.params });
        requests.push({ lane, frame });
        const result = respond(lane, frame, response);
        if (result !== undefined) {
          response.writeHead(200, { 'Content-Type': 'application/json' });
          response.end(JSON.stringify({ jsonrpc: '2.0', id: frame.id, ...result }));
        }
      } catch (error) {
        failures.push(error);
        response.destroy();
      }
    });
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    t.after(async () => {
      const closed = new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
      server.closeAllConnections();
      await closed;
    });
    // HTTP is confined to loopback fixtures. Deployed-lane TLS qualification is separate.
    clients[lane] = new HiveClient(`http://127.0.0.1:${server.address().port}/ivy`, { credential, batchReads: false });
  }
  const record = async (name, value) => {
    assert.match(name, /^[a-z0-9-]+$/);
    const file = await open(join(root, `${name}.json`), 'wx', 0o600);
    try { await file.writeFile(JSON.stringify(value)); await file.sync(); } finally { await file.close(); }
  };
  return { root, requests, failures, record, ...clients };
}

test('differential harness uses real SDK framing and exact domain errors after durable evidence', async t => {
  const f = await fixture(t, (lane, frame) => frame.params.caseId === 'conflict'
    ? { error: { code: -32000, message: 'Fixture conflict', data: { code: 'revision_conflict', outcome: 'not_executed' } } }
    : { result: { value: frame.params.value, operationId: frame.params.operationId, observedAt: lane } });
  const steps = [
    { id: 'echo', method: 'fixture.echo', params: { caseId: 'echo', operationId: 'original', value: '\ud800/é' },
      expected: { result: { value: '\ud800/é', operationId: 'original', observedAt: 'expected' } }, volatilePointers: ['/result/observedAt'] },
    { id: 'conflict', method: 'fixture.write', params: { caseId: 'conflict', operationId: 'conflict-original' },
      expected: { error: { code: 'revision_conflict', outcome: 'not_executed', message: 'Fixture conflict', details: null, httpStatus: 409, rpcCode: -32000 } } },
  ];
  assert.deepEqual(await runDifferential({ ...f, steps }), [{ id: 'echo', passed: true }, { id: 'conflict', passed: true }]);
  assert.deepEqual(f.failures, []);
  assert.deepEqual(f.requests.map(x => x.lane), ['reference', 'candidate', 'reference', 'candidate']);
  assert.equal(new Set(f.requests.map(x => x.frame.id)).size, 4);
  assert.equal((await readdir(f.root)).length, 8);
});

test('a real lost HTTP reply preserves the original request and stops before candidate mutation', async t => {
  const effects = { reference: 0, candidate: 0 };
  const f = await fixture(t, (lane, frame, response) => {
    effects[lane]++;
    response.destroy(); // The fixture effect happened; no result was delivered.
  });
  const params = { caseId: 'write', operationId: 'retained-original' };
  await assert.rejects(runDifferential({ ...f, steps: [
    { id: 'write', method: 'fixture.write', params, expected: { result: { committed: true } } },
  ] }), /reconcile its original request/);
  assert.deepEqual(f.failures, []);
  assert.deepEqual(effects, { reference: 1, candidate: 0 });
  assert.equal(f.requests.length, 1);
  assert.deepEqual(f.requests[0].frame.params, params);
  assert.deepEqual((await readdir(f.root)).sort(), ['write-reference-request.json', 'write-reference-unknown.json']);
  assert.equal(JSON.parse(await readFile(join(f.root, 'write-reference-unknown.json'), 'utf8')).outcome, 'unknown');
});
