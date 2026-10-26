import test from 'node:test';
import assert from 'node:assert/strict';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { buildNativeDeployment } from '../tools/build/build-native-deployment.mjs';

for (const [target, platform] of [
  ['x86_64-pc-windows-msvc', 'win32-x64'],
  ['x86_64-unknown-linux-gnu', 'linux-x64'],
]) test(`Chat package contains its selected native entry, health, instructions, catalogs and codec notices (${platform})`, () => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-chat-package-'));
  const previous = process.env.CARGO_TARGET_DIR;
  process.env.CARGO_TARGET_DIR = join(root, 'target');
  try {
    for (const file of ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'services/chat-bridge/deploy.json',
      'services/chat-bridge/rust/Cargo.toml', 'packages/host-runtime/rust/Cargo.toml',
      'instructions/chat-main-instructions.ts', 'instructions/chat-notice-prompt.ts']) {
      const destination = join(root, file);
      mkdirSync(dirname(destination), { recursive: true });
      writeFileSync(destination, readFileSync(file));
    }
    cpSync('services/chat-bridge/rust/licenses', join(root, 'services/chat-bridge/rust/licenses'), { recursive: true });
    mkdirSync(join(root, 'specs/native/example'), { recursive: true });
    writeFileSync(join(root, 'specs/native/example/catalog.json'), '{"retained":true}\n');
    const suffix = platform.startsWith('win32') ? '.exe' : '';
    const identity = { schemaVersion: 1, version: '1.0.0', buildId: 'sha256:' + 'a'.repeat(64) };
    const cargo = (args, options) => {
      const helper = args[0] === 'metadata' ? args.includes(join(root, 'packages/host-runtime/rust/Cargo.toml')) : args.includes('ivy-host-runtime');
      const name = helper ? 'ivy-host-runtime' : 'ivy-chat-bridge';
      const targets = (helper ? ['ivy-health', 'ivy-job'] : ['ivy-chat-bridge']).map(name => ({ name, kind: ['bin'],
        src_path: join(root, helper ? 'packages/host-runtime/rust/src' : 'services/chat-bridge/rust/src', name + '.rs') }));
      if (args[0] === 'metadata') return JSON.stringify({ workspace_members: [name], packages: [{ id: name, name,
        manifest_path: join(root, helper ? 'packages/host-runtime/rust/Cargo.toml' : 'services/chat-bridge/rust/Cargo.toml'), targets }] });
      return targets.filter(binary => args.includes(binary.name)).map(binary => {
        const executable = join(options.env.CARGO_TARGET_DIR, target, 'release', binary.name + suffix);
        mkdirSync(dirname(executable), { recursive: true }); writeFileSync(executable, 'fixture-' + binary.name);
        return JSON.stringify({ reason: 'compiler-artifact', package_id: name, target: binary, executable });
      }).join('\n');
    };
    const built = buildNativeDeployment({ component: 'chat-bridge', target, root, identity, cargo });
    const artifact = join(root, 'dist');
    assert.deepEqual(built.platforms, [platform]);
    for (const command of [built.plan.entrypoint, built.plan.readiness.command]) {
      assert.deepEqual(command.args, []);
      assert.ok(existsSync(join(root, command.executable)));
    }
    assert.deepEqual(readdirSync(join(artifact, 'native')), [platform]);
    assert.equal(existsSync(join(artifact, 'native', platform, 'ivy-job' + suffix)), platform.startsWith('win32'));
    assert.equal(JSON.parse(readFileSync(join(artifact, 'specs/native/example/catalog.json'))).retained, true);
    assert.equal(typeof JSON.parse(readFileSync(join(artifact, 'instructions/chat-main-instructions.json'))), 'string');
    assert.equal(JSON.parse(readFileSync(join(artifact, 'instructions/chat-notice-prompt.json'))).length, 3);
    for (const name of readdirSync('services/chat-bridge/rust/licenses')) {
      assert.deepEqual(readFileSync(join(artifact, 'licenses/chat-bridge', name)), readFileSync(join('services/chat-bridge/rust/licenses', name)));
    }
    assert.equal(existsSync(join(artifact, 'services/chat-bridge/src/main.js')), false);
    assert.equal(existsSync(join(artifact, 'node_modules')), false);
    const source = JSON.parse(readFileSync(resolve('services/chat-bridge/deploy.json')));
    assert.deepEqual(built.plan.storage, source.storage);
    assert.deepEqual(built.plan.runtimeReset, source.runtimeReset);
    assert.equal(built.stamp.version, source.version);
  } finally {
    if (previous === undefined) delete process.env.CARGO_TARGET_DIR; else process.env.CARGO_TARGET_DIR = previous;
    rmSync(root, { recursive: true, force: true });
  }
});
