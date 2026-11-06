import test from 'node:test';
import assert from 'node:assert/strict';
import { reactive } from 'vue';
import type { Agent, RpcClient } from '../packages/sdk/src/client.js';
import { accountExecutionDefaultsPath, accountExecutionProfile, emptyAccountExecutionDefaults, readAccountExecutionDefaults,
  readExecutionDefaults, withAccountExecutionProfile } from '../packages/ui-client/src/execution-defaults.js';

const backend: Agent.BackendIdentity = { backendId: 'primary', provider: 'codex', accountId: 'work' };
const defaults: Agent.AccountExecutionDefaults = { ...emptyAccountExecutionDefaults(), model: 'same-model', serviceTier: 'fast' };
const saved = withAccountExecutionProfile(null, backend, defaults);
test('execution defaults require the complete account identity and never fall back to a provider profile', () => {
  assert.deepEqual(accountExecutionProfile(saved, backend), defaults);
  assert.equal(accountExecutionProfile(saved, undefined), null);
  for (const other of [{ ...backend, accountId: 'personal' }, { ...backend, provider: 'claude' as const }, { ...backend, backendId: 'other' }])
    assert.equal(accountExecutionProfile(saved, other), null);
});
test('account default edits snapshot reactive values, preserve neighbours and remove empty overrides', () => {
  const neighbour: Agent.BackendIdentity = { ...backend, accountId: 'personal' };
  const source = withAccountExecutionProfile(saved, neighbour, { ...defaults, model: 'another-model' });
  const input = reactive({ ...defaults, effort: 'high' });
  const changed = withAccountExecutionProfile(source, backend, input);
  input.model = 'later-edit';
  assert.equal(accountExecutionProfile(changed, backend)?.model, 'same-model');
  assert.equal(accountExecutionProfile(source, backend)?.effort, null);
  assert.deepEqual(accountExecutionProfile(changed, neighbour), accountExecutionProfile(source, neighbour));
  const cleared = withAccountExecutionProfile(changed, backend, emptyAccountExecutionDefaults());
  assert.equal(accountExecutionProfile(cleared, backend), null);
  assert.equal(cleared.accounts.length, 1);
});
test('account defaults reject ambiguous identities and bound the complete document', () => {
  const ambiguous = { ...saved, accounts: [...saved.accounts, { backend, defaults: { ...defaults, effort: 'high' } }] };
  assert.throws(() => accountExecutionProfile(ambiguous, backend), /duplicate account/);
  assert.throws(() => withAccountExecutionProfile(ambiguous, backend, defaults), /duplicate account/);
  const full: Agent.AccountExecutionDefaultsDocument = { schemaVersion: 1, accounts: Array.from({ length: 128 }, (_, n) => ({ backend: { ...backend, accountId: 'account-' + n }, defaults })) };
  assert.throws(() => withAccountExecutionProfile(full, backend, defaults));
  assert.equal(withAccountExecutionProfile(full, full.accounts[0]!.backend, defaults).accounts.length, 128);
});
test('account default reads pin the document contract and revision and keep legacy data separate', async () => {
  const calls: { method: string; params: Record<string, unknown> }[] = [];
  let contractKey = 'agent/account-execution-defaults', version = '1.0.0', document: unknown = saved, archived = false;
  const client = { async request(method: string, params: Record<string, unknown>) {
    calls.push({ method, params });
    return method === 'objects.stat' ? { id: 'defaults', currentRevision: 7, effectivelyArchived: archived } :
      { object: { contractKey }, revision: { contractVersion: version }, content: { encoding: 'json', value: document } };
  } } as unknown as RpcClient;
  assert.deepEqual((await readAccountExecutionDefaults(client))?.document, saved);
  assert.deepEqual(calls, [{ method: 'objects.stat', params: { path: accountExecutionDefaultsPath } }, { method: 'objects.read', params: { objectId: 'defaults', revision: 7 } }]);
  version = '2.0.0'; await assert.rejects(readAccountExecutionDefaults(client), /different document contract/);
  version = '1.0.0'; contractKey = 'agent/execution-defaults';
  await assert.rejects(readAccountExecutionDefaults(client), /different document contract/);
  document = { schemaVersion: 1, codex: { model: 'old', mode: null, effort: null, permission: null }, claude: { model: null, mode: null, effort: null, permission: null } };
  assert.equal((await readExecutionDefaults(client))?.document.codex.model, 'old');
  archived = true; calls.length = 0;
  assert.equal(await readAccountExecutionDefaults(client), null); assert.equal(calls.length, 1);
});
test('missing defaults inherit while access failures remain visible', async () => {
  let code = 'not_found';
  const client = { async request() { throw Object.assign(new Error('No document access'), { code }); } } as unknown as RpcClient;
  assert.equal(await readAccountExecutionDefaults(client), null);
  code = 'forbidden'; await assert.rejects(readAccountExecutionDefaults(client), { code: 'forbidden' });
});
