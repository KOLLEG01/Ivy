import assert from 'node:assert/strict';
import { test } from 'node:test';
import { resolveCodexHome, instanceOwnedCodexHome } from '../packages/host-runtime/src/codex-home.js';
import { runtimeEnvironment } from '../packages/host-runtime/src/process.js';
import { hostConfig } from '../packages/host-runtime/src/host-config.js';
import { mkdtemp, mkdir, writeFile, symlink, rm, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

test('Codex home resolution preserves explicit homes and otherwise uses the executing account on both platforms', () => {
  for (const [platform, home, explicit, inherited] of [
    ['linux', '/home/ivy', '/srv/ivy/codex', '/home/other/codex'],
    ['win32', 'C:\\Users\\Ivy', 'D:\\Ivy\\codex', 'C:\\Users\\Other\\codex'],
  ] as const) {
    const options = { platform, userHome: home, environment: { CODEX_HOME: inherited } };
    assert.deepEqual(resolveCodexHome({ codexHome: explicit }, options), { path: explicit, source: 'host-configuration' });
    assert.deepEqual(resolveCodexHome({ nativeHome: explicit }, options), { path: explicit, source: 'existing-native-home' });
    assert.deepEqual(resolveCodexHome({}, options), { path: inherited, source: 'environment' });
    const result = resolveCodexHome({}, { ...options, environment: {} });
    assert.equal(result.path, home + (platform === 'win32' ? '\\.codex' : '/.codex'));
    assert.equal(result.source, 'user-home');
    assert.throws(() => resolveCodexHome({ codexHome: explicit, nativeHome: inherited }, options));
    assert.equal(options.environment.CODEX_HOME, inherited);
  }
});

test('Windows default uses USERPROFILE and whitespace-only CODEX_HOME does not select a home', () => {
  assert.equal(resolveCodexHome({}, { platform: 'win32', environment: { USERPROFILE: 'D:\\User', CODEX_HOME: '  ' } }).path, 'D:\\User\\.codex');
  assert.equal(resolveCodexHome({}, { platform: 'linux', environment: { CODEX_HOME: './profile' }, cwd: '/srv/ivy' }).path, '/srv/ivy/profile');
  assert.equal(resolveCodexHome({ codexHome: 'D:\\User\\.codex', nativeHome: 'd:\\user\\.codex' }, { platform: 'win32' }).source, 'host-configuration');
});

test('only an explicit descendant belongs to the instance; shared homes and implicit defaults do not', () => {
  assert.equal(instanceOwnedCodexHome({ codexHome: '/srv/instance/data/codex' }, '/srv/instance/data', 'linux'), '/srv/instance/data/codex');
  for (const path of ['/home/ivy/.codex', '/srv/instance/data', '/srv/instance/data-other/codex', '/srv/instance/data/../other']) {
    assert.equal(instanceOwnedCodexHome({ codexHome: path }, '/srv/instance/data', 'linux'), null);
  }
  assert.equal(instanceOwnedCodexHome({}, '/srv/instance/data', 'linux'), null);
  assert.equal(instanceOwnedCodexHome({ nativeHome: 'D:\\instance\\data\\native' }, 'D:\\instance\\data', 'win32'), 'D:\\instance\\data\\native');
});

test('the host process boundary forwards the account home while explicit child overrides stay scoped', () => {
  const original = process.env['CODEX_HOME']; process.env['CODEX_HOME'] = '/synthetic/account-profile';
  try {
    assert.equal(runtimeEnvironment()['CODEX_HOME'], '/synthetic/account-profile');
    assert.equal(runtimeEnvironment({ CODEX_HOME: '/synthetic/child-profile' })['CODEX_HOME'], '/synthetic/child-profile');
    assert.equal(process.env['CODEX_HOME'], '/synthetic/account-profile');
  } finally { if (original === undefined) delete process.env['CODEX_HOME']; else process.env['CODEX_HOME'] = original; }
});

test('a future shared home cannot enter owned runtime through a linked ancestor', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-home-boundary-')), runtime = join(root, 'runtime'), alias = join(root, 'alias');
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(runtime); await symlink(runtime, alias, process.platform === 'win32' ? 'junction' : 'dir');
  const path = join(root, 'host.json');
  await writeFile(path, JSON.stringify({ schemaVersion: 1, hostId: 'test-home', publicBaseUrl: 'http://127.0.0.1:39080/ivy',
    runtimeRoot: runtime, artifactRoot: join(root, 'artifacts'), stagingRoot: join(root, 'staging'), executables: { node: process.execPath },
    instances: [{ instanceId: 'agent', serviceNodeId: 'test-agent', componentId: 'agent-manager', engine: 'process', enabled: false,
      settings: { codexHome: join(alias, 'future-home') } }] }));
  await assert.rejects(hostConfig(path), (error: unknown) => (error as { code?: string }).code === 'target_conflict');
  await assert.rejects(access(join(runtime, 'future-home')), { code: 'ENOENT' });
});
