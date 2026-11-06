import test from 'node:test';
import assert from 'node:assert/strict';
import { nativeOwnerKey, nativeResourceKey, nativeRouteOwner, nativeRouteQuery, nativeStorageOwner } from '../packages/ui-client/src/native-route.js';
import { nativeHosts } from '../packages/ui-client/src/native-hosts.js';

const backend = { backendId: 'secondary', provider: 'claude' as const, accountId: 'personal' };
test('native routes retain all owner fields and reject incomplete, foreign or inferred selectors', () => {
  const owner = { serviceNodeId: 'owner', namespace: backend.backendId, backend };
  const query = new URLSearchParams(nativeRouteQuery(owner, { id: 'same-thread', turn: 'same-turn' }));
  assert.deepEqual(nativeRouteOwner(query), owner);
  for (const missing of ['backend', 'provider', 'account']) {
    const invalid = new URLSearchParams(query); invalid.delete(missing);
    assert.throws(() => nativeRouteOwner(invalid));
  }
  const wrong = new URLSearchParams(query); wrong.set('namespace', 'primary');
  assert.throws(() => nativeRouteOwner(wrong), /namespace/);
  assert.deepEqual(nativeRouteOwner(new URLSearchParams('node=owner&id=same-thread')), { serviceNodeId: 'owner', namespace: 'codex' });
  assert.throws(() => nativeRouteOwner(new URLSearchParams('node=owner&namespace=secondary')), /missing/);
  assert.equal(nativeStorageOwner('owner'), 'owner', 'Unscoped retained mutations keep their original address.');
  for (const other of [undefined, { ...backend, provider: 'codex' as const }, { ...backend, accountId: 'work' }]) {
    assert.notEqual(nativeStorageOwner('owner', other), nativeStorageOwner('owner', backend));
    assert.notEqual(nativeResourceKey({ ...owner, kind: 'thread', nativeId: 'same-thread' }, other), nativeResourceKey({ ...owner, kind: 'thread', nativeId: 'same-thread' }, backend));
  }
  assert.notEqual(nativeOwnerKey(owner), nativeOwnerKey({ ...owner, namespace: 'primary' }));
});

test('physical host presentation keeps service owners and healthy siblings separate', () => {
  const nodes = [
    { serviceNodeId: 'host.first', hostId: 'host', connected: true, ready: false },
    { serviceNodeId: 'host.second', hostId: 'host', connected: true, ready: true },
    { serviceNodeId: 'offline.first', hostId: 'offline', connected: false, ready: false },
  ];
  const hosts = nativeHosts([...nodes, nodes[0]!]);
  assert.equal(hosts.length, 2); assert.equal(hosts[0]!.serviceNodeId, 'host.second');
  assert.equal(hosts[0]!.ready, true); assert.deepEqual(hosts[0]!.nodes, nodes.slice(0, 2));
  assert.equal(hosts[1]!.connected, false);
});
