import test from 'node:test';
import assert from 'node:assert/strict';
import { HiveStore } from '../services/hive/src/store.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { operationId } from '../packages/contracts/src/operation-id.js';

test('external authentication binds once and internal mutations reuse the bound principal', t => {
  const store = new HiveStore(':memory:'); t.after(() => store.close());
  const credentials = [{ principalId: 'one', digest: digest('one') }, { principalId: 'two', digest: digest('two') }];
  store.configureCredentials(credentials); const get = store.get.bind(store); let checks = 0;
  t.mock.method(store, 'get', (sql: string, ...args: Parameters<HiveStore['get']> extends [string, ...infer A] ? A : never) => {
    if (sql.includes('FROM credentials')) checks++; return get(sql, ...args);
  });
  const context = { credentialDigest: credentials[0]!.digest };
  const principal = store.authenticate(context); assert.equal(principal.principalId, 'one');
  for (let i = 0; i < 100; i++) store.mutate({ ...context, principalId: principal.principalId }, 'fixture.write', { mutationId: operationId(store.runtimeEpoch, Date.now(), 'mutation-' + i), value: i }, () => ({ value: { i } }));
  assert.equal(checks, 1);
  store.configureCredentials([credentials[1]!]);
  assert.throws(() => store.authenticate(context), { code: 'unauthenticated' });
});

test('browser logout is enforced at the next external session binding', t => {
  const store = new HiveStore(':memory:'); t.after(() => store.close());
  const credentialDigest = digest('browser'), sessionDigest = digest('session');
  store.configureCredentials([{ principalId: 'browser', digest: credentialDigest }]);
  store.run('INSERT INTO browser_sessions VALUES (?,?,?)', sessionDigest, credentialDigest, new Date().toISOString());
  const context = { credentialDigest, sessionDigest };
  store.authenticate(context);
  store.run('DELETE FROM browser_sessions WHERE digest=?', sessionDigest);
  assert.throws(() => store.authenticate(context), { code: 'unauthenticated' });
});
