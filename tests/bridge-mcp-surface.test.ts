import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { chatRegistry } from '../services/chat-bridge/src/registry.js';
import { PhoneBridge } from '../services/phone-bridge/src/runtime/bridge.js';
import type { PhoneFlow } from '../services/phone-bridge/src/runtime/flow.js';
import type { PhoneNativeClient } from '../services/phone-bridge/src/runtime/native.js';
import { phoneRegistry, validatePhoneService } from '../services/phone-bridge/src/runtime/registry.js';
import type { InvocationContext } from '../packages/sdk/src/service.js';

test('PhoneBridge exposes call and Voice controls while keeping diagnostics on ivy_dev', () => {
  const tools = phoneRegistry().namespaces[0]!.tools;
  const mcp = (name: string) => tools.find(tool => tool.name === name)?.discovery?.mcp?.name;
  assert.equal(mcp('status'), 'phone_bridge_status');
  assert.equal(mcp('voiceCall'), 'phone_bridge_call');
  assert.equal(mcp('call'), 'phone_bridge_status');
  assert.equal(mcp('operation'), 'phone_bridge_status');
  assert.equal(mcp('hangup'), 'phone_bridge_hangup');
  for (const [name, publicName] of [['selectVoice', 'phone_bridge_select_voice'], ['restartVoice', 'phone_bridge_restart_voice']]) {
    const tool = tools.find(tool => tool.name === name)!;
    assert.equal(mcp(name!), publicName);
    assert.equal(tool.discovery?.mcp?.surface, 'ivy');
    assert.equal(tool.annotations?.readOnlyHint, false);
    assert.equal(tool.annotations?.idempotentHint, true);
  }
  assert.equal(mcp('request'), undefined);
  for (const name of ['probeLoopback', 'audioSetup', 'codecTest', 'inventory', 'reconnect', 'logs', 'reconcileArchive']) {
    assert.ok(tools.some(tool => tool.name === name));
    assert.ok(mcp(name)?.startsWith('phone_bridge_'));
    assert.equal(tools.find(tool => tool.name === name)?.discovery?.mcp?.surface, 'ivy_dev');
  }
  assert.equal(tools.find(tool => tool.name === 'voiceCall')?.annotations?.readOnlyHint, false);
  assert.equal(tools.find(tool => tool.name === 'call')?.annotations?.readOnlyHint, true);
});

test('the public voice call requires a configured recipient and a nonblank initial prompt', async () => {
  const operationId = randomUUID();
  assert.throws(() => validatePhoneService('PhoneVoiceCall', { operationId, recipientId: 'user' }));
  assert.throws(() => validatePhoneService('PhoneVoiceCall', { operationId, recipientId: 'user', initialPrompt: ' \t\n ' }));
  assert.throws(() => validatePhoneService('PhoneVoiceCall', { operationId, destination: '+49123456789', initialPrompt: 'Hello' }));
  const args = { operationId, recipientId: 'user', initialPrompt: 'Please tell the user why I am calling.' };
  validatePhoneService('PhoneVoiceCall', args);
  let received: unknown[] | undefined;
  const marker = new Error('dispatched');
  const flow = { request: (...values: unknown[]) => { received = values; throw marker; } } as unknown as PhoneFlow;
  const bridge = new PhoneBridge(flow, {} as PhoneNativeClient);
  const context = { callerPrincipalId: 'agent', signal: new AbortController().signal } as InvocationContext;
  await assert.rejects(bridge.invoke('voiceCall', args, context), error => error === marker);
  assert.deepEqual(received, ['agent', operationId, 'user', 'voice', undefined, args.initialPrompt]);
});

test('ChatBridge status publishes the configured channels and binding revision schema', () => {
  const tools = chatRegistry().namespaces[0]!.tools;
  const workspace = tools.find(tool => tool.name === 'workspace')!;
  assert.equal(workspace.discovery?.mcp?.name, 'chat_bridge_status');
  assert.equal(workspace.discovery?.mcp?.surface, 'ivy_dev');
  assert.equal(tools.some(tool => tool.discovery?.mcp?.name === 'chat_bridge_workspace'), false);
  assert.equal(workspace.annotations?.readOnlyHint, true);
  const schema = JSON.stringify(workspace.outputSchema);
  assert.match(schema, /expectedBridge/);
  assert.match(schema, /definitionHash/);
  assert.match(schema, /channels/);
  assert.match(schema, /binding/);
});
