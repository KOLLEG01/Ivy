import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { digest } from '../packages/contracts/src/canonical.js';
import { operationId } from '../packages/contracts/src/operation-id.js';
import { HiveKernel } from '../services/hive/src/kernel.js';
import type { Operation, OperationName, Params, Result } from '../packages/contracts/src/generated.js';

test('Hive settings persist independently of services and retain original CAS mutation receipts', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-settings-'));
  const credential = { principalId: 'settings-fixture', digest: digest('synthetic-settings-key') };
  const options = { filename: join(root, 'hive.sqlite'), publicBaseUrl: 'https://hive.example',
    version: 'fixture', buildId: digest('fixture'), credentials: [credential], validateMessages: true };
  let kernel = new HiveKernel(options);
  t.after(async () => { kernel.close(); await rm(root, { recursive: true, force: true }); });
  const call = <M extends OperationName>(method: M, params: Params<M>): Result<M> =>
    kernel.execute({ principalId: credential.principalId, credentialDigest: credential.digest, transport: 'http' },
      { jsonrpc: '2.0', id: 'fixture', method, params }).value as Result<M>;
  assert.deepEqual(call('settings.get', {}), { revision: 0, settings: { schemaVersion: 1, quietHours: null } });
  const request: Operation.SettingsSaveParams = { mutationId: operationId(kernel.store.runtimeEpoch), expectedRevision: 0,
    settings: { schemaVersion: 1, quietHours: { start: '23:00', end: '07:00', timeZone: 'Europe/Berlin' } } };
  const saved = call('settings.save', request);
  assert.equal(saved.revision, 1);
  assert.deepEqual(call('settings.save', request), saved);
  assert.throws(() => call('settings.save', { ...request, mutationId: operationId(kernel.store.runtimeEpoch) }), { code: 'revision_conflict' });
  for (const quietHours of [
    { start: '24:00', end: '07:00', timeZone: 'Europe/Berlin' },
    { start: '23:00', end: '23:00', timeZone: 'Europe/Berlin' },
    { start: '23:00', end: '07:00', timeZone: 'invalid/zone' },
  ]) assert.throws(() => call('settings.save', { ...request, mutationId: operationId(kernel.store.runtimeEpoch), expectedRevision: 1,
    settings: { schemaVersion: 1, quietHours } }), { code: 'invalid_arguments' });
  const read = call('objects.read', { path: '/ivy-settings' });
  assert.throws(() => call('objects.write', { mutationId: operationId(kernel.store.runtimeEpoch),
    objectId: read.object.id, expectedRevision: 1, contractVersion: '1.0.0', references: {},
    content: { encoding: 'json', value: { schemaVersion: 1, quietHours: null } } }), { code: 'forbidden' });
  call('settings.save', { mutationId: operationId(kernel.store.runtimeEpoch), expectedRevision: 1,
    settings: { schemaVersion: 1, quietHours: null } });
  assert.deepEqual(call('settings.save', request), saved);
  kernel.close(); kernel = new HiveKernel(options);
  assert.deepEqual(call('settings.get', {}), { revision: 2, settings: { schemaVersion: 1, quietHours: null } });
});
