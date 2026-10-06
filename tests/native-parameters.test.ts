import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { hashJson } from '../packages/contracts/src/canonical.js';
import { NativeContract } from '../packages/contracts/src/native-contract.js';
import { saveNativePlan, readNativePlan, validateNativePlanDraft } from '../packages/sdk/src/native-plan.js';
import { readNativeParameters, saveNativeParameters } from '../packages/sdk/src/native-parameters.js';
import { nativeThreadProject } from '../packages/sdk/src/native-project.js';
import type { Agent } from '../packages/contracts/src/generated.js';
import type { OperationName, Params, RpcClient } from '../packages/sdk/src/client.js';

const selected = (version = '0.154.0') => new NativeContract(JSON.parse(readFileSync(`specs/native/codex-${version}/catalog.json`, 'utf8')) as Agent.Catalog);
const noIo = (): RpcClient => ({ request: async <M extends OperationName>(_method: M, _params: Params<M>) => { throw new Error('Native parameter envelopes must not use Hive I/O.'); } }) as RpcClient;

test('native plans preflight templates and keep bounded validated parameter envelopes inline', async () => {
  const contract = selected(), client = noIo(), draft: Agent.PlanDraft = { nativeVersion: contract.catalog.version, catalogSourceHash: contract.catalog.sourceHash,
    definitions: { threadStart: hashJson(contract.definition('thread/start')), threadResume: hashJson(contract.definition('thread/resume')),
      turnStart: hashJson(contract.definition('turn/start')), turnInterrupt: hashJson(contract.definition('turn/interrupt')) },
    threadStart: { cwd: '/fixture' }, threadResume: {}, turnStart: { input: [{ type: 'text', text: 'original', text_elements: [] }] } };
  for (const invalid of [{ ...draft, turnStart: { input: [], threadId: 'other' } }, { ...draft, threadResume: { path: '/replacement' } },
    { ...draft, threadStart: { invented: true } }, { ...draft, definitions: { ...draft.definitions, turnStart: 'sha256:' + '0'.repeat(64) } }])
    await assert.rejects(saveNativePlan(client, contract, { draft: invalid, kind: 'task-board', parentId: null, mutationId: 'ignored' }));
  const plan = await saveNativePlan(client, contract, { draft, kind: 'task-board', parentId: null, mutationId: 'ignored' });
  assert.deepEqual(await readNativePlan(client, contract, { plan, parentId: null, kind: 'task-board' }), draft);
  assert.deepEqual(plan.turnStart.params, draft.turnStart);
  assert.equal(JSON.stringify(plan).includes('objectId'), false);
  const chat = { ...draft, turnStart: {} }; validateNativePlanDraft(contract, chat, 'chat');
  assert.throws(() => validateNativePlanDraft(contract, { ...chat, turnStart: { input: [] } }, 'chat'));
});

test('internal plans pin native project membership before retaining start parameters', async () => {
  const contract = selected('0.158.0');
  let projectId = 'native-internal', reads = 0;
  const client = { request: async (method: string, args: { qualifiedName?: string }) => {
    if (method === 'tools.list') return { provider: { node: { serviceNodeId: 'agent' } }, items: [
      { qualifiedName: 'agent.projects', definitionHash: hashJson('projects'), definition: { interfaceVersion: '1.0.0' } }], nextCursor: null };
    assert.equal(method, 'tools.call'); assert.equal(args.qualifiedName, 'agent.projects'); reads++;
    return { source: 'native', observedAt: new Date().toISOString(), projects: [
      { nativeId: projectId, source: 'native', name: 'Internal', paths: ['/internal'] }] };
  } } as unknown as RpcClient;
  const draft: Agent.PlanDraft = { nativeVersion: contract.catalog.version, catalogSourceHash: contract.catalog.sourceHash,
    definitions: { threadStart: contract.definitionHash('thread/start'), threadResume: contract.definitionHash('thread/resume'),
      turnStart: contract.definitionHash('turn/start'), turnInterrupt: contract.definitionHash('turn/interrupt') },
    location: { serviceNodeId: 'agent', hostId: 'host', kind: 'internal', cwd: '/internal/main', projectId: 'allocation' },
    threadStart: { cwd: '/internal/main' }, threadResume: {}, turnStart: {} };
  const prepared = { ...draft, threadStart: await nativeThreadProject(client, 'agent', draft.threadStart) };
  prepared.location = { ...draft.location!, projectId: String(prepared.threadStart['projectId']) };
  const plan = await saveNativePlan(client, contract, { draft: prepared, kind: 'chat', parentId: null, mutationId: 'ignored' });
  assert.equal(plan.location?.projectId, 'native-internal'); assert.deepEqual(plan.threadStart.params, { ...draft.threadStart, projectId: 'native-internal' });
  assert.equal(draft.threadStart['projectId'], undefined);
  projectId = 'changed-default';
  const recovered = await readNativePlan(client, contract, { plan, kind: 'chat', parentId: null });
  assert.deepEqual(recovered, prepared);
  assert.equal(recovered.threadStart['projectId'], 'native-internal'); assert.equal(reads, 1);
});

test('native parameter envelopes preserve null and reject altered method, catalog and payload', async () => {
  const contract = selected(), client = noIo();
  const input = { method: 'account/rateLimits/read', params: null, parentId: 'irrelevant-local-owner', mutationId: 'ignored' };
  const saved = await saveNativeParameters(client, contract, input);
  assert.equal(await readNativeParameters(client, contract, { ...input, ref: saved }), null);
  await assert.rejects(saveNativeParameters(client, contract, { ...input, params: { unrelated: true } }), { code: 'invalid_arguments' });
  await assert.rejects(readNativeParameters(client, contract, { ...input, method: 'thread/start', ref: saved }), { code: 'native_parameters_contract_mismatch' });
  await assert.rejects(readNativeParameters(client, contract, { ...input, ref: { ...saved, catalogHash: 'sha256:' + '0'.repeat(64) } }), { code: 'native_parameters_contract_mismatch' });
});

test('native parameter admission rejects invalid and oversized data before external I/O', async () => {
  const contract = selected(), client = noIo(), input = { method: 'thread/start', params: { model: 'gpt-5.6-luna' }, parentId: null, mutationId: 'ignored' };
  const saved = await saveNativeParameters(client, contract, input);
  assert.deepEqual(await readNativeParameters(client, contract, { ...input, ref: saved }), input.params);
  await assert.rejects(saveNativeParameters(client, contract, { ...input, params: { invented: 'value' } }), { code: 'invalid_arguments' });
  await assert.rejects(saveNativeParameters(client, contract, { ...input, params: { model: 'x'.repeat(1048576) } }), { code: 'content_too_large' });
});
