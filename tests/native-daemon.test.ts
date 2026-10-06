import test from 'node:test';
import assert from 'node:assert/strict';
import { join, resolve } from 'node:path';
import { daemonObservation, initializedServerVersion } from '../services/agent-manager/src/daemon.js';
import { expectedServerHome, usesExternalAppServer } from '../packages/host-runtime/src/codex-home.js';
import { nativeCatalogPath, checkedNativeContract } from '../packages/contracts/src/checked-native-contract.js';
import { sharedNativeHomes } from '../packages/host-runtime/src/native-backup-paths.js';
import type { Host } from '../packages/contracts/src/generated.js';
import { readFileSync } from 'node:fs';
import { hashJson } from '../packages/contracts/src/canonical.js';
import { validateChatNativeDraft } from '../services/chat-bridge/src/native-plan.js';

test('daemon is the implicit mode while owned stdio remains explicit', () => {
  assert.equal(usesExternalAppServer({}), true);
  assert.equal(usesExternalAppServer({ appServer: { mode: 'external-proxy' } }), true);
  assert.equal(usesExternalAppServer({ appServer: { mode: 'owned-stdio' } }), false);
  const home = resolve('.local/example-codex-home');
  assert.equal(expectedServerHome({ codexHome: home }), home);
  assert.equal(expectedServerHome({ nativeHome: home }), home);
  const assigned = resolve('.local/explicit-server-home');
  assert.equal(expectedServerHome({ codexHome: home, appServer: { mode: 'external-proxy', socketPath: join(assigned, 'server.sock'), expectedCodexHome: assigned } }), assigned);
});

test('shared daemon homes never enter captured service storage, including the modern layout', () => {
  const root = resolve('.local/daemon-backup-example');
  const config = { runtimeRoot: join(root, 'runtime'), artifactRoot: join(root, 'artifacts'), stagingRoot: join(root, 'staging'), servicesRoot: join(root, 'services'),
    instances: [{ instanceId: 'agent', componentId: 'agent-manager', settings: { codexHome: join(root, 'services/agent-manager/data/native'), internalProjectRoot: join(root, 'projects') } }] } as unknown as Host.HostConfig;
  assert.throws(() => sharedNativeHomes(config), { code: 'target_conflict' });
  config.instances[0]!.settings['appServer'] = { mode: 'owned-stdio' };
  assert.deepEqual(sharedNativeHomes(config), []);
  config.instances[0]!.settings['appServer'] = { mode: 'external-proxy' };
  config.instances[0]!.settings['codexHome'] = join(root, 'shared-user');
  assert.equal(sharedNativeHomes(config)[0]?.path, join(root, 'shared-user'));
  config.instances[0]!.settings['codexHome'] = root;
  assert.throws(() => sharedNativeHomes(config), { code: 'target_conflict' });
});

test('lifecycle refuses an old running server even when local CLI and installed binary are current', () => {
  const good = { status: 'alreadyRunning', socketPath: resolve('server.sock'), managedCodexPath: process.execPath,
    managedCodexVersion: '0.154.0', cliVersion: '0.154.0', appServerVersion: '0.154.0' };
  const observed = daemonObservation(good, '0.154.0');
  assert.equal(observed.status, 'alreadyRunning'); assert.equal(observed.pid, null);
  for (const key of ['managedCodexVersion', 'cliVersion', 'appServerVersion']) {
    assert.throws(() => daemonObservation({ ...good, [key]: '0.153.4' }, '0.154.0'), { code: 'native_server_version_mismatch' });
    assert.throws(() => daemonObservation({ ...good, [key]: null }, '0.154.0'), { code: 'native_server_version_mismatch' });
  }
  for (const change of [{ socketPath: 'relative.sock' }, { managedCodexPath: 'relative.exe' }, { status: 'bootstrapped' }, { pid: 0 }, { pid: '123' }])
    assert.throws(() => daemonObservation({ ...good, ...change }, '0.154.0'), { code: 'native_daemon_response_invalid' });
  assert.equal(daemonObservation({ ...good, status: 'started', pid: 42 }, '0.154.0').pid, 42);
});

test('version is taken from the actual initialization response, never the caller title suffix', () => {
  assert.equal(initializedServerVersion('ivy-agent-manager/0.154.0 (Windows; x86_64) unknown (ivy-agent-manager; 0.1.0)'), '0.154.0');
  assert.equal(initializedServerVersion('Codex Desktop/0.154.0 (Debian 12.0.0; x86_64) unknown (ivy_rollout_inventory; 1)'), '0.154.0');
  assert.equal(initializedServerVersion('Codex Desktop/0.153.4 (Linux; x86_64) client/0.154.0'), '0.153.4');
  assert.equal(initializedServerVersion('ivy-agent-manager/0.153.4 (Linux; x86_64) client/0.154.0'), '0.153.4');
  for (const value of [undefined, {}, '', '0.154.0', 'ivy-agent-manager', 'ivy-agent-manager/latest', 'ivy-agent-manager/0.154.0-extra'])
    assert.throws(() => initializedServerVersion(value), { code: 'native_server_version_unavailable' });
});

test('the admitted Linux 0.154 catalog selects its Linux executable identity', () => {
  const path = nativeCatalogPath(resolve('.'), '0.154.0', 'linux', 'x64');
  assert.equal(path, resolve('specs/native/codex-0.154.0/catalog.linux-x64.json'));
  const catalog = checkedNativeContract('0.154.0');
  assert.equal(catalog.catalog.nativeExecutableHash, process.platform === 'linux'
    ? 'sha256:3188814c35471432d4123203e0eb38e5bddc60226e3d7ddf0e59e649ea140022'
    : 'sha256:be96b992178b1e467c225800da0d65f2c86d5eba1ef0b14632f65db381cbdfde');
  assert.throws(() => nativeCatalogPath(resolve('.'), '../0.154.0'), { code: 'native_version_unsupported' });
});

test('cross-host plans select the retained target catalog instead of the workflow host platform', () => {
  const linux = JSON.parse(readFileSync(resolve('specs/native/codex-0.154.0/catalog.linux-x64.json'), 'utf8'));
  const windows = JSON.parse(readFileSync(resolve('specs/native/codex-0.154.0/catalog.json'), 'utf8'));
  assert.notEqual(linux.sourceHash, windows.sourceHash);
  for (const catalog of [linux, windows]) {
    for (const identity of [{ sourceHash: catalog.sourceHash }, { catalogHash: hashJson(catalog) }, { nativeExecutableHash: catalog.nativeExecutableHash }]) {
      const selected = checkedNativeContract('0.154.0', identity);
      assert.equal(selected.catalogHash, hashJson(catalog)); assert.equal(selected.catalog.nativeExecutableHash, catalog.nativeExecutableHash);
    }
    validateChatNativeDraft({ nativeVersion: '0.154.0', catalogSourceHash: catalog.sourceHash, method: 'turn/start', params: { threadId: 'target-thread', input: [{ type: 'text', text: 'Target-platform request' }] } });
  }
  assert.throws(() => checkedNativeContract('0.154.0', { sourceHash: linux.sourceHash, nativeExecutableHash: windows.nativeExecutableHash }), { code: 'native_catalog_mismatch' });
  assert.throws(() => checkedNativeContract('0.154.0', { catalogHash: hashJson('unretained') }), { code: 'native_catalog_mismatch' });
});
