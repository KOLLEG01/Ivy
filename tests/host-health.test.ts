import test from 'node:test';
import assert from 'node:assert/strict';
import { localHiveEndpoint } from '../packages/host-runtime/src/health.js';
import { HiveClient } from '../packages/sdk/src/client.js';

const config = { publicBaseUrl: 'https://ivy.example/ivy', settings: {
  listenHost: '0.0.0.0', listenPort: 39081, credentials: [],
  backup: { directory: '/data/backups', intervalHours: 24, retain: 7 }
} };
const bridge = { host: '172.20.0.1', port: 39081, containerPort: 39081 };

test('Docker Hive health uses its loopback binding independently of ingress ordering', () => {
  const local = { host: '127.0.0.1', port: 39082, containerPort: 39081 };
  for (const ports of [[bridge, local], [local, bridge]]) {
    const endpoint = localHiveEndpoint(config, { imageRepository: 'ivy', ports });
    assert.equal(endpoint.href, 'http://127.0.0.1:39082/ivy');
    assert.doesNotThrow(() => new HiveClient(endpoint.href));
  }
  assert.throws(() => new HiveClient('http://172.20.0.1:39081/ivy'), /HTTPS/);
});

test('Docker Hive health rejects absent or ambiguous local listener bindings', () => {
  for (const ports of [[bridge], [{ ...bridge, host: '127.0.0.1', containerPort: 9999 }],
    [{ ...bridge, host: '127.0.0.1' }, { ...bridge, host: '::1' }]]) {
    assert.throws(() => localHiveEndpoint(config, { imageRepository: 'ivy', ports }), /exactly one loopback/);
  }
});

test('Hive health retains wildcard, IPv6 and native local listener support', () => {
  for (const [host, expected] of [['0.0.0.0', '127.0.0.1'], ['::', '[::1]'], ['::1', '[::1]']]) {
    const endpoint = localHiveEndpoint(config, { imageRepository: 'ivy', ports: [{ ...bridge, host: host! }] });
    assert.equal(endpoint.hostname, expected);
    assert.doesNotThrow(() => new HiveClient(endpoint.href));
  }
  assert.equal(localHiveEndpoint(config).href, 'http://127.0.0.1:39081/ivy');
});
