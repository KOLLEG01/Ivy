import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { validateAgent } from '../packages/contracts/src/validation.js';
import { hashJson } from '../packages/contracts/src/canonical.js';
import { SchemaValidators } from '../packages/contracts/src/schema.js';
import type { Schema } from '../packages/contracts/src/types.js';
import type { Agent } from '../packages/contracts/src/generated.js';

test('native read contract preserves checked parameter shapes and excludes mutation or operation claims', () => {
  const schemas = new SchemaValidators();
  for (const nativeVersion of ['0.154.0', '0.158.0', '0.159.2'] as const) {
    const catalog = JSON.parse(readFileSync(`specs/native/codex-${nativeVersion}/catalog.json`, 'utf8')) as Agent.Catalog;
    for (const method of ['thread/read', 'thread/turns/list', 'thread/items/list'] as const) {
      const params = method === 'thread/read' ? { threadId: 'thread', includeTurns: false }
        : method === 'thread/turns/list' ? { threadId: 'thread', cursor: null, limit: 50, itemsView: 'notLoaded', sortDirection: 'desc' }
          : { threadId: 'thread', turnId: 'turn', cursor: 'opaque', limit: 50, sortDirection: 'asc' };
      const input = { nativeVersion, method, params };
      validateAgent('ReadInput', input);
      schemas.validate(catalog.clientRequests.find(value => value.method === method)!.inputSchema as Schema, params);
      if (nativeVersion === '0.159.2' && method === 'thread/items/list') {
        const anchored = { ...params, cursor: { type: 'item', itemId: 'saved-item', extra: { retained: true } } };
        validateAgent('ReadInput', { nativeVersion, method, params: anchored });
        schemas.validate(catalog.clientRequests.find(value => value.method === method)!.inputSchema as Schema, anchored);
        assert.throws(() => validateAgent('ReadInput', { nativeVersion, method, params: { ...anchored, cursor: { type: 'item' } } }));
      }
      const observation = { schemaVersion: 1, observationId: 'observation', callerPrincipalId: 'caller', serviceNodeId: 'node',
        nativeExecutableHash: catalog.nativeExecutableHash, catalogHash: hashJson(catalog), epoch: 'epoch', requestId: 'native-id',
        observedAt: new Date().toISOString(), requestHash: hashJson({ method, params }), ...input,
        reply: { error: { code: -1, message: 'Native error', data: { original: null } } } };
      validateAgent('ReadObservation', observation);
      for (const change of [{ method: 'turn/start' }, { nativeVersion: '0.150.0' }, { params: { ...params, input: [] } },
        { params: { ...params, threadId: null } }, { operationId: 'mutation' }]) assert.throws(() => validateAgent('ReadInput', { ...input, ...change }));
      for (const change of [{ epoch: null }, { reply: null }, { phase: 'succeeded' }, { operationId: 'mutation' }])
        assert.throws(() => validateAgent('ReadObservation', { ...observation, ...change }));
    }
  }
});

test('native prevention contract requires the exact original operation and projected binding', () => {
  const input = { operationId: 'original-operation', nativeVersion: '0.154.0', method: 'account/logout', params: null,
    expectedDefinitionHash: hashJson({ native: 'binding' }) };
  validateAgent('PreventInput', input);
  for (const key of Object.keys(input)) { const missing = { ...input } as Record<string, unknown>; delete missing[key]; assert.throws(() => validateAgent('PreventInput', missing)); }
  for (const change of [{ nativeVersion: 'unknown' }, { expectedDefinitionHash: 'wrong' }, { callerPrincipalId: 'someone-else' }])
    assert.throws(() => validateAgent('PreventInput', { ...input, ...change }));
});
