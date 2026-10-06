import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readdirSync } from 'node:fs';
import { hashJson } from '../packages/contracts/src/canonical.js';
import { validateAgent } from '../packages/contracts/src/validation.js';
import { checkedNativeContract, nativeCatalogPath } from '../packages/contracts/src/checked-native-contract.js';

test('every checked native catalogue admits its exact read observations and prevention while rejecting unknown methods and arguments', () => {
  const versions = readdirSync('specs/native').filter(name => /^codex-\d+\.\d+\.\d+$/.test(name)).map(name => name.slice(6));
  assert.deepEqual(versions.sort(), ['0.142.3', '0.154.0', '0.158.0', '0.159.2']);
  for (const nativeVersion of ['0.144.1', '0.149.1', '0.153.4']) {
    assert.throws(() => checkedNativeContract(nativeVersion), { code: 'native_version_unsupported' });
    assert.throws(() => nativeCatalogPath('.', nativeVersion), { code: 'native_version_unsupported' });
    assert.throws(() => validateAgent('ReadInput', { nativeVersion, method: 'thread/read', params: { threadId: 'original' } }));
    assert.throws(() => validateAgent('PreventInput', { nativeVersion, operationId: 'retired', method: 'thread/start', params: {}, expectedDefinitionHash: hashJson('definition') }));
  }
  for (const nativeVersion of versions) {
    assert.equal(checkedNativeContract(nativeVersion).catalog.version, nativeVersion);
    assert.ok(existsSync(nativeCatalogPath('.', nativeVersion)));
    for (const method of ['thread/read', 'thread/turns/list', 'thread/items/list']) {
      const params = method === 'thread/read' ? { threadId: 'original', includeTurns: false } : {
        threadId: 'original', limit: 1, cursor: null, sortDirection: 'desc', ...(method === 'thread/items/list' ? { turnId: 'turn' } : { itemsView: 'full' }),
      };
      const request = { nativeVersion, method, params }; validateAgent('ReadInput', request);
      validateAgent('ReadObservation', { schemaVersion: 1, observationId: 'observation', callerPrincipalId: 'caller', serviceNodeId: 'native',
        nativeExecutableHash: hashJson('binary'), catalogHash: hashJson('catalog'), epoch: 'epoch', requestId: 1, observedAt: '2026-09-08T10:00:00.000Z',
        requestHash: hashJson({ method, params }), ...request, reply: { result: {} } });
      assert.throws(() => validateAgent('ReadInput', { ...request, params: { ...params, unrecognized: true } }));
      assert.throws(() => validateAgent('ReadInput', { ...request, method: 'turn/start' }));
      assert.throws(() => validateAgent('ReadInput', { ...request, nativeVersion: 'unrecognized' }));
    }
    validateAgent('PreventInput', { operationId: 'original', nativeVersion, method: 'thread/start', params: {}, expectedDefinitionHash: hashJson('definition') });
  }
});
