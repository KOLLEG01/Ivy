import test from 'node:test';
import assert from 'node:assert/strict';
import type { Operation, RpcClient } from '../packages/sdk/src/client.js';
import { nativeHosts, nativeServiceNodes } from '../packages/ui-client/src/native-hosts.js';

test('native host discovery shares a complete bounded inventory and groups distinct services by physical host', async () => {
  const nodes = [{ serviceNodeId: 'host.first', hostId: 'host', connected: true, ready: false },
    { serviceNodeId: 'host.second', hostId: 'host', connected: true, ready: true }];
  const calls: Record<string, unknown>[] = [];
  const client = { async request(_method: string, params: Record<string, unknown>) {
    calls.push(params); return { items: [nodes[params['cursor'] ? 1 : 0]], nextCursor: params['cursor'] ? null : 'next' };
  } } as unknown as RpcClient;
  const found = await nativeServiceNodes(client, undefined, 'host');
  assert.deepEqual(found, nodes); assert.equal(calls[0]!['hostId'], 'host');
  assert.deepEqual(nativeHosts(found), [{ hostId: 'host', serviceNodeId: 'host.second', connected: true, ready: true, nodes }]);
});
test('native host discovery refuses cycles and excessive empty pages instead of returning partial owners', async () => {
  let calls = 0, cycle = true;
  const client = { async request() { calls++; return { items: [] as Operation.ServiceNode[], nextCursor: cycle ? 'same' : 'empty-' + calls }; } } as unknown as RpcClient;
  await assert.rejects(nativeServiceNodes(client), { code: 'native_hosts_unavailable' }); assert.equal(calls, 2);
  cycle = false; calls = 0;
  await assert.rejects(nativeServiceNodes(client), { code: 'native_hosts_unavailable' }); assert.equal(calls, 20);
});
