import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { NativeContract } from '../packages/contracts/src/native-contract.js';
import { hashJson, digest } from '../packages/contracts/src/canonical.js';
import { toolDefinitionHash } from '../packages/contracts/src/tool-definition.js';
import { readNativeContract } from '../packages/sdk/src/native.js';
import { agentRegistry } from '../services/agent-manager/src/registry.js';
import type { RpcClient } from '../packages/sdk/src/client.js';
import type { Agent, Wire } from '../packages/contracts/src/generated.js';

const catalog = (version: string) => JSON.parse(readFileSync(`specs/native/codex-${version}/catalog.json`, 'utf8')) as Agent.Catalog;

test('shared native contract preserves exact arguments and rejects reserved, foreign and unrecognized values', () => {
  for (const version of ['0.154.0']) {
    const original = catalog(version), contract = new NativeContract(original);
    const params = { threadId: 'original', input: [{ type: 'text', text: 'An isolated contract example.' }] };
    contract.validateInput('turn/start', params);
    contract.validateInput('account/rateLimits/read', null);
    {
      contract.validateInput('account/rateLimits/read', {});
      contract.validateInput('account/rateLimits/read', { excludeResetCreditDetails: true, supportsLunaReserve: false });
      assert.throws(() => contract.validateInput('account/rateLimits/read', { supportsLunaReserve: 'yes' }));
    }
    assert.throws(() => contract.validateInput('account/rateLimits/read', { unrelated: true }));
    assert.throws(() => contract.validateTemplate('account/rateLimits/read', null, ['threadId']));
    contract.validateResult('turn/interrupt', {});
    contract.validateTemplate('turn/start', {}, ['threadId', 'input', 'clientUserMessageId']);
    assert.throws(() => contract.validateTemplate('turn/start', { threadId: 'replacement' }, ['threadId', 'input']));
    assert.throws(() => contract.validateInput('turn/start', { ...params, unrecognizedField: true }));
    assert.throws(() => contract.validateInput('turn/start', { ...params, input: [{ type: 'image', imageUrl: 'guessed' }] }));
    assert.throws(() => contract.validateResult('turn/interrupt', null));
    assert.throws(() => contract.definition('unrecognized/method'));
    const added = { ...params, serviceTierForTurn: null };
    contract.validateInput('turn/start', added);
    const definition = contract.definition('turn/start');
    assert.equal(contract.definitionHash('turn/start'), hashJson(definition));
    assert.throws(() => contract.definitionHash('unrecognized/method'));
    assert.throws(() => { definition.description = 'changed retained definition'; }, TypeError);
    assert.throws(() => { Object.assign(definition.inputSchema, { description: 'changed retained schema' }); }, TypeError);
    contract.verifyDefinition('turn/start', definition, hashJson(definition));
    const decorated = { ...definition, discovery: { group: 'codex/turns', summary: 'Start a native turn.' } };
    assert.equal(toolDefinitionHash(decorated), contract.definitionHash('turn/start'));
    contract.verifyDefinition('turn/start', decorated, toolDefinitionHash(decorated));
    assert.throws(() => contract.verifyDefinition('turn/start', { ...definition, interfaceVersion: 'another-version' }, hashJson(definition)));
    const changed = { ...definition, description: 'changed discovered definition' };
    assert.throws(() => contract.verifyDefinition('turn/start', changed, hashJson(changed)));
    assert.throws(() => contract.verifyDefinition('turn/start', definition, digest('other')));
    original.clientRequests.length = 0;
    assert.equal(contract.definitionHash('turn/start'), hashJson(definition));
    assert.ok(contract.definition('turn/start')); assert.equal(contract.catalogHash, hashJson(contract.catalog));
    const copy = contract.definitions() as Map<string, Wire.ToolDefinition>; copy.clear(); assert.ok(contract.definition('turn/start'));
  }
});

test('schema annotation removal preserves native properties named title, description and schema keywords', () => {
  const selected = catalog('0.154.0'), method = selected.clientRequests.find(item => item.method === 'turn/interrupt')!;
  method.inputSchema = { $ref: '#/$defs/Root', $defs: { Root: { type: 'object', description: 'Schema annotation',
    properties: { title: { type: 'string' }, description: { type: 'string' }, $id: { type: 'string' } }, required: ['title', 'description', '$id'] } } };
  const contract = new NativeContract(selected), value = { title: 'Native field', description: 'Native field', $id: 'Native field' };
  contract.validateInput(method.method, value);
  assert.throws(() => contract.validateInput(method.method, { title: value.title, $id: value.$id }));
  assert.throws(() => contract.validateInput(method.method, { ...value, title: 1 }));
  assert.throws(() => contract.validateInput(method.method, { ...value, additional: true }));
});

test('nullable outer arguments reject unknown fields while nested native extension maps stay open', () => {
  const selected = catalog('0.154.0'), method = selected.clientRequests.find(item => item.method === 'account/rateLimits/read')!;
  method.inputSchema = { anyOf: [{ $ref: '#/$defs/Root' }, { type: 'null' }], $defs: { Root: { type: 'object',
    properties: { extension: { type: 'object', additionalProperties: true } } } } };
  const contract = new NativeContract(selected);
  contract.validateInput(method.method, null);
  contract.validateInput(method.method, { extension: { providerOwned: { arbitrary: true } } });
  assert.throws(() => contract.validateInput(method.method, { extension: {}, unrelated: true }));
  assert.throws(() => contract.validateInput(method.method, { extension: 'invalid' }));
  assert.throws(() => contract.validateTemplate(method.method, null, ['threadId']));
});

test('SDK catalog discovery retains exact owner and epoch, with no native mutation or substituted catalog', async () => {
  const source = catalog('0.154.0'), registry = agentRegistry(source), calls: string[] = [];
  const target = { serviceNodeId: 'native-owner', hostId: 'host-a', nativeVersion: source.version, nativeExecutableHash: source.nativeExecutableHash, catalogHash: hashJson(source) };
  let epoch = 'original-epoch', observedCatalog = source, statusCount = 0, replaceEpoch = false, replaceOwner = false;
  const client = { async request(method: string, params: Record<string, unknown>) {
    if (method === 'tools.list') {
      const name = String(params['namePrefix']), definition = registry.namespaces[1]!.tools.find(item => item.name === name)!;
      return { provider: { node: { serviceNodeId: replaceOwner ? 'different-owner' : target.serviceNodeId } }, items: [{ qualifiedName: 'agent.' + name, definition, definitionHash: hashJson(definition) }], nextCursor: null };
    }
    assert.equal(method, 'tools.call'); assert.equal(params['operationId'], undefined); assert.equal(params['serviceNodeId'], target.serviceNodeId);
    const qualifiedName = String(params['qualifiedName']); calls.push(qualifiedName);
    if (qualifiedName === 'agent.catalog') return observedCatalog;
    assert.equal(qualifiedName, 'agent.status'); statusCount++;
    return { ...target, serviceNodeId: replaceOwner ? 'different-owner' : target.serviceNodeId,
      epoch: replaceEpoch && statusCount % 2 === 0 ? 'replacement-epoch' : epoch, pid: null, state: 'ready', observedAt: '2026-09-08T12:00:00.000Z',
      code: null, initialized: {}, pendingInputs: 0, operations: { retained: 0, maximum: 100, bytes: 0, maximumBytes: 1048576 }, observedMethods: [], windowsShell: null };
  } } as unknown as RpcClient;
  const result = await readNativeContract(client, target); assert.equal(result.epoch, epoch); assert.equal(result.contract.catalogHash, target.catalogHash);
  assert.deepEqual(calls, ['agent.status', 'agent.catalog', 'agent.status']);
  replaceEpoch = true; await assert.rejects(readNativeContract(client, target), { code: 'native_epoch_changed' }); replaceEpoch = false;
  observedCatalog = { ...source, sourceHash: digest('different-catalog') }; await assert.rejects(readNativeContract(client, target), { code: 'native_catalog_mismatch' }); observedCatalog = source;
  replaceOwner = true; await assert.rejects(readNativeContract(client, target), { code: 'native_owner_mismatch' }); replaceOwner = false;
  await assert.rejects(readNativeContract(client, { ...target, catalogHash: digest('other') }), { code: 'native_owner_mismatch' });
});
