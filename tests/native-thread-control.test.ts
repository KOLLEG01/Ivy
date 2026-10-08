import test from 'node:test';
import assert from 'node:assert/strict';
import { digest } from '../packages/contracts/src/canonical.js';
import { readNativeThreadControl } from '../packages/ui-client/src/native-thread-control.js';
import type { RpcClient } from '../packages/sdk/src/client.js';

function fixture(version = '0.154.0') {
  const status = { hostId: 'host', serviceNodeId: 'owner', state: 'ready', epoch: 'epoch', nativeVersion: version, nativeExecutableHash: digest('native'), catalogHash: digest('catalog') };
  const thread: Record<string, unknown> = { id: 'thread', status: { type: 'idle' } };
  let reads = 0;
  const behavior = { changeEpoch: false };
  const client = { async request(method: string, raw: Record<string, unknown>) {
    if (method === 'serviceNodes.get') return { connected: true, synced: true, ready: true, desiredEnabled: true };
    if (method === 'system.status') return { runtimeEpoch: '11111111-1111-4111-8111-111111111111' };
    if (method === 'tools.list') return { provider: { node: { serviceNodeId: 'owner' } }, items: [{ qualifiedName: raw.namespace + '.' + raw.namePrefix, definitionHash: digest('definition'), definition: {} }], nextCursor: null };
    assert.equal(method, 'tools.call');
    if (raw.qualifiedName === 'agent.status') return { ...status, epoch: behavior.changeEpoch && reads++ ? 'changed' : status.epoch };
    assert.equal(raw.qualifiedName, 'codex.thread/read'); return { thread };
  } } as unknown as RpcClient;
  return { thread, behavior, read: () => readNativeThreadControl(client, 'owner', 'thread', new AbortController().signal) };
}

test('native capability alone establishes direct input; missing, denied and unsupported ownership stays unavailable', async () => {
  for (const capability of [undefined, null, false, true]) {
    const f = fixture(); if (capability !== undefined) f.thread.canAcceptDirectInput = capability;
    const result = await f.read(); assert.equal(result.attached, capability === true);
    assert.equal(result.source, capability === true ? 'native-capability' : 'unconfirmed');
  }
  await assert.rejects(fixture('0.149.1').read(), /not ready or supported/);
  const changing = fixture(); changing.behavior.changeEpoch = true; await assert.rejects(changing.read(), /connection changed/);
  const foreign = fixture(); foreign.thread.id = 'another'; await assert.rejects(foreign.read(), /another task/);
});
