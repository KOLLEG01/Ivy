import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { win32, posix } from 'node:path';

// Deployment identities are supplied by the lab operator, never distribution defaults.
export function coreConfiguration(input, credential = randomBytes(32).toString('hex')) {
  const { os, root, hostId, publicBaseUrl, executables, principalId } = input;
  assert.ok(['win32', 'linux'].includes(os));
  const path = os === 'win32' ? win32 : posix;
  assert.ok(path.isAbsolute(root));
  assert.ok(hostId && principalId);
  assert.match(credential, /^[a-f0-9]{64}$/);
  const url = new URL(publicBaseUrl);
  assert.equal(url.protocol, 'https:');
  assert.equal(url.pathname, '/ivy');
  assert.ok(!url.username && !url.password && !url.search && !url.hash);
  for (const name of ['node', 'git', 'tar']) assert.ok(path.isAbsolute(executables[name]));
  const configPath = path.join(root, 'config.json');
  const components = os === 'linux' ? ['host-executor', 'hive', 'service-manager'] : ['host-executor', 'service-manager'];
  const settings = {
    'host-executor': { hostConfigPath: configPath },
    'service-manager': { hostConfigPath: configPath },
    hive: {
      listenHost: '127.0.0.1', listenPort: input.listenPort,
      trustedProxyAddresses: ['127.0.0.1'],
      credentials: [{ principalId, token: credential }],
      packagePublisherPrincipalIds: [principalId],
      backup: { directory: path.join(root, 'backups', 'hive'), intervalHours: 24, retain: 7 },
    },
  };
  return {
    schemaVersion: 1, hostId, ivyRoot: root, configPath,
    servicesRoot: path.join(root, 'services'), runtimeRoot: path.join(root, 'runtime'),
    artifactRoot: path.join(root, 'artifacts'), stagingRoot: path.join(root, 'staging'),
    publicBaseUrl, executables, packageUpdates: { intervalSeconds: 60 },
    instances: components.map(componentId => ({
      componentId, instanceId: `${hostId}-${componentId}`, serviceNodeId: `${hostId}.${componentId}`,
      enabled: false, engine: 'process', credential,
      ...(os === 'win32' && componentId === 'service-manager' ? { process: { allowWindowsBreakaway: true } } : {}),
      ...(componentId === 'hive' ? { secretPaths: ['/settings/credentials'] } : {}),
      settings: settings[componentId],
    })),
  };
}

export function validateEmittedSettings(config, validateHost) {
  // Validate what will be serialized, including required settings omitted by a wrong lookup.
  validateHost('HostConfigInput', config);
  for (const instance of config.instances) {
    assert.ok(instance.settings, `Missing settings for ${instance.componentId}`);
    validateHost(instance.componentId === 'hive' ? 'HiveSettings' : 'ManagerSettings', instance.settings);
  }
}

export function assertLaneIsolation(lanes) {
  assert.equal(lanes.length, 2);
  const seen = new Set();
  const unique = (kind, value) => { const key = `${kind}:${value}`; assert.ok(!seen.has(key), `Shared ${kind}`); seen.add(key); };
  for (const lane of lanes) {
    unique('origin', new URL(lane.linux.publicBaseUrl).origin);
    assert.equal(lane.linux.publicBaseUrl, lane.windows.publicBaseUrl);
    const hive = lane.linux.instances.find(x => x.componentId === 'hive');
    assert.ok(hive);
    assert.equal(hive.settings.credentials[0].token, hive.credential);
    unique('credential', hive.credential);
    unique('principal', hive.settings.credentials[0].principalId);
    for (const [os, config] of [['linux', lane.linux], ['win32', lane.windows]]) {
      const path = os === 'win32' ? win32 : posix;
      const root = path.resolve(config.ivyRoot);
      assert.ok(root !== path.parse(root).root, 'A filesystem root is not a lab root');
      unique('host', config.hostId);
      unique('root', os === 'win32' ? root.toLowerCase() : root);
      for (const key of ['configPath', 'servicesRoot', 'runtimeRoot', 'artifactRoot', 'stagingRoot']) {
        const relative = path.relative(root, config[key]);
        assert.ok(relative && !relative.startsWith('..') && !path.isAbsolute(relative), `${key} escapes lab root`);
      }
      for (const instance of config.instances) {
        assert.equal(instance.enabled, false);
        assert.equal(instance.engine, 'process');
        assert.equal(instance.credential, hive.credential);
        unique('instance', instance.instanceId); unique('node', instance.serviceNodeId);
      }
    }
  }
  for (const os of ['linux', 'windows']) {
    const path = os === 'windows' ? win32 : posix;
    const a = path.resolve(lanes[0][os].ivyRoot), b = path.resolve(lanes[1][os].ivyRoot);
    for (const [left, right] of [[a,b],[b,a]]) {
      const relative = path.relative(left,right);
      assert.ok(relative.startsWith('..') || path.isAbsolute(relative), 'Lane roots must not overlap');
    }
  }
}
