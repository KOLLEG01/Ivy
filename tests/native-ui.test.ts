import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { confirmsAbsence, enumStrings, modelsFrom, schemaNode, supportsFields } from '../packages/ui-client/src/native.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { agentRegistry } from '../services/agent-manager/src/registry.js';
import { hashJson } from '../packages/contracts/src/canonical.js';
import type { Agent, Wire } from '../packages/contracts/src/generated.js';
import { nativeImageBlob, nativeImagePath } from '../packages/ui-client/src/native-images.js';

test('agent image paths resolve against the owning Windows or Linux task, never the browser host', () => {
  for (const [source, cwd, expected] of [
    ['C:%5CUsers%5CAgent%5Cshot.png', '/srv/other', 'C:/Users/Agent/shot.png'],
    ['shots/light%20room%20(2).png', 'C:\\prj\\demo', 'C:/prj/demo/shots/light room (2).png'],
    ['../shots/light.png', '/srv/project', '/srv/shots/light.png'],
    ['file:///C:/prj/demo/light%20room.png', '/srv/other', 'C:/prj/demo/light room.png'],
    ['file:///tmp/light%20room.png', 'C:\\prj\\demo', '/tmp/light room.png'],
    ['https://tracker.example/image.png', '/srv/project', null],
    ['javascript:alert(1)', '/srv/project', null],
  ] as const) assert.equal(nativeImagePath(source, cwd), expected);
  assert.throws(() => nativeImagePath('shots/light.png', ''), /task folder/);
  assert.throws(() => nativeImagePath('/tmp/%00.png', '/srv/project'), /invalid/);
});

test('agent images validate decoded raster bytes instead of trusting an extension or declared media type', async () => {
  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=', 'base64');
  const blob = nativeImageBlob(png.toString('base64'));
  assert.equal(blob.type, 'image/png');
  assert.deepEqual(Buffer.from(await blob.arrayBuffer()), png);
  assert.throws(() => nativeImageBlob(Buffer.from('<svg onload="alert(1)"></svg>').toString('base64')), /not a supported image/);
  assert.throws(() => nativeImageBlob('invalid'), /invalid/);
  assert.throws(() => nativeImageBlob('a'.repeat(12000000)), /larger than 8 MiB/);
});

test('AgentUI never treats missing discovery/routing or another identity as proof a native operation was absent', () => {
  const call: Wire.ToolCall = { serviceNodeId: 'selected-owner', qualifiedName: 'codex.turn/start', operationId: 'original-intent', expectedDefinitionHash: 'sha256:' + 'a'.repeat(64), arguments: {} };
  const evidence = { kind: 'agent_operation_absent', serviceNodeId: 'selected-owner', operationId: 'original-intent', epoch: 'current-epoch' };
  assert.equal(confirmsAbsence(new IvyError('not_found', 'Missing namespace or tool.'), call), false);
  assert.equal(confirmsAbsence(new IvyError('not_found', 'Missing operation.', 'not_executed', evidence), call), true);
  for (const details of [{ ...evidence, serviceNodeId: 'another-owner' }, { ...evidence, operationId: 'another-operation' }, { ...evidence, epoch: false }, { ...evidence, kind: 'unrelated' }]) {
    assert.equal(confirmsAbsence(new IvyError('not_found', 'No matching proof.', 'not_executed', details), call), false);
  }
  assert.equal(confirmsAbsence(new IvyError('not_found', 'Uncertain provider response.', 'unknown', evidence), call), false);
});

test('AgentUI basic fields and exact native approval choices resolve from the complete supported catalog shape', () => {
  for (const version of ['0.154.0', '0.158.0', '0.159.2']) {
    const catalog = JSON.parse(readFileSync('specs/native/codex-' + version + '/catalog.json', 'utf8')) as Agent.Catalog;
    const registry = agentRegistry(catalog);
    for (const [method, supplied] of [['thread/start', ['cwd', 'model']], ['thread/resume', ['threadId']], ['turn/start', ['threadId', 'input', 'model', 'effort']], ['turn/interrupt', ['threadId', 'turnId']]] as const) {
      const definition = registry.namespaces[0]!.tools.find(d => d.name === method)!;
      const binding = { qualifiedName: 'codex.' + method, definition, definitionHash: hashJson(definition), serviceNodeId: 'explicit-owner' };
      assert.equal(supportsFields(binding, [...supplied]), true, version + ' ' + method);
      assert.equal(supportsFields(binding, [...supplied, 'inventedOverride']), false);
      const root = schemaNode(definition.inputSchema, definition.inputSchema);
      const changed = { ...binding, definition: { ...definition, inputSchema: { ...root, required: [...(root.required as string[] ?? []), 'newRequiredField'] } } };
      assert.equal(supportsFields(changed, [...supplied]), false);
    }
    const schema = catalog.serverRequests.find(m => m.method === 'item/commandExecution/requestApproval')!.outputSchema;
    const root = schemaNode(schema, schema), choices = enumStrings((root.properties as Record<string, unknown>)['decision'], schema);
    assert.deepEqual(new Set(choices), new Set(['accept', 'acceptForSession', 'decline', 'cancel']));
    assert.equal(choices.includes('acceptWithExecpolicyAmendment'), false, 'Structured policy amendments must not become guessed enum answers.');
  }
  assert.deepEqual(modelsFrom({ data: [{ id: 'actual', model: 'native-name', displayName: 'Actual native model', supportedReasoningEfforts: [{ reasoningEffort: 'provider-custom' }], defaultReasoningEffort: 'provider-custom' }] })[0]?.efforts, ['provider-custom']);
  assert.deepEqual(modelsFrom({ data: [{ id: 'hidden', model: 'hidden', hidden: true },
    { id: 'gpt-6.1-sol', model: 'gpt-6.1-sol', displayName: 'GPT-6.1 Sol' }] }).map(model => [model.id, model.hidden]),
    [['gpt-6.1-sol', false], ['hidden', true]]);
});
