import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { digest } from '../packages/contracts/src/canonical.js';
import { operationId } from '../packages/contracts/src/operation-id.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { HiveKernel } from '../services/hive/src/kernel.js';
import type { Operation, OperationName, Params, Result } from '../packages/contracts/src/generated.js';

test('Hive persists the OpenAI identity with revision checks and original mutation receipts', async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-openai-'));
  const credential = { principalId: 'openai-fixture', digest: digest('synthetic-openai-key') };
  const options = { filename: join(root, 'hive.sqlite'), publicBaseUrl: 'https://hive.example',
    version: 'fixture', buildId: digest('fixture'), credentials: [credential], validateMessages: true };
  let kernel = new HiveKernel(options);
  t.after(async () => { kernel.close(); await rm(root, { recursive: true, force: true }); });
  const call = <M extends OperationName>(method: M, params: Params<M>): Result<M> =>
    kernel.execute({ principalId: credential.principalId, credentialDigest: credential.digest, transport: 'http' },
      { jsonrpc: '2.0', id: 'fixture', method, params }).value as Result<M>;
  assert.deepEqual(call('openai.getConfiguration', {}), { revision: 0, configuration: { schemaVersion: 1, appId: null } });
  const request: Operation.OpenaiSaveConfigurationParams = {
    mutationId: operationId(kernel.store.runtimeEpoch), expectedRevision: 0,
    configuration: { schemaVersion: 1, appId: 'asdk_app_0123456789abcdef0123456789abcdef' },
  };
  const saved = call('openai.saveConfiguration', request);
  assert.equal(saved.revision, 1);
  assert.deepEqual(call('openai.saveConfiguration', request), saved);
  assert.throws(() => call('openai.saveConfiguration', { ...request, mutationId: operationId(kernel.store.runtimeEpoch) }),
    (error: unknown) => error instanceof IvyError && error.code === 'revision_conflict');
  assert.throws(() => call('openai.saveConfiguration', { ...request, mutationId: operationId(kernel.store.runtimeEpoch),
    expectedRevision: 1, configuration: { schemaVersion: 1, appId: 'asdk_app_invalid' } }),
    (error: unknown) => error instanceof IvyError && error.code === 'invalid_arguments');
  call('openai.saveConfiguration', { mutationId: operationId(kernel.store.runtimeEpoch), expectedRevision: 1,
    configuration: { schemaVersion: 1, appId: null } });
  assert.deepEqual(call('openai.saveConfiguration', request), saved);
  assert.equal(call('openai.getConfiguration', {}).configuration.appId, null);
  kernel.close();
  kernel = new HiveKernel(options);
  assert.deepEqual(call('openai.getConfiguration', {}), { revision: 2, configuration: { schemaVersion: 1, appId: null } });
});
