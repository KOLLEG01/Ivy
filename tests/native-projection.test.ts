import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { gunzipSync } from 'node:zlib';
import { generateProjection } from '../tools/contracts/native-projection.mjs';
import { SchemaValidators } from '../packages/contracts/src/schema.js';
import { validateAgent } from '../packages/contracts/src/validation.js';
import { digest } from '../packages/contracts/src/canonical.js';
import type { Schema } from '../packages/contracts/src/types.js';
import type { Agent } from '../packages/contracts/src/generated.js';

const input = (version: string) => JSON.parse(gunzipSync(readFileSync(`specs/native/codex-${version}/source.json.gz`), { maxOutputLength: 32 * 1024 * 1024 }).toString('utf8')) as {
  json: Record<string, string>; typescript: Record<string, string>; commonRust: string;
};
const catalog = (version: string) => JSON.parse(readFileSync(`specs/native/codex-${version}/catalog.json`, 'utf8')) as Agent.Catalog;

test('the complete installed native exports regenerate and compile without excluding legacy methods, notifications or alternatives', { timeout: 60_000 }, () => {
  for (const [version, requests, notifications] of [['0.154.0', 162, 83], ['0.158.0', 170, 85], ['0.159.2', 170, 85]] as const) {
    const saved = catalog(version), actual = generateProjection(input(version));
    assert.deepEqual(actual, saved); validateAgent('Catalog', saved);
    assert.equal(saved.clientRequests.length, requests); assert.equal(saved.serverRequests.length, 11);
    assert.equal(saved.serverNotifications.length, notifications); assert.equal(saved.clientNotifications.length, 1);
    const validators = new SchemaValidators();
    for (const group of [saved.clientRequests, saved.serverRequests, saved.serverNotifications, saved.clientNotifications]) {
      assert.equal(new Set(group.map(value => value.method)).size, group.length);
      for (const value of group) {
        validators.compile(value.inputSchema as Schema);
        if ('outputSchema' in value) validators.compile(value.outputSchema as Schema);
      }
    }
    const auth = saved.clientRequests.find(value => value.method === 'getAuthStatus')!;
    validators.validate(auth.inputSchema as Schema, {});
    validators.validate(auth.inputSchema as Schema, { includeToken: null, refreshToken: false });
    assert.throws(() => validators.validate(auth.inputSchema as Schema, { includeToken: 'yes' }));
    const diff = saved.clientRequests.find(value => value.method === 'gitDiffToRemote')!;
    validators.validate(diff.inputSchema as Schema, { cwd: '/an/actual/host/path' });
    assert.throws(() => validators.validate(diff.inputSchema as Schema, { cwd: 123 }));
    const summary = saved.clientRequests.find(value => value.method === 'getConversationSummary')!;
    validators.validate(summary.inputSchema as Schema, { conversationId: 'native-thread' });
    validators.validate(summary.inputSchema as Schema, { rolloutPath: '/native/session.jsonl' });
    assert.throws(() => validators.validate(summary.inputSchema as Schema, {}));
    const rateLimits = saved.clientRequests.find(value => value.method === 'account/rateLimits/read')!;
    assert.equal(rateLimits.paramsRequired, false); validators.validate(rateLimits.inputSchema as Schema, null);
    validators.validate(rateLimits.inputSchema as Schema, {});
    assert.equal(saved.clientRequests.find(value => value.method === 'config/value/write')!.responseType, 'v2::ConfigWriteResponse');
    assert.ok(saved.serverNotifications.some(value => value.method === 'rawResponseItem/completed'));
  }
});

test('native generation fails on source/export coverage, unsupported constructs and unresolved schema references', () => {
  const changed = input('0.154.0');
  changed.commonRust = changed.commonRust.replace('ThreadStart => "thread/start"', 'ThreadStart => "missing/renamed"');
  assert.throws(() => generateProjection(changed), /method mappings differ/);
  const duplicate = input('0.154.0');
  duplicate.typescript['ClientRequest.ts'] = duplicate.typescript['ClientRequest.ts']!.replace('"method": "thread/start"', '"method": "initialize"');
  assert.throws(() => generateProjection(duplicate));
  const invalid = input('0.154.0'), schema = JSON.parse(invalid.json['ClientRequest.json']!) as { oneOf: { properties: { params: Record<string, unknown> } }[] };
  schema.oneOf[0]!.properties.params['$ref'] = '#/definitions/DoesNotExist';
  invalid.json['ClientRequest.json'] = JSON.stringify(schema);
  assert.throws(() => generateProjection(invalid), /Unresolved native JSON reference/);
  const unsupported = input('0.154.0'), root = JSON.parse(unsupported.json['v2/ThreadStartResponse.json']!) as Record<string, unknown>;
  root['unhandledAssertion'] = true; unsupported.json['v2/ThreadStartResponse.json'] = JSON.stringify(root);
  assert.throws(() => generateProjection(unsupported), /Unsupported native JSON keyword/);
  const legacy = input('0.154.0');
  legacy.typescript['GitDiffToRemoteParams.ts'] = legacy.typescript['GitDiffToRemoteParams.ts']!.replace('cwd: string', 'cwd: [string, string]');
  assert.throws(() => generateProjection(legacy), /Unsupported generated native TypeScript construct/);
  const rejecting = input('0.154.0'); rejecting.json['v2/ThreadStartResponse.json'] = 'false';
  assert.equal((generateProjection(rejecting) as Agent.Catalog).clientRequests.find(value => value.method === 'thread/start')!.outputSchema, false);
});

test('native outcome and answer contracts cannot invent success, omit dispatch identity or mix native reply variants', () => {
  const operation: Agent.Operation = { schemaVersion: 1, operationId: 'original-native-action', callerPrincipalId: 'caller', serviceNodeId: 'owner', nativeVersion: '0.154.0',
    nativeExecutableHash: digest('fixture-executable'), method: 'turn/start', params: { threadId: 'native-thread', input: [] }, requestHash: digest('fixture-request'),
    phase: 'accepted', createdAt: '2026-09-05T00:00:00Z', updatedAt: '2026-09-05T00:00:00Z', epoch: null, requestId: null, reply: null, code: null };
  validateAgent('Operation', operation);
  assert.throws(() => validateAgent('Operation', { ...operation, phase: 'succeeded' }));
  assert.throws(() => validateAgent('Operation', { ...operation, phase: 'dispatched' }));
  const sent = { ...operation, epoch: 'native-epoch', requestId: 1 };
  validateAgent('Operation', { ...sent, phase: 'dispatched' });
  validateAgent('Operation', { ...sent, phase: 'succeeded', reply: { result: { turn: { id: 'real-native-turn' } } } });
  assert.throws(() => validateAgent('Operation', { ...sent, phase: 'outcome_unknown', reply: { result: {} }, code: 'connection_lost' }));
  validateAgent('Operation', { ...sent, phase: 'outcome_unknown', code: 'connection_lost' });
  assert.throws(() => validateAgent('Reply', { result: {}, error: { code: -1, message: 'mixed' } }));
  const pending: Agent.PendingInput = { identity: { serviceNodeId: 'owner', epoch: 'native-epoch', requestId: '1' }, method: 'item/tool/requestUserInput',
    params: {}, observedAt: operation.createdAt, updatedAt: operation.createdAt, threadId: 'native-thread', turnId: 'turn', state: 'pending', answerOperationId: null, answerCallerPrincipalId: null, reply: null, code: null };
  validateAgent('PendingInput', pending);
  validateAgent('InputIdentity', { ...pending.identity, requestId: -1 });
  validateAgent('InputIdentity', { ...pending.identity, requestId: 'native-id'.repeat(100) });
  assert.throws(() => validateAgent('InputIdentity', { ...pending.identity, requestId: Number.MAX_SAFE_INTEGER + 1 }));
  assert.throws(() => validateAgent('PendingInput', { ...pending, state: 'answered' }));
  assert.notDeepEqual(pending.identity, { ...pending.identity, requestId: 1 });
});
